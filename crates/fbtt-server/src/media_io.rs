//! The one way a media route puts a torrent's file onto a blocking thread.
//!
//! fbtt-media reads a container through `std::io`, and fbt's [`FileReader`] is a tokio `AsyncRead`: the
//! media routes do their container work on the blocking pool, where nothing may await, so something has to
//! turn each blocking read and seek into one short drive of the runtime's engine. That is this file's whole
//! job, and it is deliberately not `std::io::SyncIoBridge`: a bridge parks a worker on the reader and offers
//! no deadline, so a piece that never arrives would take the thread with it. [`BlockingReader`] answers the
//! two questions a torrent has that a file does not.
//!
//! - **How long may one read take?** The engine is waiting on peers for a piece that may never come. Every
//!   read here, with the engine seek it may start with, carries the calling route's timeout and gives up
//!   on it, leaving an [`std::io::ErrorKind::TimedOut`] for the route to turn into its `504`.
//! - **What if the viewer hung up mid-read?** The bytes a media route produces are consumed by a response
//!   body, and a body that is dropped is nobody waiting for those bytes any more. The route hands over a
//!   `watch::Receiver` whose sender it holds; when that sender goes, the next `poll` of a pending read
//!   returns instead of going on waiting, and the reader, its engine slot and the route's permit go with it.
//!
//! There is a third thing a torrent minds that a file does not: **where the reader is.** The engine fetches
//! ahead of its reader's position, so every seek of the engine reader is news to the engine, even one that
//! lands where the reader already was. The demuxers ask the stream where it is and how long it is for every
//! frame (a `stream_position`, a seek to the end and a seek back), and none of that is somewhere to read.
//! [`BlockingReader`] answers those from its own position and the file's length, and moves the engine
//! reader only when a read is about to happen somewhere other than where the engine reader stands.
//!
//! A reader built here is used from inside `tokio::task::spawn_blocking` and nowhere else: that is what
//! makes [`Handle::block_on`] legal here (a blocking thread is not a runtime worker) and what keeps a slow
//! piece from ever sitting on one.

use std::io::{self, Read, Seek, SeekFrom};
use std::time::Duration;

use fbt::FileReader;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeek, AsyncSeekExt};
use tokio::runtime::Handle;
use tokio::select;
use tokio::sync::watch;
use tokio::time::timeout;

/// A torrent file that a blocking thread may read, one bounded read at a time.
///
/// Built from the engine's [`FileReader`] and handed to fbtt-media, which asks of it what it asks of a file:
/// read here, seek there, never more than one thing at a time. See [`crate::media_io`] for why a bridge is
/// not enough and what the two leashes are for.
///
/// A read that hits its deadline ends the caller's work: nothing here asks the reader for a second thing
/// after a timeout, because the first request may still be in flight inside the engine and a cancelled one
/// is not answered out from under it. A route that times out answers and lets this go.
///
/// `R` is the engine's [`FileReader`] everywhere but this file's tests, which put a recording reader in its
/// place to see every move the engine would have been told about.
pub(crate) struct BlockingReader<R = FileReader> {
    /// The engine's own reader for one file of one torrent: verified pieces only, and the read-ahead the
    /// engine keeps warm for whoever is playing.
    reader: R,
    /// The file's length, known when the reader was opened, so that a seek from the end is arithmetic.
    len: u64,
    /// Where the caller is: the place the next read reads from and the answer to every seek. Seeks move
    /// this and nothing else.
    position: u64,
    /// Where the engine reader is, or `None` after a read that failed and may have left it anywhere. A read
    /// seeks the engine reader first only when this is not `position`.
    engine_at: Option<u64>,
    /// The runtime the engine runs on, captured by the handler that opened the file. `block_on` needs it,
    /// and it needs to be asked from inside a runtime, which a handler is and a blocking thread is not.
    handle: Handle,
    /// How long one read or one seek may take before it becomes [`io::ErrorKind::TimedOut`]. The media
    /// route that built this reader took it from [`crate::routes::Shared`], where the operator - or a test -
    /// sets it.
    timeout: Duration,
    /// The watch whose sender the response holds. Its `changed()` is not interested in values - nobody ever
    /// sends on the channel - it is interested in the sender going away, which is the client hanging up or
    /// the response ending, and the only thing that can wake a read that is waiting for a piece.
    stop: watch::Receiver<()>,
}

impl BlockingReader {
    /// Wraps `reader`, to be driven on the runtime `handle` names, with `timeout` on each read and
    /// `stop` as the hang-up signal.
    ///
    /// `handle` must come from inside a runtime ([`Handle::current`] from a handler), and this must only be
    /// used from a blocking thread: see [`crate::media_io`]. `stop`'s sender has to be held by whoever is
    /// consuming what is read, or the wake never arrives.
    pub(crate) fn new(
        reader: FileReader,
        handle: Handle,
        timeout: Duration,
        stop: watch::Receiver<()>,
    ) -> Self {
        let (len, at) = (reader.len(), reader.position());
        BlockingReader::over(reader, len, at, handle, timeout, stop)
    }
}

impl<R> BlockingReader<R> {
    /// Wraps any engine-shaped `reader` that is `len` bytes long and stands at `at`.
    fn over(
        reader: R,
        len: u64,
        at: u64,
        handle: Handle,
        timeout: Duration,
        stop: watch::Receiver<()>,
    ) -> Self {
        BlockingReader {
            reader,
            len,
            position: at,
            engine_at: Some(at),
            handle,
            timeout,
            stop,
        }
    }
}

impl<R: AsyncRead + AsyncSeek + Unpin> Read for BlockingReader<R> {
    /// One read, driven to completion or to one of the two ends: the deadline, or the client going away.
    ///
    /// The engine reader is moved first only when it is not already where this read starts, and a read at
    /// or past the end is answered here, the way the engine would answer it, without telling the engine
    /// anything.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || self.position >= self.len {
            return Ok(0);
        }
        let Self {
            reader,
            position,
            engine_at,
            handle,
            timeout: budget,
            stop,
            ..
        } = self;
        let at = *position;
        let moved = *engine_at != Some(at);
        let outcome = handle.block_on(async {
            select! {
                // The response this read feeds is gone: nobody is waiting for these bytes, so stop waiting
                // for the piece that would fill them.
                _ = stop.changed() => Err(hung_up()),
                // The read either answers, or the deadline reaches it first. A seek of the engine reader is
                // answered from the torrent's metadata, so it shares the read's deadline rather than
                // having one of its own.
                outcome = timeout(*budget, async {
                    if moved {
                        reader.seek(SeekFrom::Start(at)).await?;
                    }
                    reader.read(buf).await
                }) => match outcome {
                    Ok(read) => read,
                    Err(_) => Err(timed_out("reading a torrent file")),
                },
            }
        });
        match outcome {
            Ok(read) => {
                *position = at.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
                *engine_at = Some(*position);
                Ok(read)
            }
            Err(problem) => {
                // Wherever the engine reader is now, the next read puts it back before it asks.
                *engine_at = None;
                Err(problem)
            }
        }
    }
}

impl<R> Seek for BlockingReader<R> {
    /// One seek, answered here from this reader's position and the file's length. The engine reader is not
    /// touched: the engine is told where the reader is when a read happens there, and not before.
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let target = match pos {
            SeekFrom::Start(at) => Some(at),
            SeekFrom::End(delta) => offset(self.len, delta),
            SeekFrom::Current(delta) => offset(self.position, delta),
        }
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek before the file start"))?;
        self.position = target;
        Ok(target)
    }
}

/// `base + delta`, saturating above and `None` below zero: the arithmetic fbt's own reader does for a seek.
fn offset(base: u64, delta: i64) -> Option<u64> {
    if delta >= 0 {
        Some(base.saturating_add(delta.unsigned_abs()))
    } else {
        base.checked_sub(delta.unsigned_abs())
    }
}

/// The verdict for a read that ran out its time: the kind fbt-lib itself leaves for a piece that
/// did not arrive in time, so the media route's `504` cannot tell the engine's deadline from this one.
fn timed_out(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, format!("{what} timed out"))
}

/// The verdict for a read whose response went away mid-read. Not an engine failure and not the file's:
/// nobody asked any more, which is the one reason a media stream has to stop that is not its own error.
fn hung_up() -> io::Error {
    io::Error::new(
        io::ErrorKind::BrokenPipe,
        "the client stopped reading this stream",
    )
}

#[cfg(test)]
mod tests {
    use std::io::{BufReader, Read, Seek, SeekFrom};
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll};
    use std::time::Duration;

    use tokio::io::{AsyncRead, AsyncSeek, ReadBuf};
    use tokio::runtime::Handle;
    use tokio::sync::watch;

    use super::BlockingReader;

    /// One thing the engine would have been told: its reader moved to a place, or read bytes from one.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Told {
        Moved(u64),
        Read { at: u64, len: usize },
    }

    /// The engine's reader as far as [`BlockingReader`] can see it: a file in memory, every seek and every
    /// read written down. fbt's own reader tells its torrent about every seek it is given, so a seek here
    /// is exactly a move the engine would have heard about.
    struct Recording {
        bytes: Arc<[u8]>,
        at: u64,
        told: Arc<Mutex<Vec<Told>>>,
    }

    impl AsyncRead for Recording {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            let this = self.get_mut();
            let from = usize::try_from(this.at)
                .unwrap_or(usize::MAX)
                .min(this.bytes.len());
            let len = buf.remaining().min(this.bytes.len() - from);
            buf.put_slice(&this.bytes[from..from + len]);
            this.told
                .lock()
                .unwrap()
                .push(Told::Read { at: this.at, len });
            this.at += len as u64;
            Poll::Ready(Ok(()))
        }
    }

    impl AsyncSeek for Recording {
        fn start_seek(self: Pin<&mut Self>, position: SeekFrom) -> std::io::Result<()> {
            let this = self.get_mut();
            let SeekFrom::Start(to) = position else {
                panic!(
                    "the engine reader was sent {position:?}; it only ever needs a place to read from"
                );
            };
            this.at = to;
            this.told.lock().unwrap().push(Told::Moved(to));
            Ok(())
        }

        fn poll_complete(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<std::io::Result<u64>> {
            Poll::Ready(Ok(self.at))
        }
    }

    /// The media crate's multi-track Matroska fixture, the file the server's media tests seed.
    fn fixture() -> Arc<[u8]> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../fbtt-media/tests/fixtures/h264-aac-opus-srt-ass.mkv");
        std::fs::read(path).expect("read the media fixture").into()
    }

    /// Checks what the engine was told while a demuxer walked the file: it never heard of the end of the
    /// file, and it never heard of a place nobody then read from. Every move is followed at once by a read
    /// at exactly that place.
    fn assert_moves_only_to_reads(what: &str, told: &[Told], len: u64) {
        let reads = told
            .iter()
            .filter(|t| matches!(t, Told::Read { .. }))
            .count();
        assert!(reads > 0, "{what}: nothing was read");
        for (i, t) in told.iter().enumerate() {
            let Told::Moved(to) = *t else { continue };
            assert!(
                to < len,
                "{what}: the engine reader was moved to {to}, at or past the end ({len}); told: {told:?}"
            );
            assert!(
                matches!(told.get(i + 1), Some(Told::Read { at, .. }) if *at == to),
                "{what}: the engine reader was moved to {to} and the next thing was {:?}, not a read there",
                told.get(i + 1)
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn demuxing_moves_the_engine_reader_only_to_where_it_reads() {
        let bytes = fixture();
        let len = bytes.len() as u64;
        let handle = Handle::current();
        tokio::task::spawn_blocking(move || {
            // The stream route's path: the demuxer straight over the reader, every frame of the video track.
            let told = Arc::new(Mutex::new(Vec::new()));
            let (_hold, stop) = watch::channel(());
            let reader = BlockingReader::over(
                Recording {
                    bytes: bytes.clone(),
                    at: 0,
                    told: told.clone(),
                },
                len,
                0,
                handle.clone(),
                Duration::from_secs(5),
                stop,
            );
            let mut stream = fbtt_media::open_track(reader, 1, 0).expect("open the video track");
            let mut fragments = 0;
            while stream.next_fragment().expect("a fragment").is_some() {
                fragments += 1;
            }
            assert!(fragments > 1, "the video track gave {fragments} fragments");
            assert_moves_only_to_reads("streaming", &told.lock().unwrap(), len);

            // The cue route's path: the same demuxer behind the route's 64 KiB buffer, scanning frames for
            // the embedded SubRip track.
            let told = Arc::new(Mutex::new(Vec::new()));
            let (_hold, stop) = watch::channel(());
            let reader = BlockingReader::over(
                Recording {
                    bytes,
                    at: 0,
                    told: told.clone(),
                },
                len,
                0,
                handle,
                Duration::from_secs(5),
                stop,
            );
            let cues = fbtt_media::embedded_cues(
                BufReader::with_capacity(64 * 1024, reader),
                4,
                0,
                10_000,
            )
            .expect("the embedded cues");
            assert!(!cues.is_empty(), "the fixture's SubRip track has cues");
            assert_moves_only_to_reads("cue scan", &told.lock().unwrap(), len);
        })
        .await
        .expect("the blocking job");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn seeks_are_answered_without_the_engine_and_reads_go_where_they_point() {
        let bytes: Arc<[u8]> = (0..=255u8).collect::<Vec<u8>>().into();
        let handle = Handle::current();
        let told = Arc::new(Mutex::new(Vec::new()));
        let recorded = told.clone();
        tokio::task::spawn_blocking(move || {
            let (_hold, stop) = watch::channel(());
            let mut reader = BlockingReader::over(
                Recording {
                    bytes,
                    at: 0,
                    told: recorded,
                },
                256,
                0,
                handle,
                Duration::from_secs(5),
                stop,
            );
            assert_eq!(reader.stream_position().unwrap(), 0);
            assert_eq!(reader.seek(SeekFrom::End(0)).unwrap(), 256);
            assert_eq!(reader.seek(SeekFrom::End(-6)).unwrap(), 250);
            assert_eq!(reader.seek(SeekFrom::Current(-50)).unwrap(), 200);
            assert!(reader.seek(SeekFrom::Current(-201)).is_err());
            assert_eq!(reader.stream_position().unwrap(), 200);
            let mut two = [0u8; 2];
            reader.read_exact(&mut two).unwrap();
            assert_eq!(two, [200, 201]);
            reader.read_exact(&mut two).unwrap();
            assert_eq!(two, [202, 203]);
            reader.seek(SeekFrom::End(0)).unwrap();
            assert_eq!(reader.read(&mut two).unwrap(), 0);
        })
        .await
        .expect("the blocking job");
        assert_eq!(
            *told.lock().unwrap(),
            vec![
                Told::Moved(200),
                Told::Read { at: 200, len: 2 },
                Told::Read { at: 202, len: 2 },
            ],
            "one move, to where the first read was, and nothing for the seeks or the read at the end"
        );
    }
}
