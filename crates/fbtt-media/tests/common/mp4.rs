// An MP4 box and sample-table model for the mov_text cue tests.
//
// The production cue read answers questions about a track it has just parsed. A test that aims at
// one sample needs the same answers plus the byte position each table is written at: it changes one
// number in one table, and it has to know which sample that changes and which bytes the reader will
// then look at. This module is that second half. It walks containers by the lengths they state for
// themselves to get positions, and it takes every value from `mp4-atom`, the library the reader
// parses with, then checks the two against each other: the box walked to is parsed and compared to
// the box the `moov` parse produced, and each table's stated length and every one of its words is
// compared to the parsed table. A patch made here is therefore a patch at a number the reader
// reads, and a file that stops matching this model fails here rather than aiming a test at the
// wrong sample.
//
// It is a test's model, not a second reader. The sample walk is the walk `mp4in::text_samples`
// walks - runs of `stts` durations, chunk sizes from `stsc`, a chunk's bytes accumulated from
// `stsz` - and not the binary search the production seek uses. It insists the tables agree about the
// sample count and that every sample's bytes lie inside the file. Those are not all the checks the
// reader makes; they are the ones these answers would be wrong without.
use std::io::Cursor;

use fbtt_media::Cue;
use mp4_atom::{
    Atom, Codec, FourCC, Header, Moov, ReadAtom as _, ReadFrom, Stbl, StscEntry, Stsz, StszSamples,
    SttsEntry,
};

/// A track's timescale, the box its samples are looked up in, and the tables inside that box.
///
/// The positions are the reason [`locate`] exists: a patch is a write at an offset, and the reader
/// re-reads a table from the file, so a write has to sit inside the box the reader seeks to. The
/// tables travel with the track because placing a sample needs all of them, and because checking
/// them against the bytes they were read from is what makes those positions worth trusting.
pub struct Track {
    pub track_id: u32,
    /// Ticks per second, from this track's `mdhd`: the timeline its sample times are counted in.
    pub timescale: u64,
    /// The sample table box: where it starts, and how long the bytes inside its header are.
    pub stbl: SampleTable,
    /// The boxes on the path down to the sample table box. Each states a length that covers it, so
    /// a rewrite of the tables grows or shrinks every one of them by what the tables changed by.
    moov_at: usize,
    trak_at: usize,
    mdia_at: usize,
    minf_at: usize,
    /// The sample description box, which a rewrite copies rather than rebuilds.
    stsd_at: usize,
    stsd_len: usize,
    /// Where `stsz` sits: the box a single sample's length is patched inside.
    stsz_at: usize,
    sizes: Sizes,
    /// `stts`: `(duration ticks, sample count)` runs, in the order the table writes them.
    runs: Vec<(u32, u32)>,
    /// `stsc`: `(first chunk, samples per chunk, sample description index)` groups.
    groups: Vec<(u32, u32, u32)>,
    /// `stco`: where each chunk starts.
    offsets: Vec<u32>,
}

/// The sample table box, as the bytes a rewrite replaces.
pub struct SampleTable {
    pub at: usize,
    /// Its stated length less its header: the body a rewrite hands back in place of.
    pub body_len: usize,
}

/// One sample: the index the tables look it up by, and the bytes its `stsz` entry states at the
/// offset the chunk tables place it at.
pub struct Sample {
    pub index: u32,
    pub offset: u64,
    pub size: u32,
    /// Presentation time in the track's ticks. This model calls a sample's decode time its
    /// presentation time, which [`locate`] allows only for a track with no `ctts` to move it.
    pub pts_ticks: i128,
    duration: u64,
}

/// `stsz`, in whichever shape the box states it in.
///
/// ISO 14496-12 §8.7.3 holds either one length for every sample, or a zero there followed by the
/// count and one length per sample. `mp4-atom`'s `StszSamples` takes the same branch, and `mp4in`
/// maps both onto a length per sample, so these are the shapes the reader works from.
enum Sizes {
    Identical { count: u32, size: u32 },
    Different(Vec<u32>),
}

impl Sizes {
    /// How many samples the table states lengths for.
    fn count(&self) -> usize {
        match self {
            Sizes::Identical { count, .. } => *count as usize,
            Sizes::Different(sizes) => sizes.len(),
        }
    }

    /// The length sample `index` (0-based) claims for itself.
    fn at(&self, index: usize) -> u32 {
        match self {
            Sizes::Identical { size, .. } => *size,
            Sizes::Different(sizes) => sizes[index],
        }
    }
}

/// The track a test is built for, found by the id the file states for it, with its boxes located.
///
/// `mp4in` resolves a track number through `tkhd`, so taking the first `trak` box would model some
/// other track. The track is refused unless the reader would read cues from it: a handler of `sbtl`,
/// `subt` or `text`, the three `probe::mp4_track` counts as a subtitle track, and a first sample
/// description of `tx3g`, which is what makes the reader's codec `mov_text` instead of one it
/// refuses. The tables are then each checked against the box they were parsed from, so the offsets
/// this module hands back name the fields the reader reads.
pub fn locate(buf: &[u8], want: u32) -> Track {
    let (moov_at, moov_len, moov) = parse_moov(buf);
    let traks: Vec<(usize, usize)> = children(buf, moov_at, moov_len)
        .into_iter()
        .filter(|(kind, _, _)| *kind == *b"trak")
        .map(|(_, at, len)| (at, len))
        .collect();
    assert_eq!(
        traks.len(),
        moov.trak.len(),
        "the walk of the moov at {moov_at} found {} trak boxes and mp4-atom parsed {}",
        traks.len(),
        moov.trak.len()
    );
    let index = moov
        .trak
        .iter()
        .position(|trak| trak.tkhd.track_id == want)
        .unwrap_or_else(|| panic!("no trak box in this file names track {want}"));
    let trak = &moov.trak[index];
    let stbl_of = &trak.mdia.minf.stbl;
    let (trak_at, trak_len) = traks[index];
    let (mdia_at, mdia_len) = child(buf, trak_at, trak_len, b"mdia");
    let (minf_at, minf_len) = child(buf, mdia_at, mdia_len, b"minf");
    let (stbl_at, stbl_len) = child(buf, minf_at, minf_len, b"stbl");
    assert_eq!(
        read_box::<Stbl>(buf, stbl_at, stbl_len),
        *stbl_of,
        "the sample table box walked to at {stbl_at} is not the box mp4-atom parsed for track {want}"
    );

    let handler = trak.mdia.hdlr.handler;
    assert!(
        [
            FourCC::new(b"sbtl"),
            FourCC::new(b"subt"),
            FourCC::new(b"text")
        ]
        .contains(&handler),
        "track {want}'s handler is {handler}, which is not a subtitle track's"
    );
    assert!(
        matches!(stbl_of.stsd.codecs.first(), Some(Codec::Tx3g(..))),
        "track {want}'s first sample description is not a tx3g, so its samples are not mov_text"
    );
    assert!(
        stbl_of.ctts.is_none(),
        "track {want} carries a ctts, whose composition offsets this model does not apply"
    );

    let (stsd_at, stsd_len) = child(buf, stbl_at, stbl_len, b"stsd");
    let (stts_at, stts_len) = child(buf, stbl_at, stbl_len, b"stts");
    let (stsc_at, stsc_len) = child(buf, stbl_at, stbl_len, b"stsc");
    let (stsz_at, stsz_len) = child(buf, stbl_at, stbl_len, b"stsz");
    let (stco_at, stco_len) = child(buf, stbl_at, stbl_len, b"stco");
    check_stts(buf, stts_at, stts_len, &stbl_of.stts.entries);
    check_stsc(buf, stsc_at, stsc_len, &stbl_of.stsc.entries);
    let sizes = check_stsz(buf, stsz_at, stsz_len, &stbl_of.stsz);
    let offsets = stbl_of
        .stco
        .as_ref()
        .expect("a track whose samples this model places has a stco chunk offset table")
        .entries
        .as_slice();
    check_stco(buf, stco_at, stco_len, offsets);

    Track {
        track_id: want,
        timescale: u64::from(trak.mdia.mdhd.timescale),
        stbl: SampleTable {
            at: stbl_at,
            body_len: stbl_len - header_len(buf, stbl_at),
        },
        moov_at,
        trak_at,
        mdia_at,
        minf_at,
        stsd_at,
        stsd_len,
        stsz_at,
        sizes,
        runs: stbl_of
            .stts
            .entries
            .iter()
            .map(|entry| (entry.sample_delta, entry.sample_count))
            .collect(),
        groups: stbl_of
            .stsc
            .entries
            .iter()
            .map(|entry| {
                (
                    entry.first_chunk,
                    entry.samples_per_chunk,
                    entry.sample_description_index,
                )
            })
            .collect(),
        offsets: offsets.to_vec(),
    }
}

/// The file's `moov`: where it starts, how long it is, and what `mp4-atom` reads inside it.
///
/// Boxes ahead of it are skipped by their own lengths, as `probe::read_moov` skips them, and the
/// scan stops at the `moov` because `read_moov` stops there: the reads a test's reader makes before
/// it touches a sample are header reads, never media bytes.
fn parse_moov(buf: &[u8]) -> (usize, usize, Moov) {
    let mut at = 0_usize;
    while let Some((kind, len, _)) = header(buf, at, buf.len()) {
        if kind == *b"moov" {
            return (at, len, read_box::<Moov>(buf, at, len));
        }
        at += len;
    }
    panic!("this file has no moov box, so it has no track to model");
}

/// A box's contents, read by `mp4-atom` out of the bytes the file states for that box.
///
/// `mp4-atom` fixes where each field of a table sits, so handing it the box the file describes -
/// rather than a slice of the file whose length this module guessed - is what keeps this module's
/// box the reader's box.
fn read_box<T: Atom>(buf: &[u8], at: usize, len: usize) -> T {
    let mut cursor = Cursor::new(&buf[at..at + len]);
    let header = <Option<Header> as ReadFrom>::read_from(&mut cursor)
        .expect("a box header")
        .expect("a box at the position this module walked to");
    assert_eq!(
        header.kind,
        T::KIND,
        "the box at {at} is a {} box, not the {} box this module came looking for",
        header.kind,
        T::KIND,
    );
    T::read_atom(&header, &mut cursor)
        .expect("a box this module located by walking the lengths its parents state")
}

/// One word of a box's contents, as the tables write it: big-endian, like every number in this
/// container (ISO 14496-12 §1.2).
fn word(buf: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]])
}

/// One box header: its kind, the whole length it states, and the length of the header that states
/// it.
///
/// A length smaller than the header carrying it, or longer than the space its parent allows, is not
/// a box; a length of `0` runs the box to the end of that space (ISO 14496-12 §4.2). Anywhere a
/// length is not one of the forms the specification allows, the walk stops rather than inventing a
/// sibling out of bytes that are somebody else's data.
fn header(buf: &[u8], at: usize, end: usize) -> Option<([u8; 4], usize, usize)> {
    if at + 8 > end {
        return None;
    }
    let size = u32::from_be_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]]);
    let kind = [buf[at + 4], buf[at + 5], buf[at + 6], buf[at + 7]];
    let mut declared = u64::from(size);
    let mut head = 8_usize;
    if size == 1 {
        if at + 16 > end {
            return None;
        }
        declared = u64::from_be_bytes([
            buf[at + 8],
            buf[at + 9],
            buf[at + 10],
            buf[at + 11],
            buf[at + 12],
            buf[at + 13],
            buf[at + 14],
            buf[at + 15],
        ]);
        head = 16;
    }
    if declared == 0 {
        declared = (end - at) as u64;
    }
    if declared < head as u64 || declared > (end - at) as u64 {
        return None;
    }
    Some((kind, declared as usize, head))
}

/// How many bytes a box's header takes: sixteen when it states a 64-bit length after its kind, and
/// eight otherwise.
fn header_len(buf: &[u8], at: usize) -> usize {
    if u32::from_be_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]]) == 1 {
        16
    } else {
        8
    }
}

/// The boxes inside one container, in the order they are written, bounded by the length that
/// container states for itself.
///
/// A container's children are where the container says they are, inside its own stated length,
/// which is the only thing that can tell a box apart from four bytes of sample data that happen to
/// spell the same four characters: a declared length is any 32-bit value at all.
fn children(buf: &[u8], at: usize, len: usize) -> Vec<([u8; 4], usize, usize)> {
    let end = at + len;
    let mut at = at + header_len(buf, at);
    let mut out = Vec::new();
    while let Some((kind, child_len, _)) = header(buf, at, end) {
        out.push((kind, at, child_len));
        at += child_len;
    }
    out
}

/// The one box of one kind inside a container. Two of them would mean this model and the reader
/// disagree about which box holds a track's times, its lengths or its chunk offsets.
fn child(buf: &[u8], at: usize, len: usize, kind: &[u8; 4]) -> (usize, usize) {
    let name = fourcc(kind);
    let found: Vec<(usize, usize)> = children(buf, at, len)
        .into_iter()
        .filter(|(found, _, _)| found == kind)
        .map(|(_, at, len)| (at, len))
        .collect();
    assert_eq!(
        found.len(),
        1,
        "the container at {at} holds {} boxes named {name}, and a track has one {name}",
        found.len()
    );
    found[0]
}

fn fourcc(kind: &[u8; 4]) -> String {
    String::from_utf8_lossy(kind).into_owned()
}

/// `stts`, checked against the box it was parsed from.
///
/// ISO 14496-12 §8.7.3.1: an entry count, then one row of eight bytes per entry, `(sample_count,
/// sample_delta)`. A box whose stated length or words disagree with the parsed table is a box whose
/// offsets this module could not name honestly, so both are checked.
fn check_stts(buf: &[u8], at: usize, len: usize, entries: &[SttsEntry]) {
    assert_eq!(
        len,
        16 + 8 * entries.len(),
        "the stts box at {at} states {len} bytes, which is not an entry count and eight bytes per \
         entry"
    );
    assert_eq!(
        word(buf, at + 8),
        0,
        "the stts box at {at} is not version 0 with no flags, which is all this module rewrites"
    );
    assert_eq!(
        usize::try_from(word(buf, at + 12)).expect("an entry count that fits a length"),
        entries.len(),
        "the stts box at {at} states a row count mp4-atom did not read"
    );
    for (index, entry) in entries.iter().enumerate() {
        let row = at + 16 + 8 * index;
        assert_eq!(
            word(buf, row),
            entry.sample_count,
            "the stts row at {row} states a sample count mp4-atom did not read"
        );
        assert_eq!(
            word(buf, row + 4),
            entry.sample_delta,
            "the stts row at {} states a duration mp4-atom did not read",
            row + 4
        );
    }
}

/// `stsc`, checked against the box it was parsed from: an entry count, then one twelve-byte
/// `(first_chunk, samples_per_chunk, sample_description_index)` group per entry (§8.7.4).
fn check_stsc(buf: &[u8], at: usize, len: usize, entries: &[StscEntry]) {
    assert_eq!(
        len,
        16 + 12 * entries.len(),
        "the stsc box at {at} states {len} bytes, which is not an entry count and twelve bytes per \
         group"
    );
    assert_eq!(
        word(buf, at + 8),
        0,
        "the stsc box at {at} is not version 0 with no flags, which is all this module rewrites"
    );
    assert_eq!(
        usize::try_from(word(buf, at + 12)).expect("an entry count that fits a length"),
        entries.len(),
        "the stsc box at {at} states a group count mp4-atom did not read"
    );
    for (index, entry) in entries.iter().enumerate() {
        let row = at + 16 + 12 * index;
        assert_eq!(
            word(buf, row),
            entry.first_chunk,
            "the stsc row at {row} states a first chunk mp4-atom did not read"
        );
        assert_eq!(
            word(buf, row + 4),
            entry.samples_per_chunk,
            "the stsc row at {} states a chunk size mp4-atom did not read",
            row + 4
        );
        assert_eq!(
            word(buf, row + 8),
            entry.sample_description_index,
            "the stsc row at {} states a description index mp4-atom did not read",
            row + 8
        );
    }
}

/// `stsz`, checked against the box it was parsed from, and the lengths it states.
///
/// ISO 14496-12 §8.7.3: one length for every sample, or `0` there and then the count and one length
/// per sample. The per-sample form is where one sample's length is patched, so its entry offsets
/// matter, and every length is checked against the parsed table so that a patch lands on the number
/// `mp4in` builds a sample's length from.
fn check_stsz(buf: &[u8], at: usize, len: usize, stsz: &Stsz) -> Sizes {
    assert_eq!(
        word(buf, at + 8),
        0,
        "the stsz box at {at} is not version 0 with no flags, which is all this module rewrites"
    );
    match &stsz.samples {
        StszSamples::Identical { count, size } => {
            assert_eq!(
                len, 20,
                "the stsz box at {at} states {len} bytes, which is more than one length and a count"
            );
            assert_eq!(
                word(buf, at + 12),
                *size,
                "the stsz box at {at} does not state the length mp4-atom read out of it"
            );
            assert_eq!(
                word(buf, at + 16),
                *count,
                "the stsz box at {at} does not state the sample count mp4-atom read out of it"
            );
            Sizes::Identical {
                count: *count,
                size: *size,
            }
        }
        StszSamples::Different { sizes } => {
            assert_eq!(
                len,
                20 + 4 * sizes.len(),
                "the stsz box at {at} states {len} bytes, which is not a zero length, a count and \
                 four bytes per sample"
            );
            assert_eq!(
                word(buf, at + 12),
                0,
                "the length at {} is not zero, so every sample is that long and no sample has an \
                 entry of its own",
                at + 12
            );
            assert_eq!(
                usize::try_from(word(buf, at + 16)).expect("a sample count that fits a length"),
                sizes.len(),
                "the stsz box at {at} states a sample count mp4-atom did not read"
            );
            for (index, size) in sizes.iter().enumerate() {
                let entry = at + 20 + 4 * index;
                assert_eq!(
                    word(buf, entry),
                    *size,
                    "the stsz entry at {entry} states a length mp4-atom did not read"
                );
            }
            Sizes::Different(sizes.clone())
        }
    }
}

/// `stco`, checked against the box it was parsed from: a count, then one 32-bit chunk offset per
/// chunk (§8.7.5).
fn check_stco(buf: &[u8], at: usize, len: usize, offsets: &[u32]) {
    assert_eq!(
        len,
        16 + 4 * offsets.len(),
        "the stco box at {at} states {len} bytes, which is not a count and four bytes per chunk"
    );
    assert_eq!(
        word(buf, at + 8),
        0,
        "the stco box at {at} is not version 0 with no flags, which is all this module rewrites"
    );
    assert_eq!(
        usize::try_from(word(buf, at + 12)).expect("a chunk count that fits a length"),
        offsets.len(),
        "the stco box at {at} states a chunk count mp4-atom did not read"
    );
    for (index, offset) in offsets.iter().enumerate() {
        let entry = at + 16 + 4 * index;
        assert_eq!(
            word(buf, entry),
            *offset,
            "the stco entry at {entry} states a chunk offset mp4-atom did not read"
        );
    }
}

/// The track's samples, walked the way `mp4in::text_samples` walks them.
///
/// A chunk's samples are contiguous, so a sample's offset is its chunk's `stco` offset plus the
/// bytes of it already consumed; `stts` says how long each sample lasts and `stsc` says how many
/// share a chunk, the last group running to the end of the offset table. The count `stsz` states is
/// checked against the samples `stts` covers and against the room `stsc` and the offsets hold,
/// because a model built on tables that disagree about the count walks the wrong number of samples,
/// and against the file, because a sample whose bytes lie past its end is a sample no test can
/// place at all.
pub fn samples(buf: &[u8], track: &Track) -> Vec<Sample> {
    assert!(
        !track.runs.is_empty() && !track.groups.is_empty(),
        "track {} has no stts run or no stsc group, so its samples have no times and no place",
        track.track_id
    );
    let count = track.sizes.count();
    let timed: u128 = track.runs.iter().map(|run| u128::from(run.1)).sum();
    assert_eq!(
        timed, count as u128,
        "track {}'s stts covers {} samples and its stsz states lengths for {}",
        track.track_id, timed, count
    );
    let capacity: u128 = track
        .groups
        .iter()
        .enumerate()
        .map(|(index, group)| {
            u128::from(chunks_in_group(
                &track.groups,
                index,
                track.offsets.len() as u64,
            )) * u128::from(group.1)
        })
        .sum();
    assert_eq!(
        capacity, count as u128,
        "track {}'s stsc and chunk offsets hold room for {} samples and its stsz states lengths \
         for {}",
        track.track_id, capacity, count
    );
    for group in &track.groups {
        assert_eq!(
            group.2, 1,
            "track {} samples from sample description {}, which the reader refuses and this model \
             does not follow",
            track.track_id, group.2
        );
    }

    let mut out = Vec::with_capacity(count);
    let mut ticks = 0_i128;
    let mut run = 0_usize;
    let mut run_left = track.runs[run].1;
    let mut group = 0_usize;
    let mut chunks_left = chunks_in_group(&track.groups, 0, track.offsets.len() as u64);
    let mut in_chunk = track.groups[group].1;
    let mut chunk = 0_usize;
    let mut chunk_bytes = 0_u64;
    for index in 0..count {
        let duration = u64::from(track.runs[run].0);
        let size = track.sizes.at(index);
        let offset = u64::from(track.offsets[chunk]) + chunk_bytes;
        assert!(
            offset + u64::from(size) <= buf.len() as u64,
            "sample {index} of track {track} claims {size} bytes at {offset}, past the {bytes} \
             bytes of this file",
            track = track.track_id,
            bytes = buf.len(),
        );
        out.push(Sample {
            index: index as u32,
            offset,
            size,
            pts_ticks: ticks,
            duration,
        });
        ticks += i128::from(duration);
        run_left -= 1;
        if run_left == 0 && run + 1 < track.runs.len() {
            run += 1;
            run_left = track.runs[run].1;
        }
        if in_chunk == 1 {
            chunk += 1;
            chunk_bytes = 0;
            chunks_left -= 1;
            if chunks_left == 0 && group + 1 < track.groups.len() {
                group += 1;
                chunks_left = chunks_in_group(&track.groups, group, track.offsets.len() as u64);
            }
            in_chunk = track.groups[group].1;
        } else {
            in_chunk -= 1;
            chunk_bytes += u64::from(size);
        }
    }
    out
}

/// How many chunks `stsc` group `index` covers: up to the next group's first chunk, and to the end
/// of the offset table for the last one, which is how `mp4in::chunks_in_group` counts them.
fn chunks_in_group(groups: &[(u32, u32, u32)], index: usize, chunks: u64) -> u64 {
    let first = u64::from(groups[index].0);
    let limit = match groups.get(index + 1) {
        Some(next) => u64::from(next.0),
        None => chunks + 1,
    };
    let span = limit
        .checked_sub(first)
        .expect("stsc groups start in order, each before the chunk the one before ends on");
    assert_ne!(
        span, 0,
        "an stsc group starts at the chunk it would have to end on, so it covers none"
    );
    span
}

/// The cues the track's own tables put on screen, over the whole track: the production cue read's
/// gap rule, its 2-byte length prefix and its lossy text, with none of its bounds applied.
///
/// A sample of no bytes, or of a length prefix of nothing, is the gap a muxer writes between cues:
/// the walk reads it, counts it, and makes no cue of it. What the reader refuses about a file - a
/// length past a sample's own bytes, a total past a cap - is what the tests this module serves are
/// about, so this says only what the tables mean.
pub fn cues(buf: &[u8], track: &Track) -> Vec<Cue> {
    let mut out = Vec::new();
    for sample in samples(buf, track) {
        if sample.size < 2 {
            continue;
        }
        let at = usize::try_from(sample.offset).expect("a sample offset is a place in a file");
        let end = at + usize::try_from(sample.size).expect("a sample length is a number of bytes");
        let record = &buf[at..end];
        let len = u16::from_be_bytes([record[0], record[1]]) as usize;
        assert!(
            2 + len <= record.len(),
            "sample {index} states {len} bytes of text inside the {bytes} bytes it holds",
            index = sample.index,
            bytes = record.len(),
        );
        if len == 0 {
            continue;
        }
        out.push(Cue {
            start_ms: ms_from_ticks(sample.pts_ticks, track.timescale),
            end_ms: ms_from_ticks(
                sample.pts_ticks + i128::from(sample.duration),
                track.timescale,
            ),
            text: String::from_utf8_lossy(&record[2..2 + len]).into_owned(),
        });
    }
    out
}

/// The window's two tick targets, rounded as `mp4in::ticks_for` rounds them: the start target a
/// landing may sit at or before, and the end target a cue walk stops at.
pub fn window_ticks(track: &Track, from_ms: u64, to_ms: u64) -> (i128, i128) {
    (
        ticks_for(from_ms, track.timescale, false),
        ticks_for(to_ms, track.timescale, true),
    )
}

/// The track's ticks for a millisecond value: the product and the division by a thousand
/// `mp4in::ticks_for` takes, rounding up only for the window's end target.
fn ticks_for(ms: u64, timescale: u64, ceil: bool) -> i128 {
    let product = u128::from(ms) * u128::from(timescale);
    let ticks = if ceil {
        product.div_ceil(1_000)
    } else {
        product / 1_000
    };
    i128::try_from(ticks)
        .expect("a time this model converts fits the timeline the reader answers in")
}

/// Milliseconds from ticks: `mp4in::ms_from_ticks`'s scale, with its clamp of a negative time to `0`
/// and of a time past a `u64` to that type's maximum.
fn ms_from_ticks(ticks: i128, timescale: u64) -> u64 {
    let ms = ticks * 1_000 / i128::from(timescale);
    match u64::try_from(ms) {
        Ok(ms) => ms,
        Err(_) if ticks < 0 => 0,
        Err(_) => u64::MAX,
    }
}

/// Where the file's media data begins: just past the header of its `mdat` box.
///
/// `mdat` is found by its kind among the top level boxes rather than assumed to follow the `moov`,
/// because a file with its media in front is a file whose tables name bytes that have already gone
/// by. Chunk offsets name places in this file, so a rewrite that moves the media has to know where
/// it landed.
pub fn mdat_payload(buf: &[u8]) -> usize {
    let mut at = 0_usize;
    while let Some((kind, len, head)) = header(buf, at, buf.len()) {
        if kind == *b"mdat" {
            return at + head;
        }
        at += len;
    }
    panic!("this file has no mdat box for a chunk offset to point at");
}

/// Where sample `index`'s (0-based) declared length sits in the file: the four bytes a size patch
/// writes.
///
/// Only the per-sample form has an entry to write. In the form that states one length for all
/// samples, one sample's length is that field, and changing it changes every sample at once.
pub fn stsz_entry_at(track: &Track, index: u32) -> usize {
    match &track.sizes {
        Sizes::Identical { count, .. } => panic!(
            "the stsz box at {} states one length for all {count} samples, so sample {index} has \
             no entry of its own",
            track.stsz_at
        ),
        Sizes::Different(sizes) => {
            let index = usize::try_from(index).expect("a sample index that fits a count");
            assert!(
                index < sizes.len(),
                "the stsz box at {} states lengths for {} samples, so it has no entry {index}",
                track.stsz_at,
                sizes.len()
            );
            // Header, version and flags, the all-samples length and the count: the entries follow
            // them, four bytes apiece, which is where `mp4-atom` reads them.
            track.stsz_at + 20 + 4 * index
        }
    }
}

/// A full box around fields this module wrote: the header, and the zero version and flags every one
/// of these tables carries.
fn full_box(kind: &[u8; 4], fields: &[u8]) -> Vec<u8> {
    let len = 12 + fields.len();
    assert!(
        u32::try_from(len).is_ok(),
        "a table of {len} bytes needs the 64-bit length this module does not write"
    );
    let mut out = Vec::with_capacity(len);
    out.extend_from_slice(&(len as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(fields);
    out
}

/// `stts` as a box: one `(sample_count, sample_delta)` row per entry.
pub fn stts_box(runs: &[(u32, u32)]) -> Vec<u8> {
    let mut fields = Vec::with_capacity(4 + 8 * runs.len());
    fields.extend_from_slice(&(runs.len() as u32).to_be_bytes());
    for (sample_count, sample_delta) in runs {
        fields.extend_from_slice(&sample_count.to_be_bytes());
        fields.extend_from_slice(&sample_delta.to_be_bytes());
    }
    full_box(b"stts", &fields)
}

/// `stsc` as a box: one `(first_chunk, samples_per_chunk)` group per entry, every one of them
/// sampling from the single description `stsd` holds.
pub fn stsc_box(groups: &[(u32, u32)]) -> Vec<u8> {
    let mut fields = Vec::with_capacity(4 + 12 * groups.len());
    fields.extend_from_slice(&(groups.len() as u32).to_be_bytes());
    for (first_chunk, samples_per_chunk) in groups {
        fields.extend_from_slice(&first_chunk.to_be_bytes());
        fields.extend_from_slice(&samples_per_chunk.to_be_bytes());
        fields.extend_from_slice(&1u32.to_be_bytes());
    }
    full_box(b"stsc", &fields)
}

/// `stsz` as a box in the form that states `size` for all `count` samples: no entry array, so a
/// track of a million samples costs eight bytes instead of four million.
pub fn stsz_box(count: u32, size: u32) -> Vec<u8> {
    let mut fields = Vec::with_capacity(8);
    fields.extend_from_slice(&size.to_be_bytes());
    fields.extend_from_slice(&count.to_be_bytes());
    full_box(b"stsz", &fields)
}

/// `stco` as a box, naming one chunk offset each.
pub fn stco_box(offsets: &[u32]) -> Vec<u8> {
    let mut fields = Vec::with_capacity(4 + 4 * offsets.len());
    fields.extend_from_slice(&(offsets.len() as u32).to_be_bytes());
    for offset in offsets {
        fields.extend_from_slice(&offset.to_be_bytes());
    }
    full_box(b"stco", &fields)
}

/// The body of a rewritten sample table box: the file's own `stsd` copied whole, then the tables a
/// test rebuilt, in the order a sample table holds them.
///
/// `stsd` is copied because a rewrite here is about times, lengths and offsets. It is the box that
/// makes these samples text to the reader at all, and rebuilding it would put this module's guess
/// about a box it never reads in the way of the verdict.
pub fn stbl_body(buf: &[u8], track: &Track, tables: &[&[u8]]) -> Vec<u8> {
    let want = [b"stts", b"stsc", b"stsz", b"stco"];
    assert_eq!(
        tables.len(),
        want.len(),
        "a rewritten sample table is this file's stsd plus {} tables",
        want.len()
    );
    let mut out =
        Vec::with_capacity(track.stsd_len + tables.iter().map(|table| table.len()).sum::<usize>());
    out.extend_from_slice(&buf[track.stsd_at..track.stsd_at + track.stsd_len]);
    for (table, kind) in tables.iter().zip(want) {
        let (found, _, _) =
            header(table, 0, table.len()).expect("a rebuilt table that states its own length");
        assert_eq!(
            found,
            *kind,
            "a rewritten sample table holds a {} box where a reader expects a {} box",
            fourcc(&found),
            fourcc(kind)
        );
        out.extend_from_slice(table);
    }
    out
}

/// The same file with one track's sample tables replaced by `stbl_body`.
///
/// The splice sits inside five boxes that each state a length covering it - `moov`, `trak`, `mdia`,
/// `minf`, and the `stbl` whose body is being replaced - and all five start before it, so each is
/// corrected at its own offset and each by the same amount. Everything the splice moves keeps its
/// own length, which is right because nothing outside the chunk offsets states a position, and those
/// are the caller's to write: `stbl_body` takes the offsets a test chose.
pub fn rewrite_stbl(buf: &[u8], track: &Track, stbl_body: &[u8]) -> Vec<u8> {
    let growth = stbl_body.len() as i64 - track.stbl.body_len as i64;
    let stbl_len = 8 + stbl_body.len();
    assert!(
        u32::try_from(stbl_len).is_ok(),
        "a sample table of {stbl_len} bytes needs the 64-bit length this module does not write"
    );
    let mut out = Vec::with_capacity(((buf.len() as i64) + growth).max(0) as usize);
    out.extend_from_slice(&buf[..track.stbl.at]);
    out.extend_from_slice(&(stbl_len as u32).to_be_bytes());
    out.extend_from_slice(b"stbl");
    out.extend_from_slice(stbl_body);
    out.extend_from_slice(&buf[track.stbl.at + 8 + track.stbl.body_len..]);
    for at in [track.moov_at, track.trak_at, track.mdia_at, track.minf_at] {
        assert!(
            at < track.stbl.at,
            "the box at {at} states a length covering the sample tables at {}, and it starts after \
             them",
            track.stbl.at
        );
        let (_, len, _) = header(&out, at, out.len()).expect("a box that states its own length");
        let grown = (len as i64) + growth;
        assert!(
            u32::try_from(grown).is_ok(),
            "the box at {at} would need a 64-bit length to cover the replaced tables"
        );
        out[at..at + 4].copy_from_slice(&(grown as u32).to_be_bytes());
    }
    out
}
