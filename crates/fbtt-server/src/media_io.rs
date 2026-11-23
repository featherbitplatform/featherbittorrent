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
//! reader only when a read is about to happen somewhere other than where the engine reader stands. The
//! buffering a media route puts on top of it is [`SeekAwareReader`], which answers the same questions and
//! keeps the bytes it read across them, so a walk of frames costs one read per buffer of bytes instead of
//! one per frame.
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

/// A read buffer for a reader whose caller seeks as often as it reads, and almost always to a place the
/// buffer already holds.
///
/// `std::io::BufReader` gives its buffer up on a seek, which is the right answer for a reader that seeks
/// because it wants other bytes: there the seek is a promise that what is held is no longer what will be
/// read. The Matroska demuxer seeks several times a frame and never means that. `ebml::remaining_len`
/// asks where it stands and where the end of the stream is before it sizes anything, and comes back to
/// where it was; `next_frame` seeks back to a block's header before reading the block. Behind a
/// `BufReader` each of those costs the buffer and a fresh fill, so a cue scan pays one 64 KiB read per
/// frame of a subtitle track whose frames are a couple of hundred bytes.
///
/// This one keeps what it read. The questions and the way back are answered from its own bookkeeping:
/// `stream_position` is a seek to where it already stands, and `SeekFrom::End` is the length it was
/// opened with plus the delta, the same arithmetic [`BlockingReader`] does. A seek to a byte inside what
/// is held costs nothing either, so only a read that wants a byte outside it moves `inner` and fills
/// again. Over a torrent file, where a read is a request to a peer, that is the difference between a
/// fill per 64 KiB of media and a fill per frame.
///
/// `Read` hands out the bytes held, so it can answer with less than a caller asked for: `Read` allows
/// that, and `read_exact` loops over it. The buffer is all this holds beyond the reader, and it is
/// allocated once, so its capacity is the most bytes a reader like this costs.
pub(crate) struct SeekAwareReader<R> {
    /// The reader behind the buffer, moved and asked for bytes only when the byte wanted is not held.
    inner: R,
    /// The file's length, known when the reader was opened: the answer to `SeekFrom::End`, and the place
    /// after which there is nothing left to hand out.
    len: u64,
    /// Where the next read reads from. Reads move it and seeks set it, and `inner` is never asked where it
    /// stands: this is the position the caller is owed, and the buffer's bytes are placed against it.
    position: u64,
    /// The file offset of the first byte held.
    held_from: u64,
    /// The bytes held: a run of the file starting at `held_from`, sized to what the last fill read and
    /// keeping its capacity from fill to fill.
    bytes: Vec<u8>,
}

impl<R> SeekAwareReader<R> {
    /// Wraps any `Read + Seek` `inner`, a file `len` bytes long standing at `at`, with a buffer of
    /// `capacity` bytes. Nothing is read here: the first read fills.
    fn over(capacity: usize, inner: R, len: u64, at: u64) -> Self {
        Self {
            inner,
            len,
            position: at,
            held_from: at,
            bytes: Vec::with_capacity(capacity),
        }
    }

    /// Whether `at` names one of the bytes held.
    fn holds(&self, at: u64) -> bool {
        at >= self.held_from
            && at - self.held_from < u64::try_from(self.bytes.len()).unwrap_or(u64::MAX)
    }
}

impl<R: Read + Seek> SeekAwareReader<R> {
    /// Gives up what is held and reads the next buffer's worth of the file from where the caller stands.
    ///
    /// `inner` is moved first: this runs only when the byte the caller wants is outside what was held, and
    /// a reader that answers its own seeks never told `inner` about the places this one went to. The
    /// request stops at the file's length, so a fill never reads past the end looking for bytes that are
    /// not there. A fill that fails holds nothing, and the error path below says why: the buffer was
    /// resized to the room it asked for, which is zeros, and a retry that believed those were file bytes
    /// would walk on through a hole with no error to show for it.
    fn fill(&mut self) -> io::Result<()> {
        let at = self.position;
        self.inner.seek(SeekFrom::Start(at))?;
        let room = self
            .bytes
            .capacity()
            .min(usize::try_from(self.len - at).unwrap_or(usize::MAX));
        self.bytes.clear();
        self.bytes.resize(room, 0_u8);
        let got = match self.inner.read(&mut self.bytes) {
            Ok(got) => got,
            Err(problem) => {
                // The read asked for `room` bytes and the buffer is holding `room` zeros, which are
                // not the file's bytes. Empty it before handing the error over: a caller that retries
                // (`read_exact` retries `ErrorKind::Interrupted` on its own) would otherwise be told
                // the zeros are a run of the file, and the walk would go on with invented bytes and no
                // error to show for it. `clear` keeps the capacity, which is the point of the buffer.
                self.bytes.clear();
                return Err(problem);
            }
        };
        self.bytes.truncate(got);
        self.held_from = at;
        if got == 0 {
            // The file ends short of the length it was opened with. Remember the end that was found, so
            // every later read answers at once instead of asking `inner` the same question again.
            self.len = at;
        }
        Ok(())
    }
}

impl<R> BlockingReader<R> {
    /// Wraps this reader in a [`SeekAwareReader`] whose buffer holds `capacity` bytes.
    ///
    /// The length this reader was opened knowing goes with it, which is what lets the buffer answer a
    /// question about the end of the file without reaching the engine. See [`SeekAwareReader`] for what
    /// that buys a route that reads a container frame by frame.
    pub(crate) fn buffered_across_seeks(self, capacity: usize) -> SeekAwareReader<Self> {
        let (len, at) = (self.len, self.position);
        SeekAwareReader::over(capacity, self, len, at)
    }
}

impl<R: Read + Seek> Read for SeekAwareReader<R> {
    /// The bytes held, or one seek and one fill of `inner` and then the bytes held.
    ///
    /// A read at or past the end is answered here, the way [`BlockingReader`] answers it, without telling
    /// the engine anything.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || self.position >= self.len {
            return Ok(0);
        }
        if !self.holds(self.position) {
            self.fill()?;
        }
        let from = usize::try_from(self.position - self.held_from).unwrap_or(usize::MAX);
        let give = buf.len().min(self.bytes.len().saturating_sub(from));
        if give == 0 {
            // A fill at this place found the end of the file, which is what leaves nothing to hand out
            // while this reader still believes it has room: the file is shorter than it said.
            return Ok(0);
        }
        buf[..give].copy_from_slice(&self.bytes[from..from + give]);
        self.position = self
            .position
            .saturating_add(u64::try_from(give).unwrap_or(u64::MAX));
        Ok(give)
    }
}

impl<R> Seek for SeekAwareReader<R> {
    /// Sets where the next read reads from, and nothing else.
    ///
    /// The buffer is what makes this free: a seek inside the bytes held is answered from them, and the two
    /// questions a demuxer asks on the way to a frame (here, and the end) are arithmetic here too. `inner`
    /// hears about the position only when a fill reads from it.
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
    use std::io::{Read, Seek, SeekFrom};
    use std::pin::Pin;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll};
    use std::time::Duration;

    use tokio::io::{AsyncRead, AsyncSeek, ReadBuf};
    use tokio::runtime::Handle;
    use tokio::sync::watch;

    use super::{BlockingReader, SeekAwareReader};

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

            // The cue route's path: the same demuxer behind the route's buffer, scanning frames for
            // the embedded SubRip track. That buffer answers the demuxer's per-frame questions about
            // position and length, so the engine should hear about none of them.
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
            )
            .buffered_across_seeks(64 * 1024);
            let cues = fbtt_media::embedded_cues(reader, 4, 0, 10_000).expect("the embedded cues");
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

    /// A file in memory that counts the reads it was asked for: the reader under [`SeekAwareReader`]
    /// below, where the number worth knowing is how often the buffer had to ask for more bytes.
    struct Counting {
        bytes: Vec<u8>,
        at: u64,
        reads: Arc<AtomicUsize>,
    }

    impl Read for Counting {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.reads.fetch_add(1, Ordering::Relaxed);
            let from = usize::try_from(self.at)
                .unwrap_or(usize::MAX)
                .min(self.bytes.len());
            let give = buf.len().min(self.bytes.len() - from);
            buf[..give].copy_from_slice(&self.bytes[from..from + give]);
            self.at += give as u64;
            Ok(give)
        }
    }

    impl Seek for Counting {
        fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
            // The buffer answers every seek it is given, and moves what is under it only to read from
            // where it stands, so a move is always a place to read from, never a relative question.
            let SeekFrom::Start(at) = pos else {
                panic!(
                    "the reader under the buffer was sent {pos:?}; it only ever needs a place to read from"
                );
            };
            self.at = at;
            Ok(at)
        }
    }

    /// Frames read one at a time cost one read of the file per buffer of them, not one per frame.
    ///
    /// The shape is the cue scan's, counted at the reader under the buffer rather than at the engine:
    /// 40 frames of 3 KiB taken from a 300 KiB file, so 120 KiB of it is read. On the way to each
    /// frame the walk asks where it stands, asks where the end of the stream is the way
    /// `ebml::remaining_len` does (a capped reader measures the room for an element from the end, so
    /// that lands on the end of the file and comes back), and seeks back to the block's header before
    /// reading the block.
    ///
    /// The bytes moved are 120 KiB whatever the buffer does, so what is being measured is how often
    /// those reads ask the reader underneath for more than they use. Bytes kept across the seeks cost
    /// one fill per 64 KiB of media; bytes given up at every seek cost one fill per frame, each of
    /// them a request to a peer for a piece of a film.
    #[test]
    fn reading_frame_by_frame_costs_one_read_per_buffer_fill() {
        /// The cue route's buffer: `HEADER_READ` in `routes::media`.
        const BUFFER: usize = 64 * 1024;
        const FRAME: usize = 3 * 1024;
        const FRAMES: usize = 40;
        const FILE: usize = 300 * 1024;

        let reads = Arc::new(AtomicUsize::new(0));
        let mut file = SeekAwareReader::over(
            BUFFER,
            Counting {
                bytes: vec![0x41_u8; FILE],
                at: 0,
                reads: Arc::clone(&reads),
            },
            FILE as u64,
            0,
        );
        let mut frame = vec![0_u8; FRAME];
        // A header's worth of bytes in front of the first frame, so the first fill does not start on
        // a round buffer line.
        file.seek(SeekFrom::Start(4)).expect("past the header");
        for round in 0..FRAMES {
            let here = file.stream_position().expect("where the walk stands");
            // `remaining_len`, and the capped reader's answer to it: where am I, where is the end,
            // land on the end (the cap is 256 MiB and this file is 300 KiB) and back here.
            let end = file.seek(SeekFrom::End(0)).expect("where the end is");
            assert_eq!(
                end, FILE as u64,
                "round {round}: the end is the length this reader was opened knowing"
            );
            file.seek(SeekFrom::Start(end))
                .expect("the capped answer lands on the end");
            file.seek(SeekFrom::Start(here))
                .expect("and the walk goes back to where it stood");
            let header = file.stream_position().expect("the block's own start");
            file.seek(SeekFrom::Start(header))
                .expect("back to the header to read the block");
            file.read_exact(&mut frame).expect("the frame's bytes");
            assert!(
                frame.iter().all(|byte| *byte == 0x41),
                "round {round}: the frame is the file's own bytes at the place the walk stood"
            );
            file.seek(SeekFrom::Start(here + FRAME as u64))
                .expect("the next frame follows this one");
        }
        let got = reads.load(Ordering::Relaxed);
        // 120 KiB of frames out of a file is two fills' worth of a 64 KiB buffer, and one more is the
        // slack for a walk that started mid-buffer. A reader that throws its buffer away at each seek
        // pays one per frame instead, which is `FRAMES` of them.
        let fills = (FRAMES * FRAME).div_ceil(BUFFER);
        assert!(
            got > 0 && got <= fills + 1,
            "{FRAMES} frames of {FRAME} bytes read {} of the file through {got} reads of the reader \
             under the buffer; keeping the buffer is about one read per {BUFFER} of them ({fills} \
             for these frames), and giving it up at every seek is one per frame ({FRAMES})",
            FRAMES * FRAME,
        );
    }

    /// A file whose next read fails once with `Interrupted` and whose later reads answer normally: the
    /// one error a caller retries by itself, which is what makes a half filled buffer dangerous.
    struct OnceInterrupted {
        bytes: Vec<u8>,
        at: u64,
        /// Whether the next read fails instead of answering.
        fail: bool,
    }

    impl Read for OnceInterrupted {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.fail {
                self.fail = false;
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "the read was interrupted",
                ));
            }
            let from = usize::try_from(self.at)
                .unwrap_or(usize::MAX)
                .min(self.bytes.len());
            let give = buf.len().min(self.bytes.len() - from);
            buf[..give].copy_from_slice(&self.bytes[from..from + give]);
            self.at += give as u64;
            Ok(give)
        }
    }

    impl Seek for OnceInterrupted {
        fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
            let SeekFrom::Start(at) = pos else {
                panic!(
                    "the reader under the buffer was sent {pos:?}; it only ever needs a place to read from"
                );
            };
            self.at = at;
            Ok(at)
        }
    }

    /// A fill that fails holds nothing, so the retry reads the file instead of the failed read's zeros.
    ///
    /// A fill empties its buffer, resizes it to the room it means to read, and only then asks the
    /// reader underneath for bytes. In between, the buffer holds `room` zeros, and those are not the
    /// file's bytes. `read_exact` retries `ErrorKind::Interrupted` on its own, so one interrupted
    /// read is enough for a caller to come back for the same bytes and be handed them: a hole read as
    /// a run of the file, the walk going on from there with no error to show for it.
    #[test]
    fn a_failed_fill_holds_nothing() {
        let mut file = SeekAwareReader::over(
            4,
            OnceInterrupted {
                bytes: b"abcdefgh".as_slice().to_vec(),
                at: 0,
                fail: true,
            },
            8,
            0,
        );
        let mut buf = [0_u8; 4];
        // The first read fails inside the fill, `read_exact` asks again, and the second answer has to
        // come from the file rather than from what the failed read left in the buffer.
        file.read_exact(&mut buf).expect("the retry reads the file");
        assert_eq!(
            &buf, b"abcd",
            "the retried read gives the file's own bytes, not the failed fill's zeros"
        );
        assert_eq!(
            file.stream_position().expect("where the read landed"),
            4,
            "the retry moved the position the way a read does"
        );
        // And the buffer keeps working from the file afterwards: the next read fills again, and this
        // time the read underneath answers.
        file.read_exact(&mut buf)
            .expect("the walk goes on after the retry");
        assert_eq!(
            &buf, b"efgh",
            "the second fill reads the bytes after the ones it retried"
        );
    }
}
