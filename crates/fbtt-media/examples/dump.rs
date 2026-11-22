// Dumps one Matroska track as a fragmented-MP4 stream: the init segment and
// every fragment, in order, to a file. A scratch tool for ffprobe to verify
// the remuxer against an independent parser.
use fbtt_media::FragmentStream;
use fbtt_media::mkv::open_track;
use std::fs::File;
use std::io::{BufReader, Seek, Write};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let usage = "usage: dump <file> <track> <start_ms> <out>";
    let mut args = std::env::args().skip(1);
    let path = args.next().expect(usage);
    let track = args.next().expect(usage).parse().expect(usage);
    let start_ms = args.next().expect(usage).parse().expect(usage);
    let out = args.next().expect(usage);

    let reader = BufReader::new(File::open(&path)?);
    let source = open_track(reader, track, start_ms)?;
    let mut stream = FragmentStream::new(source)?;
    let mut file = File::create(&out)?;
    let mut parts = 0;
    while let Some(fragment) = stream.next_fragment()? {
        file.write_all(&fragment)?;
        parts += 1;
    }
    println!(
        "wrote {} parts ({} bytes) to {out} (start_ms {})",
        parts,
        file.stream_position()?,
        stream.start_ms()
    );
    Ok(())
}
