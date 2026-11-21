//! `CappedLen` and `ReadBudget`: a reader whose answer about its own length is held down to a cap,
//! and a limit on the bytes and the read and seek calls it may spend in total.
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
//! measured against. Bytes alone do not bound a hostile header either: a `SeekHead` that points at
//! itself gets through on a few dozen bytes a pass, so a budget counts calls beside bytes, and
//! [`MAX_HEADER_OPS`] is the most a header is worth of them.
//!
//! Reads pass straight through, and so does every seek except `SeekFrom::End(0)`: seeking past the
//! cap is precisely how a demuxer skips an element it decided not to read, and `SeekFrom::End(n)`
//! with `n != 0` is a real position in a real file, which only the file's own length can answer.

use std::io::{Error as IoError, ErrorKind, Read, Seek, SeekFrom};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// The most read and seek calls one header is worth.
///
/// Bytes bound what a header can make a reader hand out, but not how many times it can ask: a
/// `SeekHead` that points at itself reads only a few dozen bytes a pass and would still cost
/// hundreds of millions of calls on an [`ELEMENT_CAP`](crate::ELEMENT_CAP) byte budget, and in
/// production every call
/// goes through a blocking reader. A 2 hour MKV with a cue point every second costs about 200k
/// calls, so a million is a real header's worth with room to spare.
pub const MAX_HEADER_OPS: u64 = 1 << 20;

/// How many bytes a reader may hand out in total, shared by everything reading through it.
///
/// The per element cap cannot bound a header, because a header is a sequence of elements and each
/// one is measured on its own. A budget is the number that is not per element: every byte the
/// reader hands out is subtracted from it, so a file that declares 1.6 GiB of `CodecPrivate`
/// across eight tracks costs eight reads' worth, not eight allocations' worth, and a `SeekHead`
/// that points at itself stops when the budget runs out instead of walking forever. Bytes are
/// half of it: every `read` and every `seek` through [`CappedLen`] also spends one of
/// [`MAX_HEADER_OPS`] calls, because a loop that reads a few dozen bytes a pass empties a byte
/// budget in hundreds of millions of calls, not a million.
///
/// It is a shared counter ([`Arc`]) rather than a field, because the same budget has to cover
/// every reader a caller passes on, and because a caller that wants to bound a header and then
/// read media data through the same wrapper raises it with [`ReadBudget::lift`] instead of
/// building a second one.
#[derive(Debug, Clone)]
pub struct ReadBudget {
    bytes: Arc<AtomicU64>,
    ops: Arc<AtomicU64>,
}

impl ReadBudget {
    /// A budget of `bytes` bytes and [`MAX_HEADER_OPS`] calls.
    #[must_use]
    pub fn new(bytes: u64) -> Self {
        Self {
            bytes: Arc::new(AtomicU64::new(bytes)),
            ops: Arc::new(AtomicU64::new(MAX_HEADER_OPS)),
        }
    }

    /// Raise both counters to everything a reader can hold.
    ///
    /// A header is bounded because it is untrusted: it is the part of the file that says how big
    /// the rest is. Media data is bounded by the fragments Task 2 cuts instead, so the header read
    /// is the last thing that needs a budget. Task 3 reuses [`CappedLen`] for playback and calls
    /// this once `open` has returned, which is why the budget is a shared counter and not a field
    /// of the wrapper.
    pub fn lift(&self) {
        self.bytes.store(u64::MAX, Ordering::Relaxed);
        self.ops.store(u64::MAX, Ordering::Relaxed);
    }

    /// The bytes still unspent.
    #[must_use]
    pub fn left(&self) -> u64 {
        self.bytes.load(Ordering::Relaxed)
    }

    /// Spend `bytes` of the byte budget, saturating at zero.
    fn spend(&self, bytes: u64) {
        // A compare exchange loop rather than `fetch_sub`: a reader that is handed a shared
        // budget can meet one that another reader has already spent down, and the subtraction
        // must clamp at zero instead of wrapping.
        let mut left = self.bytes.load(Ordering::Relaxed);
        loop {
            let spent = left.saturating_sub(bytes);
            match self.bytes.compare_exchange_weak(
                left,
                spent,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(actual) => left = actual,
            }
        }
    }

    /// Spend one call of the call budget, reporting whether a call was left to spend.
    ///
    /// Same shared-counter arithmetic as the byte half, at unit scale: whoever runs out of calls
    /// first is the reader that says no.
    fn try_spend_op(&self) -> bool {
        let mut left = self.ops.load(Ordering::Relaxed);
        loop {
            let next = match left.checked_sub(1) {
                Some(next) => next,
                None => return false,
            };
            match self
                .ops
                .compare_exchange_weak(left, next, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => return true,
                Err(actual) => left = actual,
            }
        }
    }
}

/// The refusal both halves of a [`ReadBudget`] give when they are spent.
///
/// It is an `InvalidData`, not an `EOF`: the stream has bytes left, this reader is refusing to
/// hand them out, which is the demuxer's signal that the file asked for more header than the
/// file is worth.
fn budget_spent() -> IoError {
    IoError::new(ErrorKind::InvalidData, "read budget spent")
}

/// A reader that reports at most `cap` bytes of room from wherever it stands, and at most what is
/// left of `budget` in bytes and calls.
///
/// Feed it to [`probe`](crate::probe::probe) or to `matroska_demuxer::MatroskaFile::open` with
/// [`ELEMENT_CAP`](crate::ELEMENT_CAP) and no element it meets can be given more than that, however
/// large a number the file declared, no run of elements can cost more than the budget, and no
/// header can cost more than [`MAX_HEADER_OPS`] calls.
#[derive(Debug)]
pub struct CappedLen<R> {
    inner: R,
    cap: u64,
    budget: ReadBudget,
}

impl<R> CappedLen<R> {
    /// Wraps `inner`, so that its length is answered as at most `cap` bytes from the current
    /// position, its reads are stopped once the budget's bytes are spent, and both stop once its
    /// calls are spent.
    pub fn new(inner: R, cap: u64, budget: ReadBudget) -> Self {
        Self { inner, cap, budget }
    }
}

impl<R: Read> Read for CappedLen<R> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, IoError> {
        if !self.budget.try_spend_op() {
            return Err(budget_spent());
        }
        let left = self.budget.left();
        if left == 0 {
            // A budget of zero is not "no more room in the file", which is what an `EOF` would
            // say: it is this reader refusing to be asked for more, which is the demuxer's signal
            // that the file asked for more header than the file is worth.
            return Err(budget_spent());
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
        // A seek is a round trip to the file too, and a looping `SeekHead` asks for one about as
        // often as it asks for bytes, so the call budget is charged here as well.
        if !self.budget.try_spend_op() {
            return Err(budget_spent());
        }
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
