//! Codec identity: what kind of samples a track holds, which codec encodes
//! them, and the codec strings a player or an HLS manifest asks for.
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
            let dec = &entry.esds.es_desc.dec_config;
            let config = dec
                .dec_specific
                .as_ref()
                .map(|specific| specific.raw.as_slice());
            (Codec::Aac, mp4a_strings(dec.object_type_indication, config))
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
    // Still Picture, which is 0x60 on the wire and `6` in the string.
    let compatibility = u32::from_be_bytes(record.general_profile_compatibility_flags);
    let compatibility = format!("{:X}", compatibility.reverse_bits());
    let compatibility = compatibility.trim_start_matches('0');

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
        string.push_str(compatibility);
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
fn audio_specific_config_string(config: &[u8]) -> Option<String> {
    Some(format!("mp4a.40.{}", audio_object_type(config)?))
}

/// The codec strings for an `mp4a` sample entry. Its `esds` carries an
/// `AudioSpecificConfig` for AAC, and nothing for ADTS AAC or MPEG Layer III in
/// MPEG-4 syntax, whose object type indication names them instead.
fn mp4a_strings(object_type_indication: u8, audio_specific_config: Option<&[u8]>) -> Vec<String> {
    if let Some(string) = audio_specific_config.and_then(audio_specific_config_string) {
        return vec![string];
    }
    match object_type_indication {
        // MPEG Layer III, and its MPEG-2 half rate and full rate relatives.
        0x6b => vec!["mp4a.6B".to_string()],
        0x66 => vec!["mp4a.66".to_string()],
        0x67 => vec!["mp4a.67".to_string()],
        _ => Vec::new(),
    }
}
