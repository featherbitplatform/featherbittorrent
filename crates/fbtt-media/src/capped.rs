//! `CappedLen`: a reader whose answer about its own length is held down to a cap.
//!
//! A Matroska element states its size before its contents do, so a 200 KiB file can claim a 2 GiB
//! `CodecPrivate`, and a demuxer that trusts that allocates 2 GiB before it finds out.
//! `matroska-demuxer` measures the room left in a stream in exactly one place, the `remaining_len`
//! in its `ebml.rs`: `stream_position`, then `SeekFrom::End(0)`, then back to where it was. Every
//! allocation it makes on the strength of a declared size is checked against that answer first.
//!
//! Wrapping the reader in [`CappedLen`] answers that one question with "at most `cap` bytes from
//! where you are now", so no element can push a `Vec` past `cap`. Reads pass straight through, and
//! so does every seek except `SeekFrom::End(0)`: seeking past the cap is precisely how a demuxer
//! skips an element it decided not to read, and `SeekFrom::End(n)` with `n != 0` is a real position
//! in a real file, which only the file's own length can answer.

use std::io::{Error as IoError, Read, Seek, SeekFrom};

/// A reader that reports at most `cap` bytes of room from wherever it stands.
///
/// Feed it to `matroska_demuxer::MatroskaFile::open` with [`ELEMENT_CAP`](crate::ELEMENT_CAP) and
/// no element it meets can allocate more than that, however large a number the file declared.
#[derive(Debug)]
pub struct CappedLen<R> {
    inner: R,
    cap: u64,
}

impl<R> CappedLen<R> {
    /// Wraps `inner`, so that its length is answered as at most `cap` bytes from the current
    /// position.
    pub fn new(inner: R, cap: u64) -> Self {
        Self { inner, cap }
    }
}

impl<R: Read> Read for CappedLen<R> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, IoError> {
        self.inner.read(buf)
    }
}

impl<R: Seek> Seek for CappedLen<R> {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64, IoError> {
        match pos {
            SeekFrom::End(0) => {
                let here = self.inner.stream_position()?;
                let real_end = self.inner.seek(SeekFrom::End(0))?;
                // The cap is the room we are willing to admit to, and the real end is the most
                // there is. Land on the smaller of the two, which is never past the end of the
                // stream, so that a reader asked where it is agrees with the number it was given.
                let capped = real_end.min(here.saturating_add(self.cap));
                self.inner.seek(SeekFrom::Start(capped))?;
                Ok(capped)
            }
            SeekFrom::Start(_) | SeekFrom::Current(_) | SeekFrom::End(_) => self.inner.seek(pos),
        }
    }
}
