//! What the operating system handed this process: a `magnet:` link or a `.torrent` file.
//!
//! A second launch is how most systems deliver one (the single-instance plugin forwards the arguments and
//! the first copy shows it), and a cold start gets them as plain process arguments. Either way the
//! arguments are somebody else's input - a desktop file, a registry entry, a browser - so they are read
//! the same way remote input is read everywhere else in this product: a fixed shape, a fixed window of
//! two hundred fifty-six arguments to read, a fixed bound of sixteen things to open, and anything that
//! is neither a `magnet:` URI nor an existing `.torrent` file on this machine is ignored rather than
//! trusted.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use url::Url;

/// The most things one launch may hand over: a fixed amount of work for the Add page, whatever a browser
/// or a file manager decided to select, and no allocation that grows with it.
const MAX_OPENED: usize = 16;

/// The most arguments after `args[0]` that are looked at, whatever the launcher handed over. A kept item
/// costs at most a `metadata`, and the kept list stops at [`MAX_OPENED`]; what this bounds is the walk
/// itself, because an argument that keeps nothing would otherwise be read one by one however many a
/// second launch forwarded. argv is somebody else's list, so its length gets the same fixed shape as
/// everything else read off it.
const MAX_ARGS_EXAMINED: usize = 256;

/// The scheme this app registers itself for. Read case-insensitively: a desktop entry and a browser's
/// handler list both hand over `MAGNET:` as readily as `magnet:`.
const MAGNET_PREFIX: &str = "magnet:";

/// The extension a torrent file is named with, also read case-insensitively: `Video.TORRENT` is somebody's
/// camera roll name, and it is the same file.
const TORRENT_SUFFIX: &str = ".torrent";

/// The scheme that spells a path on this machine, which is how some systems hand a file over.
const FILE_PREFIX: &str = "file:";

/// One thing the arguments say to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Opened {
    /// A `magnet:` URI, spelled as it was handed over apart from any padding a launcher put around it.
    /// The text is kept as it came: an info hash is case-sensitive, so lowercasing one to compare it would
    /// ask for a different torrent.
    Magnet(String),
    /// A `.torrent` file that is actually on this machine.
    TorrentFile(PathBuf),
}

/// What the arguments name, as things to open, in the order they arrived.
///
/// `args` is the whole argument list with the program's own path first: a second launch is how most
/// systems deliver a link or a file (the single-instance plugin forwards these arguments to the copy that
/// is already running), and a cold start gets them the same way.
///
/// `args[0]` is the program and never something to open, and anything starting with `-` is somebody else's
/// switch, which is safe to skip because neither a torrent file's path nor a magnet URI starts with one.
/// At most 256 arguments after it (the module's `MAX_ARGS_EXAMINED`) are looked at, whatever list a
/// second launch forwarded. What is kept is at most the bound this module sets (sixteen items), with an
/// argument naming something already named skipped:
/// a system that delivers one file as both a path and a `file://` URL is naming one torrent, not two.
///
/// Everything else is ignored rather than trusted, because these arguments come from a desktop entry, a
/// registry key or a browser: a path that does not end `.torrent`, a `.torrent` that is not there (a
/// download already moved or deleted says nothing about what to add), an https address, a stray word. The
/// page gets a list of things to open, not a list of things a launcher half-understood.
pub fn opened_from_args(args: &[OsString]) -> Vec<Opened> {
    let mut opened: Vec<Opened> = Vec::new();
    for arg in args.iter().skip(1).take(MAX_ARGS_EXAMINED) {
        if opened.len() == MAX_OPENED {
            break;
        }
        let Some(item) = opened_from_arg(arg) else {
            continue;
        };
        if !opened.contains(&item) {
            opened.push(item);
        }
    }
    opened
}

/// One argument as something to open, or `None` for anything that is not.
fn opened_from_arg(arg: &OsString) -> Option<Opened> {
    let text = arg.to_string_lossy();
    if text.starts_with('-') {
        return None;
    }
    // A launcher pads what it hands over, and the padding is not part of a link: a URL-shaped argument is
    // read from the trimmed text, which is what the URL parser would strip for us anyway. A path is used
    // exactly as it was handed over, because a space is part of a file name.
    let trimmed = text.trim();
    let scheme = trimmed.get(..MAGNET_PREFIX.len());
    if scheme.is_some_and(|head| head.eq_ignore_ascii_case(MAGNET_PREFIX)) {
        // The whole URI is kept as handed over, case included: an info hash is case-sensitive, and the
        // server is what reads the rest. A scheme with nothing after it opens nothing.
        let rest = &trimmed[MAGNET_PREFIX.len()..];
        return (!rest.is_empty()).then(|| Opened::Magnet(trimmed.to_string()));
    }
    if trimmed
        .get(..FILE_PREFIX.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(FILE_PREFIX))
    {
        // `Url` is what a URL-shaped argument means: the same parser a browser uses, which percent-decodes
        // the path into the bytes this machine's file names are made of. A `file://` URL naming another
        // machine is not a file on this one and has no path to return.
        return Url::parse(trimmed)
            .ok()
            .filter(|url| url.scheme() == "file")
            .and_then(|url| url.to_file_path().ok())
            .and_then(|path| torrent_file(&path));
    }
    torrent_file(Path::new(arg))
}

/// `path` as something to open: a name ending `.torrent` in whatever case it is spelled, and a file that
/// is really there.
///
/// The extension is checked on the lossy text of the name, which is exact for this purpose: the suffix is
/// ASCII, and the only bytes a lossy conversion changes are the non-ASCII ones, so a name whose tail is
/// `.torrent` in bytes ends `.torrent` here and one whose tail is not cannot become `.torrent` by accident.
fn torrent_file(path: &Path) -> Option<Opened> {
    let name = path.file_name()?.to_string_lossy().to_lowercase();
    if !name.ends_with(TORRENT_SUFFIX) {
        return None;
    }
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file())
        .then(|| Opened::TorrentFile(path.to_path_buf()))
}

#[cfg(test)]
mod tests {
    use super::{MAX_ARGS_EXAMINED, MAX_OPENED, Opened, opened_from_args};
    use fbtt_core::scratch::ScratchDir;
    use std::ffi::OsString;
    use url::Url;

    /// An argument list the way a launch actually hands one over: the program's own path first, then
    /// whatever a browser, a file manager or a second launch passed along, in whatever mixture.
    #[test]
    fn args_pick_magnets_and_torrent_files() {
        let dir = ScratchDir::new("desktop_handoff");
        let existing = dir.path().join("Video.TORRENT");
        std::fs::write(&existing, b"torrent bytes").expect("write a file that is there");
        let missing = dir.path().join("Gone.torrent");
        let file_url =
            Url::from_file_path(&existing).expect("a file url for a path on this machine");

        let args = vec![
            OsString::from("/usr/bin/featherbittorrent-desktop"), // argv[0] is the program itself
            OsString::from("--flag"), // flags belong to the launcher, not to the torrent list
            OsString::from("-x"),
            OsString::from("MAGNET:?xt=urn:btih:aaa"), // the scheme is case-insensitive
            OsString::from("  magnet:?xt=urn:btih:bbb  "), // a launcher's padding is not part of the link
            existing.clone().into(), // an existing file, whatever case its extension is in
            missing.clone().into(),  // a .torrent that is not there opens nothing
            file_url.as_str().into(), // the same file spelled as a url: still one torrent
            OsString::from("/tmp/not-a-torrent"), // a path that is not a torrent file
            OsString::from("https://example.com/x.torrent"), // an address is not a file on this machine
            OsString::from("magnet:"), // a scheme with nothing after it opens nothing
        ];

        assert_eq!(
            opened_from_args(&args),
            vec![
                Opened::Magnet("MAGNET:?xt=urn:btih:aaa".to_string()),
                Opened::Magnet("magnet:?xt=urn:btih:bbb".to_string()),
                Opened::TorrentFile(existing.clone()),
            ],
            "the magnets in the order they arrived, then the one file the arguments name twice"
        );

        // Nothing to open is nothing opened, including a launch with only its own name in it.
        assert!(opened_from_args(&[]).is_empty());
        assert!(opened_from_args(&[OsString::from("featherbittorrent-desktop")]).is_empty());

        // A flood of hand-offs is bounded, and the ones kept are the first ones in order: the first
        // argument is the program again, so twenty magnets leave nineteen candidates.
        let flood: Vec<OsString> = (0..20)
            .map(|n| format!("magnet:?xt=urn:btih:{n:02}").into())
            .collect();
        let opened = opened_from_args(&flood);
        assert_eq!(MAX_OPENED, 16, "the bound the arguments are read against");
        assert_eq!(
            opened.len(),
            MAX_OPENED,
            "nineteen candidates, sixteen opened"
        );
        assert!(
            matches!(opened.first(), Some(Opened::Magnet(text)) if text == "magnet:?xt=urn:btih:01"),
            "the first one that was not argv[0] is the first one opened"
        );
        assert!(
            opened.iter().all(|item| matches!(item, Opened::Magnet(_))),
            "a magnet argument is never read as a path"
        );
        assert_eq!(
            opened.last(),
            Some(&Opened::Magnet("magnet:?xt=urn:btih:16".to_string())),
            "and the sixteenth is the last one kept"
        );
    }

    /// The arguments are read for a fixed window, whatever the launcher handed over: argv[0], then
    /// `MAX_ARGS_EXAMINED` of them, and nothing after that is looked at. A flood of a thousand fillers
    /// costs a bounded amount of work, and a magnet parked deep inside one is not something this app
    /// goes hunting for.
    #[test]
    fn args_past_the_examined_cap_are_ignored() {
        assert_eq!(
            MAX_ARGS_EXAMINED, 256,
            "the window the arguments are read against"
        );
        let mut args: Vec<OsString> = (0..300)
            .map(|n| OsString::from(format!("filler-{n}")))
            .collect();
        args[0] = OsString::from("/usr/bin/featherbittorrent-desktop");

        // The only real thing in the list sits past the window: the 280th argument is one more than
        // the 256 this reads after argv[0].
        args[280] = OsString::from("magnet:?xt=urn:btih:deep");
        assert!(
            opened_from_args(&args).is_empty(),
            "an argument at position 280 is past the {} that are examined",
            MAX_ARGS_EXAMINED,
        );

        // The window is exactly that wide: the 256th argument after argv[0] is still read.
        args[280] = OsString::from("filler-280");
        args[MAX_ARGS_EXAMINED] = OsString::from("magnet:?xt=urn:btih:edge");
        assert_eq!(
            opened_from_args(&args),
            vec![Opened::Magnet("magnet:?xt=urn:btih:edge".to_string())],
            "the 256th argument after argv[0] is the last one read"
        );
    }
}
