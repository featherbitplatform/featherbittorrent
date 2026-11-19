//! Finding a media player, and starting it on one file without putting that file's address on its
//! command line.
//!
//! The player is not this program: it is mpv or VLC, found the way decision 8 of the milestone 3 plan
//! says (`PATH` first, then where each one is usually installed), or whatever `config.yaml` names.
//! What it is handed is either an absolute path - a file that is already whole - or the name of a
//! private playlist file that holds the stream address. The second shape is the point of this module:
//! a stream address carries a token that is a credential while it lives, and on every platform this
//! program runs on another user can read another process's arguments (`ps`). A playlist path names
//! nothing secret, and its file is readable only by this user (decision 3).
//!
//! [`play`] is the whole of it for anybody else: find, write, start, say what was opened in one line.
//! The player outlives this program - quitting does not stop a film somebody is watching - and the playlist
//! outlives neither the player, nor the play that failed to start one, nor the run that quit mid-film.

use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf, is_separator};
use std::process::Stdio;
use std::time::{Duration, SystemTime};

use thiserror::Error;
use tokio::process::Command;

use crate::client::{PlaySource, PlayTarget};
use crate::config::PlayerConfig;

/// The Windows creation flag that keeps a spawned player from being handed a console window of its
/// own: `CREATE_NO_WINDOW`.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// How long a playlist may sit on disk after the player was started: longer than any player takes to
/// read three lines, shorter than a pause at the start of a film.
const LAUNCH_GRACE: Duration = Duration::from_secs(30);
/// How old a leftover playlist has to be before [`sweep_playlists`] takes it: longer than a run that
/// died could have kept one alive, so anything past this belongs to nobody.
const SWEEP_AFTER: Duration = Duration::from_secs(12 * 60 * 60);
/// The random bytes behind a playlist's name, spelled as hex: twice as many characters.
const NAME_BYTES: usize = 16;
/// How many fresh names one play is willing to draw before it calls the folder cursed.
const NAME_TRIES: usize = 4;

/// Who answers "is this program here?" - a test's own table in a unit test, this process's environment
/// in a run.
pub trait Lookup: Send + Sync {
    /// The program called `name`, as some directory on `PATH` holds it, or `None` when none does.
    fn find_in_path(&self, name: &str) -> Option<PathBuf>;

    /// Whether this exact path is something to run.
    fn exists(&self, path: &Path) -> bool;
}

/// [`Lookup`] over this process's real environment.
#[derive(Debug, Clone)]
pub struct SystemLookup;

impl Lookup for SystemLookup {
    fn find_in_path(&self, name: &str) -> Option<PathBuf> {
        let path = std::env::var_os("PATH")?;
        // Windows runs a program by the extensions `PATHEXT` lists, so `vlc` means `vlc.exe`; with no
        // such variable `.exe` is the one it always takes. An empty entry would mean "whatever is in
        // the directory this process happens to be in", which is not a directory on `PATH`.
        #[cfg(windows)]
        let extensions: Vec<String> = std::env::var("PATHEXT")
            .map(|list| list.split(';').map(path_extension).collect())
            .unwrap_or_else(|_| vec![".exe".to_string()]);
        for dir in std::env::split_paths(&path).filter(|dir| !dir.as_os_str().is_empty()) {
            let direct = dir.join(name);
            if direct.is_file() {
                return Some(direct);
            }
            #[cfg(windows)]
            for extension in &extensions {
                let with_extension = dir.join(format!("{name}{extension}"));
                if with_extension.is_file() {
                    return Some(with_extension);
                }
            }
        }
        None
    }

    fn exists(&self, path: &Path) -> bool {
        // Whether it would actually run is the spawn's answer; this only says whether something is
        // there under this name.
        path.is_file()
    }
}

/// One `PATHEXT` entry spelled with its dot: the list reads `.EXE;.BAT;.CMD`, and an entry that has
/// its dot already needs no second one.
#[cfg(windows)]
fn path_extension(entry: &str) -> String {
    let entry = entry.trim();
    if entry.is_empty() || entry.starts_with('.') {
        return entry.to_string();
    }
    format!(".{entry}")
}

/// Which player a program is, which is what decides the arguments it needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerKind {
    /// mpv: options end with `--` before a file, and a playlist goes in through `--playlist=`.
    Mpv,
    /// VLC: `--play-and-exit`, so it is not left holding an empty window after the film.
    Vlc,
    /// Anything else: beyond the arguments `config.yaml` gave it, the media is its only argument.
    Other,
}

/// A program to start, and what shape its arguments take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Player {
    /// The program to run: found on `PATH`, found at an install path, or named by `config.yaml`.
    pub program: PathBuf,
    /// Which player it is.
    pub kind: PlayerKind,
}

/// Why nothing was started.
#[derive(Debug, Error)]
pub enum PlayError {
    /// Nothing to start, which the person can fix themselves: install one of the two players this
    /// program looks for, or name one in `config.yaml`.
    #[error("No media player found. Install mpv or VLC, or set player.program in config.yaml.")]
    NotFound,
    /// `player.program` named something specific, and it is not there. The line names what was asked
    /// for, because that is the line to fix.
    #[error("The player in config.yaml was not found: {0}")]
    Configured(String),
    /// The playlist file would not be written.
    #[error("Could not write the playlist file: {source}")]
    Playlist { source: io::Error },
    /// The program is there and would not start.
    #[error("Could not start {program}: {source}")]
    Spawn {
        /// What this run tried to run.
        program: PathBuf,
        source: io::Error,
    },
}

/// The player this run should start, found by the rules in decision 8.
///
/// `auto` (the default) tries mpv first and VLC second: everything on `PATH` before anything found at
/// an install path, because `PATH` is where a package manager puts the player somebody installed on
/// purpose. `mpv` and `vlc` look for that one by the same rules. Anything else is taken as it is
/// spelled: a path that exists, otherwise a name on `PATH`.
pub fn find_player(config: &PlayerConfig, lookup: &dyn Lookup) -> Result<Player, PlayError> {
    match config.program.as_str() {
        "auto" => auto_player(lookup).ok_or(PlayError::NotFound),
        "mpv" => named_player("mpv", PlayerKind::Mpv, lookup).ok_or(PlayError::NotFound),
        "vlc" => named_player("vlc", PlayerKind::Vlc, lookup).ok_or(PlayError::NotFound),
        other => configured_player(other, lookup),
    }
}

/// `auto`: mpv and VLC on `PATH`, and then the places either one is usually installed.
fn auto_player(lookup: &dyn Lookup) -> Option<Player> {
    for (kind, name) in [(PlayerKind::Mpv, "mpv"), (PlayerKind::Vlc, "vlc")] {
        if let Some(found) = lookup.find_in_path(name) {
            return Some(Player {
                program: found,
                kind,
            });
        }
    }
    install_paths()
        .into_iter()
        .find(|(_, path)| lookup.exists(path))
        .map(|(kind, program)| Player { program, kind })
}

/// One player named in `config.yaml` as `mpv` or `vlc`: the same two rules, for that one alone.
fn named_player(name: &str, kind: PlayerKind, lookup: &dyn Lookup) -> Option<Player> {
    if let Some(found) = lookup.find_in_path(name) {
        return Some(Player {
            program: found,
            kind,
        });
    }
    install_paths()
        .into_iter()
        .filter(|(install_kind, _)| *install_kind == kind)
        .find(|(_, path)| lookup.exists(path))
        .map(|(install_kind, program)| Player {
            program,
            kind: install_kind,
        })
}

/// A program `config.yaml` named itself: a path that is there is used as it was spelled, and anything
/// else gets the one `PATH` try its name would get.
fn configured_player(program: &str, lookup: &dyn Lookup) -> Result<Player, PlayError> {
    let as_path = PathBuf::from(program);
    if program.chars().any(is_separator) && lookup.exists(&as_path) {
        return Ok(Player {
            program: as_path,
            kind: PlayerKind::Other,
        });
    }
    lookup
        .find_in_path(program)
        .map(|found| Player {
            program: found,
            kind: PlayerKind::Other,
        })
        .ok_or_else(|| PlayError::Configured(program.to_string()))
}

/// The places mpv and VLC end up when nobody put them on `PATH`, in the order `auto` tries them:
/// mpv's machine-wide install, VLC's, then the same two in the folder a person installs into for
/// themselves. Linux has none: that is what `PATH` and a package manager are for.
fn install_paths() -> Vec<(PlayerKind, PathBuf)> {
    #[cfg(target_os = "macos")]
    {
        let mut paths = vec![
            (
                PlayerKind::Mpv,
                PathBuf::from("/Applications/mpv.app/Contents/MacOS/mpv"),
            ),
            (
                PlayerKind::Vlc,
                PathBuf::from("/Applications/VLC.app/Contents/MacOS/VLC"),
            ),
        ];
        if let Some(home) = std::env::var_os("HOME") {
            let home = PathBuf::from(home);
            paths.push((
                PlayerKind::Mpv,
                home.join("Applications/mpv.app/Contents/MacOS/mpv"),
            ));
            paths.push((
                PlayerKind::Vlc,
                home.join("Applications/VLC.app/Contents/MacOS/VLC"),
            ));
        }
        paths
    }

    #[cfg(windows)]
    {
        let mut paths = Vec::new();
        if let Some(files) = std::env::var_os("ProgramFiles") {
            let files = PathBuf::from(files);
            paths.push((PlayerKind::Mpv, files.join("mpv").join("mpv.exe")));
            paths.push((
                PlayerKind::Vlc,
                files.join("VideoLAN").join("VLC").join("vlc.exe"),
            ));
        }
        // A 32-bit VLC on a 64-bit host lands in the other Program Files folder.
        if let Some(files) = std::env::var_os("ProgramFiles(x86)") {
            paths.push((
                PlayerKind::Vlc,
                PathBuf::from(files)
                    .join("VideoLAN")
                    .join("VLC")
                    .join("vlc.exe"),
            ));
        }
        paths
    }

    #[cfg(not(any(target_os = "macos", windows)))]
    {
        Vec::new()
    }
}

/// The arguments to start `player` with: the configured ones first, then this player's own, then the
/// media last.
///
/// mpv gets `--no-terminal`, because this program is the terminal already, and a `--` before a path so
/// that a file whose name starts with a dash reaches it as a name; a playlist goes in through
/// `--playlist=`, which is how mpv knows to read entries out of the file instead of treating it as one
/// more media file. VLC gets `--play-and-exit`. Anything else gets the media and nothing else, because
/// this program does not know its option syntax.
pub fn player_args(player: &Player, extra: &[String], media: &Path) -> Vec<OsString> {
    let mut args: Vec<OsString> = extra.iter().map(OsString::from).collect();
    match player.kind {
        PlayerKind::Mpv => {
            args.push("--no-terminal".into());
            if is_playlist(media) {
                // Named as a playlist, or mpv treats the file as one more media file and plays its
                // name instead of its entries.
                let mut flag = OsString::from("--playlist=");
                flag.push(media.as_os_str());
                args.push(flag);
            } else {
                // Ends the options, so a file named `-weird.mkv` arrives as a name.
                args.push("--".into());
                args.push(media.as_os_str().to_owned());
            }
        }
        PlayerKind::Vlc => {
            args.push("--play-and-exit".into());
            args.push(media.as_os_str().to_owned());
        }
        PlayerKind::Other => args.push(media.as_os_str().to_owned()),
    }
    args
}

/// Whether this path is a playlist file: the extension is all there is to go on, and `.m3u` is what
/// [`write_playlist`] writes.
fn is_playlist(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("m3u"))
}

/// The playlist to hand a player for `target`, written private to this user, and where it is.
///
/// Only a stream address needs one: a file that is already whole is played by its own path and carries
/// no secret. The name is random, so two plays of the same file are two files and nothing about a name
/// says which torrent it belongs to, and the file is made `create_new` for the same reason - whatever
/// is already under that name stays under it. On Unix it is readable only by this user, which is what
/// makes it a safe place for a credential a command line would not have been.
pub fn write_playlist(dir: &Path, target: &PlayTarget) -> io::Result<PathBuf> {
    let PlaySource::Url(url) = target.source() else {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "a file that is already whole is played by its own path",
        ));
    };
    create_private_dir(dir)?;
    // A line break in an address would turn one entry into two. The title needs no such care: it came
    // through `PlayTarget`, which takes every line break out of it on the way in.
    let url = one_line(url.as_str());
    let body = format!("#EXTM3U\n#EXTINF:-1,{}\n{url}\n", target.title());
    for _ in 0..NAME_TRIES {
        let path = dir.join(format!("{}.m3u", random_name()?));
        match private_file(&path) {
            Ok(mut file) => match file.write_all(body.as_bytes()) {
                Ok(()) => return Ok(path),
                Err(source) => {
                    drop(file);
                    // Half a playlist would leave a token sitting where nobody can finish reading it.
                    let _ = fs::remove_file(&path);
                    return Err(source);
                }
            },
            // Something is already under this name - two plays drawing the same 32 hex digits, or a
            // folder somebody filled by hand. Draw again rather than touch what is there.
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(source) => return Err(source),
        }
    }
    Err(io::Error::other(
        "every playlist name this run drew was already taken",
    ))
}

/// `dir`, and any directory above it this library has not made yet, each readable only by this user
/// (`0700` on Unix).
///
/// A directory that is already there keeps the mode it has: this program does not change permissions on a
/// folder somebody else made. The recursive rule is `std`'s own, which as a method is still private, both
/// because `<data_dir>/play` on a library nobody has run yet needs `<data_dir>` under it and because the
/// folders above are not this program's to ask permission from (see [`create_dir_chain`]).
fn create_private_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    let builder = {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        builder
    };
    #[cfg(not(unix))]
    let builder = fs::DirBuilder::new();
    create_dir_chain(dir, &|path| builder.create(path), &|path| path.is_dir())
}

/// The recursive directory create `std` uses, with the making and the checking handed in so that a test can
/// answer the way another platform does.
///
/// Ask for the directory itself; climb to its parent only when something above it is genuinely missing
/// (`NotFound`); and count a refusal as done when the path turns out to be a folder anyway, whatever reason
/// was given for it. That last rule is what a platform needs that does not answer `AlreadyExists` for a
/// folder that is already there: Windows answers *access denied* for a drive root, so making every folder
/// above first would ask `C:\` for permission it never had and fail every stream play on a library at the
/// root of a disk - where the folder this program needed was there all along.
fn create_dir_chain(
    dir: &Path,
    mkdir: &dyn Fn(&Path) -> io::Result<()>,
    is_dir: &dyn Fn(&Path) -> bool,
) -> io::Result<()> {
    if dir.as_os_str().is_empty() {
        // An empty path is the folder this process is standing in, which this program neither makes nor
        // needs made.
        return Ok(());
    }
    match mkdir(dir) {
        Ok(()) => return Ok(()),
        // Something above it is missing: that answer, and only that answer, sends this code up the tree.
        Err(problem) if problem.kind() == io::ErrorKind::NotFound => {}
        Err(_) if is_dir(dir) => return Ok(()),
        Err(problem) => return Err(problem),
    }
    match dir.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        Some(parent) => create_dir_chain(parent, mkdir, is_dir)?,
        // Nothing above it left to make, and it is not a folder itself: the first answer stands.
        None => {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("{dir:?} has no parent left to make"),
            ));
        }
    }
    match mkdir(dir) {
        Ok(()) => Ok(()),
        // Somebody else made it while this was climbing - another play, another run - which says the same
        // thing it said before: what is there stays exactly as it is.
        Err(_) if is_dir(dir) => Ok(()),
        Err(problem) => Err(problem),
    }
}

/// A new file, readable only by this user (`0600` on Unix), that refuses to replace one already there.
fn private_file(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// A playlist's own name: random bytes as hex, so nothing about a name says which torrent it belongs
/// to and two plays cannot land on one file.
fn random_name() -> io::Result<String> {
    let mut bytes = [0u8; NAME_BYTES];
    if let Err(problem) = getrandom::fill(&mut bytes) {
        // A name that is not random would be the same name every time, which is no name at all here.
        return Err(io::Error::other(format!(
            "no random source for a playlist name: {problem}"
        )));
    }
    let mut name = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        // Writing into a String does not fail.
        let _ = std::fmt::Write::write_fmt(&mut name, format_args!("{byte:02x}"));
    }
    Ok(name)
}

/// `s` with its line breaks taken out: what one line of a playlist is made of.
fn one_line(s: &str) -> String {
    s.chars().filter(|c| !matches!(c, '\r' | '\n')).collect()
}

/// Takes out the playlists a run that died left behind: `*.m3u` in `dir` older than twelve hours.
///
/// A playlist is written for one player and taken out when that player exits (see [`play`]), so
/// anything still here belongs to a run that never got to clean up. Everything about this is best
/// effort: a folder that is not there, or a file that will not say how old it is, says nothing, and
/// the run starts anyway.
pub fn sweep_playlists(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() || !is_playlist(&path) {
            continue;
        }
        // A file this program cannot age stays where it is: a player may still be reading it, and an
        // answer about its age that the clock refutes (a file from the future) is no answer.
        let Ok(modified) = meta.modified() else {
            continue;
        };
        let Ok(age) = SystemTime::now().duration_since(modified) else {
            continue;
        };
        if age > SWEEP_AFTER {
            let _ = fs::remove_file(path);
        }
    }
}

/// A playlist file, and the promise that it does not outlive the reason for it.
///
/// The promise is kept whenever this is dropped: at the end of the cleanup task, half way through it because
/// the run quit inside the grace period, or on the way out of a `play` whose player refused to start. It has
/// to be a drop rather than a line of code because no line of code covers all three - dropping a runtime
/// cancels the tasks on it instead of finishing them - and what is in the file is an address with a token in
/// it, which should not sit on disk until some later run's sweep happens to walk past it twelve hours from
/// now. Removal is best effort, and that sweep is the backstop for the platform that will not remove a file
/// another process still has open.
struct PlaylistGuard(Option<PathBuf>);

impl Drop for PlaylistGuard {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = fs::remove_file(path);
        }
    }
}

/// Finds a player, hands it `target`, and says in one line what was opened.
///
/// * A stream address goes to the player inside a playlist, never on its command line, so no token
///   appears in a process listing (decision 3).
/// * Its stdin, stdout and stderr are closed: a player writing to the terminal this program is holding
///   would fight the screen for it.
/// * On Unix it starts in its own process group, so a Ctrl-C at this program's terminal is not sent to
///   the film as well; on Windows it starts with no console window.
/// * It is not killed when this program forgets it.
/// * A task nobody awaits waits for the player and lets go of the playlist when the player exits, or thirty
///   seconds after the launch, whichever comes first - and lets go of it if this run quits in between, which
///   is the same promise a player that refused to start gets on the way out (see `PlaylistGuard`). Being
///   detached is what lets it: a wait anybody had to finish would make quitting cost as long as the film.
pub async fn play(
    config: &PlayerConfig,
    target: PlayTarget,
    dir: &Path,
    lookup: &dyn Lookup,
) -> Result<String, PlayError> {
    // Finding a player walks `PATH` and stats the handful of install paths it has; writing a playlist
    // writes one small file. Neither is a wait, so both happen here rather than on the blocking pool -
    // which would need to own the lookup this signature only borrows. Whoever awaits this decides where
    // they happen: the binary awaits it from a job on its own worker thread (see its `switch.rs`), and
    // never from the thread that draws the screen.
    let player = find_player(config, lookup)?;
    // A playlist comes with the promise about it, before anything is started: from here to the end of this
    // function, every way out that does not hand the file to a running player drops the promise, and the
    // file with it - which is what a player that was found and would not start owes.
    let (media, playlist) = match target.source() {
        PlaySource::Path(path) => (path.clone(), PlaylistGuard(None)),
        PlaySource::Url(_) => {
            let path =
                write_playlist(dir, &target).map_err(|source| PlayError::Playlist { source })?;
            (path.clone(), PlaylistGuard(Some(path)))
        }
    };

    let mut command = Command::new(&player.program);
    command.args(player_args(&player, &config.args, &media));
    // Closed rather than inherited: a player that writes to the terminal this program is holding would
    // draw over the screen.
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // Not killed when this program forgets it: quitting does not stop a film somebody is watching, and
    // the address in its playlist stays good by its own rules (see `crate::stream::tokens`).
    command.kill_on_drop(false);
    #[cfg(unix)]
    // Its own process group, so a Ctrl-C at this program's terminal is not delivered to the player too.
    command.process_group(0);
    #[cfg(windows)]
    // No console window behind it: it brings its own.
    command.creation_flags(CREATE_NO_WINDOW);

    let child = command.spawn().map_err(|source| PlayError::Spawn {
        program: player.program.clone(),
        source,
    })?;

    // A task nobody holds a handle to, on purpose: shutting down must not cost as long as the film. The
    // playlist travels into it, promise included, so that it goes whether that task finishes waiting or is
    // cancelled half way through - which is what a quit inside the grace period is.
    tokio::spawn(tidy_up(child, playlist));

    Ok(opening_line(&player, target.title()))
}

/// Waits for the player, then lets go of its playlist.
///
/// The timeout is no deadline for the film: it bounds how long a playlist - one entry carrying an address
/// with its token in it - sits on disk once nobody is watching the process that read it. Thirty seconds is
/// longer than any player takes to read three lines and shorter than a pause at the start of a film. After
/// it the player is simply left alone: this program's part was the file. Waiting is also what reaps the
/// process, which is what `kill_on_drop(false)` needs while its handle is held.
async fn tidy_up(mut child: tokio::process::Child, playlist: PlaylistGuard) {
    tokio::select! {
        // The player exited, which is how a film ends.
        _ = child.wait() => {}
        _ = tokio::time::sleep(LAUNCH_GRACE) => {}
    }
    // Nothing special happens on this line: the file goes because this function is ending, and it goes just
    // as surely when the runtime is dropped instead - which cancels this task rather than finishing it. See
    // `PlaylistGuard`.
    drop(playlist);
}

/// The one line to show: what is opening, in which player.
fn opening_line(player: &Player, title: &str) -> String {
    let named = match player.kind {
        PlayerKind::Mpv => "mpv".to_string(),
        PlayerKind::Vlc => "VLC".to_string(),
        // A program `config.yaml` named by path: its own file name is the shortest thing that says
        // which one this was.
        PlayerKind::Other => player.program.file_name().map_or_else(
            || player.program.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        ),
    };
    format!("Opening {title} in {named}")
}

#[cfg(test)]
mod tests {
    use super::{
        Lookup, PlayError, Player, PlayerKind, create_dir_chain, create_private_dir, find_player,
        player_args, sweep_playlists, write_playlist,
    };
    use crate::client::{PlaySource, PlayTarget, StreamUrl};
    use crate::config::PlayerConfig;
    use crate::scratch::ScratchDir;
    use std::cell::RefCell;
    use std::fs;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, SystemTime};

    /// The questions [`find_player`] asks, answered from a table the test wrote: no environment this
    /// test has to set (and could only set globally), and no real player anywhere near it.
    struct FakeLookup {
        /// What `PATH` holds: name, then the program it names.
        on_path: Vec<(String, PathBuf)>,
        /// The paths that are there.
        there: Vec<PathBuf>,
    }

    impl FakeLookup {
        fn new(on_path: &[(&str, &str)], there: &[&str]) -> FakeLookup {
            FakeLookup {
                on_path: on_path
                    .iter()
                    .map(|(name, path)| (name.to_string(), PathBuf::from(path)))
                    .collect(),
                there: there.iter().map(PathBuf::from).collect(),
            }
        }
    }

    impl Lookup for FakeLookup {
        fn find_in_path(&self, name: &str) -> Option<PathBuf> {
            self.on_path
                .iter()
                .find(|(found, _)| found == name)
                .map(|(_, path)| path.clone())
        }

        fn exists(&self, path: &Path) -> bool {
            self.there.iter().any(|found| found == path)
        }
    }

    /// Nothing to find anywhere.
    fn nowhere() -> FakeLookup {
        FakeLookup::new(&[], &[])
    }

    fn auto() -> PlayerConfig {
        PlayerConfig::default()
    }

    /// Which player `auto` settles on: mpv is the first choice and VLC the second, whichever way round
    /// they happen to be listed.
    #[test]
    fn auto_prefers_mpv_then_vlc() {
        let both = FakeLookup::new(&[("vlc", "/usr/bin/vlc"), ("mpv", "/usr/bin/mpv")], &[]);
        let found = find_player(&auto(), &both).expect("both are installed");
        assert_eq!(
            found,
            Player {
                program: PathBuf::from("/usr/bin/mpv"),
                kind: PlayerKind::Mpv,
            },
            "mpv is the first choice even when VLC is listed first"
        );

        let vlc_only = FakeLookup::new(&[("vlc", "/opt/vlc")], &[]);
        let found = find_player(&auto(), &vlc_only).expect("VLC is installed");
        assert_eq!(found.kind, PlayerKind::Vlc);
        assert_eq!(found.program, PathBuf::from("/opt/vlc"));

        // `mpv` and `vlc` in `config.yaml` ask for one of them by name and get that one.
        let config = PlayerConfig {
            program: "vlc".to_string(),
            args: Vec::new(),
        };
        let found = find_player(&config, &both).expect("VLC is named and installed");
        assert_eq!(found.kind, PlayerKind::Vlc);
        assert_eq!(found.program, PathBuf::from("/usr/bin/vlc"));
    }

    /// On macOS a player installed from its disk image is not on `PATH` at all, so `auto` has to know
    /// where the bundle puts its binary.
    #[cfg(target_os = "macos")]
    #[test]
    fn auto_finds_macos_app_bundle() {
        let bundles = FakeLookup::new(
            &[],
            &[
                "/Applications/mpv.app/Contents/MacOS/mpv",
                "/Applications/VLC.app/Contents/MacOS/VLC",
            ],
        );
        let found = find_player(&auto(), &bundles).expect("a bundle is an install");
        assert_eq!(found.kind, PlayerKind::Mpv);
        assert_eq!(
            found.program,
            PathBuf::from("/Applications/mpv.app/Contents/MacOS/mpv")
        );

        // VLC alone is still a player; nothing on `PATH` and no bundle at all is not.
        let vlc_bundle = FakeLookup::new(&[], &["/Applications/VLC.app/Contents/MacOS/VLC"]);
        assert_eq!(
            find_player(&auto(), &vlc_bundle)
                .expect("VLC's bundle")
                .kind,
            PlayerKind::Vlc
        );
        assert!(matches!(
            find_player(&auto(), &nowhere()),
            Err(PlayError::NotFound)
        ));
    }

    /// A program the file named and this run cannot find is its own answer: naming what was asked for
    /// is the line that helps, where `auto` failing says what to install instead.
    #[test]
    fn configured_program_not_found() {
        let config = PlayerConfig {
            program: "mpv-legacy".to_string(),
            args: Vec::new(),
        };
        let problem = find_player(&config, &nowhere()).expect_err("nothing by that name");
        assert!(
            matches!(&problem, PlayError::Configured(name) if name == "mpv-legacy"),
            "the error carries the program it looked for: {problem:?}"
        );
        assert_eq!(
            problem.to_string(),
            "The player in config.yaml was not found: mpv-legacy"
        );

        // A path that is there is used as it is spelled, and is neither of the two players this run
        // knows the arguments of.
        let there = FakeLookup::new(&[], &["/opt/bin/my-player"]);
        let config = PlayerConfig {
            program: "/opt/bin/my-player".to_string(),
            args: Vec::new(),
        };
        let found = find_player(&config, &there).expect("the path is there");
        assert_eq!(found.kind, PlayerKind::Other);
        assert_eq!(found.program, PathBuf::from("/opt/bin/my-player"));

        // A name with a separator in it that is *not* there still gets the `PATH` try it would get
        // anyway, and then says so.
        let config = PlayerConfig {
            program: "./my-player".to_string(),
            args: Vec::new(),
        };
        assert!(matches!(
            find_player(&config, &nowhere()),
            Err(PlayError::Configured(_))
        ));
    }

    /// The line for a host with no player at all, exactly as the plan words it: it names two things to
    /// install and one setting to set instead.
    #[test]
    fn no_player_message() {
        let problem = find_player(&auto(), &nowhere()).expect_err("no player anywhere");
        assert_eq!(
            problem.to_string(),
            "No media player found. Install mpv or VLC, or set player.program in config.yaml."
        );
    }

    /// mpv's options end before the path does: `--no-terminal` is ours, `--` ends them, and a playlist
    /// has to be named as one or mpv reads it as another file.
    #[test]
    fn mpv_args_end_options_before_path() {
        let mpv = Player {
            program: PathBuf::from("/usr/bin/mpv"),
            kind: PlayerKind::Mpv,
        };
        let extra = ["--volume=50".to_string()];

        let args = player_args(&mpv, &extra, Path::new("/tmp/a file.mkv"));
        assert_eq!(
            args,
            vec!["--volume=50", "--no-terminal", "--", "/tmp/a file.mkv"],
            "the configured argument first, our option next, the path last and after a `--`"
        );

        let args = player_args(&mpv, &extra, Path::new("/data/play/0f3a.m3u"));
        assert_eq!(
            args,
            vec![
                "--volume=50",
                "--no-terminal",
                "--playlist=/data/play/0f3a.m3u"
            ],
            "a playlist is named as one, with no `--` needed for it"
        );

        // A file whose name starts with a dash still arrives as a name, because `--` came first.
        let args = player_args(&mpv, &[], Path::new("/tmp/-weird.mkv"));
        assert_eq!(args, vec!["--no-terminal", "--", "/tmp/-weird.mkv"]);
    }

    /// VLC wants one option of its own and every other program wants none: what follows them is the
    /// media, in one argument, whatever it contains.
    #[test]
    fn vlc_and_other_players_get_only_their_one_option() {
        let extra = ["--intf=dummy".to_string()];

        let vlc = Player {
            program: PathBuf::from("/usr/bin/vlc"),
            kind: PlayerKind::Vlc,
        };
        let args = player_args(&vlc, &extra, Path::new("/data/play/0f3a.m3u"));
        assert_eq!(
            args,
            vec!["--intf=dummy", "--play-and-exit", "/data/play/0f3a.m3u"]
        );

        let other = Player {
            program: PathBuf::from("/opt/bin/my-player"),
            kind: PlayerKind::Other,
        };
        let args = player_args(&other, &extra, Path::new("/tmp/a file.mkv"));
        assert_eq!(args, vec!["--intf=dummy", "/tmp/a file.mkv"]);

        let args = player_args(&other, &[], Path::new("/tmp/x.M3U"));
        assert_eq!(
            args,
            vec!["/tmp/x.M3U"],
            "no option is guessed for an unknown player"
        );
    }

    /// The playlist is three lines, one of them the address, in a file only this user can read: the
    /// reason it exists at all.
    #[test]
    fn playlist_is_private_and_one_entry() {
        let dir = ScratchDir::new("player_playlist");
        let play_dir = dir.path().join("play");
        let url = "http://127.0.0.1:8090/api/v1/torrents/abc/files/0/stream?token=s3cret";
        // A title that carries a second `#EXTINF` in it: written as it came, it would be two entries,
        // and the second one somebody else's address. `PlayTarget` takes line breaks out.
        let target = PlayTarget::new(
            "A Film\n#EXTINF:0,/etc/passwd",
            PlaySource::Url(StreamUrl::new(url.to_string())),
        );

        let path = write_playlist(&play_dir, &target).expect("a playlist in a scratch directory");
        assert_eq!(path.parent(), Some(play_dir.as_path()));
        assert_eq!(path.extension().and_then(|ext| ext.to_str()), Some("m3u"));
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()).map(str::len),
            Some(36),
            "a 32 hex character name plus `.m3u`"
        );

        let contents = fs::read_to_string(&path).expect("the playlist just written");
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(
            lines,
            ["#EXTM3U", "#EXTINF:-1,A Film #EXTINF:0,/etc/passwd", url],
            "three lines: the header, one entry with the smuggled line break gone, the address"
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path)
                .expect("the playlist")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "only this user may read the token");
            let mode = fs::metadata(&play_dir)
                .expect("the folder")
                .permissions()
                .mode();
            assert_eq!(
                mode & 0o777,
                0o700,
                "only this user may see the names in it"
            );
        }

        // A file that is already whole needs no playlist: it has nothing to hide.
        let whole = PlayTarget::new("A Film", PlaySource::Path(PathBuf::from("/tmp/a file.mkv")));
        let problem = write_playlist(&play_dir, &whole).expect_err("a path is not an address");
        assert_eq!(problem.kind(), io::ErrorKind::Unsupported);
    }

    /// The sweep is twelve hours of grace and nothing more: a player that is still reading its
    /// playlist must not have it taken out from under it.
    #[test]
    fn sweep_playlists_removes_only_old_ones() {
        use std::fs::File;

        let dir = ScratchDir::new("player_sweep");
        let play_dir = dir.path().join("play");
        fs::create_dir_all(&play_dir).expect("a play directory");
        let old = play_dir.join("0000000000000000000000000000000a.m3u");
        let fresh = play_dir.join("0000000000000000000000000000000b.m3u");
        let not_a_playlist = play_dir.join("notes.txt");
        for path in [&old, &fresh, &not_a_playlist] {
            fs::write(path, "#EXTM3U\n").expect("a playlist to sweep");
        }
        // Two of them are thirteen hours old: one is a playlist, one only looks the part.
        let long_ago = SystemTime::now() - Duration::from_secs(13 * 60 * 60);
        for path in [&old, &not_a_playlist] {
            let file = File::options()
                .write(true)
                .open(path)
                .expect("open to age it");
            file.set_modified(long_ago).expect("age the file");
        }

        sweep_playlists(&play_dir);

        assert!(!old.exists(), "an old playlist is gone");
        assert!(fresh.exists(), "a young one may still be open in a player");
        assert!(
            not_a_playlist.exists(),
            "anything not a playlist is none of our business"
        );

        // A folder that is not there says nothing rather than failing: the run starts anyway.
        sweep_playlists(&dir.path().join("no-such-folder"));
    }

    /// Making room for a playlist on a library nobody has run yet: `<data_dir>` does not exist either, so
    /// every folder above the play folder that is missing gets made, each one private, and none of them is
    /// touched if it is already there.
    #[test]
    fn create_private_dir_makes_every_missing_parent() {
        let dir = ScratchDir::new("player_dir_chain");
        let deep = dir.path().join("a").join("b").join("c");

        create_private_dir(&deep).expect("the three missing folders");

        assert!(deep.is_dir(), "the play folder is there now");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for path in [dir.path().join("a"), dir.path().join("a/b"), deep] {
                let mode = fs::metadata(&path)
                    .unwrap_or_else(|problem| panic!("{} should exist: {problem}", path.display()))
                    .permissions()
                    .mode();
                assert_eq!(
                    mode & 0o777,
                    0o700,
                    "{} was made by this program, so only this user may see inside it",
                    path.display()
                );
            }
        }
    }

    /// A folder somebody else made keeps the mode they gave it: an earlier run made it `0700`, a person who
    /// pointed `FBTT_DATA_DIR` at a folder of their own may have made it anything, and asking twice for the
    /// same folder is not a failure either time.
    #[test]
    fn create_private_dir_leaves_an_existing_folder_alone() {
        let dir = ScratchDir::new("player_dir_exists");
        let play_dir = dir.path().join("play");
        fs::create_dir(&play_dir).expect("a folder made by somebody else");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&play_dir, fs::Permissions::from_mode(0o755)).expect("their mode");
        }

        create_private_dir(&play_dir).expect("an existing folder is not an error");
        create_private_dir(&play_dir).expect("nor the second time");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&play_dir)
                .expect("the folder")
                .permissions()
                .mode();
            assert_eq!(
                mode & 0o777,
                0o755,
                "this program does not change permissions on a folder it did not make"
            );
        }
    }

    /// A folder this test has put into a mode it must not be left in, and the way back. Restoring it on drop
    /// is what lets `ScratchDir` remove everything under the folder - and it happens whatever this test's
    /// assertions did, so a failure cannot leave a folder nobody can clean up in the temp directory.
    #[cfg(unix)]
    struct LockedParent(PathBuf);

    #[cfg(unix)]
    impl Drop for LockedParent {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt;

            let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o700));
        }
    }

    /// A folder that is already there needs nothing from the folders above it, and an ancestor that would
    /// refuse to be made must not decide otherwise. On Unix that shape is a `0500` parent - readable and
    /// searchable, but no room in it to make anything - and a missing folder under it still fails, so this is
    /// not the same thing as ignoring what went wrong.
    #[cfg(unix)]
    #[test]
    fn create_private_dir_accepts_a_folder_under_a_parent_that_cannot_be_written_to() {
        use std::os::unix::fs::PermissionsExt;

        let dir = ScratchDir::new("player_dir_locked_parent");
        let locked = dir.path().join("data");
        let play_dir = locked.join("play");
        fs::create_dir(&locked).expect("the parent");
        fs::create_dir(&play_dir).expect("the folder this test is about");

        // Locked first, and only then asked whether the lock holds. Asking before would answer "writable"
        // about a folder that was never locked, which is how this test used to skip itself every time it ran.
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o500))
            .expect("no room to make anything in there");
        let _unlock = LockedParent(locked.clone());

        // Root ignores the mode, and so does a file system that does not honour it: if something can still be
        // made in there now, the refusal this test is built on does not exist on this host, and saying so out
        // loud is the honest answer rather than passing for no reason. `_unlock` goes back to `0700` on the
        // way out either way - including the way out of a failed assertion - which is what keeps the scratch
        // directory removable.
        let probe = locked.join("probe");
        match fs::create_dir(&probe) {
            Ok(()) => {
                let _ = fs::remove_dir(&probe);
                eprintln!(
                    "skipped: {} takes a new folder even at 0500, so this host has no such thing as the refusal this test is about",
                    locked.display()
                );
                return;
            }
            Err(problem) if problem.kind() == io::ErrorKind::PermissionDenied => {}
            Err(problem) => panic!(
                "{} should refuse a new folder at 0500: {problem}",
                locked.display()
            ),
        }

        create_private_dir(&play_dir).expect("a folder that is there needs nothing made above it");

        let problem = create_private_dir(&locked.join("fresh"))
            .expect_err("and a folder that has to be made in there still cannot be");
        assert_eq!(
            problem.kind(),
            io::ErrorKind::PermissionDenied,
            "the answer is still the reason it failed"
        );
    }

    /// A disk that answers the way Windows answers: making a folder that is already there is refused as
    /// *access denied* rather than answered "already exists". `C:\` answers that way to every attempt to
    /// make it, so a library at the root of a disk looks exactly like this table - and nothing here touches
    /// a real disk, so both tests below run wherever they are run.
    struct LockedTree {
        /// The folders that are already there.
        there: RefCell<Vec<PathBuf>>,
        /// Every path `mkdir` was asked about, in the order it was asked about.
        asked: RefCell<Vec<PathBuf>>,
    }

    impl LockedTree {
        fn new(there: &[&str]) -> LockedTree {
            LockedTree {
                there: RefCell::new(there.iter().map(PathBuf::from).collect()),
                asked: RefCell::new(Vec::new()),
            }
        }

        /// Refused where the folder is there, made where its parent is, `NotFound` where even that would
        /// need something above it that is missing.
        fn mkdir(&self, path: &Path) -> io::Result<()> {
            self.asked.borrow_mut().push(path.to_path_buf());
            if self.is_dir(path) {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "Access is denied.",
                ));
            }
            match path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .filter(|parent| self.is_dir(parent))
            {
                Some(_) => {
                    self.there.borrow_mut().push(path.to_path_buf());
                    Ok(())
                }
                None => Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "The system cannot find the path specified.",
                )),
            }
        }

        fn is_dir(&self, path: &Path) -> bool {
            self.there.borrow().iter().any(|found| found == path)
        }

        /// What was asked about, for an assertion.
        fn asked(&self) -> Vec<PathBuf> {
            self.asked.borrow().clone()
        }
    }

    /// A play folder that is already there needs no permission from anybody above it, whatever answer its
    /// own making was refused with - and the folders above it are none of this program's business.
    #[test]
    fn create_dir_chain_accepts_a_folder_that_answers_like_windows() {
        let tree = LockedTree::new(&["/", "/data", "/data/library", "/data/library/play"]);
        let mkdir = |path: &Path| tree.mkdir(path);
        let is_dir = |path: &Path| tree.is_dir(path);

        create_dir_chain(Path::new("/data/library/play"), &mkdir, &is_dir)
            .expect("a folder that is there needs nothing made above it");

        assert_eq!(
            tree.asked(),
            [PathBuf::from("/data/library/play")],
            "and the refusal at the top of the tree was never even asked about"
        );
    }

    /// The climb belongs to a folder that is missing and to nothing else: this program asks for the two
    /// folders it has to make, in the order it needs them, and never for a folder that is already there.
    #[test]
    fn create_dir_chain_climbs_only_when_something_above_is_missing() {
        let tree = LockedTree::new(&["/", "/data", "/data/library"]);
        let mkdir = |path: &Path| tree.mkdir(path);
        let is_dir = |path: &Path| tree.is_dir(path);

        create_dir_chain(Path::new("/data/library/play/deeper"), &mkdir, &is_dir).expect(
            "the two missing folders get made, and the three that are there stay as they are",
        );

        assert_eq!(
            tree.asked(),
            [
                PathBuf::from("/data/library/play/deeper"),
                PathBuf::from("/data/library/play"),
                PathBuf::from("/data/library/play/deeper"),
            ],
            "the missing folder, then its parent, then the missing folder again - no folder above them"
        );
    }
}
