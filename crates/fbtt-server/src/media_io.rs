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
//!   read and every seek here carries the calling route's timeout and gives up on it, leaving an
//!   [`std::io::ErrorKind::TimedOut`] for the route to turn into its `504`.
//! - **What if the viewer hung up mid-read?** The bytes a media route produces are consumed by a response
//!   body, and a body that is dropped is nobody waiting for those bytes any more. The route hands over a
//!   `watch::Receiver` whose sender it holds; when that sender goes, the next `poll` of a pending read
//!   returns instead of going on waiting, and the reader, its engine slot and the route's permit go with it.
//!
//! A reader built here is used from inside `tokio::task::spawn_blocking` and nowhere else: that is what
//! makes [`Handle::block_on`] legal here (a blocking thread is not a runtime worker) and what keeps a slow
//! piece from ever sitting on one.

use std::io::{self, Read, Seek, SeekFrom};
use std::time::Duration;

use fbt::FileReader;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::runtime::Handle;
use tokio::select;
use tokio::sync::watch;
use tokio::time::timeout;

/// A torrent file that a blocking thread may read, one bounded read or seek at a time.
///
/// Built from the engine's [`FileReader`] and handed to fbtt-media, which asks of it what it asks of a file:
/// read here, seek there, never more than one thing at a time. See [`crate::media_io`] for why a bridge is
/// not enough and what the two leashes are for.
///
/// A read or seek that hits its deadline ends the caller's work: nothing here asks the reader for a second
/// thing after a timeout, because the first request may still be in flight inside the engine and a
/// cancelled one is not answered out from under it. A route that times out answers and lets this go.
pub(crate) struct BlockingReader {
    /// The engine's own reader for one file of one torrent: verified pieces only, and the read-ahead the
    /// engine keeps warm for whoever is playing.
    reader: FileReader,
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
    /// Wraps `reader`, to be driven on the runtime `handle` names, with `timeout` on each read and seek and
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
        BlockingReader {
            reader,
            handle,
            timeout,
            stop,
        }
    }
}

impl Read for BlockingReader {
    /// One read, driven to completion or to one of the two ends: the deadline, or the client going away.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let Self {
            reader,
            handle,
            timeout: budget,
            stop,
        } = self;
        handle.block_on(async {
            select! {
                // The response this read feeds is gone: nobody is waiting for these bytes, so stop waiting
                // for the piece that would fill them.
                _ = stop.changed() => Err(hung_up()),
                // The read either answers, or the deadline reaches it first.
                outcome = timeout(*budget, reader.read(buf)) => match outcome {
                    Ok(read) => read,
                    Err(_) => Err(timed_out("reading a torrent file")),
                },
            }
        })
    }
}

impl Seek for BlockingReader {
    /// One seek. Seeks are answered from the torrent's metadata rather than from the wire, so the deadline
    /// here is the engine's own book-keeping taking too long, not a piece that has not arrived.
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let Self {
            reader,
            handle,
            timeout: budget,
            stop,
        } = self;
        handle.block_on(async {
            select! {
                _ = stop.changed() => Err(hung_up()),
                outcome = timeout(*budget, reader.seek(pos)) => match outcome {
                    Ok(seeked) => seeked,
                    Err(_) => Err(timed_out("seeking in a torrent file")),
                },
            }
        })
    }
}

/// The verdict for a read or seek that ran out its time: the kind fbt-lib itself leaves for a piece that
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
