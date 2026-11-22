// Dumps one Matroska track as a fragmented-MP4 stream: the init segment and
// every fragment, in order, to a file. A scratch tool for ffprobe to verify
// the remuxer against an independent parser.
use fbtt_media::open_track;
use std::fs::File;
use std::io::{Seek, Write};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let usage = "usage: dump <file> <track> <start_ms> <out>";
    let mut args = std::env::args().skip(1);
    let path = args.next().expect(usage);
    let track = args.next().expect(usage).parse().expect(usage);
    let start_ms = args.next().expect(usage).parse().expect(usage);
    let out = args.next().expect(usage);

    // The `File` goes to `open_track` directly: a `BufReader` here would
    // double-buffer across the demuxer's seeks and re-read bytes the source
    // already saw, which is the accounting rule the task's decisions set.
    let mut stream = open_track(File::open(&path)?, track, start_ms)?;
    let mime = stream.mime();
    let mut file = File::create(&out)?;
    let mut parts = 0;
    while let Some(fragment) = stream.next_fragment()? {
        file.write_all(&fragment)?;
        parts += 1;
    }
    println!(
        "wrote {} parts ({} bytes) to {out} (start_ms {}, mime {mime})",
        parts,
        file.stream_position()?,
        stream.start_ms(),
    );
    Ok(())
}
