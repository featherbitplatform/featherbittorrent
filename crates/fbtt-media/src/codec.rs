//! Codec identity: what kind of samples a track holds, which codec encodes
//! them, the codec strings a player or an HLS manifest asks for, and the sample
//! entry an fMP4 needs for each codec.
use crate::MediaError;
use crate::probe::TrackInfo;
use mp4_atom::{self as atom, Atom};

/// What kind of samples a track holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TrackKind {
    /// Compressed video.
    Video,
    /// Compressed audio.
    Audio,
    /// Subtitles, text or bitmap.
    Subtitle,
}

/// Which codec a track's samples are, named the way `ffprobe` and browsers name it.
///
/// Containers identify codecs their own way: `V_MPEG4/ISO/AVC` in Matroska, an
/// `avc1` sample entry in MP4. A recognised identifier becomes the matching
/// variant. Anything fbtt does not name becomes [`Codec::Other`] holding the
/// container's own identifier string, so a file with an exotic track still
/// probes and the dashboard has something to show.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Codec {
    /// H.264/AVC: `V_MPEG4/ISO/AVC`, `avc1` or `avc3`.
    H264,
    /// H.265/HEVC: `V_MPEGH/ISO/HEVC`, `hvc1` or `hev1`.
    Hevc,
    /// AV1: `V_AV1`, `av01`.
    Av1,
    /// VP8: `V_VP8`, `vp08`.
    Vp8,
    /// VP9: `V_VP9`, `vp09`.
    Vp9,
    /// AAC of any object type; the config record says which. `A_AAC*`, `mp4a`.
    Aac,
    /// Opus: `A_OPUS`, `Opus`.
    Opus,
    /// FLAC: `A_FLAC`, `fLaC`.
    Flac,
    /// MPEG Layer III: `A_MPEG/L3`, `mp3 ` or `Mp3 `.
    Mp3,
    /// Dolby AC-3: `A_AC3`, `ac-3`.
    Ac3,
    /// Dolby E-AC-3: `A_EAC3`, `ec-3`.
    Eac3,
    /// DTS of any profile: `A_DTS*`, `dts `.
    Dts,
    /// Dolby TrueHD: `A_TRUEHD`.
    TrueHd,
    /// Vorbis: `A_VORBIS`.
    Vorbis,
    /// SubRip text subtitles: `S_TEXT/UTF8`.
    SubRip,
    /// SubStation Alpha text subtitles: `S_TEXT/ASS` and `S_TEXT/SSA`.
    Ass,
    /// WebVTT text subtitles: `S_TEXT/WEBVTT`, `wvtt`.
    WebVtt,
    /// QuickTime `mov_text` text subtitles: `tx3g`.
    MovText,
    /// HDMV presentation graphics bitmap subtitles: `S_HDMV/PGS`.
    Pgs,
    /// VobSub bitmap subtitles: `S_VOBSUB`.
    VobSub,
    /// A codec fbtt does not name, holding the container's identifier.
    Other(String),
}

impl Codec {
    /// The codec name, as `ffprobe` prints it and as the web codec vocabulary spells it.
    ///
    /// [`Codec::Other`] returns the container's identifier verbatim, which is
    /// the best name available for a codec fbtt does not model.
    pub fn name(&self) -> &str {
        match self {
            Codec::H264 => "h264",
            Codec::Hevc => "hevc",
            Codec::Av1 => "av1",
            Codec::Vp8 => "vp8",
            Codec::Vp9 => "vp9",
            Codec::Aac => "aac",
            Codec::Opus => "opus",
            Codec::Flac => "flac",
            Codec::Mp3 => "mp3",
            Codec::Ac3 => "ac3",
            Codec::Eac3 => "eac3",
            Codec::Dts => "dts",
            Codec::TrueHd => "truehd",
            Codec::Vorbis => "vorbis",
            Codec::SubRip => "subrip",
            Codec::Ass => "ass",
            Codec::WebVtt => "webvtt",
            Codec::MovText => "mov_text",
            Codec::Pgs => "pgs",
            Codec::VobSub => "vobsub",
            Codec::Other(name) => name,
        }
    }

    /// Whether the track's samples can be copied into an fMP4 untouched.
    ///
    /// These are the codecs a browser demuxes from an fMP4 with a sample entry
    /// fbtt can write: the three video codecs every browser carries, and the
    /// audio codecs whose sample entries are spelled out in ISO/IEC 14496-3,
    /// 14496-30 and the Dolby and Opus registrations. Video codecs Matroska or
    /// MPEG-TS carry alone, Vorbis, DTS, TrueHD, and bitmap or text subtitles
    /// are not remuxable, and get an empty [`TrackInfo::mime_codecs`](crate::TrackInfo::mime_codecs).
    pub fn remuxable(&self) -> bool {
        matches!(
            self,
            Codec::H264
                | Codec::Hevc
                | Codec::Av1
                | Codec::Aac
                | Codec::Opus
                | Codec::Flac
                | Codec::Mp3
                | Codec::Ac3
                | Codec::Eac3
        )
    }

    /// Whether a subtitle track's payload is text fbtt can rewrite as WebVTT.
    pub fn text_subtitle(&self) -> bool {
        matches!(
            self,
            Codec::SubRip | Codec::Ass | Codec::WebVtt | Codec::MovText
        )
    }
}

/// What a Matroska track holds: its codec and codec strings.
///
/// `id` is the container's `CodecID` string and `private` its `CodecPrivate`
/// element. For H.264, HEVC and AV1 Matroska stores the plain ISO BMFF
/// configuration record (`avcC`, `hvcC`, `av1C`) without its box header, for AAC
/// the `AudioSpecificConfig`, and for FLAC the `fLaC` stream header, so the
/// record layouts the MP4 path uses apply to both containers.
pub(crate) fn matroska_codec(id: &str, private: Option<&[u8]>) -> (Codec, Vec<String>) {
    let codec = match id {
        "V_MPEG4/ISO/AVC" => Codec::H264,
        "V_MPEGH/ISO/HEVC" => Codec::Hevc,
        "V_AV1" => Codec::Av1,
        "V_VP8" => Codec::Vp8,
        "V_VP9" => Codec::Vp9,
        "A_OPUS" => Codec::Opus,
        "A_FLAC" => Codec::Flac,
        "A_MPEG/L3" => Codec::Mp3,
        "A_AC3" => Codec::Ac3,
        "A_EAC3" => Codec::Eac3,
        "A_VORBIS" => Codec::Vorbis,
        "S_TEXT/UTF8" => Codec::SubRip,
        "S_TEXT/ASS" | "S_TEXT/SSA" => Codec::Ass,
        "S_TEXT/WEBVTT" => Codec::WebVtt,
        "S_HDMV/PGS" => Codec::Pgs,
        "S_VOBSUB" => Codec::VobSub,
        // Matroska splits AAC into sub-ids (`A_AAC/MPEG4/LC`, ...) and DTS into
        // profiles (`A_DTS`, `A_DTS/LOSSLESS`, ...), and writes `A_TRUEHD` for
        // TrueHD, so those three are matched by prefix.
        _ if id.starts_with("A_AAC") => Codec::Aac,
        _ if id.starts_with("A_DTS") => Codec::Dts,
        "A_TRUEHD" => Codec::TrueHd,
        _ => Codec::Other(id.to_string()),
    };
    let mime_codecs = match &codec {
        Codec::H264 => private
            .and_then(avcc_record)
            .map(|record| vec![avc1_from(&record)])
            .unwrap_or_default(),
        Codec::Hevc => private
            .and_then(hvcc_record)
            .map(|record| vec![hvc1_from(&record)])
            .unwrap_or_default(),
        Codec::Av1 => private
            .and_then(av1c_record)
            .map(|record| vec![av01_from(&record)])
            .unwrap_or_default(),
        Codec::Aac => private
            .and_then(audio_specific_config_string)
            .map(|string| vec![string])
            .unwrap_or_default(),
        // `fLaC` is the sample entry an fMP4 uses for FLAC, next to the
        // lowercase name players match on, and `mp4a.6B` is how RFC 6381 names
        // MPEG Layer III inside MPEG-4.
        Codec::Flac => vec!["flac".to_string(), "fLaC".to_string()],
        Codec::Mp3 => vec!["mp4a.6B".to_string()],
        Codec::Opus => vec!["opus".to_string()],
        Codec::Ac3 => vec!["ac-3".to_string()],
        Codec::Eac3 => vec!["ec-3".to_string()],
        _ => Vec::new(),
    };
    (codec, mime_codecs)
}

/// What an MP4 track holds: its codec and codec strings, from its sample entry.
pub(crate) fn mp4_codec(entry: &atom::Codec) -> (Codec, Vec<String>) {
    match entry {
        atom::Codec::Avc1(entry) => (Codec::H264, vec![avc1_from(&entry.avcc)]),
        atom::Codec::Avc3(entry) => (Codec::H264, vec![avc1_from(&entry.avcc)]),
        atom::Codec::Hvc1(entry) => (Codec::Hevc, vec![hvc1_from(&entry.hvcc)]),
        atom::Codec::Hev1(entry) => (Codec::Hevc, vec![hvc1_from(&entry.hvcc)]),
        atom::Codec::Av01(entry) => (Codec::Av1, vec![av01_from(&entry.av1c)]),
        atom::Codec::Mp4a(entry) => {
            // The `objectTypeIndication` is what names the codec, and the AudioSpecificConfig is a
            // detail of the AAC types: MPEG Layer III in MPEG-4 syntax, and AAC carried with ADTS
            // headers, legitimately carry no descriptor config at all, so consulting it for the
            // other types invents an object type the file never stated.
            let dec = &entry.esds.es_desc.dec_config;
            let object_type = dec.object_type_indication;
            match object_type {
                // AAC of any object type: MPEG-4, and the MPEG-2 backwards compatible ones. A
                // descriptor with no `DecoderSpecificInfo` still names a codec, so it gets the
                // string AAC-LC carries in practice rather than none: `mime_codecs` is empty only
                // for codecs fbtt does not remux, and this one it does.
                0x40 | 0x66 | 0x67 | 0x68 => (
                    Codec::Aac,
                    dec.dec_specific
                        .as_ref()
                        .and_then(|specific| audio_specific_config_string(&specific.raw))
                        .map(|string| vec![string])
                        .unwrap_or_else(|| vec!["mp4a.40.2".to_string()]),
                ),
                // MPEG Layer III, and its MPEG-1 source.
                0x6b => (Codec::Mp3, vec!["mp4a.6B".to_string()]),
                0x69 => (Codec::Mp3, vec!["mp4a.69".to_string()]),
                // Anything else in an `mp4a` sample entry is not something fbtt remuxes.
                _ => (other(&format!("mp4a.{object_type:02X}")), Vec::new()),
            }
        }
        atom::Codec::Mp3(_) => (Codec::Mp3, vec!["mp4a.6B".to_string()]),
        atom::Codec::Flac(_) => (Codec::Flac, vec!["flac".to_string(), "fLaC".to_string()]),
        atom::Codec::Opus(_) => (Codec::Opus, vec!["opus".to_string()]),
        atom::Codec::Ac3(_) => (Codec::Ac3, vec!["ac-3".to_string()]),
        atom::Codec::Eac3(_) => (Codec::Eac3, vec!["ec-3".to_string()]),
        atom::Codec::Tx3g(_) => (Codec::MovText, Vec::new()),
        atom::Codec::Wvtt(_) => (Codec::WebVtt, Vec::new()),
        atom::Codec::Vp08(_) => (Codec::Vp8, Vec::new()),
        atom::Codec::Vp09(_) => (Codec::Vp9, Vec::new()),
        atom::Codec::Unknown(fourcc, _) => (other(&fourcc_text(fourcc)), Vec::new()),
        atom::Codec::Jpeg(_) => (other("jpeg"), Vec::new()),
        atom::Codec::Uncv(_) => (other("uncv"), Vec::new()),
        atom::Codec::Ipcm(_) => (other("ipcm"), Vec::new()),
        atom::Codec::Fpcm(_) => (other("fpcm"), Vec::new()),
        atom::Codec::Sowt(_) => (other("sowt"), Vec::new()),
        atom::Codec::Twos(_) => (other("twos"), Vec::new()),
        atom::Codec::Lpcm(_) => (other("lpcm"), Vec::new()),
        atom::Codec::In24(_) => (other("in24"), Vec::new()),
        atom::Codec::In32(_) => (other("in32"), Vec::new()),
        atom::Codec::Fl32(_) => (other("fl32"), Vec::new()),
        atom::Codec::Fl64(_) => (other("fl64"), Vec::new()),
        atom::Codec::S16l(_) => (other("s16l"), Vec::new()),
        atom::Codec::Samr(_) => (other("samr"), Vec::new()),
        atom::Codec::Mett(_) => (other("mett"), Vec::new()),
        atom::Codec::Metx(_) => (other("metx"), Vec::new()),
        atom::Codec::Urim(_) => (other("urim"), Vec::new()),
        atom::Codec::Camm(_) => (other("camm"), Vec::new()),
        atom::Codec::Evte(_) => (other("evte"), Vec::new()),
        atom::Codec::Mebx(_) => (other("mebx"), Vec::new()),
        // A sample entry mp4-atom does not model. Its fourcc lives in a variant
        // there is no name for, so the codec gets a plain label.
        _ => (other("unknown"), Vec::new()),
    }
}

/// A [`Codec::Other`] with no codec strings, for a codec fbtt does not remux.
fn other(name: &str) -> Codec {
    Codec::Other(name.to_string())
}

/// The fourcc of a sample entry fbtt does not name, as text.
fn fourcc_text(fourcc: &mp4_atom::FourCC) -> String {
    let bytes = u32::from(*fourcc).to_be_bytes();
    String::from_utf8_lossy(&bytes).trim().to_string()
}

/// An `AVCDecoderConfigurationRecord`, from the bytes Matroska stores.
fn avcc_record(bytes: &[u8]) -> Option<mp4_atom::Avcc> {
    mp4_atom::Avcc::decode_body(&mut &bytes[..]).ok()
}

/// The same record, for the fmp4 test that checks a written sample entry against
/// the fixture's own `CodecPrivate` bytes rather than a restatement of them.
#[cfg(test)]
pub(crate) fn avcc_private(bytes: &[u8]) -> Option<mp4_atom::Avcc> {
    avcc_record(bytes)
}

/// A `HEVCDecoderConfigurationRecord`, from the bytes Matroska stores.
fn hvcc_record(bytes: &[u8]) -> Option<mp4_atom::Hvcc> {
    mp4_atom::Hvcc::decode_body(&mut &bytes[..]).ok()
}

/// An `AV1CodecConfigurationRecord`, from the bytes Matroska stores.
fn av1c_record(bytes: &[u8]) -> Option<mp4_atom::Av1c> {
    mp4_atom::Av1c::decode_body(&mut &bytes[..]).ok()
}

/// The `avc1` codec string (ISO/IEC 14496-15 Annex B.2.1): the profile,
/// profile-compatibility and level indication bytes of the `avcC` record, as
/// `avc1.64000A`.
fn avc1_from(record: &mp4_atom::Avcc) -> String {
    format!(
        "avc1.{:02X}{:02X}{:02X}",
        record.avc_profile_indication, record.profile_compatibility, record.avc_level_indication
    )
}

/// The `hvc1` codec string, ISO/IEC 14496-15 Annex E.6, from an `hvcC` record:
/// the profile space as a letter, the profile idc, the profile compatibility
/// flags, the tier and level, and the constraint indicators. A Main profile
/// stream at Level 3.0 reads `hvc1.1.6.L30.90`, which is what a browser's
/// `canPlayType` and an HLS manifest's `CODECS` attribute expect.
fn hvc1_from(record: &mp4_atom::Hvcc) -> String {
    let space = match record.general_profile_space {
        0 => "",
        1 => "A",
        2 => "B",
        _ => "C",
    };
    // The compatibility flags are written least significant bit first, so the
    // Annex reads them bit reversed: a Main profile stream flags Main10 and Main
    // Still Picture, which is 0x60 on the wire and `6` in the string. Annex E.6
    // prints the field for whatever value it holds, so zero is `0` and not an
    // absent field: `hvc1.1.0.L93`, which is the string a browser compares
    // against when it is handed a profile with no compatibility flags.
    let compatibility =
        match u32::from_be_bytes(record.general_profile_compatibility_flags).reverse_bits() {
            0 => String::from("0"),
            flags => format!("{flags:X}"),
        };

    // Constraint indicators keep their byte order, minus trailing zero bytes.
    let mut constraints: Vec<String> = record
        .general_constraint_indicator_flags
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect();
    while constraints.last().is_some_and(|last| last == "00") {
        constraints.pop();
    }

    let tier = if record.general_tier_flag { 'H' } else { 'L' };
    let level = record.general_level_idc;

    let mut string = String::from("hvc1.");
    string.push_str(space);
    string.push_str(&record.general_profile_idc.to_string());
    if !compatibility.is_empty() {
        string.push('.');
        string.push_str(&compatibility);
    }
    string.push('.');
    string.push(tier);
    string.push_str(&level.to_string());
    if !constraints.is_empty() {
        string.push('.');
        string.push_str(&constraints.join("."));
    }
    string
}

/// The `av01` codec string, ISO/IEC 14496-15 Annex E.5: the sequence profile,
/// the level as two digits with its tier letter, and the bit depth, as
/// `av01.0.00M.08`.
fn av01_from(record: &mp4_atom::Av1c) -> String {
    let tier = if record.seq_tier_0 { 'H' } else { 'M' };
    let depth = if record.twelve_bit {
        12
    } else if record.high_bitdepth {
        10
    } else {
        8
    };
    format!(
        "av01.{}.{:02}{}.{:02}",
        record.seq_profile, record.seq_level_idx_0, tier, depth
    )
}

/// The `audioObjectType` of an `AudioSpecificConfig`: its first five bits, or
/// those five bits all set plus a six bit escape (ISO/IEC 14496-3 1.6.2.3).
fn audio_object_type(config: &[u8]) -> Option<u8> {
    let first = *config.first()?;
    let object_type = first >> 3;
    if object_type != 31 {
        return Some(object_type);
    }
    let second = *config.get(1)?;
    Some(32 + ((first & 0x07) << 3 | second >> 5))
}

/// The `mp4a.40.N` codec string for an `AudioSpecificConfig`, where `N` is its
/// audio object type: 2 for AAC-LC, which is the `mp4a.40.2` every browser accepts.
///
/// Both containers reach this with the same bytes: MP4 through the `esds`
/// `DecoderSpecificInfo`, Matroska through a `CodecPrivate` that is the config
/// record on its own. A track whose descriptor carries no config gets no codec
/// string, because the file never stated which object type it holds.
fn audio_specific_config_string(config: &[u8]) -> Option<String> {
    Some(format!("mp4a.40.{}", audio_object_type(config)?))
}

/// The sample entry one track needs in an fMP4's `stsd`.
///
/// `track` says which kind of media the samples are and which codec they use.
/// The other four are what the containers state alongside, and which one matters
/// depends on the codec:
///
/// * H.264, HEVC, AV1 and AAC: `private` is Matroska's `CodecPrivate`, which for
///   the three video codecs is the plain `avcC`/`hvcC`/`av1C` configuration
///   record and for AAC the `AudioSpecificConfig`; MP4 carries the same records
///   as its sample entries.
/// * Opus and FLAC: `private` is the Ogg-style header the containers store -
///   `OpusHead` and the `fLaC` stream header - from which the ISOBMFF
///   equivalent (`dOps`, `dfLa`) is rebuilt.
/// * AC-3 and E-AC-3: no `CodecPrivate` exists, so `first_frame` is one complete
///   elementary stream frame whose syncframe header says everything `dac3` and
///   `dec3` hold. A file is accepted because its first frame parses; frames that
///   contradict it are a Task 3 problem, like every other sample-bound claim.
/// * `audio` is the track's (sample rate, channels) when the container states
///   them, and `video` its (width, height); AC-3 and E-AC-3 take rate and channel
///   count from their frame header instead, so the `Audio` box cannot disagree
///   with the `dac3`/`dec3` beside it.
///
/// A codec with no fMP4 sample entry fbtt can write is [`MediaError::Unsupported`],
/// and so is a codec whose sample entry needs a configuration record the file
/// does not carry: there is nothing to have got wrong. A record that is there but
/// does not parse is [`MediaError::Malformed`], because the file claimed it and
/// then did not hold it up.
pub fn sample_entry(
    track: &TrackInfo,
    private: Option<&[u8]>,
    first_frame: Option<&[u8]>,
    audio: Option<(u32 /*rate*/, u16 /*channels*/)>,
    video: Option<(u16, u16)>,
) -> Result<atom::Codec, MediaError> {
    match track.kind {
        TrackKind::Video => video_entry(track, private, video),
        TrackKind::Audio => audio_entry(track, private, first_frame, audio),
        TrackKind::Subtitle => Err(MediaError::Unsupported(format!(
            "subtitle track ({}): fMP4 has no sample entry fbtt writes for it",
            track.codec.name()
        ))),
    }
}

/// The video sample entry: the configuration record out of `CodecPrivate`, under
/// the sample entry the RFC 6381 codec string names.
fn video_entry(
    track: &TrackInfo,
    private: Option<&[u8]>,
    video: Option<(u16, u16)>,
) -> Result<atom::Codec, MediaError> {
    let (width, height) = video.unwrap_or((0, 0));
    let visual = atom::Visual {
        data_reference_index: 1,
        width,
        height,
        ..Default::default()
    };
    match &track.codec {
        Codec::H264 => {
            let record = configuration(private, "H.264", avcc_record)?;
            Ok(atom::Codec::Avc1(atom::Avc1 {
                visual,
                avcc: record,
                btrt: None,
                colr: Vec::new(),
                pasp: None,
                taic: None,
                fiel: None,
                spatial: None,
            }))
        }
        Codec::Hevc => {
            let record = configuration(private, "HEVC", hvcc_record)?;
            Ok(atom::Codec::Hvc1(atom::Hvc1 {
                visual,
                hvcc: record,
                lhvc: None,
                btrt: None,
                colr: Vec::new(),
                pasp: None,
                taic: None,
                fiel: None,
                ccst: None,
                spatial: None,
            }))
        }
        Codec::Av1 => {
            let record = configuration(private, "AV1", av1c_record)?;
            Ok(atom::Codec::Av01(atom::Av01 {
                visual,
                av1c: record,
                btrt: None,
                ccst: None,
                colr: Vec::new(),
                pasp: None,
                taic: None,
                spatial: None,
            }))
        }
        other => Err(MediaError::Unsupported(format!(
            "{} video has no fMP4 sample entry fbtt writes",
            other.name()
        ))),
    }
}

/// `CodecPrivate` as a configuration record: absent is `Unsupported`, present and
/// unreadable is `Malformed`, per [`sample_entry`].
fn configuration<T>(
    private: Option<&[u8]>,
    codec: &str,
    parse: fn(&[u8]) -> Option<T>,
) -> Result<T, MediaError> {
    match private {
        None => Err(MediaError::Unsupported(format!(
            "{codec} track without a CodecPrivate configuration record"
        ))),
        Some(bytes) => parse(bytes).ok_or_else(|| {
            MediaError::Malformed(format!("{codec} configuration record does not parse"))
        }),
    }
}

/// The audio sample entry, dispatched by codec: each codec's sample entry is the
/// one ISOBMFF and its codec's own specification register.
fn audio_entry(
    track: &TrackInfo,
    private: Option<&[u8]>,
    first_frame: Option<&[u8]>,
    audio: Option<(u32, u16)>,
) -> Result<atom::Codec, MediaError> {
    match &track.codec {
        // AAC: the `AudioSpecificConfig` goes into the `esds` verbatim under the
        // object-type indication MPEG-4 AAC carries, 0x40. The audio object type
        // its first five bits state (2 for AAC-LC, the config these fixtures
        // carry) rides on inside the config itself and in the codec string,
        // which is exactly how the RFC 6381 `mp4a.40.2` is spelled.
        Codec::Aac => match private {
            None => Err(MediaError::Unsupported(
                "AAC track without an AudioSpecificConfig CodecPrivate".to_string(),
            )),
            Some(config) => {
                audio_object_type(config).ok_or_else(|| {
                    MediaError::Malformed(
                        "AudioSpecificConfig is too short for its object type".to_string(),
                    )
                })?;
                Ok(atom::Codec::Mp4a(atom::Mp4a {
                    audio: audio_box(audio, "AAC")?,
                    esds: esds_for(0x40, Some(config.to_vec())),
                    btrt: None,
                    taic: None,
                }))
            }
        },
        // MPEG Layer III: RFC 6381's `mp4a.6B`, an `mp4a` whose descriptor names
        // Layer III and carries no config, because Layer III frames are self
        // describing. (The `.mp3` sample entry exists too; the `mp4a` one is what
        // browsers and FFmpeg write for Layer III in ISO BMFF.)
        Codec::Mp3 => Ok(atom::Codec::Mp4a(atom::Mp4a {
            audio: audio_box(audio, "MPEG Layer III")?,
            esds: esds_for(0x6b, None),
            btrt: None,
            taic: None,
        })),
        // Opus: RFC 7845 §5.1 spells `dOps` as a restatement of `OpusHead`, and
        // the `Audio` box always plays at 48 kHz whatever `OpusHead` says the
        // original input rate was: that rate is metadata, and Opus decodes at 48.
        Codec::Opus => match private {
            None => Err(MediaError::Unsupported(
                "Opus track without an OpusHead CodecPrivate".to_string(),
            )),
            Some(head) => {
                let dops = dops_from_opus_head(head)?;
                Ok(atom::Codec::Opus(atom::Opus {
                    audio: atom::Audio {
                        data_reference_index: 1,
                        channel_count: u16::from(dops.output_channel_count),
                        sample_size: 16,
                        sample_rate: OPUS_PLAYBACK_RATE.into(),
                    },
                    dops,
                    btrt: None,
                }))
            }
        },
        // FLAC: `dfLa` holds the stream's metadata blocks, and Matroska keeps
        // only `STREAMINFO`, which is all an fMP4 needs: it states the sample
        // rate, the channel count and the bit depth the `Audio` box repeats.
        Codec::Flac => match private {
            None => Err(MediaError::Unsupported(
                "FLAC track without a fLaC CodecPrivate header".to_string(),
            )),
            Some(header) => {
                let block = stream_info(header)?;
                let (header_rate, header_channels, bits) = flac_stream_fields(&block);
                let (rate, channels) = match audio {
                    Some((rate, channels)) => (rate, channels),
                    None => (header_rate, header_channels),
                };
                Ok(atom::Codec::Flac(atom::Flac {
                    audio: audio_box_rate(rate, channels, bits)?,
                    dfla: atom::Dfla {
                        blocks: vec![block],
                    },
                }))
            }
        },
        Codec::Ac3 => match first_frame {
            None => Err(MediaError::Unsupported(
                "AC-3 track without a first frame to read its header from".to_string(),
            )),
            Some(frame) => {
                let sync = dolby_syncframe(frame)?;
                match sync.ac3 {
                    Some(ac3) => {
                        agree(audio, sync.sample_rate, sync.channels, "AC-3")?;
                        Ok(atom::Codec::Ac3(ac3))
                    }
                    None => Err(MediaError::Malformed(
                        "an AC-3 track's first frame is an E-AC-3 syncframe".to_string(),
                    )),
                }
            }
        },
        Codec::Eac3 => match first_frame {
            None => Err(MediaError::Unsupported(
                "E-AC-3 track without a first frame to read its header from".to_string(),
            )),
            Some(frame) => {
                let sync = dolby_syncframe(frame)?;
                match sync.eac3 {
                    Some(eac3) => {
                        agree(audio, sync.sample_rate, sync.channels, "E-AC-3")?;
                        Ok(atom::Codec::Eac3(eac3))
                    }
                    None => Err(MediaError::Malformed(
                        "an E-AC-3 track's first frame is an AC-3 syncframe".to_string(),
                    )),
                }
            }
        },
        other => Err(MediaError::Unsupported(format!(
            "{} audio has no fMP4 sample entry fbtt writes",
            other.name()
        ))),
    }
}

/// Opus always decodes at 48 kHz; the sample rate the `OpusHead` states is the
/// rate the original input had, which `dOps` keeps as `input_sample_rate` beside
/// this one.
const OPUS_PLAYBACK_RATE: u16 = 48_000;

/// The `Audio` box's fixed fields from the container's (rate, channels).
fn audio_box(audio: Option<(u32, u16)>, codec: &str) -> Result<atom::Audio, MediaError> {
    match audio {
        None => Err(MediaError::Unsupported(format!(
            "{codec} track without a stated sample rate"
        ))),
        Some((rate, channels)) => audio_box_rate(rate, channels, 16),
    }
}

/// The `Audio` box's fixed fields, with the sample size given: 16 for everything
/// that carries encoded bits per sample, and FLAC's real bit depth for FLAC.
///
/// A rate over `u16::MAX` is `Malformed`: no real audio track runs that fast, and
/// the box cannot write one.
fn audio_box_rate(rate: u32, channels: u16, sample_size: u16) -> Result<atom::Audio, MediaError> {
    if channels == 0 {
        return Err(MediaError::Malformed(
            "an audio track with no channels has no Audio box".to_string(),
        ));
    }
    let whole = u16::try_from(rate)
        .map_err(|_| MediaError::Malformed(format!("sample rate {rate} is above a u16")))?;
    Ok(atom::Audio {
        data_reference_index: 1,
        channel_count: channels,
        sample_size,
        sample_rate: whole.into(),
    })
}

/// What `dOps` restates from an `OpusHead`: same pre-skip, input rate, gain and
/// channel mapping table, with the version the Opus-specific box calls 0.
///
/// Bad magic or a header too short for its own channel table is `Malformed`,
/// because the private data is there and does not hold together; a version fbtt
/// does not know is `Unsupported`, because that is a different encapsulation.
fn dops_from_opus_head(head: &[u8]) -> Result<atom::Dops, MediaError> {
    if head.len() < 19 || &head[..8] != b"OpusHead" {
        return Err(MediaError::Malformed(
            "an Opus CodecPrivate is a 19-byte OpusHead".to_string(),
        ));
    }
    if head[8] != 1 {
        return Err(MediaError::Unsupported(format!(
            "OpusHead version {} is not the 1 this writes",
            head[8]
        )));
    }
    let channels = head[9];
    let pre_skip = u16::from_le_bytes([head[10], head[11]]);
    let input_sample_rate = u32::from_le_bytes([head[12], head[13], head[14], head[15]]);
    let output_gain = i16::from_le_bytes([head[16], head[17]]);
    let family = head[18];
    let channel_mapping = if family == 0 {
        if channels > 2 {
            return Err(MediaError::Malformed(format!(
                "channel mapping family 0 covers 1 or 2 channels, not {channels}"
            )));
        }
        None
    } else {
        if head.len() < 21 + usize::from(channels) {
            return Err(MediaError::Malformed(format!(
                "OpusHead's channel table needs {} bytes and the header holds {}",
                21 + usize::from(channels),
                head.len()
            )));
        }
        let streams = head[19];
        let coupled = head[20];
        let mapping = head[21..21 + usize::from(channels)].to_vec();
        if streams == 0 || u16::from(coupled) > u16::from(streams) {
            return Err(MediaError::Malformed(format!(
                "OpusHead states {coupled} coupled streams out of {streams}"
            )));
        }
        let decoded = u16::from(streams) + u16::from(coupled);
        if mapping
            .iter()
            .any(|&index| index != 255 && u16::from(index) >= decoded)
        {
            return Err(MediaError::Malformed(
                "OpusHead maps a channel it does not decode".to_string(),
            ));
        }
        Some(atom::OpusChannelMapping {
            family,
            stream_count: streams,
            coupled_count: coupled,
            channel_mapping: mapping,
        })
    };
    Ok(atom::Dops {
        output_channel_count: channels,
        pre_skip,
        input_sample_rate,
        output_gain,
        channel_mapping,
    })
}

/// The mandatory first metadata block of a `fLaC` stream header, as a `dfLa`
/// can hold it.
///
/// RFC 9639 puts a `STREAMINFO` block first and demands it, and Matroska stores
/// the stream header exactly that way. A header without it is `Malformed`;
/// [`sample_entry`] has already said so for a header that is absent.
fn stream_info(header: &[u8]) -> Result<atom::FlacMetadataBlock, MediaError> {
    if header.len() < 8 || &header[..4] != b"fLaC" {
        return Err(MediaError::Malformed(
            "a FLAC CodecPrivate is a fLaC stream header".to_string(),
        ));
    }
    // RFC 9639 §8.2 lays the 34-byte payload out as: two block sizes, two
    // frame sizes, then one 64-bit word holding the sample rate (20 bits), the
    // channel count minus one (3), the bit depth minus one (5), and the sample
    // count (36), and a final 16-byte MD5 of the decoded stream.
    let block = u32::from_be_bytes([header[4], header[5], header[6], header[7]]);
    let kind = (block >> 24) & 0x7f;
    let length = block & 0x00ff_ffff;
    // 4 fLaC + 4 block header + 34 payload, or the slice below reaches past
    // the bytes that are there.
    if kind != 0 || length != 34 || header.len() < 42 {
        return Err(MediaError::Malformed(
            "a FLAC stream header's first block is a 34-byte STREAMINFO".to_string(),
        ));
    }
    let p = &header[8..42];
    let word = u64::from_be_bytes([p[10], p[11], p[12], p[13], p[14], p[15], p[16], p[17]]);
    let sample_rate = (word >> 44) as u32;
    if sample_rate == 0 {
        return Err(MediaError::Malformed(
            "a STREAMINFO with a zero sample rate states no stream".to_string(),
        ));
    }
    Ok(atom::FlacMetadataBlock::StreamInfo {
        minimum_block_size: u16::from_be_bytes([p[0], p[1]]),
        maximum_block_size: u16::from_be_bytes([p[2], p[3]]),
        minimum_frame_size: atom::u24::from([p[4], p[5], p[6]]),
        maximum_frame_size: atom::u24::from([p[7], p[8], p[9]]),
        sample_rate,
        num_channels_minus_one: ((word >> 41) & 0x07) as u8,
        bits_per_sample_minus_one: ((word >> 36) & 0x1f) as u8,
        number_of_interchannel_samples: word & 0x000f_ffff_ffff,
        md5_checksum: p[18..34].to_vec(),
    })
}

/// What a parsed `STREAMINFO` states for the `Audio` box: rate, channels, bits.
fn flac_stream_fields(block: &atom::FlacMetadataBlock) -> (u32, u16, u16) {
    match block {
        atom::FlacMetadataBlock::StreamInfo {
            sample_rate,
            num_channels_minus_one,
            bits_per_sample_minus_one,
            ..
        } => (
            *sample_rate,
            u16::from(*num_channels_minus_one) + 1,
            u16::from(*bits_per_sample_minus_one) + 1,
        ),
        // `stream_info` only ever returns a `STREAMINFO`.
        _ => (48_000, 2, 16),
    }
}

/// An `esds` with one descriptor of `object_type` and, when `config` is `Some`,
/// that config as its `DecoderSpecificInfo`.
///
/// Buffer sizes and the stream type stay at their defaults: they describe an
/// object-descriptor system MP4 audio never uses, and FFmpeg writes the same
/// zeros.
fn esds_for(object_type: u8, config: Option<Vec<u8>>) -> atom::Esds {
    use mp4_atom::esds::{DecoderConfig, DecoderSpecific, EsDescriptor};
    atom::Esds {
        es_desc: EsDescriptor {
            dec_config: DecoderConfig {
                object_type_indication: object_type,
                dec_specific: config.map(|raw| DecoderSpecific {
                    profile: 0,
                    freq_index: 0,
                    chan_conf: 0,
                    raw,
                }),
                ..Default::default()
            },
            ..Default::default()
        },
    }
}

/// The container's own rate and channel count, when it states them, against what
/// a codec's frame header says: AC-3 and E-AC-3 take both from the frame, so a
/// container that says 44.1 kHz while its frames say 48 is a file that
/// contradicts itself and gets [`MediaError::Malformed`].
fn agree(
    audio: Option<(u32, u16)>,
    rate: u32,
    channels: u16,
    codec: &str,
) -> Result<(), MediaError> {
    match audio {
        Some((track_rate, track_channels)) if track_rate != rate || track_channels != channels => {
            Err(MediaError::Malformed(format!(
                "{codec} frames say {rate} Hz and {channels} channels, the track says {track_rate} Hz and {track_channels}"
            )))
        }
        _ => Ok(()),
    }
}

/// What an AC-3 or E-AC-3 syncframe header states, in the fields ISOBMFF wants.
///
/// The `ac3` and `eac3` halves are mutually exclusive: the bitstream id at bits
/// 40-44 picks the codec, as FFmpeg's parser does (AC-3 caps it at 10, E-AC-3 at
/// 16), and exactly one half is `Some`.
struct DolbySync {
    sample_rate: u32,
    channels: u16,
    /// The `ac-3` box for a normal AC-3 frame, or `None` for an E-AC-3 one.
    ac3: Option<atom::Ac3>,
    /// The `ec-3` box for an E-AC-3 frame, or `None` for a normal AC-3 one.
    eac3: Option<atom::Eac3>,
}

/// The syncframe header of one complete AC-3 or E-AC-3 frame.
///
/// The fields live at fixed offsets in the first seven bytes, and both codecs
/// hide their shape behind the bitstream id at bits 40-44, which FFmpeg reads the
/// same way before branching. A short frame, a wrong syncword, a reserved code,
/// and a frame shorter than its own header count are all
/// [`MediaError::Malformed`]; a bitstream id above the 16 E-AC-3 allows, and the
/// DolbyNet and RealAudio AC-3 ids ISOBMFF never registered, are
/// [`MediaError::Unsupported`].
fn dolby_syncframe(frame: &[u8]) -> Result<DolbySync, MediaError> {
    if frame.len() < 7 {
        return Err(MediaError::Malformed(
            "a Dolby syncframe header is seven bytes".to_string(),
        ));
    }
    let head = u64::from_be_bytes([
        frame[0], frame[1], frame[2], frame[3], frame[4], frame[5], frame[6], 0,
    ]);
    // `bit(start, width)` counts from the top of the syncframe; `head` holds the
    // first 56 bits of the frame left-aligned in its top half.
    let bit = |start: u32, width: u32| -> u32 {
        // Every reader below keeps `start + width` at most 56, so the shift is
        // never negative and the masked value always fits a u32.
        let shift = 64 - start - width;
        u32::try_from((head >> shift) & (u64::MAX >> (64 - width))).expect("56 bits fit a u32")
    };
    if bit(0, 16) != 0x0b77 {
        return Err(MediaError::Malformed(format!(
            "a Dolby syncframe starts with the syncword 0B77, not {:04X}",
            bit(0, 16)
        )));
    }
    let bsid = bit(40, 5) as u8;
    if bsid > 16 {
        return Err(MediaError::Unsupported(format!(
            "bitstream id {bsid} is above the 16 E-AC-3 allows"
        )));
    }
    if bsid <= 10 {
        // The bitstream mode sits at 45-47 in a normal AC-3 header, where it is
        // one of `dac3`'s fields; in E-AC-3 those same bits belong to the volume
        // control parameters, so only the AC-3 branch reads them.
        let bsmod = bit(45, 3) as u8;
        return ac3_syncframe(frame, bsid, bsmod, &bit);
    }
    eac3_syncframe(frame, bsid, &bit)
}

/// Sample rates for the two-bit sample rate code.
const AC3_RATES: [u32; 3] = [48_000, 44_100, 32_000];
/// Channels per three-bit channel mode, and the low-frequency one on top.
const AC3_CHANNELS: [u16; 8] = [2, 1, 2, 3, 3, 4, 4, 5];
/// Block counts per two-bit block count code.
const EAC3_BLOCKS: [u32; 4] = [1, 2, 3, 6];
/// Frame sizes in 16-bit words, per frame size code and sample rate code: FFmpeg's
/// `ff_ac3_frame_size_tab`, which is the ETSI TS 102 366 table.
const AC3_FRAME_WORDS: [[u16; 3]; 38] = [
    [64, 69, 96],
    [64, 70, 96],
    [80, 87, 120],
    [80, 88, 120],
    [96, 104, 144],
    [96, 105, 144],
    [112, 121, 168],
    [112, 122, 168],
    [128, 139, 192],
    [128, 140, 192],
    [160, 174, 240],
    [160, 175, 240],
    [192, 208, 288],
    [192, 209, 288],
    [224, 243, 336],
    [224, 244, 336],
    [256, 278, 384],
    [256, 279, 384],
    [320, 348, 480],
    [320, 349, 480],
    [384, 417, 576],
    [384, 418, 576],
    [448, 487, 672],
    [448, 488, 672],
    [512, 557, 768],
    [512, 558, 768],
    [640, 696, 960],
    [640, 697, 960],
    [768, 835, 1152],
    [768, 836, 1152],
    [896, 975, 1344],
    [896, 976, 1344],
    [1024, 1114, 1536],
    [1024, 1115, 1536],
    [1152, 1253, 1728],
    [1152, 1254, 1728],
    [1280, 1393, 1920],
    [1280, 1394, 1920],
];

/// The normal AC-3 branch: everything `dac3` holds sits in the first seven bytes.
///
/// * fscod, the sample rate code, at 32-33: code 3 is no rate.
/// * The frame size code at 34-39: it stands for the (bit rate code, odd pad bit)
///   pair `dac3` writes as `bit_rate_code`, and for the frame's own length through
///   [`AC3_FRAME_WORDS`].
/// * The bitstream mode at 45-47, the channel mode at 48-50, the mix-level fields
///   the channel mode makes room for, and the low-frequency bit after them.
///
/// A bitstream id of 6 through 8 is DolbyNet and 9 or 10 is RealAudio: ISOBMFF's
/// `ac-3` registers the Dolby families up to 8, matching what FFmpeg refuses to
/// write. Bitstream ids under 8 also halve the sample rate once per step below 8
/// (the `max(bsid, 8) - 8` shift FFmpeg applies), so the `Audio` box says 24 kHz
/// where the rate code alone would say 48.
fn ac3_syncframe(
    frame: &[u8],
    bsid: u8,
    bsmod: u8,
    bit: &impl Fn(u32, u32) -> u32,
) -> Result<DolbySync, MediaError> {
    if bsid > 8 {
        return Err(MediaError::Unsupported(format!(
            "AC-3 bitstream id {bsid} is DolbyNet or RealAudio, which ISOBMFF does not define"
        )));
    }
    let fscod = bit(32, 2) as u8;
    if fscod == 3 {
        return Err(MediaError::Malformed(
            "AC-3 sample rate code 3 is not a rate".to_string(),
        ));
    }
    let frame_size_code = bit(34, 6) as usize;
    if frame_size_code > 37 {
        return Err(MediaError::Malformed(format!(
            "AC-3 frame size code {frame_size_code} is above 37"
        )));
    }
    let acmod = bit(48, 3) as usize;
    let mut at = 51;
    if acmod == 2 {
        at += 2; // the surround mode takes the two bits the mix levels would.
    } else {
        if acmod != 1 && acmod & 1 == 1 {
            at += 2; // center mix level
        }
        if acmod & 4 != 0 {
            at += 2; // surround mix level
        }
    }
    let lfeon = bit(at, 1) == 1;

    let frame_words = AC3_FRAME_WORDS[frame_size_code][fscod as usize];
    let frame_bytes = usize::from(frame_words) * 2;
    if frame.len() < frame_bytes {
        return Err(MediaError::Malformed(format!(
            "the AC-3 frame header counts {frame_bytes} bytes and the frame holds {}",
            frame.len()
        )));
    }

    let shift = u32::from(bsid.max(8) - 8);
    let sample_rate = AC3_RATES[fscod as usize] >> shift;
    let channels = AC3_CHANNELS[acmod] + u16::from(lfeon);
    Ok(DolbySync {
        sample_rate,
        channels,
        ac3: Some(atom::Ac3 {
            audio: audio_box_rate(sample_rate, channels, 16)?,
            dac3: atom::Ac3SpecificBox {
                fscod,
                bsid,
                bsmod,
                acmod: acmod as u8,
                lfeon,
                bit_rate_code: (frame_size_code >> 1) as u8,
            },
        }),
        eac3: None,
    })
}

/// The E-AC-3 branch: the independent substream's `dec3` fields.
///
/// * The frame type at 16-17 (3 is reserved) and the substream id at 18-20.
/// * The frame size at 21-31, in 16-bit words: it bounds the frame against the
///   sample's real length, and it is what the data rate is computed from.
/// * The sample rate code at 32-33; code 3 means the two bits after it are the
///   reduced rate code (24, 22.05, 16 kHz) and the frame holds six blocks, while
///   for the ordinary rates those two bits are the block count.
/// * The channel mode at 36-38 and the low-frequency bit at 39.
///
/// The `bit_rate` that `dec3` calls `data_rate` is FFmpeg's own computation: the
/// frame's bits over the samples its blocks carry. The substream's `bsmod`,
/// `asvc` and dependent-substream fields come from conditional metadata a fixed
/// seven bytes cannot reach, so they stay at what FFmpeg writes when it cannot
/// see them either: 0, false and none.
fn eac3_syncframe(
    frame: &[u8],
    bsid: u8,
    bit: &impl Fn(u32, u32) -> u32,
) -> Result<DolbySync, MediaError> {
    if bit(16, 2) == 3 {
        return Err(MediaError::Malformed(
            "E-AC-3 frame type 3 is reserved".to_string(),
        ));
    }
    let words = bit(21, 11) as u64;
    let frame_bytes = (words + 1) * 2;
    if frame_bytes < 7 {
        return Err(MediaError::Malformed(
            "an E-AC-3 frame is at least seven bytes".to_string(),
        ));
    }
    if u64::try_from(frame.len()).unwrap_or(u64::MAX) < frame_bytes {
        return Err(MediaError::Malformed(format!(
            "the E-AC-3 frame header counts {frame_bytes} bytes and the frame holds {}",
            frame.len()
        )));
    }
    let fscod = bit(32, 2) as u8;
    let (sample_rate, blocks) = if fscod == 3 {
        let reduced = bit(34, 2) as usize;
        if reduced == 3 {
            return Err(MediaError::Malformed(
                "E-AC-3 reduced sample rate code 3 is not a rate".to_string(),
            ));
        }
        (AC3_RATES[reduced] / 2, 6_u64)
    } else {
        (
            AC3_RATES[fscod as usize],
            u64::from(EAC3_BLOCKS[bit(34, 2) as usize]),
        )
    };
    let acmod = bit(36, 3) as usize;
    let lfeon = bit(39, 1) == 1;
    let channels = AC3_CHANNELS[acmod] + u16::from(lfeon);

    // 8 * frame bytes * rate / (blocks * 256 samples) is the bit rate FFmpeg
    // puts in `dec3`; it is computed in pieces so an absurd header cannot wrap
    // the multiplication, and the 13-bit field tops out well below any frame
    // the size code can state.
    let data_rate = 8_u64
        .checked_mul(frame_bytes)
        .and_then(|v| v.checked_mul(u64::from(sample_rate)))
        .and_then(|v| v.checked_div(blocks * 256))
        .ok_or_else(|| MediaError::Malformed("the E-AC-3 frame header overflows".to_string()))?
        / 1000;
    let data_rate = u16::try_from(data_rate).map_err(|_| {
        MediaError::Malformed(format!(
            "an E-AC-3 data rate of {data_rate} kbps is over a u16"
        ))
    })?;

    Ok(DolbySync {
        sample_rate,
        channels,
        ac3: None,
        eac3: Some(atom::Eac3 {
            audio: audio_box_rate(sample_rate, channels, 16)?,
            dec3: atom::Ec3SpecificBox {
                data_rate,
                substreams: vec![atom::Ec3IndependentSubstream {
                    fscod,
                    bsid,
                    asvc: false,
                    bsmod: 0,
                    acmod: acmod as u8,
                    lfeon,
                    num_dep_sub: 0,
                    chan_loc: None,
                }],
            },
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    // The round-trip tests encode and decode whole sample entries.
    use mp4_atom::{Decode, Encode};
    // `mp4_atom` re-exports the `Mp4a` and `Esds` boxes at its root but leaves the
    // MPEG-4 object descriptors in the module they are declared in.
    use mp4_atom::esds::{DecoderConfig, DecoderSpecific, EsDescriptor};
    use std::io::Cursor;
    use std::path::PathBuf;

    /// A [`TrackInfo`] for the codec `sample_entry` is being asked about.
    fn track(kind: TrackKind, codec: Codec) -> TrackInfo {
        TrackInfo {
            id: 1,
            kind,
            codec,
            mime_codecs: Vec::new(),
            language: None,
            name: None,
            default: true,
            forced: false,
        }
    }

    /// The `mp4a` sample entry, an `esds` with `object_type` and, when `config`
    /// is `Some`, that as its `DecoderSpecificInfo`.
    fn mp4a(object_type: u8, config: Option<Vec<u8>>) -> mp4_atom::Codec {
        mp4_atom::Codec::Mp4a(mp4_atom::Mp4a {
            audio: mp4_atom::Audio {
                data_reference_index: 1,
                channel_count: 1,
                sample_size: 16,
                sample_rate: mp4_atom::FixedPoint::default(),
            },
            esds: mp4_atom::Esds {
                es_desc: EsDescriptor {
                    dec_config: DecoderConfig {
                        object_type_indication: object_type,
                        dec_specific: config.map(|raw| DecoderSpecific {
                            profile: 0,
                            freq_index: 0,
                            chan_conf: 0,
                            raw,
                        }),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            },
            btrt: None,
            taic: None,
        })
    }

    /// The `esds` object type indication picks the codec, not the config record.
    ///
    /// 0x6B is MPEG Layer III in MPEG-4 syntax, and a file carrying it has no
    /// `AudioSpecificConfig` to consult, so this is the case in which consulting
    /// the config decided that the answer was AAC. 0xDD is nobody's Layer III:
    /// it is a codec fbtt names for nobody to remux.
    #[test]
    fn mp4a_object_types() {
        // Layer III, with no descriptor config at all.
        let (codec, strings) = mp4_codec(&mp4a(0x6b, None));
        assert_eq!(codec, Codec::Mp3, "0x6B is MPEG Layer III");
        assert_eq!(strings, ["mp4a.6B"], "named the way RFC 6381 names it");
        assert!(codec.remuxable(), "and a browser demuxes it from an fMP4");

        // The MPEG-1 half of the same family.
        let (codec, strings) = mp4_codec(&mp4a(0x69, None));
        assert_eq!(codec, Codec::Mp3);
        assert_eq!(strings, ["mp4a.69"]);

        // AAC with a config: object type 2, 48 kHz, which is what ffmpeg writes.
        let (codec, strings) = mp4_codec(&mp4a(0x40, Some(vec![0x12, 0x10])));
        assert_eq!(codec, Codec::Aac);
        assert_eq!(strings, ["mp4a.40.2"]);

        // The MPEG-2 backwards compatible types are AAC too, and their string comes
        // from the config like any other AAC track.
        for object_type in [0x66_u8, 0x67, 0x68] {
            let (codec, strings) = mp4_codec(&mp4a(object_type, Some(vec![0x12, 0x10])));
            assert_eq!(codec, Codec::Aac, "0x{object_type:02X} is AAC");
            assert_eq!(strings, ["mp4a.40.2"], "and the object type is its own");
        }

        // An object type fbtt does not know keeps the number it came with, is not
        // remuxable, and gets no codec string.
        let (codec, strings) = mp4_codec(&mp4a(0xdd, None));
        assert_eq!(codec, Codec::Other("mp4a.DD".to_string()));
        assert_eq!(codec.name(), "mp4a.DD");
        assert!(!codec.remuxable(), "nothing remuxes 0xDD");
        assert!(strings.is_empty(), "so a manifest has nothing to name");

        // AAC whose descriptor carries no config: still AAC, and still a codec fbtt
        // remuxes, so the string AAC-LC carries in practice goes with it.
        let (codec, strings) = mp4_codec(&mp4a(0x40, None));
        assert_eq!(codec, Codec::Aac);
        assert_eq!(
            strings,
            ["mp4a.40.2"],
            "no config is not an empty manifest list"
        );
    }

    /// A descriptor that names AAC but holds no `DecoderSpecificInfo`.
    ///
    /// `TrackInfo::mime_codecs` is empty only for codecs fbtt does not remux, and an
    /// `mp4a` without a config is still remuxable AAC, so it gets the object type
    /// such files carry in practice, AAC-LC, rather than nothing a browser can name.
    #[test]
    fn mp4a_aac_without_config_is_lc() {
        let (codec, strings) = mp4_codec(&mp4a(0x40, None));
        assert_eq!(codec, Codec::Aac);
        assert!(
            codec.remuxable(),
            "the track is still AAC an fMP4 can carry"
        );
        assert_eq!(
            strings,
            ["mp4a.40.2"],
            "AAC-LC, what a configless descriptor means in practice"
        );
    }

    /// A zero `general_profile_compatibility_flags` is printed as `0`, not dropped.
    ///
    /// ISO/IEC 14496-15 Annex E.6 spells a profile with no compatibility flags
    /// `hvc1.1.0.L93`: the field is always one of the dot separated parts.
    /// Omitting it produced `hvc1.1.L93`, which is a different string from the
    /// one a browser's `canPlayType` or an HLS manifest's `CODECS` attribute is
    /// compared against.
    #[test]
    fn hvc1_zero_compat_flags() {
        let record = mp4_atom::Hvcc {
            configuration_version: 1,
            general_profile_idc: 1,
            general_profile_compatibility_flags: [0, 0, 0, 0],
            general_constraint_indicator_flags: [0x90, 0, 0, 0, 0, 0],
            general_level_idc: 93,
            ..Default::default()
        };
        let string = hvc1_from(&record);
        assert!(
            string.contains(".0.L"),
            "a zero compatibility field is printed, not skipped: got {string}"
        );
        assert_eq!(
            string, "hvc1.1.0.L93.90",
            "the flags are printed as 0 between the profile and the level"
        );

        // A stream that does carry flags is spelled the way the fixture's real
        // `hvcC` is: 0x60 on the wire, bit reversed to `6`.
        let flagged = mp4_atom::Hvcc {
            general_profile_compatibility_flags: [0x60, 0, 0, 0],
            general_constraint_indicator_flags: [0x90, 0, 0, 0, 0, 0],
            general_level_idc: 30,
            ..record.clone()
        };
        assert_eq!(hvc1_from(&flagged), "hvc1.1.6.L30.90");
    }

    /// The AAC-LC config `12 10` writes an `mp4a` whose esds says AAC-LC and
    /// hands the config back unchanged.
    ///
    /// 0x12 is object type 2 with a 48 kHz frequency index and 0x10 is channel
    /// configuration 2: the config FFmpeg writes for a stereo AAC-LC track, and
    /// the one `expected.json`'s `mp4a.40.2` fixture carries. A sample entry the
    /// browser will accept has to survive the box round trip with its object
    /// type and its config both intact, so the bytes are encoded, decoded, and
    /// read field by field.
    #[test]
    fn aac_esds_round_trips() {
        let config = [0x12_u8, 0x10];
        let entry = sample_entry(
            &track(TrackKind::Audio, Codec::Aac),
            Some(&config),
            None,
            Some((48_000, 2)),
            None,
        )
        .expect("AAC-LC is remuxable");
        let entry = match entry {
            atom::Codec::Mp4a(entry) => entry,
            other => panic!("AAC writes an mp4a, got {other:?}"),
        };

        let mut bytes = Vec::new();
        atom::Codec::Mp4a(entry).encode(&mut bytes).expect("encode");
        let back = match atom::Codec::decode(&mut Cursor::new(&bytes)).expect("decode") {
            atom::Codec::Mp4a(entry) => entry,
            other => panic!("an mp4a decodes back as an mp4a, got {other:?}"),
        };

        let dec = &back.esds.es_desc.dec_config;
        assert_eq!(dec.object_type_indication, 0x40, "AAC in MPEG-4 syntax");
        assert_eq!(
            dec.dec_specific.as_ref().map(|s| s.raw.as_slice()),
            Some(config.as_slice()),
            "the AudioSpecificConfig survives verbatim"
        );
        assert_eq!(back.audio.channel_count, 2);
        assert_eq!(back.audio.sample_rate.integer(), 48_000);
        assert_eq!(back.audio.sample_size, 16);

        // And it is the same object type the codec strings use: `mp4a.40.2`.
        assert_eq!(
            audio_specific_config_string(&config).as_deref(),
            Some("mp4a.40.2")
        );
    }

    /// An `OpusHead` becomes a `dOps` that says the same thing.
    ///
    /// 312 samples of pre-skip and 2 output channels, the shape of a stereo Opus
    /// track: `dOps` restates both, adds the 48 kHz input rate the head stated,
    /// and for mapping family 0 carries no table at all.
    #[test]
    fn opus_head_becomes_dops() {
        let mut head = Vec::new();
        head.extend_from_slice(b"OpusHead");
        head.push(1); // version
        head.push(2); // channels
        head.extend_from_slice(&312_u16.to_le_bytes()); // pre-skip
        head.extend_from_slice(&48_000_u32.to_le_bytes()); // input sample rate
        head.extend_from_slice(&0_i16.to_le_bytes()); // output gain
        head.push(0); // channel mapping family
        assert_eq!(head.len(), 19);

        let entry = sample_entry(
            &track(TrackKind::Audio, Codec::Opus),
            Some(&head),
            None,
            None,
            None,
        )
        .expect("Opus is remuxable");
        let entry = match entry {
            atom::Codec::Opus(entry) => entry,
            other => panic!("Opus writes an Opus box, got {other:?}"),
        };

        let mut bytes = Vec::new();
        atom::Codec::Opus(entry).encode(&mut bytes).expect("encode");
        let back = match atom::Codec::decode(&mut Cursor::new(&bytes)).expect("decode") {
            atom::Codec::Opus(entry) => entry,
            other => panic!("an Opus entry decodes back as one, got {other:?}"),
        };

        assert_eq!(back.dops.pre_skip, 312, "the pre-skip is the head's");
        assert_eq!(
            back.dops.output_channel_count, 2,
            "and so is the channel count"
        );
        assert_eq!(back.dops.input_sample_rate, 48_000);
        assert_eq!(back.dops.output_gain, 0);
        assert!(
            back.dops.channel_mapping.is_none(),
            "family 0 has no channel table"
        );
        assert_eq!(back.audio.channel_count, 2);

        // A head too short for its own table is Malformed, and a version this
        // does not know is Unsupported: one file lied, the other is another
        // encapsulation.
        let short = {
            let mut head = head.clone();
            head[18] = 1; // family 1
            head.push(1); // one byte of a table that needs 2 + 2: 20 bytes in all
            head
        };
        assert!(matches!(
            sample_entry(
                &track(TrackKind::Audio, Codec::Opus),
                Some(&short),
                None,
                None,
                None
            ),
            Err(MediaError::Malformed(_))
        ));
        let future = {
            let mut head = head.clone();
            head[8] = 2; // version
            head
        };
        assert!(matches!(
            sample_entry(
                &track(TrackKind::Audio, Codec::Opus),
                Some(&future),
                None,
                None,
                None
            ),
            Err(MediaError::Unsupported(_))
        ));
    }

    /// The first AC-3 frame of `av1-ac3-eac3.mkv` gives a `dac3` whose fields say
    /// what ffprobe says, and the first E-AC-3 frame the same for `dec3`.
    ///
    /// The fixture is 48 kHz mono 96 kbps on both Dolby tracks, as
    /// `dev/make-media-fixtures.sh` built it and `expected.json` records it:
    /// fscod 0 (48 kHz), bsid 8 (a Dolby AC-3 stream), bsmod 0, acmod 1 (mono),
    /// no LFE, and bit rate code 6 (96 kbps), which is the frame size code 12
    /// this fixture's encoder chose, halved. The `Audio` boxes carry the rate and
    /// channel count `expected.json` states, because the frame header agrees with
    /// the track header or the file would not be playable at all.
    ///
    /// These tests read the fixture: the codec's own bit layout is only
    /// trustworthy against real bytes, and these are the only Dolby bytes in the
    /// repo.
    #[test]
    fn ac3_header_becomes_dac3() {
        let info = crate::probe::probe(fixture("av1-ac3-eac3.mkv")).expect("probe");
        let ac3_track = info
            .tracks
            .iter()
            .find(|t| t.codec.name() == "ac3")
            .expect("an AC-3 track")
            .clone();
        let eac3_track = info
            .tracks
            .iter()
            .find(|t| t.codec.name() == "eac3")
            .expect("an E-AC-3 track")
            .clone();

        // ffprobe's recorded answers for both Dolby tracks.
        let tracks = expected_tracks("av1-ac3-eac3.mkv");
        let ac3_expected = &tracks[1];
        let eac3_expected = &tracks[2];

        let (ac3_frame, eac3_frame) = first_dolby_frames();

        // AC-3: 0B 77 58 BA 0C 40 2F 84, the fixture's own first frame.
        let entry = sample_entry(
            &ac3_track,
            None,
            Some(&ac3_frame),
            Some((
                ac3_expected["sample_rate"].as_u64().expect("rate") as u32,
                ac3_expected["channels"].as_u64().expect("channels") as u16,
            )),
            None,
        )
        .expect("the fixture's AC-3 track is remuxable");
        let entry = match entry {
            atom::Codec::Ac3(entry) => entry,
            other => panic!("AC-3 writes an ac-3 box, got {other:?}"),
        };
        assert_eq!(entry.dac3.fscod, 0, "48 kHz");
        assert_eq!(entry.dac3.bsid, 8, "Dolby AC-3");
        assert_eq!(entry.dac3.bsmod, 0);
        assert_eq!(entry.dac3.acmod, 1, "mono");
        assert!(!entry.dac3.lfeon, "the fixture carries no LFE");
        assert_eq!(entry.dac3.bit_rate_code, 6, "96 kbps");
        assert_eq!(
            entry.audio.sample_rate.integer() as u64,
            ac3_expected["sample_rate"].as_u64().expect("rate"),
            "the Audio box says the rate ffprobe says"
        );
        assert_eq!(
            entry.audio.channel_count,
            ac3_expected["channels"].as_u64().expect("channels") as u16,
            "and the mono layout means one channel"
        );

        // The box a browser demuxes has to be the box we wrote: encode it, read
        // it back, and compare every field. A value packed into the wrong bits
        // of the 24-byte dac3 fails here first.
        let mut bytes = Vec::new();
        atom::Codec::Ac3(entry.clone())
            .encode(&mut bytes)
            .expect("encode ac-3");
        assert_eq!(
            atom::Codec::decode(&mut Cursor::new(&bytes)).expect("decode ac-3"),
            atom::Codec::Ac3(entry.clone()),
            "the dac3 round trip is lossless"
        );

        // E-AC-3: 0B 77 00 BF 32 87 C0 00, six blocks of 48 kHz mono at 96 kbps.
        let entry = sample_entry(
            &eac3_track,
            None,
            Some(&eac3_frame),
            Some((
                eac3_expected["sample_rate"].as_u64().expect("rate") as u32,
                eac3_expected["channels"].as_u64().expect("channels") as u16,
            )),
            None,
        )
        .expect("the fixture's E-AC-3 track is remuxable");
        let entry = match entry {
            atom::Codec::Eac3(entry) => entry,
            other => panic!("E-AC-3 writes an ec-3 box, got {other:?}"),
        };
        let sub = &entry.dec3.substreams[0];
        assert_eq!(sub.fscod, 0, "48 kHz");
        assert_eq!(sub.bsid, 16, "E-AC-3 main substream");
        assert_eq!(sub.acmod, 1, "mono");
        assert!(!sub.lfeon);
        assert_eq!(entry.dec3.data_rate, 96, "the 96 kbps FFmpeg computes");
        assert_eq!(
            entry.audio.sample_rate.integer() as u64,
            eac3_expected["sample_rate"].as_u64().expect("rate")
        );
        assert_eq!(
            entry.audio.channel_count,
            eac3_expected["channels"].as_u64().expect("channels") as u16
        );

        // The 13-bit data rate and the 24-bit substream word survive the same
        // round trip.
        let mut bytes = Vec::new();
        atom::Codec::Eac3(entry.clone())
            .encode(&mut bytes)
            .expect("encode ec-3");
        assert_eq!(
            atom::Codec::decode(&mut Cursor::new(&bytes)).expect("decode ec-3"),
            atom::Codec::Eac3(entry.clone()),
            "the dec3 round trip is lossless"
        );
    }

    /// A short first frame is Malformed, both ways shortness shows up. Three
    /// bytes cannot even state the fields a `dac3` is read from, and a frame
    /// whose seven header bytes claim 384 while the sample holds 32 describes
    /// a stream the file does not have.
    #[test]
    fn short_ac3_frame_is_malformed() {
        let stub = [0x0b, 0x77, 0x58];
        let result = sample_entry(
            &track(TrackKind::Audio, Codec::Ac3),
            None,
            Some(&stub),
            Some((48_000, 1)),
            None,
        );
        assert!(
            matches!(result, Err(MediaError::Malformed(_))),
            "three bytes cannot state a syncframe: {result:?}"
        );

        let mut frame = vec![0x0b, 0x77, 0x58, 0xba, 0x0c, 0x40, 0x2f, 0x84];
        frame.resize(32, 0);
        let result = sample_entry(
            &track(TrackKind::Audio, Codec::Ac3),
            None,
            Some(&frame),
            Some((48_000, 1)),
            None,
        );
        assert!(
            matches!(result, Err(MediaError::Malformed(_))),
            "a truncated frame is a file that lied: {result:?}"
        );
    }

    /// The fixture path a unit test can open.
    fn fixture(name: &str) -> std::io::BufReader<std::fs::File> {
        let path = PathBuf::from(format!(
            "{}/tests/fixtures/{name}",
            env!("CARGO_MANIFEST_DIR")
        ));
        std::io::BufReader::new(std::fs::File::open(path).expect("fixture"))
    }

    /// ffprobe's recorded view of one fixture.
    fn expected(name: &str) -> &'static serde_json::Value {
        static ALL: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();
        let all = ALL.get_or_init(|| {
            let path = PathBuf::from(format!(
                "{}/tests/fixtures/expected.json",
                env!("CARGO_MANIFEST_DIR")
            ));
            let text = std::fs::read_to_string(path).expect("expected.json");
            serde_json::from_str(&text).expect("expected.json is JSON")
        });
        &all["fixtures"][name]
    }

    fn expected_tracks(name: &str) -> &'static [serde_json::Value] {
        expected(name)["tracks"]
            .as_array()
            .expect("a track table")
            .as_slice()
    }

    /// The first AC-3 and the first E-AC-3 frame of the Dolby fixture, read
    /// through the same budgeted open the probe and Task 3 use.
    fn first_dolby_frames() -> (Vec<u8>, Vec<u8>) {
        use matroska_demuxer::Frame;
        let mut file = crate::probe::open_matroska(
            fixture("av1-ac3-eac3.mkv"),
            crate::ReadBudget::new(crate::ELEMENT_CAP),
        )
        .expect("open");
        // The container's track numbers, by codec id: the AC-3 track is the
        // middle one, the E-AC-3 track the last.
        let mut ac3_number = None;
        let mut eac3_number = None;
        for entry in file.tracks() {
            match entry.codec_id() {
                "A_AC3" => ac3_number = Some(entry.track_number().get()),
                "A_EAC3" => eac3_number = Some(entry.track_number().get()),
                _ => {}
            }
        }
        let (ac3_number, eac3_number) = (
            ac3_number.expect("an AC-3 track"),
            eac3_number.expect("an E-AC-3 track"),
        );

        let mut ac3_frame = Vec::new();
        let mut eac3_frame = Vec::new();
        let mut frame = Frame::default();
        while file.next_frame(&mut frame).expect("frame") {
            if frame.track == ac3_number && ac3_frame.is_empty() {
                ac3_frame = frame.data.clone();
            }
            if frame.track == eac3_number && eac3_frame.is_empty() {
                eac3_frame = frame.data.clone();
            }
            if !ac3_frame.is_empty() && !eac3_frame.is_empty() {
                break;
            }
        }
        assert!(
            !ac3_frame.is_empty() && !eac3_frame.is_empty(),
            "both tracks carry frames"
        );
        (ac3_frame, eac3_frame)
    }
}
