//! Fragmented MP4: the init segment one track, and the fragments of samples
//! that follow it.
//!
//! The bytes produced here are what the web player hands `MediaSource`: an init
//! segment ([`init_segment`]) that says what the track is, then a run of
//! fragments ([`FragmentWriter`]) that say where in the timeline its samples sit.
//! Everything is `mp4_atom` boxes; nothing here writes MP4 by hand.
//!
//! The one thing the container does not give us for free are decode times.
//! Matroska stores only presentation times, and a B-frame's presentation order is
//! not its decode order, so the writer synthesizes decode times per fragment the
//! way the plan spells it out: sort the fragment's PTS, pair
//! them with decode order, and the largest misalignment is how far back the
//! first sample's decode time has to start. The `TIME_OFFSET_MS` shift then
//! pushes the whole timeline out of the negatives. A fragment that is sealed
//! only knows its own last sample's duration once the *next* fragment is sealed,
//! because that duration is the distance to the next fragment's first decode
//! time, so the writer holds one sealed fragment back at all times, and memory
//! stays at two fragments.

use crate::codec::TrackKind;
use crate::source::{Sample, TrackConfig};
use crate::{
    MAX_FRAGMENT_BYTES, MAX_FRAGMENT_MS, MAX_FRAGMENT_SAMPLES, MediaError, TIME_OFFSET_MS,
};
use mp4_atom::{Atom, Encode, Ftyp, Header, Mdat, Moof, Moov, Mvex, Tfdt, Tfhd, Trun, TrunEntry};
// Reading boxes back is the test-only half of this module's work.
#[cfg(test)]
use mp4_atom::{Buf, Decode};
#[cfg(test)]
use std::io::Cursor;

/// Milliseconds to timeline ticks in the track's timescale, checked for the
/// absurd timescales a hostile container could state.
fn ms_to_ticks(ms: u64, timescale: u32) -> Result<i64, MediaError> {
    let ticks = ms
        .checked_mul(u64::from(timescale))
        .and_then(|v| v.checked_div(1000))
        .and_then(|v| i64::try_from(v).ok())
        .ok_or_else(|| {
            MediaError::Malformed(format!(
                "{ms} ms at timescale {timescale} is not a number of ticks"
            ))
        })?;
    Ok(ticks)
}

/// How far every decode time is shifted forward, in this track's ticks.
///
/// Exactly one timescale's worth at the constant the crate carries (1000 ms),
/// which is why the page can undo the shift with the same number.
fn offset_ticks(timescale: u32) -> Result<i64, MediaError> {
    // The offset is the first place every entry point can ask the question, so
    // the timescale check rides on it: a writer built on a zero timescale fails
    // its first push with this same error, no placeholder state needed.
    check_timescale(timescale)?;
    ms_to_ticks(TIME_OFFSET_MS, timescale)
}

/// The init segment for one track: `ftyp`, then a `moov` describing a single
/// track (id 1) with an empty sample table and a `mvex`/`trex` saying more
/// fragments follow.
///
/// The sample table is empty because an init segment carries no samples: the
/// fragments after it do, and `trex` tells the player the defaults that lets the
/// `trun` boxes leave unsaid. `tkhd` and the sample entry's `Visual`/`Audio`
/// boxes carry the track's dimensions or rate so the player can size a video or
/// state a channel layout before the first fragment arrives.
///
/// A track whose samples cannot be remuxed has no init segment: its
/// [`TrackConfig::sample_entry`] has to come from [`sample_entry`](crate::sample_entry),
/// which already said so for anything it cannot write. A timescale of 0 is
/// [`MediaError::Malformed`]: no duration, rate or sample size can be stated in
/// ticks of zero.
pub fn init_segment(config: &TrackConfig) -> Result<Vec<u8>, MediaError> {
    check_timescale(config.timescale)?;
    let handler = match config.kind {
        TrackKind::Video => *b"vide",
        TrackKind::Audio => *b"soun",
        TrackKind::Subtitle => {
            return Err(MediaError::Unsupported(
                "a subtitle track has no fragmented MP4 init segment".to_string(),
            ));
        }
    };

    let mut out = Vec::new();
    let ftyp = Ftyp {
        major_brand: b"iso6".into(),
        minor_version: 0,
        compatible_brands: vec![b"iso6".into(), b"mp41".into()],
    };
    ftyp.encode(&mut out).map_err(atom_error)?;

    let moov = Moov {
        mvhd: mp4_atom::Mvhd {
            creation_time: 0,
            modification_time: 0,
            timescale: config.timescale,
            duration: 0,
            rate: 1u16.into(),
            volume: 1u8.into(),
            matrix: mp4_atom::Matrix::default(),
            next_track_id: 2,
        },
        meta: None,
        // `mp4-atom` writes `mvex` before the track it describes, which is not
        // what FFmpeg does but is what ISO/IEC 14496-12 allows: moov's children
        // are order-free once `mvhd` is first, and demuxers find `trex` by box
        // type, not position.
        mvex: Some(Mvex {
            mehd: None,
            trex: vec![mp4_atom::Trex {
                track_id: 1,
                default_sample_description_index: 1,
                default_sample_duration: 0,
                default_sample_size: 0,
                default_sample_flags: 0,
            }],
        }),
        trak: vec![mp4_atom::Trak {
            tkhd: mp4_atom::Tkhd {
                creation_time: 0,
                modification_time: 0,
                track_id: 1,
                duration: 0,
                layer: 0,
                alternate_group: 0,
                enabled: true,
                in_movie: true,
                size_is_aspect_ratio: false,
                volume: if config.kind == TrackKind::Audio {
                    1u8.into()
                } else {
                    0u8.into()
                },
                matrix: mp4_atom::Matrix::default(),
                width: config.width.into(),
                height: config.height.into(),
            },
            edts: None,
            meta: None,
            mdia: mp4_atom::Mdia {
                mdhd: mp4_atom::Mdhd {
                    creation_time: 0,
                    modification_time: 0,
                    timescale: config.timescale,
                    duration: 0,
                    language: "und".to_string(),
                },
                hdlr: mp4_atom::Hdlr {
                    handler: handler.into(),
                    name: String::new(),
                },
                minf: mp4_atom::Minf {
                    vmhd: (config.kind == TrackKind::Video).then(mp4_atom::Vmhd::default),
                    smhd: (config.kind == TrackKind::Audio).then(mp4_atom::Smhd::default),
                    nmhd: None,
                    sthd: None,
                    hmhd: None,
                    dinf: mp4_atom::Dinf {
                        dref: mp4_atom::Dref {
                            entries: vec![mp4_atom::Url::default().into()],
                        },
                    },
                    stbl: mp4_atom::Stbl {
                        stsd: mp4_atom::Stsd {
                            codecs: vec![config.sample_entry.clone()],
                        },
                        stts: mp4_atom::Stts::default(),
                        ctts: None,
                        stss: None,
                        stsc: mp4_atom::Stsc::default(),
                        stsz: mp4_atom::Stsz::default(),
                        stco: None,
                        co64: None,
                        sbgp: Vec::new(),
                        sgpd: Vec::new(),
                        subs: Vec::new(),
                        saiz: Vec::new(),
                        saio: Vec::new(),
                        cslg: None,
                    },
                },
            },
            senc: None,
            tref: None,
            udta: None,
        }],
        udta: None,
        ainf: None,
    };
    moov.encode(&mut out).map_err(atom_error)?;
    Ok(out)
}

/// Rejects the one timescale no timestamp math can work in.
fn check_timescale(timescale: u32) -> Result<(), MediaError> {
    if timescale == 0 {
        return Err(MediaError::Malformed(
            "a track with timescale 0 states no timeline".to_string(),
        ));
    }
    Ok(())
}

/// `mp4_atom`'s encode failures, as `MediaError`. Writing a box that does not
/// fit its own field is a contradiction in what we decided to write, and what we
/// decided from is file data, so it reads as a file that does not hold together.
/// `probe`'s mapping is the same one, shared.
fn atom_error(error: mp4_atom::Error) -> MediaError {
    crate::probe::atom_error(error)
}

/// The sync-sample flag and its opposite, ISO/IEC 14496-12 §8.8.3.1.
///
/// `0x0200_0000` is `sample_depends_on = 1`: nothing else needs this sample, so
/// a decoder can start here. `0x0101_0000` is the usual non-sync combination:
/// it depends on others and can carry a lead-frame composition offset.
const SYNC_SAMPLE: u32 = 0x0200_0000;
const NON_SYNC_SAMPLE: u32 = 0x0101_0000;

/// A fragment that is sealed - every decode time and duration known except its
/// last duration - waiting for the fragment after it to say what that is.
#[derive(Debug)]
struct Sealed {
    /// The samples, in decode order, exactly as they were pushed.
    samples: Vec<Sample>,
    /// One decode time per sample, in track ticks, before the offset.
    dts: Vec<i64>,
    /// Durations for all but the last sample; the last needs the next fragment.
    durations: Vec<u32>,
    /// The first decode time, for the held fragment's last duration.
    first_dts: i64,
    /// The last decode time, for the same subtraction.
    last_dts: i64,
}

/// The writer's half-built fragment: the samples pushed since the last seal,
/// what their PTS span costs so far, and how many bytes they hold.
#[derive(Debug, Default)]
struct Open {
    samples: Vec<Sample>,
    first_pts: i64,
    bytes: usize,
}

impl Open {
    fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// Turns the pushed-so-far fragment into a sealed one, and flushes the
    /// fragment held back from the seal before it.
    ///
    /// The flush is what the returned bytes are: this fragment's first decode
    /// time is exactly the distance the previous fragment's last sample was
    /// missing, so the previous one can only be written after this one is
    /// synthesized - but before the new fragment joins the writer, which is why
    /// `push` calls it before appending the closing sample.
    fn seal(
        self,
        held: Option<Sealed>,
        writer: &mut FragmentWriter,
    ) -> Result<Option<Vec<u8>>, MediaError> {
        let SealedState {
            dts,
            durations,
            first_dts,
        } = synthesize(&self.samples, writer.is_audio, &mut writer.last_dts)?;
        let last_dts = *dts.last().expect("push only seals a non-empty fragment");
        let emitted = match held {
            None => None,
            Some(previous) => {
                let last_duration = clamp_gap(first_dts, previous.last_dts)?;
                Some(encode_fragment(writer, previous, last_duration)?)
            }
        };
        writer.held = Some(Sealed {
            samples: self.samples,
            dts,
            durations,
            first_dts,
            last_dts,
        });
        writer.last_sealed_first_dts = Some(first_dts);
        Ok(emitted)
    }
}

/// The last duration of a sealed fragment: the distance from its last decode
/// time to the first decode time of whatever comes next.
///
/// The monotone clamp inside [`synthesize`] makes this at least 1 for every
/// fragment the writer seals mid-stream; the `max(1, ·)` is what `finish` needs
/// it for, where a last fragment borrows a duration instead of meeting a
/// successor. A gap wider than a `u32` is `Malformed`: no field could hold it.
fn clamp_gap(next_first_dts: i64, last_dts: i64) -> Result<u32, MediaError> {
    let gap = next_first_dts
        .checked_sub(last_dts)
        .ok_or_else(|| malformed_overflow("a decode-time gap beyond i64"))?;
    let gap = if gap < 1 { 1 } else { gap };
    u32::try_from(gap)
        .map_err(|_| MediaError::Malformed(format!("a duration of {gap} ticks is over a u32")))
}

/// What [`synthesize`] worked out for one fragment.
struct SealedState {
    dts: Vec<i64>,
    durations: Vec<u32>,
    first_dts: i64,
}

/// Synthesize decode times and durations for one fragment's samples.
///
/// The plan's algorithm, per fragment of samples `p` in decode order, when the
/// source did not hand decode times over:
///
/// ```text
/// sorted = sort(p)
/// delay  = max(0, max_i(sorted[i] - p[i]))   // how far the PTS order leads the
///                                            // decode order at its worst point
/// dts[i] = sorted[i] - delay                  // decode order, same spacing as PTS
/// dts[i] = max(dts[i], last_dts + 1)          // strictly increasing across
///                                             // fragments; can push cts negative,
///                                             // which trun version 1 signs
/// duration[i < n-1] = dts[i+1] - dts[i]
/// ```
///
/// Audio takes `dts = pts`, stated or plain: audio has no reordering to
/// compensate for, so its decode order is the order it was pushed in, and the
/// same `last_dts` clamp applies so a track that resets its clock mid-stream
/// cannot walk backwards. When every sample of a fragment carries `dts`, those
/// given values are the base instead - clamped and checked exactly the same way.
/// A fragment that mixes given and unstated decode times is
/// [`MediaError::Malformed`]: half a stated timeline and half a synthesized one
/// is no timeline.
///
/// The last sample's duration is not here: it is the distance to the next
/// fragment's first decode time, which [`FragmentWriter`] fills in at flush time
/// (or, at end of stream, borrows a duration from - with the last sample's own
/// stated duration in place of the borrow, when it states one). Nor are the
/// other samples' stated durations: `trun` durations are what the samples add
/// up to, and a stated duration that differed from the gap would shift every
/// presentation time after it, compounding across fragments.
///
/// Timestamps outside what `i64` can subtract without wrapping are
/// [`MediaError::Malformed`], as are durations that do not fit a `u32` field.
fn synthesize(
    samples: &[Sample],
    is_audio: bool,
    last_dts: &mut Option<i64>,
) -> Result<SealedState, MediaError> {
    let given = samples.iter().all(|sample| sample.dts.is_some());
    if !given && samples.iter().any(|sample| sample.dts.is_some()) {
        return Err(MediaError::Malformed(
            "a fragment mixing given and unstated decode times is half a timeline".to_string(),
        ));
    }

    // The base decode times, before the seam clamp. The synthesized delay is
    // the largest distance a decode-order sample trails the sorted PTS it
    // pairs with - bounded by the fragment's own PTS span, so the only way its
    // subtraction wraps is if the fragment itself spans near the whole `i64`
    // range, which no container means to say. Audio's base is its PTS, because
    // audio does not reorder; a source that stated decode times says them.
    let mut base: Vec<i64> = Vec::with_capacity(samples.len());
    if given {
        base.extend(samples.iter().map(|sample| sample.dts.unwrap()));
    } else if is_audio {
        base.extend(samples.iter().map(|sample| sample.pts));
    } else {
        let pts: Vec<i64> = samples.iter().map(|sample| sample.pts).collect();
        let mut sorted = pts.clone();
        sorted.sort_unstable();
        let mut delay: i64 = 0;
        for (position, &sample_pts) in pts.iter().enumerate() {
            let lag = sorted[position]
                .checked_sub(sample_pts)
                .ok_or_else(|| malformed_overflow("a PTS span beyond i64"))?;
            delay = delay.max(lag);
        }
        for &sorted_pts in &sorted {
            base.push(
                sorted_pts
                    .checked_sub(delay)
                    .ok_or_else(|| malformed_overflow("a PTS span beyond i64"))?,
            );
        }
    }

    let mut dts = Vec::with_capacity(base.len());
    let mut previous: Option<i64> = *last_dts;
    for base_dts in base {
        let mut sample_dts = base_dts;
        if let Some(previous_dts) = previous {
            let floor = previous_dts
                .checked_add(1)
                .ok_or_else(|| malformed_overflow("decode times reached the top of i64"))?;
            sample_dts = sample_dts.max(floor);
        }
        dts.push(sample_dts);
        previous = Some(sample_dts);
    }
    *last_dts = previous;

    // Durations for all but the last: from the decode timeline and nothing
    // else. What a container stated is not believed here, because the timeline
    // the samples add up to is what a player will play; only at end of stream,
    // in `finish`, does a sample's own stated duration get used, in place of
    // the duration that call would otherwise borrow.
    let count = dts.len();
    let mut durations = Vec::with_capacity(count.saturating_sub(1));
    for position in 0..count.saturating_sub(1) {
        let derived = dts[position + 1]
            .checked_sub(dts[position])
            .ok_or_else(|| malformed_overflow("a decode-time gap beyond i64"))?;
        let duration = u32::try_from(derived).map_err(|_| {
            MediaError::Malformed(format!("a duration of {derived} ticks is over a u32"))
        })?;
        durations.push(duration);
    }

    Ok(SealedState {
        first_dts: *dts.first().ok_or_else(|| {
            MediaError::Malformed("a fragment with no samples has no decode time".to_string())
        })?,
        dts,
        durations,
    })
}

/// The one overflow message, phrased like the others.
fn malformed_overflow(what: &str) -> MediaError {
    MediaError::Malformed(format!("{what}, which no real timestamps need"))
}

/// Writes one sealed fragment: `moof` then `mdat`, ready to hand to `appendBuffer`.
///
/// `last_duration` is the last sample's duration, which only the fragment after
/// this one can state: its first decode time minus this fragment's last. A
/// non-positive result (possible when the clamp raised decode times past a
/// natural gap) becomes 1, the shortest duration the timeline can express.
///
/// `trun` carries a per-sample duration, size, composition offset, and sync or
/// non-sync flag for every sample: a fragment sealed at a cap starts mid-GOP, so
/// its first sample keeps the flag the source gave it, and a B-frame group cut
/// across a fragment boundary needs the signed composition offset `trun` version
/// 1 was made for.
/// `default_base_is_moof` and a `data_offset` measured against the finished
/// `moof`'s length point the samples at the `mdat` right after it.
/// The last duration is passed in, not derived here, because only the caller
/// knows which rule applies: mid-stream it is the distance to the next
/// fragment's first decode time, and at end of stream `finish` borrows a
/// duration, or takes the last sample's own stated one when it states one.
/// A sealed fragment is encoded exactly once, so it
/// comes by value and its sample bytes are moved straight into the `mdat`.
fn encode_fragment(
    writer: &mut FragmentWriter,
    fragment: Sealed,
    last_duration: u32,
) -> Result<Vec<u8>, MediaError> {
    let Sealed {
        samples,
        dts,
        mut durations,
        first_dts,
        last_dts: _,
    } = fragment;
    durations.push(last_duration);
    writer.prev_duration = Some(last_duration);

    let offset = writer.offset()?;
    // tfdt is this fragment's first decode time, shifted out of the negatives.
    // Every duration written is a difference of decode times, so the durations
    // before this fragment telescope onto exactly this number - the invariant
    // `fragment_tfdt_continuity` checks - and nothing needs accumulating.
    let tfdt_value = first_dts
        .checked_add(offset)
        .and_then(|v| u64::try_from(v).ok())
        .ok_or_else(|| {
            MediaError::Malformed("decode times plus the offset do not fit a tfdt".to_string())
        })?;

    let mut entries = Vec::with_capacity(samples.len());
    for (sample, (&sample_dts, &duration)) in samples.iter().zip(dts.iter().zip(durations.iter())) {
        let cts = sample
            .pts
            .checked_sub(sample_dts)
            .ok_or_else(|| malformed_overflow("a composition offset beyond i64"))?;
        let cts = i32::try_from(cts).map_err(|_| {
            MediaError::Malformed(format!(
                "a composition offset of {cts} ticks does not fit a signed trun"
            ))
        })?;
        let size = u32::try_from(sample.data.len()).map_err(|_| {
            MediaError::Malformed("a sample larger than a u32 has no trun size".to_string())
        })?;
        let flags = if writer.is_audio || sample.keyframe {
            SYNC_SAMPLE
        } else {
            NON_SYNC_SAMPLE
        };
        entries.push(TrunEntry {
            duration: Some(duration),
            size: Some(size),
            flags: Some(flags),
            cts: Some(i64::from(cts)),
        });
    }

    // Two passes, because `data_offset` is an offset into the box that contains
    // it: the field's width never changes between passes, so the length check
    // below is what proves the offset it settled on. The first pass writes a
    // placeholder offset and pays for the field; the second corrects it.
    writer.sequence = writer.sequence.saturating_add(1);
    let sequence = writer.sequence;
    let mut moof_bytes = encode_moof(sequence, tfdt_value, &entries, 0)?;
    let data_offset = moof_bytes
        .len()
        .checked_add(8)
        .and_then(|v| i32::try_from(v).ok())
        .ok_or_else(|| {
            MediaError::Malformed("a moof this long has no representable data_offset".to_string())
        })?;
    let moof_again = encode_moof(sequence, tfdt_value, &entries, data_offset)?;
    if moof_again.len() != moof_bytes.len() {
        return Err(MediaError::Malformed(
            "the moof changed length between the two data_offset passes".to_string(),
        ));
    }
    moof_bytes = moof_again;

    let payload_len: usize = samples.iter().map(|sample| sample.data.len()).sum();
    let mut fragment_bytes = Vec::with_capacity(moof_bytes.len() + 8 + payload_len);
    fragment_bytes.extend_from_slice(&moof_bytes);
    let mdat_header = Header {
        kind: Mdat::KIND,
        size: Some(payload_len),
    };
    mdat_header
        .encode(&mut fragment_bytes)
        .map_err(atom_error)?;
    for sample in samples {
        fragment_bytes.extend_from_slice(&sample.data);
    }

    Ok(fragment_bytes)
}

/// The `moof` half of a fragment, with the passed-in `data_offset` value.
fn encode_moof(
    sequence: u32,
    tfdt_value: u64,
    entries: &[TrunEntry],
    data_offset: i32,
) -> Result<Vec<u8>, MediaError> {
    let moof = Moof {
        mfhd: mp4_atom::Mfhd {
            sequence_number: sequence,
        },
        traf: vec![mp4_atom::Traf {
            tfhd: Tfhd {
                track_id: 1,
                base_data_offset: None,
                sample_description_index: None,
                default_sample_duration: None,
                default_sample_size: None,
                default_sample_flags: None,
                duration_is_empty: false,
                default_base_is_moof: true,
            },
            tfdt: Some(Tfdt {
                base_media_decode_time: tfdt_value,
            }),
            trun: vec![Trun {
                data_offset: Some(data_offset),
                samples: mp4_atom::TrunSamples::Explicit(entries.to_vec()),
            }],
            sbgp: Vec::new(),
            sgpd: Vec::new(),
            subs: Vec::new(),
            saiz: Vec::new(),
            saio: Vec::new(),
            meta: None,
            senc: None,
            udta: None,
        }],
    };
    let mut bytes = Vec::new();
    moof.encode(&mut bytes).map_err(atom_error)?;
    Ok(bytes)
}

/// The writer of one track's fragments.
///
/// Feed it [`Sample`]s in decode order - the order containers store them - and
/// it answers with finished `moof`+`mdat` bytes whenever enough time has passed
/// to seal a fragment, and one last time from [`finish`](Self::finish). A
/// fragment is emitted when the fragment *after* it is sealed, because its last
/// duration is the distance to that one's first decode time; the writer keeps at
/// most one sealed fragment and one open one in memory, so a long file streams
/// through in bounded space.
///
/// A video fragment runs from one keyframe up to (not including) the next, and
/// also closes at [`MAX_FRAGMENT_MS`] of samples, [`MAX_FRAGMENT_BYTES`] of
/// sample data, or [`MAX_FRAGMENT_SAMPLES`] samples, whichever comes first:
/// keyframes can be rare, unknown, or zero, and none of those may mean the writer
/// accumulates a whole track. An audio fragment closes once it holds a second of
/// samples, because audio has no keyframes to break on but the player still
/// wants seek granularity.
///
/// Samples that contradict the track it started on - PTS jumping more than ten
/// seconds back from the last fragment's first decode time, timestamps that wrap
/// `i64`, durations that overflow `u32` - are [`MediaError::Malformed`]: they
/// claim a timeline the fragments cannot tell. So is a fragment whose first
/// decode time falls below minus [`TIME_OFFSET_MS`]: the writer shifts decode
/// times by that much to keep them non-negative on the wire, and past that line
/// it could not state the time it would write. After any error the writer
/// stays failed: every later `push` or `finish` reports the first failure.
#[derive(Debug)]
pub struct FragmentWriter {
    timescale: u32,
    is_audio: bool,
    /// Fragments emitted so far, which is the next `mfhd` number minus one.
    sequence: u32,
    /// Decode time of the last sample ever written, which is the floor the next
    /// fragment's first sample has to clear.
    last_dts: Option<i64>,
    /// First decode time of the last sealed fragment, ten seconds of which is
    /// the room a capped fragment may jump back into.
    last_sealed_first_dts: Option<i64>,
    /// The last duration the writer settled on, which is what a final fragment
    /// with no fragment after it uses for its own last sample, unless that
    /// sample states its own.
    prev_duration: Option<u32>,
    /// The message of the first error this writer hit. After one its state is
    /// half-moved - a failed seal has eaten the open and held fragments and
    /// moved `last_dts` - and no later call can be trusted to make a coherent
    /// fragment, so every later `push` or `finish` reports `Malformed` with
    /// this message instead. (`MediaError` itself is not `Clone`: the message
    /// is kept, which is all the verdict needs.)
    failed: Option<String>,
    held: Option<Sealed>,
    open: Open,
}

impl FragmentWriter {
    /// A writer for one track's fragments, numbering `mfhd` from 1.
    ///
    /// The [`TrackConfig`] decides the timeline's ticks-per-second and whether
    /// samples are audio; its sample entry lives in the init segment, not in the
    /// fragments.
    ///
    /// A zero timescale is not rejected here - `push` and `finish` report it at
    /// first use, because no signature both share can return an error.
    pub fn new(config: &TrackConfig) -> Self {
        Self {
            timescale: config.timescale,
            is_audio: config.kind == TrackKind::Audio,
            sequence: 0,
            last_dts: None,
            last_sealed_first_dts: None,
            prev_duration: None,
            failed: None,
            held: None,
            open: Open::default(),
        }
    }

    /// The shift, in ticks, that every decode time gets before it leaves the
    /// writer - recomputed from the timescale at each use, which is what makes
    /// it double as the guard every entry point needs: a writer built on a zero
    /// timescale fails here before it invents a single timestamp.
    fn offset(&self) -> Result<i64, MediaError> {
        offset_ticks(self.timescale)
    }

    /// Adds one sample, and returns a finished fragment when adding it means
    /// the previous fragment is now sealable and the one before it is flushable.
    ///
    /// The returned fragment is always the one sealed a seal-ago, never the one
    /// this sample joins: a fragment's last duration needs the first decode time
    /// of the fragment after it, and the writer only has that once the next
    /// fragment exists complete. `Ok(None)` is the ordinary answer.
    ///
    /// A video sample that is a keyframe closes the open fragment first (the
    /// brief's "runs up to the next keyframe, not including it"); a non-keyframe
    /// sample closes it too once the open fragment has reached any of the
    /// [`MAX_FRAGMENT_MS`], [`MAX_FRAGMENT_BYTES`] or [`MAX_FRAGMENT_SAMPLES`]
    /// caps, and the sample then starts the next fragment.
    pub fn push(&mut self, sample: Sample) -> Result<Option<Vec<u8>>, MediaError> {
        self.poisoned()?;
        let result = self.push_inner(sample);
        self.poison(&result);
        result
    }

    /// Once a call has failed the writer is done with: a failed seal has eaten
    /// the open and held fragments and moved `last_dts`, so every later call
    /// repeats the first failure rather than inventing a fragment from the
    /// wreckage.
    fn poisoned(&self) -> Result<(), MediaError> {
        match &self.failed {
            Some(reason) => Err(MediaError::Malformed(format!(
                "the writer already failed and will not invent a fragment: {reason}"
            ))),
            None => Ok(()),
        }
    }

    fn poison(&mut self, result: &Result<Option<Vec<u8>>, MediaError>) {
        if let Err(error) = result {
            self.failed.get_or_insert_with(|| error.to_string());
        }
    }

    fn push_inner(&mut self, sample: Sample) -> Result<Option<Vec<u8>>, MediaError> {
        self.offset()?;
        self.check_sample(&sample)?;

        let closes = !self.open.is_empty() && self.closes_with(&sample);
        let emitted = if closes {
            let open = std::mem::take(&mut self.open);
            open.seal(self.held.take(), self)?
        } else {
            None
        };

        if self.open.is_empty() {
            self.open.first_pts = sample.pts;
        }
        self.open.bytes += sample.data.len();
        self.open.samples.push(sample);
        Ok(emitted)
    }

    /// Seals whatever is left and flushes whatever was held back, in one Vec:
    /// the held fragment with its last duration now known from the final one,
    /// and the final fragment with its last sample lasting as long as it
    /// states, or as long as the sample before it did, or 1 tick with nothing
    /// to borrow.
    ///
    /// `Ok(None)` when no sample is outstanding. A player cannot be told a
    /// fragment ended before its last sample ended, so the last duration at the
    /// end of the stream is the best guess the timeline allows.
    pub fn finish(&mut self) -> Result<Option<Vec<u8>>, MediaError> {
        self.poisoned()?;
        let result = self.finish_inner();
        self.poison(&result);
        result
    }

    fn finish_inner(&mut self) -> Result<Option<Vec<u8>>, MediaError> {
        self.offset()?;
        if self.open.is_empty() {
            // The held fragment cannot learn its last duration with no fragment
            // after it; nothing pushes after `finish` in the intended flow, so
            // end it with its last sample's stated duration if it states one,
            // and otherwise the previous duration, the fallback the plan names.
            return match self.held.take() {
                None => Ok(None),
                Some(sealed) => {
                    let last_duration =
                        match sealed.samples.last().and_then(|sample| sample.duration) {
                            Some(stated) if stated > 0 => stated,
                            _ => self.prev_duration.unwrap_or(1),
                        };
                    Ok(Some(encode_fragment(self, sealed, last_duration)?))
                }
            };
        }

        let open = std::mem::take(&mut self.open);
        let SealedState {
            dts,
            durations,
            first_dts,
        } = synthesize(&open.samples, self.is_audio, &mut self.last_dts)?;
        let last_dts = *dts.last().expect("samples were just here");
        // The final fragment has no fragment after it. If its last sample
        // states a duration, that is the one: it is the only stated duration
        // anywhere the writer believes. Otherwise the plan says the last sample
        // lasts as long as the previous one: inside the fragment that is the
        // duration of the second-to-last sample; with a one-sample fragment,
        // the last duration the writer settled on anywhere; with no history at
        // all, one tick.
        let final_duration = match (
            open.samples.last().and_then(|sample| sample.duration),
            durations.last().copied(),
            self.prev_duration,
        ) {
            (Some(stated), _, _) if stated > 0 => stated,
            (Some(_), Some(inside), _) | (None, Some(inside), _) => inside,
            (Some(_) | None, None, Some(previous)) => previous,
            (Some(_) | None, None, None) => 1,
        };

        let sealed = Sealed {
            samples: open.samples,
            dts,
            durations,
            first_dts,
            last_dts,
        };

        // Flush the held fragment first: this final fragment's first decode time
        // is exactly what its last duration was waiting for. Then the final
        // fragment, which borrows `final_duration` instead of meeting a
        // successor.
        let held_bytes = match self.held.take() {
            None => Vec::new(),
            Some(previous) => {
                let last_duration = clamp_gap(sealed.first_dts, previous.last_dts)?;
                encode_fragment(self, previous, last_duration)?
            }
        };
        let final_bytes = encode_fragment(self, sealed, final_duration)?;
        Ok(Some(concat(held_bytes, final_bytes)))
    }

    /// Whether this sample, joining the open fragment now, closes it first.
    ///
    /// The PTS span is measured source-side (the sample's PTS minus the
    /// fragment's first PTS) rather than from synthesized decode times, so the
    /// [`MAX_FRAGMENT_MS`] cap bounds what the container claimed no matter how
    /// the reordering lands.
    fn closes_with(&self, sample: &Sample) -> bool {
        if !self.is_audio && sample.keyframe {
            return true;
        }
        let cap = match self.fragment_span_cap() {
            Ok(cap) => cap,
            // An absurd timescale cap means every sample closes a fragment,
            // which is bounded, safe behaviour, not a bug to surface mid-track.
            Err(_) => return true,
        };
        if sample.pts.saturating_sub(self.open.first_pts) >= cap {
            return true;
        }
        if self.open.bytes.saturating_add(sample.data.len()) > MAX_FRAGMENT_BYTES {
            return true;
        }
        if self.open.samples.len() >= MAX_FRAGMENT_SAMPLES {
            return true;
        }
        false
    }

    /// How wide a fragment may get before the sample-span cap closes it: 5 s of
    /// video, 1 s of audio, in this track's ticks.
    fn fragment_span_cap(&self) -> Result<i64, MediaError> {
        let ms = if self.is_audio { 1000 } else { MAX_FRAGMENT_MS };
        ms_to_ticks(ms, self.timescale)
    }

    /// A sample's own claims, checked before anything is built from them.
    ///
    /// PTS more than ten seconds below the last sealed fragment's first decode
    /// time is a file whose timeline jumps backwards further than any cap cut
    /// or seeking can explain, and a `u32`-overflowing stated duration cannot
    /// be written in any MP4 field. Both are the container's lie, not ours.
    fn check_sample(&self, sample: &Sample) -> Result<(), MediaError> {
        if let Some(first_dts) = self.last_sealed_first_dts {
            let room = match ms_to_ticks(10_000, self.timescale) {
                Ok(room) => first_dts.checked_sub(room),
                Err(_) => None,
            };
            if let Some(floor) = room.filter(|floor| sample.pts < *floor) {
                return Err(MediaError::Malformed(format!(
                    "a sample at PTS {} starts more than 10 s below the last fragment's first decode time (floor {})",
                    sample.pts, floor
                )));
            }
        }
        Ok(())
    }
}

/// Two fragments, end to end, as one Vec: `finish` hands the player the held
/// fragment and the final one together.
fn concat(mut first: Vec<u8>, second: Vec<u8>) -> Vec<u8> {
    first.extend(second);
    first
}

/// Reads the moof and mdat pair back, for these tests; a caller that wants the
/// timeline walks `tfdt` and the trun entries exactly as the web player's
/// demuxer does, with `mp4_atom`'s own decoder.
#[cfg(test)]
pub(crate) fn decode_fragment(bytes: &[u8]) -> Result<(Moof, Vec<u8>), MediaError> {
    let malformed = |what: &str| MediaError::Malformed(what.to_string());
    let mut cursor = Cursor::new(bytes);
    let moof = Moof::decode(&mut cursor).map_err(atom_error)?;
    // A fragment is a moof followed by an mdat, and nothing else; the cursor's
    // remaining count is where the mdat starts.
    if cursor.remaining() < 8 {
        return Err(malformed("a fragment is a moof followed by an mdat"));
    }
    let mdat_start = bytes.len() - cursor.remaining();
    if &bytes[mdat_start + 4..mdat_start + 8] != b"mdat" {
        return Err(malformed("a fragment is a moof followed by an mdat"));
    }
    let header = Header::decode(&mut cursor).map_err(atom_error)?;
    let size = header
        .size
        .ok_or_else(|| malformed("an mdat that runs to the end of the fragment"))?;
    // After the header, the rest is payload: `size` of it, and nothing trailing.
    if cursor.remaining() != size {
        return Err(MediaError::Malformed(format!(
            "the mdat's payload does not match the bytes left: total {} remaining {} size {}",
            bytes.len(),
            cursor.remaining(),
            size
        )));
    }
    let payload_start = bytes.len() - cursor.remaining();
    Ok((moof, bytes[payload_start..].to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::Codec;
    use mp4_atom::Stsd;

    /// The video config the fragment tests run against: 1000 ticks to the
    /// second (Matroska's millisecond timescale), so PTS in the tests is PTS in
    /// milliseconds and the offset is a flat 1000.
    fn video_config() -> TrackConfig {
        TrackConfig {
            timescale: 1000,
            kind: TrackKind::Video,
            codec: Codec::H264,
            // The fragments never carry a sample entry - only the init segment
            // does - so a minimal valid one stands in for the real avc1.
            sample_entry: mp4_atom::Codec::Avc1(mp4_atom::Avc1 {
                visual: mp4_atom::Visual::default(),
                avcc: mp4_atom::Avcc::default(),
                btrt: None,
                colr: Vec::new(),
                pasp: None,
                taic: None,
                fiel: None,
                spatial: None,
            }),
            width: 64,
            height: 36,
        }
    }

    /// The audio config: 48 kHz, AAC, the rate every Opus and most AAC tracks
    /// the writer sees actually run at.
    fn audio_config() -> TrackConfig {
        TrackConfig {
            timescale: 48_000,
            kind: TrackKind::Audio,
            codec: Codec::Aac,
            sample_entry: mp4_atom::Codec::Mp3(mp4_atom::Mp3 {
                audio: mp4_atom::Audio {
                    data_reference_index: 1,
                    channel_count: 2,
                    sample_size: 16,
                    sample_rate: 48_000u16.into(),
                },
            }),
            width: 0,
            height: 0,
        }
    }

    /// A sample built from PTS, keyframe flag and a byte of filler.
    fn sample(pts: i64, keyframe: bool) -> Sample {
        Sample {
            pts,
            dts: None,
            duration: None,
            keyframe,
            data: vec![pts as u8],
        }
    }

    /// Cuts a buffer of `moof`+`mdat` pairs into individual fragments, the way
    /// a `SourceBuffer` would: box header first, payload next, box by box.
    /// `finish` hands back the fragment it held and the final one as one Vec,
    /// so more than one stream can end in one buffer.
    fn split_fragments(bytes: &[u8]) -> Vec<Vec<u8>> {
        fn next_box(rest: &[u8]) -> (&[u8], &[u8]) {
            let mut cursor = Cursor::new(rest);
            let header = Header::decode(&mut cursor).expect("a box header");
            let body = header.size.expect("no open-ended box here");
            let end = 8 + body;
            assert!(end <= rest.len(), "a box that runs past the buffer");
            (&rest[..end], &rest[end..])
        }

        let mut out = Vec::new();
        let mut rest = bytes;
        while !rest.is_empty() {
            // A fragment is one pair: the `moof` and the `mdat` it points at.
            let (moof, after) = next_box(rest);
            assert_eq!(&moof[4..8], b"moof", "a fragment starts with its moof");
            let (mdat, rest_after) = next_box(after);
            assert_eq!(&mdat[4..8], b"mdat", "a moof is followed by its mdat");
            let mut fragment = moof.to_vec();
            fragment.extend_from_slice(mdat);
            out.push(fragment);
            rest = rest_after;
        }
        out
    }

    /// Runs samples through a fresh writer and collects every fragment the
    /// writer emitted, `finish` included, one buffer per fragment.
    fn fragments(config: &TrackConfig, samples: impl IntoIterator<Item = Sample>) -> Vec<Vec<u8>> {
        let mut writer = FragmentWriter::new(config);
        let mut out = Vec::new();
        for item in samples {
            if let Some(bytes) = writer.push(item).expect("push") {
                out.extend(split_fragments(&bytes));
            }
        }
        if let Some(bytes) = writer.finish().expect("finish") {
            out.extend(split_fragments(&bytes));
        }
        out
    }

    /// The timeline a demuxer would walk: every fragment's tfdt and, per sample,
    /// its duration, composition offset, and the PTS the two imply.
    fn walk(fragment: &[u8]) -> (u64, Vec<(u32, i64)>) {
        let (moof, payload) = decode_fragment(fragment).expect("a well-formed fragment");
        let traf = moof.traf.first().expect("one traf");
        let tfdt = traf.tfdt.as_ref().expect("tfdt").base_media_decode_time;
        let trun = traf.trun.first().expect("trun");
        let mut timeline = Vec::new();
        match &trun.samples {
            mp4_atom::TrunSamples::Explicit(entries) => {
                for entry in entries {
                    timeline.push((
                        entry.duration.expect("per-sample duration"),
                        entry.cts.expect("per-sample composition offset"),
                    ));
                }
                // The mdat holds exactly the sum of the sizes the trun states.
                let total: usize = entries
                    .iter()
                    .map(|entry| entry.size.expect("a per-sample size") as usize)
                    .sum();
                assert_eq!(
                    total,
                    payload.len(),
                    "the data_offset and the mdat must describe the same bytes"
                );
                let offset = traf.trun.first().unwrap().data_offset.expect("data_offset");
                assert_eq!(
                    offset as usize,
                    fragment.len() - payload.len(),
                    "data_offset counts from the moof's start to the payload"
                );
            }
            mp4_atom::TrunSamples::Inherited { count, .. } => {
                timeline.push((*count, 0));
            }
        }
        (tfdt, timeline)
    }

    /// B-frames, the reason decode times are synthesized at all: presentation
    /// order `[0, 300, 100, 200]` for a GOP whose decode order is `[0, 100, 200,
    /// 300]` means the first decode time has to sit 100 ms *before* the first
    /// PTS, and the writer's offset keeps that pair non-negative on the wire.
    ///
    /// Two fragments (keyframes at PTS 0 and 400), both checked: every decoded
    /// `dts + cts` is the source PTS shifted by the offset, decode times rise
    /// strictly across the whole pair, and this input never needs the clamp, so
    /// every composition offset stays non-negative.
    #[test]
    fn dts_synthesis_with_b_frames() {
        let pts = [0, 300, 100, 200, 400, 700, 500, 600];
        let samples = [
            sample(0, true),
            sample(300, false),
            sample(100, false),
            sample(200, false),
            sample(400, true),
            sample(700, false),
            sample(500, false),
            sample(600, false),
        ];
        let out = fragments(&video_config(), samples.iter().cloned());
        assert_eq!(out.len(), 2, "two fragments, the keyframes 400 ms apart");

        let offset = 1000_i64; // TIME_OFFSET_MS at timescale 1000
        let mut last_dts: Option<i64> = None;
        for (number, fragment) in out.iter().enumerate() {
            let (tfdt, timeline) = walk(fragment);
            assert_eq!(timeline.len(), 4, "four samples per fragment");
            let mut dts = i64::try_from(tfdt).expect("tfdt fits i64");
            for (position, &(duration, cts)) in timeline.iter().enumerate() {
                let source = pts[number * 4 + position];
                assert_eq!(
                    dts + cts,
                    offset + source,
                    "fragment {number} sample {position}: dts + cts is the shifted PTS"
                );
                assert!(cts >= 0, "this input never needs the clamp");
                if let Some(previous) = last_dts {
                    assert!(dts > previous, "decode times rise strictly");
                }
                last_dts = Some(dts);
                dts += i64::from(duration);
            }
        }
        // The delay is exactly the sort misalignment: fragment 1's first decode
        // time is PTS 0 minus 100 ms of reordering, plus the offset.
        let (tfdt, _) = walk(&out[0]);
        assert_eq!(tfdt, 900, "sorted PTS minus the 100 ms B-frame delay");
        // The last duration of a fragment is the distance to the next
        // fragment's first decode time: PTS 400 minus fragment 1's last decode
        // time 200.
        let (_, timeline) = walk(&out[0]);
        assert_eq!(timeline[3].0, 100, "last duration reaches the next GOP");
    }

    /// When the next fragment reorders worse than the clamp expects, the clamp
    /// is what keeps `dts` increasing - and it is allowed to push a sample's
    /// composition offset around, because a signed `trun` version 1 is what the
    /// plan chose for exactly this.
    ///
    /// Fragment 1 is PTS `[0, 100]`, in order. Fragment 2 is `[200, 500, 300,
    /// 400]`: sorted, its first decode time would be 100, a collision with
    /// fragment 1's last decode time 100, so the clamp raises it to 101. `dts`
    /// rises, `dts + cts` is still the PTS shifted, and the trun carries signed
    /// offsets (version 1), which the raw bytes confirm.
    #[test]
    fn dts_clamp_at_a_growing_delay() {
        let samples = [
            sample(0, true),
            sample(100, false),
            sample(200, true),
            sample(500, false),
            sample(300, false),
            sample(400, false),
        ];
        let out = fragments(&video_config(), samples.iter().cloned());
        assert_eq!(out.len(), 2);

        let (tfdt1, timeline1) = walk(&out[0]);
        assert_eq!(tfdt1, 1000); // dts 0 + offset
        assert_eq!(timeline1[0], (100, 0));
        let (tfdt2, timeline2) = walk(&out[1]);
        // Uncorrected, fragment 2's first decode time would be 100 and its tfdt
        // 1100. The clamp raised it, and tfdt says so.
        assert_eq!(
            tfdt2, 1101,
            "the clamp lifted the first decode time 100 -> 101"
        );
        // Fragment 1's last written duration is the gap the clamp left behind:
        // its last decode time 100, and the next fragment's raised first decode
        // time 101. The clamp buys tfdt one tick and costs that sample the 100
        // ms duration its PTS gap suggested.
        assert_eq!(timeline1[1].0, 1, "the clamp squeezed the seam to one tick");
        assert_eq!(
            i64::try_from(tfdt2).expect("fits"),
            i64::try_from(tfdt1).expect("fits")
                + i64::from(timeline1[0].0)
                + i64::from(timeline1[1].0),
            "1000 + 100 + 1 = 1101: a fragment starts exactly where the last ended"
        );

        let pts2 = [200, 500, 300, 400];
        let mut dts = i64::try_from(tfdt2).expect("fits");
        let mut last = 1000 + 100; // fragment 1's last decode time + offset
        assert!(dts > last);
        for (position, &(duration, cts)) in timeline2.iter().enumerate() {
            assert_eq!(
                dts + cts,
                1000 + pts2[position],
                "the shifted PTS identity survives the clamp"
            );
            assert!(dts > last, "strictly increasing across the seam");
            last = dts;
            dts += i64::from(duration);
        }

        // The version byte of the trun box: fourcc, then version, then flags.
        for fragment in &out {
            let position = fragment
                .windows(4)
                .position(|window| window == b"trun")
                .expect("a trun box");
            assert_eq!(
                fragment[position + 4],
                1,
                "a signed composition offset needs version 1"
            );
        }
    }

    /// Keyframes that never come must not mean one endless fragment: 300
    /// samples at 40 ms, only the first a keyframe, still split at 5 s of
    /// samples, into three fragments, none longer than [`MAX_FRAGMENT_MS`].
    #[test]
    fn fragments_close_without_keyframes() {
        let samples: Vec<Sample> = (0..300)
            .map(|number| sample(i64::from(number) * 40, number == 0))
            .collect();
        let out = fragments(&video_config(), samples);
        assert_eq!(
            out.len(),
            3,
            "5000 ms caps at samples 125 and 250, finish closes the third"
        );
        for (number, fragment) in out.iter().enumerate() {
            let (_, timeline) = walk(fragment);
            let span: u64 = timeline
                .iter()
                .map(|&(duration, _)| u64::from(duration))
                .sum();
            assert!(
                span <= MAX_FRAGMENT_MS,
                "fragment {number} holds {span} ms of samples, over the {MAX_FRAGMENT_MS} cap"
            );
            let counts = [125, 125, 50];
            assert_eq!(
                timeline.len(),
                counts[number],
                "the caps cut at sample spans of 5000 ms"
            );
        }
    }

    /// The invariant the player's timeline depends on: every fragment after the
    /// first starts exactly where the previous one ended - its `tfdt` is the
    /// previous `tfdt` plus the sum of the previous durations. The
    /// last-duration-at-flush-time design is what makes this exact rather than
    /// approximate.
    #[test]
    fn fragment_tfdt_continuity() {
        let samples: Vec<Sample> = (0..170)
            .map(|number| sample(i64::from(number) * 40, number % 40 == 0))
            .collect();
        let out = fragments(&video_config(), samples);
        assert!(out.len() >= 3, "several fragments to check the seam of");
        let mut previous: Option<(i64, u64)> = None;
        for fragment in &out {
            let (tfdt, timeline) = walk(fragment);
            let tfdt = i64::try_from(tfdt).expect("fits");
            if let Some((tfdt_before, span_before)) = previous {
                assert_eq!(
                    tfdt,
                    tfdt_before + i64::try_from(span_before).expect("fits"),
                    "a fragment starts where the last one ended"
                );
            }
            previous = Some((
                tfdt,
                timeline
                    .iter()
                    .map(|&(duration, _)| u64::from(duration))
                    .sum(),
            ));
        }
    }

    /// Audio has no keyframes, so it breaks on time instead: 100 samples of 21
    /// ms each give fragments that hold at least a second - except the last one,
    /// which is whatever is left when the stream ends.
    #[test]
    fn audio_fragments_hold_a_second() {
        let samples: Vec<Sample> = (0..100)
            .map(|number| sample(i64::from(number) * 1008, false))
            .collect();
        let out = fragments(&audio_config(), samples);
        assert!(
            out.len() >= 2,
            "a second at 48 kHz is 48 samples, so 100 cannot be one fragment"
        );
        let last = out.len() - 1;
        for (number, fragment) in out.iter().enumerate() {
            let (tfdt, timeline) = walk(fragment);
            let _ = tfdt;
            let span: u64 = timeline
                .iter()
                .map(|&(duration, _)| u64::from(duration))
                .sum();
            if number == last {
                continue;
            }
            assert!(
                span >= 48_000,
                "fragment {number} holds {} ms, under the 1000 ms audio floor",
                span * 1000 / 48_000
            );
        }
        // And the last holds what remained: the whole stream's samples add up.
        let total: usize = out.iter().map(|fragment| walk(fragment).1.len()).sum();
        assert_eq!(total, 100);
    }

    /// One-byte samples at one PTS ride the byte and span caps forever:
    /// [`MAX_FRAGMENT_SAMPLES`] is the bound that always bites, and the ruling's
    /// input is 25 000 of them with only the first a keyframe. Every fragment
    /// holds at most the cap, the fragments hold all 25 000 samples between
    /// them, and no call fails.
    ///
    /// Equal PTS is also the input the decode-time synthesis handles worst: the
    /// clamp forces each sample one tick past the last, so durations stay
    /// positive and decode times strictly increase even here.
    #[test]
    fn fragments_close_at_the_sample_cap() {
        let samples: Vec<Sample> = (0..25_000)
            .map(|number| Sample {
                pts: 0,
                dts: None,
                duration: None,
                keyframe: number == 0,
                data: vec![7],
            })
            .collect();
        let out = fragments(&video_config(), samples);
        let counts: Vec<usize> = out.iter().map(|fragment| walk(fragment).1.len()).collect();
        for (number, &count) in counts.iter().enumerate() {
            assert!(
                count <= MAX_FRAGMENT_SAMPLES,
                "fragment {number} holds {count} samples, over the cap of {MAX_FRAGMENT_SAMPLES}"
            );
        }
        let total: usize = counts.iter().sum();
        assert_eq!(total, 25_000, "every sample reached a fragment");
        assert_eq!(
            counts
                .iter()
                .filter(|&&count| count == MAX_FRAGMENT_SAMPLES)
                .count(),
            2,
            "two fragments fill the cap before the last 5 000 samples"
        );
    }

    /// PTS `[0, i64::MAX]` on a video track: the two samples are half an `i64`
    /// apart, which is a duration no field can hold and a timeline no addition
    /// survives. The writer says `Malformed` instead of writing a fragment
    /// whose timeline is a wrap-around.
    #[test]
    fn huge_timestamps_are_malformed() {
        let mut writer = FragmentWriter::new(&video_config());
        let opened = writer
            .push(sample(0, true))
            .expect("the first sample opens a fragment");
        assert!(opened.is_none(), "no fragment seals yet");
        // The span alone is over MAX_FRAGMENT_MS, so this sample seals the
        // first fragment; the distance between the two is what fails.
        writer
            .push(sample(i64::MAX, false))
            .expect("the cap closes the first fragment cleanly");
        let error = writer
            .finish()
            .expect_err("the gap between the fragments does not fit a duration");
        assert!(
            matches!(error, MediaError::Malformed(_)),
            "a wrapped timeline is the file's lie, not ours: {error:?}"
        );
    }

    /// 29.97 fps at timescale 1000 with every sample stating `duration: 33`
    /// while its PTS is the nearest tick: 33 is short of what the timeline
    /// between two samples actually is, so writing the stated value mid-stream
    /// would run every later presentation time early - over 10 s by 16.7 min.
    /// Mid-stream durations come from decode times, so for every sample
    /// `tfdt + sum(previous durations) + cts` is the source PTS plus the offset.
    #[test]
    fn stated_durations_do_not_shift_presentation() {
        let samples: Vec<Sample> = (0..3000_i64)
            .map(|i| Sample {
                pts: (2 * i * 1001 + 30) / 60, // round(i * 1001 / 30)
                dts: None,
                duration: Some(33),
                keyframe: i % 30 == 0,
                data: vec![0x41],
            })
            .collect();
        let out = fragments(&video_config(), samples.clone());
        assert_eq!(
            out.len(),
            3000 / 30,
            "one fragment per keyframed GOP, nothing else closed one"
        );
        let offset = 1000_u64; // TIME_OFFSET_MS at timescale 1000.
        let mut index = 0_usize;
        for (number, fragment) in out.iter().enumerate() {
            let (tfdt, timeline) = walk(fragment);
            let mut start = tfdt;
            for (&(duration, cts), sample) in timeline.iter().zip(&samples[index..]) {
                assert_eq!(
                    start.wrapping_add(cts as u64),
                    (sample.pts as u64) + offset,
                    "sample {index} of fragment {number}: tfdt, the durations \
                     before it and cts must name its presentation time"
                );
                start += u64::from(duration);
                index += 1;
            }
        }
        assert_eq!(index, 3000, "every sample reached a fragment");
    }

    /// A given decode time is a fact about the file, not a hint: with every
    /// sample of a fragment carrying one, the writer uses it as given (clamped
    /// and checked), and a fragment that mixes given with unstated is
    /// `Malformed`.
    ///
    /// The ruling's pair, PTS `[0, 300, 100, 200]` against DTS `[-100, 0, 100,
    /// 200]`, is an ordinary B-frame GOP - and exactly what synthesis would
    /// also have produced - so the test carries a second writer whose given
    /// decode times synthesis would not have found, and checks the mix rule on
    /// a third. Without that pair of extras, "ignore given decode times again"
    /// would pass the ruling's input unchanged.
    #[test]
    fn given_dts_is_used() {
        fn written(writer: &mut FragmentWriter) -> Vec<Vec<u8>> {
            let mut out = Vec::new();
            let bytes = writer.finish().expect("finish").expect("samples pushed");
            out.extend(split_fragments(&bytes));
            out
        }

        let pts = [0_i64, 300, 100, 200];
        let check = |dts: [i64; 4]| {
            let mut writer = FragmentWriter::new(&video_config());
            for i in 0..4 {
                writer
                    .push(Sample {
                        pts: pts[i],
                        dts: Some(dts[i]),
                        duration: None,
                        keyframe: i == 0,
                        data: vec![0x42],
                    })
                    .expect("no fragment closes inside four samples");
            }
            let out = written(&mut writer);
            assert_eq!(out.len(), 1);
            let (tfdt, timeline) = walk(&out[0]);
            assert_eq!(timeline.len(), 4);
            let mut start = tfdt;
            for i in 0..4 {
                assert_eq!(
                    start,
                    (dts[i] + 1000) as u64,
                    "sample {i}: the decode time on the wire is the one given, offset"
                );
                assert_eq!(
                    (start as i64) + timeline[i].1,
                    pts[i] + 1000,
                    "sample {i}: dts plus cts is the presentation time"
                );
                start += u64::from(timeline[i].0);
            }
        };

        check([-100, 0, 100, 200]); // the ruling's pair
        check([0, 60, 120, 180]); // not what synthesis (-100, 0, 100, 200) would give

        // Mixed within one fragment is half a timeline, and half is none.
        let mut writer = FragmentWriter::new(&video_config());
        writer
            .push(Sample {
                pts: 0,
                dts: Some(-50),
                duration: None,
                keyframe: true,
                data: vec![1],
            })
            .expect("opens");
        writer
            .push(sample(100, false))
            .expect("still one open fragment");
        let error = writer.finish().expect_err("half given, half synthesized");
        assert!(
            matches!(error, MediaError::Malformed(_)),
            "a mixed fragment is malformed input: {error:?}"
        );
    }

    /// Audio does not reorder, so its decode times are its presentation times
    /// and its composition offsets are zero - including when a PTS falls below
    /// the fragment's running floor and the clamp pushes that pair back, which
    /// is the only way audio can differ from its PTS at all.
    #[test]
    fn audio_dts_equals_pts() {
        let audio = [0_i64, 21, 42, 64];
        let out = fragments(&audio_config(), audio.iter().map(|&pts| sample(pts, false)));
        assert_eq!(out.len(), 1);
        let (tfdt, timeline) = walk(&out[0]);
        assert_eq!(timeline.len(), 4);
        assert_eq!(
            tfdt,
            audio[0] as u64 + 48_000,
            "the first decode time is the first PTS, offset by one second of audio ticks"
        );
        for (&(_duration, cts), &pts) in timeline.iter().zip(&audio) {
            assert_eq!(
                cts, 0,
                "a track that never reorders has nothing to offset at PTS {pts}"
            );
        }

        // A PTS below the floor clamps forward, and the offset goes negative -
        // still the sample's own PTS against its own clamped decode time.
        let dip = [100_i64, 0];
        let out = fragments(&audio_config(), dip.iter().map(|&pts| sample(pts, false)));
        assert_eq!(out.len(), 1);
        let (tfdt, timeline) = walk(&out[0]);
        assert_eq!(
            tfdt,
            100 + 48_000,
            "the first audio decode time is its PTS even when the next PTS dips"
        );
        assert_eq!(timeline[0].1, 0);
        assert_eq!(
            timeline[0].0, 1,
            "the dip clamps the second sample to one tick after the first"
        );
        assert_eq!(
            timeline[1].1, -101,
            "and its composition offset goes negative, which trun version 1 signs"
        );
    }

    /// After a failed call the writer's state is half-moved: the open and held
    /// fragments are gone and the timeline floor has shifted. Every later call
    /// repeats the failure instead of inventing a fragment from the wreckage.
    #[test]
    fn writer_stays_failed_after_an_error() {
        let mut writer = FragmentWriter::new(&video_config());
        writer
            .push(sample(0, true))
            .expect("the first sample opens a fragment");
        writer
            .push(sample(i64::MAX, false))
            .expect("the span cap closes it cleanly");
        let error = writer
            .finish()
            .expect_err("the gap between the fragments does not fit a duration");
        assert!(matches!(error, MediaError::Malformed(_)));

        let error = writer
            .push(sample(0, true))
            .expect_err("the failure stands");
        assert!(
            matches!(error, MediaError::Malformed(_)),
            "a dead writer says so instead of reopening: {error:?}"
        );
        let error = writer.finish().expect_err("and still says so");
        assert!(
            matches!(error, MediaError::Malformed(_)),
            "finish after failure is the same verdict: {error:?}"
        );
    }

    /// The init segment for the H.264 fixture's video track: `ftyp` saying
    /// `iso6`, then a `moov` whose single track carries the fixture's real
    /// `avcC` record, an empty sample table, and the `trex` that says fragments
    /// follow.
    ///
    /// It reads through the same fixture path the probe tests read: the avcc
    /// equality is only meaningful against the fixture's own CodecPrivate, and
    /// Task 3 builds this config from exactly these two bytes.
    #[test]
    fn init_segment_decodes() {
        use matroska_demuxer::TrackType;

        let info = crate::probe::probe(fixture("h264-aac-opus-srt-ass.mkv")).expect("probe");
        let video = info
            .tracks
            .iter()
            .find(|track| track.kind == TrackKind::Video)
            .expect("a video track");

        let file = crate::probe::open_matroska(
            fixture("h264-aac-opus-srt-ass.mkv"),
            crate::ReadBudget::new(crate::ELEMENT_CAP),
        )
        .expect("open");
        let entry = file
            .tracks()
            .iter()
            .find(|entry| {
                entry.track_type() == TrackType::Video
                    && u32::try_from(entry.track_number().get()).unwrap_or(0) == video.id
            })
            .expect("the video track");
        let private = entry.codec_private().expect("h264 carries an avcC");
        let video_settings = entry.video().expect("video settings");
        let width = u16::try_from(video_settings.pixel_width().get()).expect("width fits u16");
        let height = u16::try_from(video_settings.pixel_height().get()).expect("height fits u16");

        let sample_entry =
            crate::codec::sample_entry(video, Some(private), None, None, Some((width, height)))
                .expect("the fixture's h264 track is remuxable");
        let config = TrackConfig {
            // Matroska timestamps are milliseconds, so the track's timescale is
            // the millisecond one.
            timescale: 1000,
            kind: TrackKind::Video,
            codec: video.codec.clone(),
            sample_entry: sample_entry.clone(),
            width,
            height,
        };

        let bytes = init_segment(&config).expect("an init segment");
        let mut cursor = Cursor::new(bytes.as_slice());
        let ftyp = mp4_atom::Ftyp::decode(&mut cursor).expect("ftyp first");
        assert_eq!(u32::from(ftyp.major_brand), u32::from_be_bytes(*b"iso6"));
        let brands: Vec<u32> = ftyp
            .compatible_brands
            .iter()
            .copied()
            .map(u32::from)
            .collect();
        assert_eq!(
            brands,
            vec![u32::from_be_bytes(*b"iso6"), u32::from_be_bytes(*b"mp41")]
        );

        let moov = mp4_atom::Moov::decode(&mut cursor).expect("moov after ftyp");
        assert_eq!(moov.trak.len(), 1, "one track");
        let trak = &moov.trak[0];
        assert_eq!(trak.tkhd.track_id, 1);
        assert_eq!(trak.tkhd.width.integer(), width);
        assert_eq!(trak.tkhd.height.integer(), height);
        assert_eq!(trak.mdia.mdhd.timescale, 1000);

        let mvex = moov
            .mvex
            .as_ref()
            .expect("an init segment declares fragments");
        assert_eq!(mvex.trex[0].track_id, 1);

        let stsd: &Stsd = &trak.mdia.minf.stbl.stsd;
        assert_eq!(
            stsd.codecs,
            vec![sample_entry],
            "the sample entry the track was built with is the one in stsd"
        );
        let mp4_atom::Codec::Avc1(avc1) = &stsd.codecs[0] else {
            panic!("an h264 track writes an avc1 entry");
        };
        assert_eq!(
            avc1.avcc,
            crate::codec::avcc_private(private).expect("the fixture's avcC parses"),
            "the stsd carries the fixture's own CodecPrivate record"
        );

        let stbl = &trak.mdia.minf.stbl;
        assert!(
            stbl.stts.entries.is_empty(),
            "an init segment has no samples"
        );
        assert!(stbl.stsc.entries.is_empty());
        assert!(
            stbl.stss.is_none(),
            "no sync table with no samples to index"
        );
    }

    /// The fixture path a unit test can open.
    fn fixture(name: &str) -> std::io::BufReader<std::fs::File> {
        let path = std::path::PathBuf::from(format!(
            "{}/tests/fixtures/{name}",
            env!("CARGO_MANIFEST_DIR")
        ));
        std::io::BufReader::new(std::fs::File::open(path).expect("fixture"))
    }
}
