# Web UI milestone 2: the media player. Implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A video file in a torrent plays in the web UI. The player has:
- a chooser for the audio track;
- a chooser for text subtitles, embedded or in the torrent beside the video;
- keyboard control;
- a remembered position per file.

It starts while the torrent is still downloading. The server remuxes and never transcodes.

**Architecture:**
- **`fbtt-media`, a new crate.** It is pure, synchronous Rust over `Read + Seek`.
  - It probes Matroska/WebM and MP4 files.
  - It turns one track into fragmented MP4: an init segment, then `moof`+`mdat` fragments.
  - It reads text subtitles into cues.
- **`fbtt-server` routes.** New routes run `fbtt-media` on a blocking thread over a `FileReader` from the engine. Each track is its own HTTP stream.
- **The page.** It plays them through Media Source Extensions, with one `SourceBuffer` for video and one for audio, so changing the language reloads only the audio. Subtitles arrive as JSON cues and become `VTTCue`s on a `TextTrack`.

**Tech stack:**
- **Rust:** `matroska-demuxer` 0.8.1 (Zlib OR MIT OR Apache-2.0, `#![forbid(unsafe_code)]`) and `mp4-atom` 0.17.1 (MIT OR Apache-2.0). Both are new workspace dependencies.
  - `tokio-util` 0.7 is already in `Cargo.lock` through fbt-api. It joins the workspace table, for `SyncIoBridge`.
- **Web:** React 19, TypeScript, vitest. No new npm dependency.

**Spec:** `docs/superpowers/specs/2026-11-21-web-ui-and-desktop-design.md`, section 3, goals 2 and 3, and section 7, milestone 2.
- This plan is the media player and browser streaming only.
- The other milestone 2 items are a separate plan. Those are the TUI's serve-on-request (section 2a), file priorities, settings and devices.

## Rulings made while planning

Each ruling is the builder's call. The spec stays binding where these are silent.

1. **One HTTP stream per track, not one combined stream.** The spec says choosing another language "reloads the audio stream at the current position". Separate video and audio `SourceBuffer`s make that literal, so the video keeps playing from its buffer.
2. **Cues as JSON, rendered by the browser's WebVTT engine.** Subtitles reach the page as `{start_ms, end_ms, text}` cues and become `VTTCue`s. Embedded subtitles are interleaved through the whole file, so the page asks for them in time windows as playback moves. A single `.vtt` file would need the whole file downloaded first. What the user sees is the browser's WebVTT rendering, as the spec intends.
3. **Video codecs remuxed: H.264, HEVC and AV1.**
   - **Audio codecs:** AAC, Opus, FLAC, MP3, AC-3 and E-AC-3. Each plays where the browser decodes it, and the page asks the browser.
   - **VP8/VP9 in WebM** play natively through the existing stream route when the browser says it can. In that case there is no audio choice and no embedded subtitles. Anything else is "cannot play here" with a Download link.
4. **"Open in mpv or VLC" (spec goal 3) waits for the desktop app (milestone 3).** A browser cannot start a local program.
5. **A fixed time offset.** Every remuxed track is shifted forward by `TIME_OFFSET_MS = 1000`, so synthesized decode times are never negative. The page maps between source time and media time with that constant, which the probe returns.
6. **Media work has its own limit:** `MEDIA_PERMITS = 8` concurrent probe, stream and cue jobs per server. fbt-api's stream semaphore covers only its own route.

## Global Constraints

Everything in `CLAUDE.md` applies: Rust 2024 and `rust-version = "1.99"`, `#![forbid(unsafe_code)]`, dependencies only through `[workspace.dependencies]`, no public internet in tests, display-safe remote text, Graphite colours, no emoji or em-dashes in product copy, cross-platform, and the full gate. In addition:

- **No transcoding, no native libraries.** Bytes of video and audio are copied as they are. **[owner, spec goal 3]**
- **Media routes use the existing authorization unchanged.**
  - With logins on, a cookie session with a GET works, and so does a bearer token.
  - A stream token never opens a media route: tokens stay scoped to the fbt-api stream path.
  - With logins off, the Host guard applies.
- **Remote input never panics, hangs or allocates without bound.**
  - Every read from a `FileReader` has a `READ_TIMEOUT` of 60 s.
  - Every element or frame the Matroska reader can allocate is bounded by `ELEMENT_CAP` = 256 MiB, through the `CappedLen` reader (Task 1).
  - A subtitle file is read only up to `SUBTITLE_FILE_CAP` = 4 MiB.
  - A cue window is at most `CUE_WINDOW_MAX_MS` = 600 000.
- **Time values on the wire are milliseconds of source time.** The names are `start_ms`, `end_ms`, `from_ms`, `to_ms` and `duration_ms`.
- **The page stores only view preferences.** The remembered position lives in `localStorage` under `fbtt.position.<infohash>.<file>`. Every access is wrapped in `try`/`catch`.
- **Product copy for the player is exactly:**
  - `Play`
  - `Audio`
  - `Subtitles`
  - `Off`
  - `This browser cannot play <codec> video.`
  - `No audio track this browser can play.`
  - `Image subtitles cannot be shown here.`
  - `Waiting for data...`
  - `Download`
- **Fixtures are generated, small and committed.** `dev/make-media-fixtures.sh` uses ffmpeg, a developer tool and never a test dependency. Each fixture is under 150 KB. CI never runs ffmpeg.

## Review Focus

1. **A file still downloading.** Probe, stream and cues wait for pieces up to `READ_TIMEOUT`.
   - The page shows `Waiting for data...` and never a frozen frame with no word.
   - A timeout ends the response, and the page retries from where its buffer ends.
   - Tested in Task 6 (a timeout yields an ended response, not a hang) and Task 8 (the retry).
2. **A codec the browser cannot play.** Examples are HEVC in Firefox, or AC-3 as the only audio in Chrome.
   - The result is a message plus Download, or video without audio plus the message. It is never a black player.
   - Tested in Task 8 (`chooseTracks`).
3. **A seek far ahead in a partly downloaded file.**
   - The new request starts at the keyframe the demuxer finds, and the page jumps to `X-Fbtt-Start-Ms`.
   - Reads at the new position get engine priority through the reader's position.
   - Tested in Task 3 (start at a keyframe) and Task 8 (the page applies the start header).
4. **Odd or hostile files.** No Cues (linear seek), header-stripped frames, zlib-compressed or encrypted tracks, and an element that declares a huge size.
   - Each is a clean error or a correct stream, never a panic or a giant allocation.
   - Tested in Tasks 1 and 3.
5. **A client that goes away or stops reading.** A closed tab or a seek aborts the fetch.
   - The server's blocking job ends and its permit returns.
   - A client that holds the connection without reading for 10 minutes is dropped.
   - Tested in Task 6.

---

## File structure

```
crates/fbtt-media/                  new crate, no tokio, no network
  Cargo.toml
  src/lib.rs                        public API, MediaError, constants
  src/capped.rs                     CappedLen<R>: bounds what the demuxer may allocate
  src/codec.rs                      Codec, codec-id mapping, MSE codec strings, sample entries
  src/probe.rs                      probe(): MediaInfo for Matroska and MP4
  src/fmp4.rs                       init segment and fragment writer, DTS synthesis
  src/mkv.rs                        Matroska track source
  src/mp4in.rs                      MP4 track source (moov sample tables)
  src/source.rs                     TrackSource trait, Sample, FragmentStream
  src/subtitle.rs                   Cue, SRT, ASS/SSA, WebVTT and mov_text parsing, embedded windows
  tests/fixtures/*                  generated media (dev/make-media-fixtures.sh), expected.json
  tests/probe.rs, tests/remux.rs, tests/subtitles.rs
dev/make-media-fixtures.sh          ffmpeg script that writes the fixtures and expected.json
crates/fbtt-server/src/routes/media.rs    the four media routes
crates/fbtt-server/src/media_io.rs        BlockingReader: FileReader as Read+Seek with READ_TIMEOUT
crates/fbtt-server/tests/media.rs         route tests over a seeded fixture
web/src/player/time.ts              source/media time mapping
web/src/player/tracks.ts            chooseTracks(), codec support
web/src/player/buffering.ts         read-ahead and eviction decisions
web/src/player/positions.ts         remembered position
web/src/player/cues.ts              cue windows and de-duplication
web/src/player/engine.ts            MediaSource, SourceBuffers, fetch readers, seek, audio switch
web/src/player/Player.tsx           the player UI, choosers, keyboard
web/src/components/detail/Files.tsx Play button per video file
docs/web-ui.md, CHANGELOG.md        the player section, Added entry
```

---

### Task 1: The `fbtt-media` crate, fixtures, and the probe for Matroska

**Files:**
- Create: `crates/fbtt-media/{Cargo.toml,src/lib.rs,src/capped.rs,src/codec.rs,src/probe.rs}`
- Create: `dev/make-media-fixtures.sh`
- Create: `crates/fbtt-media/tests/fixtures/` and `crates/fbtt-media/tests/probe.rs`
- Modify: the root `Cargo.toml`, with the workspace member and `matroska-demuxer = "0.8.1"`, `mp4-atom = "0.17.1"` and `tokio-util = { version = "0.7", default-features = false, features = ["io-util"] }` in `[workspace.dependencies]`

**Interfaces:**
- Produces, in `fbtt_media`:
  - `pub const TIME_OFFSET_MS: u64 = 1000;`
  - `pub const ELEMENT_CAP: u64 = 256 * 1024 * 1024;`
  - `pub enum MediaError { Unsupported(String), Malformed(String), Io(std::io::Error) }`, with `Display` and `From<io::Error>`. An `io::ErrorKind::TimedOut` stays `Io`.
  - `pub enum Container { Matroska, Mp4 }`
  - `pub enum TrackKind { Video, Audio, Subtitle }`
  - `pub enum Codec`, with these variants:
    - video: `H264`, `Hevc`, `Av1`, `Vp8`, `Vp9`;
    - audio: `Aac`, `Opus`, `Flac`, `Mp3`, `Ac3`, `Eac3`, `Dts`, `TrueHd`, `Vorbis`;
    - subtitles: `SubRip`, `Ass`, `WebVtt`, `MovText`, `Pgs`, `VobSub`;
    - `Other(String)`.
  - On `Codec`:
    - `pub fn name(&self) -> &str` returns a lowercase short name: `h264`, `hevc`, `av1`, `aac`, `opus`, `ac3`, `eac3`, `subrip`, `ass`, `pgs` and so on.
    - `pub fn remuxable(&self) -> bool` is true for H264, Hevc, Av1, Aac, Opus, Flac, Mp3, Ac3 and Eac3.
    - `pub fn text_subtitle(&self) -> bool` is true for SubRip, Ass, WebVtt and MovText.
  - `pub struct TrackInfo { pub id: u32, pub kind: TrackKind, pub codec: Codec, pub mime_codecs: Vec<String>, pub language: Option<String>, pub name: Option<String>, pub default: bool, pub forced: bool }`. `id` is the container's track number for Matroska and the `track_id` for MP4.
  - `pub struct MediaInfo { pub container: Container, pub duration_ms: Option<u64>, pub tracks: Vec<TrackInfo> }`
  - `pub fn probe<R: Read + Seek>(reader: R) -> Result<MediaInfo, MediaError>`. The container is sniffed from the first bytes: the EBML magic `1A 45 DF A3` means Matroska, and `ftyp` at offset 4 means MP4.
  - `pub struct CappedLen<R>`, with `CappedLen::new(inner: R, cap: u64)`.
    - It passes `Read`, plus `Seek` for `Start` and `Current`.
    - For `SeekFrom::End(0)` it answers `min(real_len, current_pos + cap)` without moving past the real end. This is the only way `matroska-demuxer` measures the room left (`remaining_len`), so no element over `cap` is ever allocated.
    - `SeekFrom::End(n)` with `n != 0` resolves against the real length.

- [ ] **Step 1: Write `dev/make-media-fixtures.sh`, run it, and commit the outputs.**
  - It uses `testsrc2` and `sine`: 4 s, 160x90, 10 fps, `-g 10`, and B-frames on (`-bf 2`) for H.264 and HEVC. It writes:
    - `h264-aac-opus-srt-ass.mkv`: H.264. AAC `eng`, the default. Opus `ita`. SRT `eng`. ASS `ita`.
    - `hevc-flac.mkv`.
    - `av1-ac3-eac3.mkv`.
    - `h264-mp3.mkv`.
    - `h264-live-nocues.mkv`: `-live 1`, with no Cues and unknown-size clusters.
    - `h264-aac2-movtext.mp4`, with `-movflags +faststart`.
    - `h264-aac-moovlast.mp4`, without faststart.
    - `vp9-opus.webm`.
    - `sub.srt`, `sub.ass` and `sub.vtt`, each with 3 cues. One cue has two lines, and one ASS cue has `{\i1}` tags and a comma in its text.
  - It then writes `expected.json` from `ffprobe -show_streams -show_frames`, per fixture and track: codec name, language, frame count, sample rate and channel layout for audio, each subtitle cue's start and end in ms, and the PTS in ms of every video keyframe.
  - It records the ffmpeg version in a header comment.
- [ ] **Step 2: Write the failing tests in `tests/probe.rs`.**
  - `probe_mkv_lists_every_track`:
    - `h264-aac-opus-srt-ass.mkv` gives 5 tracks, with kinds and codecs in order: H264, Aac, Opus, SubRip, Ass.
    - The languages are `eng`, `ita`, `eng` and `ita`, and the AAC track is `default`.
    - The duration is within 100 ms of 4000.
    - `mime_codecs[0]` of the video starts with `avc1.`, and the AAC one is `mp4a.40.2`.
  - `probe_mkv_codec_strings`:
    - `hevc-flac.mkv`: the video's `mime_codecs[0]` starts with `hvc1.`, and the audio's `mime_codecs` contain `flac` and `fLaC`.
    - `av1-ac3-eac3.mkv`: `av01.0.`, plus `ac-3` and `ec-3`.
    - `h264-mp3.mkv`: `mp4a.6B`.
  - `probe_mp4_faststart_and_moov_last`: both MP4 fixtures list their tracks. The mov_text track is `MovText`, and `text_subtitle()` is true.
  - `probe_refuses_unknown_bytes`: 1 KiB of zeros gives `Unsupported`.
  - `probe_truncated_mkv_is_an_error`: the first 300 bytes of an mkv give `Malformed` or `Io(UnexpectedEof)`, and no panic.
  - `capped_len_bounds_huge_elements`: hand-build an EBML header plus a Segment whose first child is a binary element declaring 2 GiB, inside a `Read + Seek` that pretends to be 3 GiB long and returns zeros lazily. `probe` returns an error at once, without allocating the 2 GiB: the declared size exceeds the room `CappedLen` reports, so the demuxer refuses before it allocates.
- [ ] **Step 3: Run them.** Run: `cargo test -p fbtt-media --test probe`. Expected: FAIL, because nothing exists yet.
- [ ] **Step 4: Implement `probe` for Matroska and MP4, `CappedLen`, and the `Codec` mapping.**
  - Matroska codec ids map as follows:
    - `V_MPEG4/ISO/AVC` to H264, `V_MPEGH/ISO/HEVC` to Hevc, `V_AV1` to Av1, `V_VP8` and `V_VP9` to Vp8 and Vp9;
    - `A_AAC*` to Aac, `A_OPUS` to Opus, `A_FLAC` to Flac, `A_MPEG/L3` to Mp3, `A_AC3` to Ac3, `A_EAC3` to Eac3, `A_DTS*` to Dts, `A_TRUEHD` to TrueHd, `A_VORBIS` to Vorbis;
    - `S_TEXT/UTF8` to SubRip, `S_TEXT/ASS` and `S_TEXT/SSA` to Ass, `S_TEXT/WEBVTT` to WebVtt, `S_HDMV/PGS` to Pgs, `S_VOBSUB` to VobSub.
  - In MP4, sample entries map as follows: `avc1`/`avc3` to H264, `hvc1`/`hev1` to Hevc, `av01` to Av1, `mp4a` with esds object type 0x40 to Aac and 0x6B to Mp3, `Opus` to Opus, `fLaC` to Flac, `ac-3` to Ac3, `ec-3` to Eac3, and `tx3g` to MovText.
  - Codec strings:
    - `avc1.%02X%02X%02X` from avcC bytes 1 to 3;
    - `hvc1.<profile_space letter or ''><profile_idc>.<compatibility flags, bit-reversed, hex without leading zeros>.<L|H><level_idc>.<constraint bytes as hex, trailing zero bytes dropped, dot-separated>` from hvcC;
    - `av01.<seq_profile>.<seq_level_idx_0, 2 digits><M|H>.<bit depth, 2 digits>` from av1C;
    - `mp4a.40.<audio object type>` from the first 5 bits of the AudioSpecificConfig;
    - `opus`, `flac` and `fLaC` (both, in that order), `mp4a.6B`, `ac-3` and `ec-3`.
    - Non-remuxable codecs get an empty `mime_codecs`.
  - Language: `language_bcp47()`, else `language()`. `und` becomes `None`.
  - The crate stays free of `fbt`. Task 6 passes every track name, language and cue text through `fbt::display_safe(s, fbt::MAX_DISPLAY_CHARS)` before sending it.
- [ ] **Step 5: Run them.** Run: `cargo test -p fbtt-media --test probe`. Expected: PASS.
- [ ] **Step 6: Run the gate and commit.** Commit: `feat(media): the fbtt-media crate, fixtures, and probing Matroska and MP4`.

### Task 2: Codec configuration and the fMP4 writer

**Files:**
- Modify: `crates/fbtt-media/src/codec.rs`
- Create: `crates/fbtt-media/src/fmp4.rs` and `crates/fbtt-media/src/source.rs`
- Test: unit tests in `fmp4.rs` and `codec.rs`

**Interfaces:**
- Produces:
  - `pub struct Sample { pub pts: i64, pub dts: Option<i64>, pub duration: Option<u32>, pub keyframe: bool, pub data: Vec<u8> }`. Times are in the track's `timescale`, before `TIME_OFFSET_MS` is applied. `dts` is `None` when the source has only presentation times, which is Matroska.
  - `pub struct TrackConfig { pub timescale: u32, pub kind: TrackKind, pub codec: Codec, pub sample_entry: mp4_atom::Codec, pub width: u16, pub height: u16 }`
  - `pub fn init_segment(config: &TrackConfig) -> Result<Vec<u8>, MediaError>` writes `ftyp` (`iso6`, compatible `iso6`, `mp41`) and a `moov` with one `trak` (track id 1), an empty sample table, and `mvex`/`trex`.
  - `pub struct FragmentWriter`:
    - `FragmentWriter::new(config: &TrackConfig)`;
    - `fn push(&mut self, sample: Sample) -> Result<Option<Vec<u8>>, MediaError>` returns a finished `moof`+`mdat` when the sample closes a fragment;
    - `fn finish(&mut self) -> Result<Option<Vec<u8>>, MediaError>` flushes the last one.
  - `pub fn sample_entry(track: &TrackInfo, private: Option<&[u8]>, first_frame: Option<&[u8]>, audio: Option<(u32 /*rate*/, u16 /*channels*/)>, video: Option<(u16, u16)>) -> Result<mp4_atom::Codec, MediaError>`
- **Fragmenting:**
  - A video fragment runs from one keyframe up to the next keyframe, not including it.
  - An audio fragment closes once it holds at least 1000 ms.
  - Every fragment's first sample carries sync-sample flags. A video sample that is not a keyframe carries `sample_is_non_sync_sample`.
- **Decode times, when the samples have none (Matroska).** The algorithm, per video fragment `p[0..n]` in decode order:
  ```
  sorted = sort(p)
  delay  = max(0, max_i(sorted[i] - p[i]))
  dts[i] = sorted[i] - delay
  dts[i] = max(dts[i], last_dts + 1)          // last_dts from the previous fragment, or -inf
  cts[i] = p[i] - dts[i]                       // trun version 1, signed
  duration[i] = dts[i+1] - dts[i]; the last one = next fragment's first dts - dts[n-1]
                (the writer holds one fragment back until the next keyframe arrives),
                or, at end of stream, the previous duration.
  ```
  - Audio uses `dts = pts`, with the same duration rule.
  - The writer adds `TIME_OFFSET_MS` in the track's timescale to every `dts` before it writes `tfdt` and `trun`.

- [ ] **Step 1: Write the failing tests.**
  - `dts_synthesis_with_b_frames`: decode order with PTS `[0, 300, 100, 200, 400, 700, 500, 600]` (two fragments, keyframes at 0 and 400). Decoding the output gives:
    - every `dts + cts == pts + offset`;
    - `dts` strictly increasing across both fragments;
    - every `cts >= 0`.
  - `fragment_tfdt_continuity`: for each fragment after the first, `tfdt == previous tfdt + sum(previous durations)`.
  - `audio_fragments_hold_a_second`: 100 AAC samples of 21 ms each give fragments with a sum of durations of at least 1000 ms, except the last.
  - `aac_esds_round_trips`: the AudioSpecificConfig `12 10` gives an `mp4a` whose esds decodes back with object type 0x40 and the same two bytes.
  - `opus_head_becomes_dops`: an OpusHead with pre-skip 312 and 2 channels gives a `dOps` with the same values.
  - `ac3_header_becomes_dac3`: the first syncframe of the `av1-ac3-eac3.mkv` AC-3 track gives `dac3` fields equal to ffprobe's sample rate and channel layout. Read them from `expected.json`.
  - `init_segment_decodes`: `init_segment` for the H.264 fixture's track decodes with `mp4_atom` to `ftyp`, then a `moov` with one `trak` whose `avc1.avcc` equals the CodecPrivate.
- [ ] **Step 2: Run them.** Run: `cargo test -p fbtt-media fmp4 codec`. Expected: FAIL.
- [ ] **Step 3: Implement `sample_entry`, `init_segment` and `FragmentWriter`.**
  - Use `mp4_atom` types for every box: `Ftyp`, `Moov`, `Mvex`, `Trex`, `Moof`, `Traf`, `Tfhd` (`default_base_is_moof`), `Tfdt` (version 1), `Trun` (version 1, with `data_offset` = moof size + 8), `Mdat`, `Avc1`, `Hvc1`, `Av01`, `Mp4a`/`Esds`, `Opus`/`Dops`, `Flac`/`Dfla`, `Ac3`/`Dac3` and `Eac3`/`Dec3`.
  - `dac3` and `dec3` come from the first frame's syncframe header. AC-3 uses fscod, bsid, bsmod, acmod, lfeon and frmsizecod. E-AC-3 uses the independent substream's fscod, bsid, acmod, lfeon and data rate.
  - FLAC: the STREAMINFO block from the CodecPrivate goes into `dfLa`.
  - MP3 is `mp4a` with esds object type 0x6B and no DecoderSpecificInfo.
- [ ] **Step 4: Run them.** Expected: PASS.
- [ ] **Step 5: Run the gate and commit.** Commit: `feat(media): fragmented MP4 init segments and fragments, with decode times for B-frames`.

### Task 3: The Matroska track source and `FragmentStream`

**Files:**
- Create: `crates/fbtt-media/src/mkv.rs`
- Modify: `crates/fbtt-media/src/source.rs` and `crates/fbtt-media/src/lib.rs`
- Test: `crates/fbtt-media/tests/remux.rs`

**Interfaces:**
- Consumes: `Sample`, `TrackConfig`, `init_segment` and `FragmentWriter` (Task 2), `CappedLen` (Task 1).
- Produces:
  - `pub fn open_track<R: Read + Seek>(reader: R, track: u32, start_ms: u64) -> Result<FragmentStream, MediaError>`
  - `pub struct FragmentStream`:
    - `pub fn init_segment(&self) -> &[u8]`;
    - `pub fn start_ms(&self) -> u64`, the source time of the first sample: for video, the first keyframe at or after the seek point;
    - `pub fn mime(&self) -> String`, for example `video/mp4; codecs="avc1.64000A"`;
    - `pub fn next_fragment(&mut self) -> Result<Option<Vec<u8>>, MediaError>`.
  - `FragmentStream` holds a `Box<dyn TrackSource + Send>`.
  - `pub(crate) trait TrackSource { fn config(&self) -> &TrackConfig; fn next_sample(&mut self) -> Result<Option<Sample>, MediaError>; }`
- **Behaviour:**
  - The source wraps the reader in `CappedLen::new(_, ELEMENT_CAP)`, opens `MatroskaFile`, and calls `seek(start_ms * 1_000_000 / timestamp_scale)` when `start_ms > 0`.
  - It skips frames of other tracks.
  - For video it drops frames until the first one with `is_keyframe == Some(true)`. If the file uses BlockGroups (`is_keyframe == None`), the first frame after the seek counts as the keyframe.
  - Timestamps convert from Matroska units (`timestamp * timestamp_scale` ns) to the track timescale with `u128` arithmetic. Video uses 90 000, and audio uses the sampling frequency rounded.
  - **Content encodings:**
    - header stripping (`ContentCompAlgo` 3): prepend the stored bytes to every frame;
    - zlib, bzip2, lzo or encryption: `Unsupported`;
    - a track whose `codec.remuxable()` is false: `Unsupported`.

- [ ] **Step 1: Write the failing tests in `tests/remux.rs`.** Each test decodes the whole output with `mp4_atom`.
  - `remux_video_from_start`: H.264 from `h264-aac-opus-srt-ass.mkv` at 0 ms:
    - the total sample count equals the frame count in `expected.json`;
    - `start_ms()` is 0;
    - every fragment starts with a sync sample;
    - `dts + cts - offset` over all samples, sorted, equals the source PTS list.
  - `remux_audio_track_by_choice`: track 3 (Opus) gives a `dOps` init and the Opus frame count. Track 2 (AAC) gives `mp4a.40.2`.
  - `remux_seek_starts_on_a_keyframe`: `start_ms = 1500` gives `start_ms()` equal to the first keyframe PTS at or after 1500 in `expected.json`. The first sample is sync.
  - `remux_without_cues`: the same seek on `h264-live-nocues.mkv` gives the same `start_ms()`.
  - `remux_hevc_and_av1`: both video tracks remux, with `hvc1` and `av01` sample entries.
  - `remux_refuses_subtitles_and_unknown_track`: track 4 gives `Unsupported`, and track 99 gives `Unsupported`.
  - `remux_header_stripping`: build a tiny Matroska in the test whose track declares header stripping of the bytes `00 00 01`. Every output sample starts with those bytes.
  - `remux_truncated_file_ends_with_an_error`: the first 60% of the fixture yields some fragments, then `Err`, never a panic.
- [ ] **Step 2: Run them.** Run: `cargo test -p fbtt-media --test remux`. Expected: FAIL.
- [ ] **Step 3: Implement `MkvSource`, `open_track` and `FragmentStream`.**
- [ ] **Step 4: Run them.** Expected: PASS.
- [ ] **Step 5: Validate outside the tests.**
  - Write the init plus fragments of `remux_video_from_start` and `remux_audio_track_by_choice` to files with a one-off `cargo run --example dump` (keep the example).
  - Run `ffprobe -v error -show_entries stream=codec_name -show_entries format=duration` on each.
  - Expected: no errors, the right codec, and a duration within 200 ms of the source. Paste the output into the report.
- [ ] **Step 6: Run the gate and commit.** Commit: `feat(media): remux one Matroska track into fragmented MP4 from any start time`.

### Task 4: MP4 input

**Files:**
- Create: `crates/fbtt-media/src/mp4in.rs`
- Modify: `open_track` dispatches on the container.
- Test: add to `tests/remux.rs`

**Interfaces:**
- Consumes: `TrackSource` and `Sample` (Task 3).
- **Behaviour:**
  - Walk the top-level box headers with `mp4_atom::Header`, seeking past `mdat` without reading it. Decode `moov` wherever it is.
  - Build each track's sample list from `stts`, `ctts`, `stsc`, `stsz`, `stco`/`co64` and `stss`.
  - Refuse a sample count above 20 million or an `stsz` entry above `ELEMENT_CAP` with `Malformed`.
  - Seek to the last sync sample at or before `start_ms`. For audio, seek to the first sample at or after it.
  - Samples carry the source `dts` and `cts`. The track timescale is the source `mdhd` timescale.
  - Sample data is read by seeking to each offset.

- [ ] **Step 1: Write the failing tests.**
  - `remux_mp4_faststart_audio_choice`: the second AAC track of `h264-aac2-movtext.mp4` remuxes with its frame count.
  - `remux_mp4_moov_last_video`: video from `h264-aac-moovlast.mp4` at 2000 ms starts on the keyframe at or before 2000. That is different from Matroska, and intended: MP4 has the sync table.
  - `mp4_huge_sample_count_is_refused`: patch an `stsz` count in a copy of the fixture, in the test, to 4 000 000 000. `open_track` returns `Malformed` quickly.
- [ ] **Step 2: Run them.** Expected: FAIL.
- [ ] **Step 3: Implement.** Expected after: PASS.
- [ ] **Step 4: Run the gate and commit.** Commit: `feat(media): remux MP4 tracks, moov first or last`.

### Task 5: Subtitles

**Files:**
- Create: `crates/fbtt-media/src/subtitle.rs`
- Test: `crates/fbtt-media/tests/subtitles.rs`

**Interfaces:**
- Produces:
  - `pub struct Cue { pub start_ms: u64, pub end_ms: u64, pub text: String }`
  - `pub enum SubtitleFormat { Srt, Ass, Vtt }`
  - `pub fn subtitle_format(file_name: &str) -> Option<SubtitleFormat>`, by extension: `.srt`, `.ass`, `.ssa` or `.vtt`, case-insensitive.
  - `pub fn file_cues(bytes: &[u8], format: SubtitleFormat) -> Result<Vec<Cue>, MediaError>`
  - `pub fn embedded_cues<R: Read + Seek>(reader: R, track: u32, from_ms: u64, to_ms: u64) -> Result<Vec<Cue>, MediaError>`
- **Text rules:**
  - Strip a UTF-8 BOM. Decode as UTF-8 lossily.
  - ASS: remove `{...}` override blocks. `\N` and `\n` become a newline, and `\h` a space.
  - SRT and VTT: remove `<...>` tags except `<i>`, `<b>` and `<u>`, which WebVTT renders.
  - Cues come back sorted by `start_ms`. A cue with `end_ms <= start_ms` is dropped.
- **Embedded cues:**
  - Matroska: seek to `from_ms` minus 10 000 (subtitles start before the seek point), read frames to `to_ms`, and keep the track's cues that overlap the window.
    - `S_TEXT/ASS` block text is the ninth comma field onward: ReadOrder, Layer, Style, Name, MarginL, MarginR, MarginV, Effect, Text.
    - The end is `timestamp + duration`. With no duration, the end is the next cue's start, or start + 5000.
  - MP4 `tx3g`: a sample is a 2-byte big-endian length, then UTF-8 text.
  - `to_ms - from_ms > 600_000` gives `Unsupported`.

- [ ] **Step 1: Write the failing tests.**
  - `srt_file`: 3 cues with the fixture's exact times, the two-line cue joined with `\n`.
  - `ass_file`: 3 cues. The tagged cue has no `{`, and the text with a comma is whole.
  - `vtt_file`: 3 cues, with the header and any `NOTE` blocks skipped.
  - `embedded_srt_window`: track 4 of `h264-aac-opus-srt-ass.mkv` with window 0 to 4000 gives the cues in `expected.json`. With window 3000 to 4000 it gives only the overlapping ones.
  - `embedded_ass_window`: track 5 gives text without override tags.
  - `embedded_movtext`: the mp4 track gives its cues.
  - `window_too_large_is_refused`.
  - `garbage_is_empty_not_a_panic`: 64 KiB of random bytes through each `file_cues` format gives `Ok` with no cues, or `Malformed`.
- [ ] **Step 2: Run them.** Expected: FAIL.
- [ ] **Step 3: Implement.** Expected after: PASS.
- [ ] **Step 4: Run the gate and commit.** Commit: `feat(media): subtitles from SRT, ASS, WebVTT and mov_text, as files or embedded`.

### Task 6: The media routes

**Files:**
- Create: `crates/fbtt-server/src/routes/media.rs` and `crates/fbtt-server/src/media_io.rs`
- Modify: `crates/fbtt-server/src/routes/mod.rs` (`mod media;`, the routes inside `router()` next to the fs routes, and a `media_permits: Arc<Semaphore>` in `Shared`)
- Modify: `crates/fbtt-server/Cargo.toml` (`fbtt-media`, `tokio-util`, and tokio `io-util`)
- Test: `crates/fbtt-server/tests/media.rs`

**Interfaces:**
- Consumes: `fbtt_media::{probe, open_track, file_cues, embedded_cues, subtitle_format, TIME_OFFSET_MS}`, and `shared.session().torrent(..)` then `handle.open_file(index)`, as in `routes/stream.rs:81`.
- Produces, in `media_io.rs`:
  - `pub(crate) struct BlockingReader`. It wraps `tokio_util::io::SyncIoBridge<fbt::FileReader>`, and every `read` and `seek` is bounded by `READ_TIMEOUT = Duration::from_secs(60)`.
  - Build it on the runtime handle with `tokio::time::timeout` inside `Handle::block_on`, from the blocking thread. A timeout is `io::ErrorKind::TimedOut`.
  - It is used only inside `tokio::task::spawn_blocking`.
- **Routes.** Paths use the 40-hex lowercase id, the same parsing as `routes/stream.rs`, and `{file}` is the file index. All are GET. Errors use `RouteError` with these codes:
  - `GET /api/v1/media/{id}/{file}` returns 200 with JSON:
    ```json
    {"container":"matroska","duration_ms":4000,"time_offset_ms":1000,
     "tracks":[{"id":1,"kind":"video","codec":"h264","mime_codecs":["avc1.64000A"],
                "language":null,"name":null,"default":true,"forced":false,"text":false}],
     "external_subtitles":[{"file":3,"name":"movie.ita.srt","format":"srt"}]}
    ```
    - `text` is `codec.text_subtitle()`.
    - `external_subtitles` lists every file in the same torrent with a subtitle extension, sorted with the files that share the video's base name first.
    - Errors: `unsupported_media` 415, `not_found` 404 (unknown torrent, file index, or pad file), `no_metadata` 409, `read_timeout` 504.
  - `GET /api/v1/media/{id}/{file}/tracks/{track}/stream?start_ms=N` returns 200 with `Content-Type` from `FragmentStream::mime()`, the header `X-Fbtt-Start-Ms: <start_ms()>`, and `Cache-Control: no-store`.
    - The body is the init segment, then fragments, sent through a `tokio::sync::mpsc` channel of capacity 4 from the blocking job.
    - The job ends when the receiver is gone, or when a `send` waits more than `IDLE_SEND = 10 min`.
    - Errors raised before the first byte map to the codes above. An error after the first byte ends the body.
  - `GET /api/v1/media/{id}/{file}/tracks/{track}/cues?from_ms=A&to_ms=B` returns 200 with `{"cues":[{"start_ms":..,"end_ms":..,"text":".."}]}`.
  - `GET /api/v1/media/{id}/{file}/subtitles/{sub}` returns 200 with the same cue JSON for subtitle file `{sub}` of the same torrent. `{sub}` must have a subtitle extension (else 415) and be at most `SUBTITLE_FILE_CAP` (else 413 `too_large`).
  - Every handler takes one permit from `media_permits` (8) with `try_acquire_owned`. When none is free it answers 503 `too_many_media_streams` with `Retry-After: 1`. The stream route moves its permit into the blocking job.

- [ ] **Step 1: Write the failing tests in `tests/media.rs`.**
  - Seed a folder holding `h264-aac-opus-srt-ass.mkv`, `movie.srt` and `notes.txt`, with logins off. Reuse the pattern of `seed_torrent` and `wait_until_seeding` in `tests/server.rs`, copying the minimal helpers.
  - `probe_route_lists_tracks_and_subtitle_files`: 5 tracks, `time_offset_ms` 1000, and `external_subtitles` holds only `movie.srt`.
  - `stream_route_returns_init_then_fragments`:
    - the content type starts with `video/mp4; codecs="avc1.`;
    - `X-Fbtt-Start-Ms` is `0`;
    - the body decodes with `mp4_atom` to `ftyp`, `moov`, then at least one `moof`.
  - `stream_route_audio_and_seek`: track 3 with `start_ms=2000` gives `audio/mp4; codecs="opus"`.
  - `cues_and_subtitle_file_routes`: cues for track 4 match `expected.json`, and the subtitle route for `movie.srt` gives 3 cues. `notes.txt` gives 415.
  - `media_routes_refuse_a_stream_token`: with logins on, a stream token issued for file 0 used as `?token=` on the probe route gives 401. A cookie session works with GET.
  - `permits_are_released_when_the_client_goes_away`:
    - start 8 streams;
    - a ninth gives 503;
    - drop the 8 responses;
    - within 5 s a new stream gives 200.
  - `read_timeout_ends_the_stream`: with the read timeout taken from a `media_read_timeout: Duration` on `Shared` (default `READ_TIMEOUT`, set through a `pub fn` on whatever the tests use to start a server, with no config key), add a torrent whose data is absent (add the `.torrent` with no files on disk and no peers). The probe answers 504 `read_timeout` within 3 s when the timeout is 1 s.
- [ ] **Step 2: Run them.** Run: `cargo test -p fbtt-server --all-features --test media`. Expected: FAIL.
- [ ] **Step 3: Implement the routes, `BlockingReader` and the permits.**
- [ ] **Step 4: Run them.** Expected: PASS.
- [ ] **Step 5: Run the gate and commit.** Commit: `feat(server): media routes: probe, per-track fragmented MP4, cues and subtitle files`.

### Task 7: The page's player logic (pure modules)

**Files:**
- Create: `web/src/player/{time,tracks,buffering,positions,cues}.ts` and their `*.test.ts`
- Modify: `web/src/api/client.ts` adds hand-typed `MediaProbe`, `MediaTrack` and `Cue` types and `probe(id, file)`, `cues(id, file, track, fromMs, toMs)` and `subtitleFile(id, file, sub)`

**Interfaces:**
- Produces:
  - `time.ts`: `toMedia(sourceMs: number, offsetMs: number): number` (seconds) and `toSource(mediaSeconds: number, offsetMs: number): number` (ms).
  - `tracks.ts`: `chooseTracks(probe: MediaProbe, supported: (mime: string) => boolean, preferredAudio?: number): TrackChoice`, where:
    ```ts
    type TrackChoice =
      | { kind: 'remux'; video: MediaTrack; videoMime: string; audio: MediaTrack | null; audioMime: string | null;
          audioOptions: { track: MediaTrack; playable: boolean }[] }
      | { kind: 'native' }            // WebM VP8/VP9 the browser plays directly
      | { kind: 'unplayable'; codec: string }
    ```
    - The mime is `video/mp4; codecs="<c>"` and `audio/mp4; codecs="<c>"`, using the first of `mime_codecs` that `supported` accepts.
    - The audio chosen is `preferredAudio` if playable, else the default track if playable, else the first playable one, else `null`.
  - `buffering.ts`:
    - `shouldRead(bufferedAheadSeconds: number): boolean` is true below 90 and false at 120 or more, with hysteresis: it keeps the last answer between 90 and 120. The state is a closure from `createReadGate()`.
    - `evictRanges(buffered: [number, number][], currentTime: number): [number, number][]` returns what lies more than 60 s behind `currentTime`.
  - `positions.ts`:
    - `loadPosition(id: string, file: number): number | null` and `savePosition(id: string, file: number, seconds: number, duration: number): void`;
    - it saves nothing below 10 s, and clears the position within 30 s of the end;
    - the key is `fbtt.position.<id>.<file>`, and every storage access is in `try`/`catch`.
  - `cues.ts`:
    - `nextWindow(currentMs: number, fetched: [number, number][]): [number, number] | null` gives a 60 000 ms window from the first unfetched point within 20 000 ms ahead;
    - `mergeCues(existing: Set<string>, cues: Cue[]): Cue[]` returns only new cues, keyed `start|end|text`.

- [ ] **Step 1: Write the failing tests.** One `describe` per module.
  - `chooseTracks`, with probes built in the test:
    - Chrome-like support (no `ac-3`), with AC-3 as the only audio: `remux`, `audio: null`, and `audioOptions[0].playable === false`;
    - HEVC with no HEVC support: `{kind:'unplayable', codec:'hevc'}`;
    - WebM VP9: `native`;
    - preferring audio track 3.
  - `shouldRead` hysteresis at 89, 100, 121, 100 and 89 gives `true, true, false, false, true`.
  - `evictRanges` with `[[0,200]]` at 100 gives `[[0,40]]`.
  - `savePosition` and `loadPosition` round-trip. Saving at 5 s stores nothing. Saving at duration minus 10 clears the position. A `localStorage` that throws on access never propagates.
  - `nextWindow` and `mergeCues` work on overlapping windows.
- [ ] **Step 2: Run them.** Run: `npm --prefix web test -- src/player`. Expected: FAIL.
- [ ] **Step 3: Implement.** Expected after: PASS.
- [ ] **Step 4: Run `npm --prefix web run lint && npm --prefix web run typecheck` and commit.** Commit: `feat(web): player logic: track choice, buffering, positions and cue windows`.

### Task 8: The engine, the player, and Play in the Files tab

**Files:**
- Create: `web/src/player/engine.ts`, `web/src/player/Player.tsx` and `web/src/player/Player.test.tsx`
- Modify: `web/src/components/detail/Files.tsx` (a Play button on video rows) and `web/src/components/DetailPane.tsx` (opening the player)

**Interfaces:**
- Consumes: everything from Task 7, plus the routes from Task 6.
- Produces:
  - `engine.ts`: `createEngine(video: HTMLVideoElement, target: {id: string; file: number}, choice: Extract<TrackChoice, {kind:'remux'}>, offsetMs: number, deps?: EngineDeps): Engine`, where:
    ```ts
    interface Engine { seek(sourceMs: number): void; setAudio(track: MediaTrack): void; state(): 'loading'|'waiting'|'playing'|'error'; destroy(): void; onState(cb: (s: string) => void): void }
    interface EngineDeps { fetch: typeof fetch; MediaSource: typeof MediaSource }   // defaults: window.fetch and window.ManagedMediaSource ?? window.MediaSource
    ```
  - **Behaviour:**
    - **Start-up.** Create the source and add the video `SourceBuffer` (then the audio one, when there is one). Fetch the video stream at the start time, read `X-Fbtt-Start-Ms`, then fetch the audio at that same time. Set `video.currentTime = toMedia(start)`.
    - **Appending.** Each body is read with `getReader()`. Chunks are appended in order, one at a time, waiting for `updateend`. Reading pauses while `shouldRead` is false and resumes on `timeupdate`.
    - **Full buffer.** On `QuotaExceededError`, remove `evictRanges(...)` and retry once.
    - **Seeking.** On `seeking` to a time outside the buffered ranges: abort both fetches (`AbortController`), `abort()` both buffers, and start again at `toSource(video.currentTime)`.
    - **Changing audio.** `setAudio` aborts only the audio fetch, removes the audio buffer from `currentTime` to the end, and fetches the new track at `toSource(currentTime)`.
    - **Waiting.** A body that ends before the end of the file, a 504, or a network error moves to `waiting`. The engine retries from the end of the buffered range after 1, 2, 4 and then every 5 s.
    - **Stopping.** `destroy()` aborts everything and revokes the object URL.
  - `Player.tsx`: `Player({ id, file, name, onClose })`. It renders in the existing modal pattern (`modal.ts`):
    - the video;
    - a seek bar in source time, from the probe's `duration_ms`;
    - play and pause, the time, mute and volume;
    - `Audio` and `Subtitles` menus, where image tracks are listed disabled with `Image subtitles cannot be shown here.`;
    - fullscreen and close;
    - `Waiting for data...` while waiting;
    - for `unplayable`: `This browser cannot play <codec> video.` plus a `Download` link to the fbt-api stream path for the file, with the `download` attribute;
    - for `native`: a plain `<video src>` on the stream path.
  - **Keyboard:** `Space` and `k` play or pause. `ArrowLeft` and `ArrowRight` move 5 s. `ArrowUp` and `ArrowDown` change the volume by 0.1. `f` is fullscreen, `m` mutes, `a` takes the next playable audio, `c` the next subtitle (including Off), and `Escape` closes.
  - **Subtitles:**
    - an embedded text track is fetched by `nextWindow` on `timeupdate`;
    - an external file is fetched once;
    - cues are added to a `TextTrack` from `video.addTextTrack('subtitles', label, lang)` as `new VTTCue(toMedia(start), toMedia(end), text)`;
    - choosing a subtitle sets that track `showing` and the others `disabled`.
  - **Position:** saved every 5 s and on close, and offered on open: start at `loadPosition` when present.
  - **Files tab:** a `Play` button (Lucide `Play`, labelled `Play`) on rows whose path ends in `.mkv`, `.mp4`, `.m4v`, `.webm` or `.mov` (case-insensitive). Clicking it opens the `Player`.

- [ ] **Step 1: Write the failing tests in `Player.test.tsx`** (jsdom, with `EngineDeps` faked and `fetch` faked to answer the routes' real shapes from Task 6).
  - `opens from the Files tab`: a `.mkv` row has `Play`, and a `.txt` row does not.
  - `lists audio and subtitle choices`:
    - from the fixture's probe shape, the Audio menu has `eng` and `ita`;
    - Subtitles has `Off`, `eng` and `ita`, plus the external file;
    - a PGS track appears disabled.
  - `unplayable shows the message and Download`, with the download link's `href` equal to the stream path.
  - `start header moves the playhead`: the fake answers `X-Fbtt-Start-Ms: 2000`, and `video.currentTime` becomes `3` (offset 1000).
  - `a stream that ends early shows Waiting for data and retries`: the first body ends after the init segment, and the second fetch starts at the buffered end.
  - `keyboard`: Space toggles `paused`, `a` switches the audio and causes one new audio fetch with the other track id, and Escape calls `onClose`.
- [ ] **Step 2: Run them.** Run: `npm --prefix web test -- Player`. Expected: FAIL.
- [ ] **Step 3: Implement.** Expected after: PASS.
- [ ] **Step 4: Run the full web gate.** Run `npm --prefix web run lint && npm --prefix web run typecheck && npm --prefix web test && npm --prefix web run build && npm --prefix web run size`. Expected: all pass, with the bundle under 300 KB gzipped.
- [ ] **Step 5: Commit.** Commit: `feat(web): the media player, with audio and subtitle choices, keyboard control and remembered position`.

### Task 9: Docs, and the real-browser check

**Files:**
- Modify: `docs/web-ui.md` (a "Playing video" section), `CHANGELOG.md` (an `### Added` entry under Unreleased), and the spec's section 7, where milestone 2 is split into "2a the media player (this plan)" and "2b the rest of parity"
- The controller runs the real-browser check. It is not an implementer step.

- [ ] **Step 1: Write the docs.** The "Playing video" section covers:
  - what plays: the codec list from ruling 3;
  - what the browser decides;
  - the choosers and the keys;
  - that playback starts while downloading;
  - that image subtitles are listed but not shown;
  - Download as the fallback.
- [ ] **Step 2: Real-browser check (controller).** Use Playwright Chrome against a full build's `serve --offline`, with logins on, reached by a host name, at 1280 px and 390 px. Seed the fixture folder, then:
  1. open the Files tab and press Play;
  2. confirm the video plays: `currentTime` advances by 2 s or more;
  3. switch the audio to `ita`: one new audio request, and the video keeps playing;
  4. turn subtitles to `eng`: a cue appears in `textTracks[...].activeCues` within the cue's time;
  5. seek to 3 s;
  6. close and reopen: the position is offered;
  7. open `hevc-flac.mkv`: either it plays or the unplayable message shows, never a black player;
  8. check that the page reports no console errors.

  Then repeat steps 1 and 2 with Safari through WebKit, if it is available, and record the result.
- [ ] **Step 3: Run the full gate and commit.** Commit: `docs: playing video in the web UI`.
