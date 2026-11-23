#!/usr/bin/env bash
# Regenerate the fbtt-media probe fixtures and the expected.json beside them.
#
# Tested with: ffmpeg 9.0.2 / ffprobe 9.0.2 (also needs python3).
# Run it from anywhere; every path is derived from this script's own location.
#
#   ./dev/make-media-fixtures.sh
#
# The fixtures are synthetic, so the same ffmpeg build and the same arguments
# below reproduce them. They are checked in because a test must not shell out to
# ffmpeg, and because libx264, libx265, libvpx and SVT-AV1 each write their own
# version string into the bitstream, so a rerun on another machine can shift
# bytes without shifting any value a test reads.
#
# What the fixtures are for, and the invariants the tests depend on:
#
#   4 seconds of testsrc2 at 160x90 and 10 fps, -g 10, so ffprobe reports a
#   keyframe PTS at 0, 1000, 2000 and 3000 ms and 40 video frames.
#   Audio is mono at 48 kHz, built from sine, so sample rate and channel layout
#   are unambiguous. Subtitle cues are 500-2000, 2500-3000 and 3200-3900 ms in
#   all three subtitle files, one cue spans two lines, and the ASS file carries
#   italic tags plus a comma, which is what the Web UI subtitle path needs.
#
# expected.json is ffprobe's own view of each fixture, written by this script.
# Do not hand edit it, and do not treat it as a transcript of what fbtt-media
# answers: the tests compare the two, which is the point of keeping it here.
#
# After regenerating, run the tests:
#
#   cargo test -p fbtt-media

set -euo pipefail

command -v ffmpeg  >/dev/null || { echo "ffmpeg is required on PATH"        >&2; exit 1; }
command -v ffprobe >/dev/null || { echo "ffprobe is required on PATH"       >&2; exit 1; }
command -v python3 >/dev/null || { echo "python3 is required on PATH"       >&2; exit 1; }

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="$ROOT/crates/fbtt-media/tests/fixtures"
mkdir -p "$OUT"

# ffmpeg refuses to mix a subtitle codec choice per stream without the stream
# index suffix, so the subtitle codecs below always carry :N.
VIDEO_SRC=(-f lavfi -i "testsrc2=duration=4:size=160x90:rate=10")
# Audio is mono at 48 kHz through output side -ar, -ac and -channel_layout, so
# sample rate and channel layout in expected.json are never a resampling
# artefact of the source filter.
SINE_440=(-f lavfi -i "sine=frequency=440:duration=4")
SINE_660=(-f lavfi -i "sine=frequency=660:duration=4")
SINE_880=(-f lavfi -i "sine=frequency=880:duration=4")

# Three subtitle files, same cues, 3 cues each. Cue 1 spans two lines. The ASS
# cue 2 keeps inline tags and a comma in its text.
cat > "$OUT/sub.srt" <<'SRT'
1
00:00:00,500 --> 00:00:02,000
First subtitle line
spanning two lines

2
00:00:02,500 --> 00:00:03,000
Second subtitle line

3
00:00:03,200 --> 00:00:03,900
Third subtitle line
SRT

cat > "$OUT/sub.ass" <<'ASS'
[Script Info]
ScriptType: v4.00+
PlayResX: 160
PlayResY: 90

[v4 Styles]
Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, TertiaryColour, BackColour, Bold, Italic, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Alpha, Encoding
Style: Default,Arial,20,&H00FFFFFF,&H00B0B0B0,&H00404040,-&H00000000,0,0,1,2,2,2,10,10,10,0,1

[Events]
Format: Marked, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text
Dialogue: Marked=0,0:00:00.50,0:00:02.00,Default,,0,0,0,,First ASS line\Nspanning two lines
Dialogue: Marked=0,0:00:02.50,0:00:03.00,Default,,0,0,0,,Second cue, {\i1}italic{\i0} text
Dialogue: Marked=0,0:00:03.20,0:00:03.90,Default,,0,0,0,,Third ASS line
ASS

cat > "$OUT/sub.vtt" <<'VTT'
WEBVTT

00:00:00.500 --> 00:00:02.000
First WebVTT line
spanning two lines

00:00:02.500 --> 00:00:03.000
Second WebVTT line

00:00:03.200 --> 00:00:03.900
Third WebVTT line
VTT

echo "== h264-aac-opus-srt-ass.mkv"
ffmpeg -hide_banner -y -loglevel error \
    "${VIDEO_SRC[@]}" "${SINE_440[@]}" "${SINE_660[@]}" \
    -f srt -i "$OUT/sub.srt" \
    -f ass -i "$OUT/sub.ass" \
    -map 0:v -map 1:a -map 2:a -map 3:0 -map 4:0 \
    -c:v libx264 -preset veryslow -crf 28 -pix_fmt yuv420p -g 10 -bf 2 \
    -c:a:0 aac -b:a 24k -ar 48000 -ac 1 -channel_layout mono \
    -c:a:1 libopus -b:a 12k -ar 48000 -ac 1 -channel_layout mono -compression_level 19 \
    -c:s:0 srt -c:s:1 ass \
    -metadata:s:a:0 language=eng -metadata:s:a:1 language=ita \
    -metadata:s:s:0 language=eng -metadata:s:s:1 language=ita \
    -metadata:s:v:0 title="Main video" -metadata:s:a:0 title="English commentary" \
    -metadata:s:a:1 title="Italian commentary" -metadata:s:s:0 title="English subtitles" \
    -metadata:s:s:1 title="Italian subtitles" \
    -metadata title="Multi track Matroska fixture" \
    "$OUT/h264-aac-opus-srt-ass.mkv"

echo "== hevc-flac.mkv"
ffmpeg -hide_banner -y -loglevel error \
    "${VIDEO_SRC[@]}" "${SINE_440[@]}" \
    -c:v libx265 -preset veryslow -x265-params log-level=none:crf=30 -pix_fmt yuv420p -g 10 -bf 2 \
    -c:a flac -compression_level 8 -ar 48000 -ac 1 -channel_layout mono \
    -metadata:s:a:0 language=eng \
    "$OUT/hevc-flac.mkv"

echo "== av1-ac3-eac3.mkv"
# E-AC3 is written as its own track. ffmpeg only downmixes to AC3 when the
# caller asks for it, so both stay present and the probe sees two codecs.
ffmpeg -hide_banner -y -loglevel error \
    "${VIDEO_SRC[@]}" "${SINE_440[@]}" "${SINE_880[@]}" \
    -map 0:v -map 1:a -map 2:a \
    -c:v libsvtav1 -preset 6 -crf 45 -g 10 \
    -c:a:0 ac3 -ar 48000 -ac 1 -channel_layout mono -b:a 96k \
    -c:a:1 eac3 -ar 48000 -ac 1 -channel_layout mono -b:a 96k \
    -metadata:s:a:0 language=eng \
    "$OUT/av1-ac3-eac3.mkv"

echo "== h264-mp3.mkv"
ffmpeg -hide_banner -y -loglevel error \
    "${VIDEO_SRC[@]}" "${SINE_440[@]}" \
    -c:v libx264 -preset veryslow -crf 28 -pix_fmt yuv420p -g 10 -bf 2 \
    -c:a mp3 -ar 48000 -ac 1 -channel_layout mono -b:a 40k \
    -metadata:s:a:0 language=eng \
    "$OUT/h264-mp3.mkv"

echo "== h264-live-nocues.mkv"
# -live 1 leaves out the Cues element, writes unknown-size clusters, and leaves
# out the Duration element too, which is the shape of a file that is still
# being recorded when something probes it.
ffmpeg -hide_banner -y -loglevel error \
    "${VIDEO_SRC[@]}" "${SINE_440[@]}" \
    -c:v libx264 -preset veryslow -crf 28 -pix_fmt yuv420p -g 10 -bf 2 \
    -c:a aac -b:a 24k -ar 48000 -ac 1 -channel_layout mono \
    -live 1 \
    "$OUT/h264-live-nocues.mkv"

echo "== h264-aac2-movtext.mp4 (faststart)"
ffmpeg -hide_banner -y -loglevel error \
    "${VIDEO_SRC[@]}" "${SINE_440[@]}" "${SINE_660[@]}" \
    -f srt -i "$OUT/sub.srt" \
    -map 0:v -map 1:a -map 2:a -map 3:0 \
    -c:v libx264 -preset veryslow -crf 28 -pix_fmt yuv420p -g 10 -bf 2 \
    -c:a aac -b:a 24k -ar 48000 -ac 1 -channel_layout mono \
    -c:s mov_text \
    -metadata:s:a:1 language=ita -metadata:s:s:0 language=eng \
    -movflags +faststart \
    "$OUT/h264-aac2-movtext.mp4"

echo "== h264-aac-moovlast.mp4 (no faststart)"
# No faststart flag: the mp4 muxer appends moov after mdat, which is the
# ordering the probe has to seek past to find the track table.
ffmpeg -hide_banner -y -loglevel error \
    "${VIDEO_SRC[@]}" "${SINE_440[@]}" \
    -c:v libx264 -preset veryslow -crf 28 -pix_fmt yuv420p -g 10 -bf 2 \
    -c:a aac -b:a 24k -ar 48000 -ac 1 -channel_layout mono \
    -metadata:s:a:0 language=eng \
    "$OUT/h264-aac-moovlast.mp4"

echo "== vp9-opus.webm"
# -compression_level 19 is what keeps this under the 150 KB cap: libvpx defaults
# to a slower, larger encode at this bitrate.
ffmpeg -hide_banner -y -loglevel error \
    "${VIDEO_SRC[@]}" "${SINE_440[@]}" \
    -c:v libvpx-vp9 -b:v 25k -deadline good -cpu-used 4 -row-mt 1 -g 10 -compression_level 19 \
    -c:a libopus -b:a 12k -ar 48000 -ac 1 -channel_layout mono -compression_level 19 \
    -metadata:s:a:0 language=eng \
    "$OUT/vp9-opus.webm"

# expected.json: ffprobe's own view of each fixture, per fixture and track.
# Codec name, language, frame count, audio sample rate and channel layout,
# each subtitle cue start and end in ms, and the PTS in ms of every video
# keyframe. For the MP4s it also records the top level box order, which is the
# thing the faststart fixture exists to exercise.
FFMPEG_VERSION="$(ffmpeg -version 2>/dev/null | sed -n '1p')" OUT_DIR="$OUT" python3 - <<'PY'
import json, os, pathlib, subprocess

out = pathlib.Path(os.environ["OUT_DIR"])
version = os.environ["FFMPEG_VERSION"]

# ffprobe reports the Matroska default language as "und" and MP4 as "und" too,
# and fbtt-media turns both into None. Record the same thing here so the tests
# can compare the two views directly.
LANGUAGE_AS_NONE = {"und", ""}


def ffprobe(path, *args):
    cmd = ["ffprobe", "-v", "error", "-print_format", "json", *args, str(path)]
    done = subprocess.run(cmd, capture_output=True, text=True)
    if done.returncode:
        raise SystemExit(f"{cmd}: {done.stderr}")
    return json.loads(done.stdout)


def language(stream):
    tags = stream.get("tags") or {}
    value = tags.get("language") or tags.get("BCP47")
    return None if value in LANGUAGE_AS_NONE else value


def frames(stream, kind):
    """Frames of one stream. Subtitle frames carry the cue window."""
    info = ffprobe(path, "-select_streams", str(stream["index"]), "-show_frames")
    frames = info.get("frames", [])
    if kind == "video":
        pts = []
        for f in frames:
            if f.get("key_frame") != 1:
                continue
            stamp = f.get("best_effort_timestamp_time") or f["pts_time"]
            pts.append(round(float(stamp) * 1000))
        return {"frame_count": len(frames), "keyframe_pts_ms": sorted(pts)}
    if kind == "subtitle":
        cues = []
        for f in frames:
            start = round(float(f["pts_time"]) * 1000) + int(f.get("start_display_time", 0))
            end = round(float(f["pts_time"]) * 1000) + int(f.get("end_display_time", 0))
            cues.append({"start_ms": start, "end_ms": end})
        return {"frame_count": len(frames), "cues": cues}
    return {"frame_count": len(frames)}


def mp4_boxes(path):
    """Top level box order, so the faststart fixture proves moov precedes mdat.

    An ISO BMFF box header is a 32 bit size then a 32 bit type, with size
    including the header, size 0 meaning "to end of file", and size 1 meaning a
    64 bit largesize follows the type.
    """
    data = path.read_bytes()
    boxes, pos = [], 0
    while pos + 8 <= len(data):
        size = int.from_bytes(data[pos:pos + 4], "big")
        kind = data[pos + 4:pos + 8].decode("latin-1")
        if size == 1:
            size = int.from_bytes(data[pos + 8:pos + 16], "big")
        elif size == 0:
            size = len(data) - pos
        boxes.append({"kind": kind, "offset": pos, "size": size})
        if size < 8:
            break
        pos += size
    return boxes


# ffprobe answers "matroska,webm" for both containers and "mov,mp4,..." for
# MP4, so the single name the tests compare against comes from the fixture's
# own extension plus that format name. The WebM half of the pair is real: the
# EBML DocType inside vp9-opus.webm says "webm", which is what fbtt-media reads.
def container_for(path, format_name):
    names = format_name.split(",")
    if path.suffix == ".webm":
        assert "webm" in names, f"{path.name}: ffprobe says {format_name}"
        return "webm"
    if path.suffix == ".mkv":
        assert "matroska" in names, f"{path.name}: ffprobe says {format_name}"
        return "matroska"
    assert "mp4" in names or "mov" in names, f"{path.name}: ffprobe says {format_name}"
    return "mp4"


fixtures = {}
for path in sorted(p for p in out.iterdir() if p.suffix in (".mkv", ".mp4", ".webm")):
    report = ffprobe(path, "-show_format", "-show_streams")
    duration = report["format"].get("duration")
    entry = {
        "container": container_for(path, report["format"]["format_name"]),
        "ffprobe_format_name": report["format"]["format_name"],
        "duration_ms": None if duration is None else round(float(duration) * 1000),
        "format_tags": report["format"].get("tags", {}),
        "tracks": [],
    }
    if path.suffix == ".mp4":
        entry["boxes"] = mp4_boxes(path)
    for stream in report["streams"]:
        kind = stream["codec_type"]
        track = {
            "ffprobe_index": stream["index"],
            # ffmpeg numbers Matroska TrackNumber and MP4 TrackID from 1 in
            # stream order, and fbtt-media reports those as TrackInfo.id.
            "expect_id": stream["index"] + 1,
            "kind": kind,
            "codec_name": stream["codec_name"],
            # Matroska writes a track name as TrackEntryName and MP4 as a ©nam
            # user data box; ffprobe surfaces both as the title tag.
            "title": (stream.get("tags") or {}).get("title") or (stream.get("tags") or {}).get("comment"),
            "language": language(stream),
            "default": bool((stream.get("disposition") or {}).get("default")),
            "forced": bool((stream.get("disposition") or {}).get("forced")),
        }
        track.update(frames(stream, kind))
        if kind == "audio":
            track["sample_rate"] = int(stream["sample_rate"])
            track["channels"] = stream["channels"]
            track["channel_layout"] = stream.get("channel_layout")
        entry["tracks"].append(track)
    fixtures[path.name] = entry

payload = {
    "note": "written by dev/make-media-fixtures.sh; rerun the script instead of editing",
    "ffmpeg_version": version,
    "fixtures": fixtures,
}
(out / "expected.json").write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n")
print(f"wrote {out / 'expected.json'}")
PY

total=0
for f in "$OUT"/*.mkv "$OUT"/*.mp4 "$OUT"/*.webm; do
    size=$(wc -c < "$f")
    total=$((total + size))
    printf '%8d  %s\n' "$size" "$(basename "$f")"
    if [ "$size" -ge 150000 ]; then
        echo "$(basename "$f") is $size bytes, over the 150 KB fixture cap" >&2
        exit 1
    fi
done
printf '%8d  total\n' "$total"
