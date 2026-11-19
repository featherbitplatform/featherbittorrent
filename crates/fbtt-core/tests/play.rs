//! Starting a player for real, against a stand-in for one: what reaches its arguments, what reaches it
//! only through the playlist file, and how that file goes away once the player has read it.
//!
//! Every test here runs `testbin/fake_player.rs`, which writes down what it was handed and exits.
//! Nothing opens a window, and no test needs mpv or VLC installed anywhere.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fbtt_core::player::{self, PlayError, SystemLookup};
use fbtt_core::scratch::ScratchDir;
use fbtt_core::{PlaySource, PlayTarget, PlayerConfig, StreamUrl};

/// The stand-in player this crate builds for these tests.
const FAKE: &str = env!("CARGO_BIN_EXE_fbtt-fake-player");
/// How long to wait for the stand-in to write its line, or for a playlist to be taken out.
const DEADLINE: Duration = Duration::from_secs(10);
/// How long the stand-in holds on after writing its line, so a test can see the state with the player
/// still alive.
const HOLD_MS: u64 = 300;
/// How long a playlist may take to go, timed from when the stand-in was seen to write its line. It holds
/// for `HOLD_MS` after writing, and the file is deleted when it exits, so this is the milestone plan's
/// one second of grace after the exit - nowhere near the thirty seconds that only bound a launch nobody
/// came back from. Seeing the line late (a busy machine) only moves the deadline by as much, never less.
const GONE_WITHIN: Duration = Duration::from_millis(HOLD_MS + 1_000);

/// The config that starts the stand-in: what it was handed goes to `log`, and it holds on for
/// `HOLD_MS` first.
fn fake_player(log: &Path) -> PlayerConfig {
    PlayerConfig {
        program: FAKE.to_string(),
        args: vec![
            "--log".to_string(),
            log.display().to_string(),
            "--hold-ms".to_string(),
            HOLD_MS.to_string(),
        ],
    }
}

/// What the stand-in recorded, as the JSON line it appended.
async fn recorded(log: &Path) -> serde_json::Value {
    let started = Instant::now();
    loop {
        if let Ok(text) = fs::read_to_string(log)
            && let Some(line) = text.lines().find(|line| !line.trim().is_empty())
        {
            return serde_json::from_str(line).expect("one JSON line from the stand-in player");
        }
        assert!(
            started.elapsed() < DEADLINE,
            "the stand-in player never wrote its line"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The arguments a recording holds.
fn args_of(recording: &serde_json::Value) -> Vec<String> {
    recording["args"]
        .as_array()
        .expect("the recorded arguments")
        .iter()
        .map(|arg| arg.as_str().expect("an argument as text").to_string())
        .collect()
}

/// The playlists sitting in `dir` at this moment, by name: either the one a test expects, or none - which
/// is how the two tests about what happens when a play goes wrong say "nothing was left behind".
fn playlists(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("m3u"))
        })
        .collect();
    found.sort();
    found
}

/// The address every test here pretends is a stream: the shape the local endpoint and a server both
/// answer with, token and all.
fn stream_url(token: &str) -> String {
    format!(
        "http://127.0.0.1:8090/api/v1/torrents/0123456789abcdef0123456789abcdef01234567/files/0/stream?token={token}"
    )
}

/// A token is a credential while it lives, so it goes to the player inside a file: nothing that looks
/// like it appears in what another user can read out of `ps`, and the whole address arrives in the
/// playlist.
#[tokio::test]
async fn player_args_hold_no_secret() {
    let dir = ScratchDir::new("play_secret");
    let play_dir = dir.path().join("play");
    let log = dir.path().join("fake.log");
    let url = stream_url("s3cret");
    let target = PlayTarget::new("The Film", PlaySource::Url(StreamUrl::new(url.clone())));

    let line = player::play(&fake_player(&log), target, &play_dir, &SystemLookup)
        .await
        .expect("the stand-in player starts");
    let named = Path::new(FAKE)
        .file_name()
        .expect("the stand-in has a file name");
    assert_eq!(
        line,
        format!("Opening The Film in {}", named.to_string_lossy()),
        "one line saying what was opened, in which player"
    );

    let recording = recorded(&log).await;
    let args = args_of(&recording);
    let whole = args.join(" ");
    assert!(
        !whole.contains("s3cret"),
        "the token stayed out of the arguments: {whole}"
    );
    assert!(
        !whole.contains("token="),
        "no stream address at all was an argument: {whole}"
    );

    let playlist = recording["playlist"]
        .as_str()
        .expect("the last argument was a playlist");
    assert!(
        playlist.contains(url.as_str()),
        "the address reached the player inside its file"
    );
    assert_eq!(playlist.lines().count(), 3, "one entry for one file");

    // The path the player was handed is that file, in the play folder this run made.
    let handed = PathBuf::from(args.last().expect("a media argument"));
    assert_eq!(handed.parent(), Some(play_dir.as_path()));

    // And it goes when the player exits: `HOLD_MS` of this wait is the player still running, so what
    // is being timed is the deletion, not the film.
    let started = Instant::now();
    while handed.exists() {
        assert!(
            started.elapsed() < GONE_WITHIN,
            "the playlist outlived the player it was written for"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A file that is already whole needs no playlist and no rewriting of its name: it goes across as one
/// argument, spaces, brackets and all.
#[tokio::test]
async fn path_target_is_passed_as_is() {
    let dir = ScratchDir::new("play_path");
    let media_dir = dir.path().join("media folder");
    fs::create_dir_all(&media_dir).expect("a folder for the file");
    let file = media_dir.join("The Film (2024) [1080p].mkv");
    fs::write(&file, "not really a film").expect("write the file");
    let play_dir = dir.path().join("play");
    let log = dir.path().join("fake.log");

    let target = PlayTarget::new("The Film", PlaySource::Path(file.clone()));
    player::play(&fake_player(&log), target, &play_dir, &SystemLookup)
        .await
        .expect("the stand-in player starts");

    let recording = recorded(&log).await;
    let args = args_of(&recording);
    assert_eq!(
        args.len(),
        5,
        "four arguments of ours and the file: nothing split it"
    );
    assert_eq!(
        args.last().expect("a media argument"),
        file.to_string_lossy().as_ref(),
        "the path arrives as one argument, spaces and brackets included"
    );
    assert_eq!(
        recording["playlist"],
        serde_json::Value::Null,
        "the last argument was a file, not a playlist"
    );
    assert!(
        !play_dir.exists(),
        "a whole file needs no folder of playlists"
    );
}

/// A player the config named and the host does not have is one line of bad news: nothing is started,
/// so nothing records anything either.
#[tokio::test]
async fn missing_player_is_an_error_not_a_spawn() {
    let dir = ScratchDir::new("play_missing");
    let log = dir.path().join("fake.log");
    let absent = dir.path().join("no-such-player");
    let config = PlayerConfig {
        program: absent.display().to_string(),
        args: vec!["--log".to_string(), log.display().to_string()],
    };
    let target = PlayTarget::new("The Film", PlaySource::Path(dir.path().join("film.mkv")));

    let problem = player::play(&config, target, &dir.path().join("play"), &SystemLookup)
        .await
        .expect_err("nothing by that name is here");
    assert!(
        matches!(problem, PlayError::Configured(_)),
        "a program named by hand that is not there says so: {problem:?}"
    );
    assert_eq!(
        problem.to_string(),
        format!(
            "The player in config.yaml was not found: {}",
            absent.display()
        )
    );
    assert!(
        !log.exists(),
        "and nothing ran that could have written to it"
    );
}

/// A player that refuses to start takes its address with it. The playlist was written for that one process
/// and nobody else will ever read it, so leaving it here would keep a live token on disk until some later
/// run's twelve-hour sweep happened to find it - which is the opposite of why the file exists.
#[tokio::test]
async fn a_failed_spawn_takes_the_playlist_with_it() {
    let dir = ScratchDir::new("play_spawn_fails");
    let play_dir = dir.path().join("play");
    // A file that is there, so finding a player succeeds, and would not run, so the spawn is what fails:
    // `0644` on Unix means no execute bit for anybody, and on every platform this is a text file with none
    // of the shape an executable has. Either way the answer is a spawn failure, which is what is being tested.
    let program = dir.path().join("not-executable");
    fs::write(&program, "print('I am not a media player')\n")
        .expect("a file for config.yaml to name");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&program, fs::Permissions::from_mode(0o644))
            .expect("take the execute bit");
    }

    let url = stream_url("s3cret");
    let target = PlayTarget::new("The Film", PlaySource::Url(StreamUrl::new(url)));
    let config = PlayerConfig {
        program: program.display().to_string(),
        args: Vec::new(),
    };

    let problem = player::play(&config, target, &play_dir, &SystemLookup)
        .await
        .expect_err("the program is here but will not run");
    assert!(
        matches!(&problem, PlayError::Spawn { .. }),
        "a program that is there and will not start says so: {problem:?}"
    );

    assert_eq!(
        playlists(&play_dir),
        Vec::<PathBuf>::new(),
        "the file it was written for goes the same way the process did"
    );
}

/// Quitting inside the grace period takes the playlist too. What removes the file has to be the cleanup
/// task letting go of it - not the part of it that runs after a wait - because dropping a runtime cancels
/// the tasks on it rather than finishing them, and a run that quits ten seconds into a film is exactly that.
#[test]
fn a_dropped_runtime_still_takes_the_playlist_out() {
    let dir = ScratchDir::new("play_quits_early");
    let play_dir = dir.path().join("play");
    let log = dir.path().join("fake.log");
    let url = stream_url("s3cret");
    let target = PlayTarget::new("The Film", PlaySource::Url(StreamUrl::new(url)));
    // Long enough that the stand-in is certainly still alive when the runtime goes, and short enough that
    // nothing here has to wait for it: this is a quit during a film, not a film that ended.
    let config = PlayerConfig {
        program: FAKE.to_string(),
        args: vec![
            "--log".to_string(),
            log.display().to_string(),
            "--hold-ms".to_string(),
            "5000".to_string(),
        ],
    };

    // A runtime of this test's own, one thread, no worker to hand the waiting task to: dropping it is the
    // cancellation a quit is, on this thread, at a moment this test chooses.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime to drop");

    let line = runtime
        .block_on(player::play(&config, target, &play_dir, &SystemLookup))
        .expect("the stand-in player starts");
    assert!(
        line.starts_with("Opening The Film in "),
        "the play was started: {line}"
    );

    let written = playlists(&play_dir);
    assert_eq!(
        written.len(),
        1,
        "the address is on disk while the player is still holding it: {written:?}"
    );

    drop(runtime);

    // The player runs on, unbothered, and this thread no longer has a runtime at all: what is being asserted
    // is that the file went with the task. Polling rather than one look only keeps a slow disk out of it.
    let started = Instant::now();
    while !playlists(&play_dir).is_empty() {
        assert!(
            started.elapsed() < GONE_WITHIN,
            "the playlist outlived the run that wrote it"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}
