//! `CappedLen` and `ReadBudget`: a reader whose answer about its own length is held down to a cap,
//! and a limit on the bytes it may hand out in total.
//!
//! A Matroska element states its size before its contents do, so a 200 KiB file can claim a 2 GiB
//! `CodecPrivate`, and a demuxer that trusts that allocates 2 GiB before it finds out.
//! `matroska-demuxer` measures the room left in a stream in exactly one place, the `remaining_len`
//! in its `ebml.rs`: `stream_position`, then `SeekFrom::End(0)`, then back to where it was. Every
//! allocation it makes on the strength of a declared size is checked against that answer first.
//!
//! That answer is per element, though, and a header is a list of elements. The demuxer measures the
//! room from each element's own position, never from the end of the element that contains it, so
//! eight `TrackEntry` elements can each claim 200 MiB of `CodecPrivate` over the same bytes and a
//! reader that bounds only one element at a time reads 1.6 GiB. [`CappedLen`] answers both
//! questions: [`ELEMENT_CAP`](crate::ELEMENT_CAP) is the room one element may be given, and
//! [`ReadBudget`] is the total the reader may spend, which is what a header's worth of elements is
//! measured against.
//!
//! Reads pass straight through, and so does every seek except `SeekFrom::End(0)`: seeking past the
//! cap is precisely how a demuxer skips an element it decided not to read, and `SeekFrom::End(n)`
//! with `n != 0` is a real position in a real file, which only the file's own length can answer.

use std::io::{Error as IoError, ErrorKind, Read, Seek, SeekFrom};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// How many bytes a reader may hand out in total, shared by everything reading through it.
///
/// The per element cap cannot bound a header, because a header is a sequence of elements and each
/// one is measured on its own. A budget is the number that is not per element: every byte the
/// reader hands out is subtracted from it, so a file that declares 1.6 GiB of `CodecPrivate`
/// across eight tracks costs eight reads' worth, not eight allocations' worth, and a `SeekHead`
/// that points at itself stops when the budget runs out instead of walking forever.
///
/// It is a shared counter ([`Arc`]) rather than a field, because the same budget has to cover
/// every reader a caller passes on, and because a caller that wants to bound a header and then
/// read media data through the same wrapper raises it with [`ReadBudget::lift`] instead of
/// building a second one.
#[derive(Debug, Clone)]
pub struct ReadBudget(Arc<AtomicU64>);

impl ReadBudget {
    /// A budget of `bytes` bytes.
    #[must_use]
    pub fn new(bytes: u64) -> Self {
        Self(Arc::new(AtomicU64::new(bytes)))
    }

    /// Raise the budget to everything a reader can hold.
    ///
    /// A header is bounded because it is untrusted: it is the part of the file that says how big
    /// the rest is. Media data is bounded by the fragments Task 2 cuts instead, so the header read
    /// is the last thing that needs a budget. Task 3 reuses [`CappedLen`] for playback and calls
    /// this once `open` has returned, which is why the budget is a shared counter and not a field
    /// of the wrapper.
    pub fn lift(&self) {
        self.0.store(u64::MAX, Ordering::Relaxed);
    }

    /// The bytes still unspent.
    #[must_use]
    pub fn left(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }

    /// Spend `bytes` of the budget, saturating at zero.
    fn spend(&self, bytes: u64) {
        // A compare exchange loop rather than `fetch_sub`: a reader that is handed a shared
        // budget can meet one that another reader has already spent down, and the subtraction
        // must clamp at zero instead of wrapping.
        let mut left = self.0.load(Ordering::Relaxed);
        loop {
            let spent = left.saturating_sub(bytes);
            match self
                .0
                .compare_exchange_weak(left, spent, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => return,
                Err(actual) => left = actual,
            }
        }
    }
}

/// A reader that reports at most `cap` bytes of room from wherever it stands, and at most what is
/// left of `budget` in total.
///
/// Feed it to [`probe`](crate::probe::probe) or to `matroska_demuxer::MatroskaFile::open` with
/// [`ELEMENT_CAP`](crate::ELEMENT_CAP) and no element it meets can be given more than that, however
/// large a number the file declared, and no run of elements can cost more than the budget.
#[derive(Debug)]
pub struct CappedLen<R> {
    inner: R,
    cap: u64,
    budget: ReadBudget,
}

impl<R> CappedLen<R> {
    /// Wraps `inner`, so that its length is answered as at most `cap` bytes from the current
    /// position and its reads are stopped once `budget` is spent.
    pub fn new(inner: R, cap: u64, budget: ReadBudget) -> Self {
        Self { inner, cap, budget }
    }
}

impl<R: Read> Read for CappedLen<R> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, IoError> {
        let left = self.budget.left();
        if left == 0 {
            // A budget of zero is not "no more room in the file", which is what an `EOF` would
            // say: it is this reader refusing to be asked for more, which is the demuxer's signal
            // that the file asked for more header than the file is worth.
            return Err(IoError::new(ErrorKind::InvalidData, "read budget spent"));
        }
        let wanted = left.min(u64::try_from(buf.len()).unwrap_or(u64::MAX));
        let limit = usize::try_from(wanted).unwrap_or(buf.len());
        let read = self.inner.read(&mut buf[..limit])?;
        self.budget.spend(u64::try_from(read).unwrap_or(u64::MAX));
        Ok(read)
    }
}

impl<R: Seek> Seek for CappedLen<R> {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64, IoError> {
        match pos {
            SeekFrom::End(0) => {
                let here = self.inner.stream_position()?;
                let real_end = self.inner.seek(SeekFrom::End(0))?;
                // The room a caller may plan an allocation against is the smaller of the cap and
                // what the budget has left, and the real end is the most there is. Land on the
                // smallest of the three, which is never past the end of the stream, so that a
                // reader asked where it is agrees with the number it was given.
                let room = self.cap.min(self.budget.left());
                let capped = real_end.min(here.saturating_add(room));
                self.inner.seek(SeekFrom::Start(capped))?;
                Ok(capped)
            }
            SeekFrom::Start(_) | SeekFrom::Current(_) | SeekFrom::End(_) => self.inner.seek(pos),
        }
    }
}
