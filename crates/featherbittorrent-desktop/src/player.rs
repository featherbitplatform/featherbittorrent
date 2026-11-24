//! Open in player: hand one file of the instance the page is on to VLC or mpv.
//!
//! The webview can only play what the system's decoders take; VLC and mpv bring their own. This is the
//! desktop twin of the terminal client's `s` (see `fbtt_core::player` and `docs/streaming.md`): the page
//! hands over a file-stream address of the instance it is connected to, and the shell starts a player on
//! it. The address goes in a private playlist, never on the player's command line, exactly as the
//! terminal client does, so no stream token appears in a process listing.
//!
//! The security boundary is the same one `download` holds: the page can only make the shell start a
//! player on a file-stream address of the instance it is connected to ([`player_target`]), and the
//! address is checked before anything is started.

use std::path::{Path, PathBuf};

use fbtt_core::player::{PlayError, SystemLookup, play, sweep_playlists};
use fbtt_core::{PlaySource, PlayTarget, PlayerConfig, StreamUrl};

use crate::commands::DesktopState;
use crate::download::{NOT_A_STREAM, instance_base, stream_target};
use crate::instances::Instances;

/// The folder a playlist handed to a system player is written to, beside the engine's own files:
/// `<data_dir>/playlists`. The terminal client's own playlists are `<data_dir>/play`; the desktop app
/// runs its engine in directories of its own, so its playlists are a folder of their own too.
const PLAYLISTS_DIR_NAME: &str = "playlists";

/// Where a playlist handed to a media player is written: `<data_dir>/playlists` (see [`Playlists`]).
pub fn playlist_dir(data_dir: &Path) -> PathBuf {
    data_dir.join(PLAYLISTS_DIR_NAME)
}

/// The folder playlists are written to, kept in the app's state so the command and the sweep at app
/// start share one place. The directory is not created here: writing a playlist makes room for it,
/// private to this user (see `fbtt_core::player::play`).
#[derive(Debug)]
pub struct Playlists {
    dir: PathBuf,
}

impl Playlists {
    /// The app's playlist folder for the library in `data_dir`.
    pub fn new(data_dir: &Path) -> Self {
        Playlists {
            dir: playlist_dir(data_dir),
        }
    }

    /// Takes out the playlists a run that died left behind, once, at app start. A playlist is written
    /// for one player and taken out when that player exits, so anything still here belongs to a run that
    /// never got to clean it up. This is a directory walk rather than a wait, so it goes to the blocking
    /// pool and off the event loop; everything about a sweep that fails is its own to ignore (see
    /// `fbtt_core::player::sweep_playlists`, which is best effort on a folder that is not even there).
    pub fn sweep_stale(&self) {
        let dir = self.dir.clone();
        tauri::async_runtime::spawn(async move {
            let _ = tokio::task::spawn_blocking(move || sweep_playlists(&dir)).await;
        });
    }
}

/// The address to start a player on: `url`, when it is a file-stream address of the instance at `base`;
/// anything else is refused with `download::NOT_A_STREAM`, before anything is started. A pure function so
/// the boundary is testable without a webview (see its tests).
fn player_target(base: &str, url: &str) -> Result<url::Url, String> {
    stream_target(base, url).ok_or_else(|| NOT_A_STREAM.to_string())
}

/// The desktop line for a player that was not started. `config.yaml` means nothing to an app user, so the
/// lines the terminal client's `find_player` answers with are reworded to what a person can do about it.
fn player_error_line(problem: &PlayError) -> &'static str {
    match problem {
        PlayError::NotFound | PlayError::Configured(_) => {
            "No media player found. Install VLC or mpv."
        }
        PlayError::Playlist { .. } | PlayError::Spawn { .. } => {
            "The media player could not be started."
        }
    }
}

/// Starts the system's media player on `url`, a file-stream address of instance `instance`, and answers
/// the line saying what was opened. Only an address of that instance is opened ([`player_target`]); the
/// address goes in a private playlist, never on the command line. A refusal or a failure rejects with the
/// line to show ([`player_error_line`]).
#[tauri::command]
pub async fn open_in_player(
    instance: String,
    url: String,
    title: String,
    state: tauri::State<'_, DesktopState>,
    instances: tauri::State<'_, Instances>,
    playlists: tauri::State<'_, Playlists>,
) -> Result<String, String> {
    let base = instance_base(&state, &instances, &instance).await?;
    let target = player_target(&base, &url)?;
    let opened = play(
        &PlayerConfig::default(),
        PlayTarget::new(&title, PlaySource::Url(StreamUrl::new(target.to_string()))),
        &playlists.dir,
        &SystemLookup,
    )
    .await
    .map_err(|problem| player_error_line(&problem).to_string())?;
    Ok(opened)
}

#[cfg(test)]
mod tests {
    use fbtt_core::PlayerConfig;
    use fbtt_core::player::{PlayError, find_player};

    use super::{player_error_line, player_target, playlist_dir};
    use crate::download::NOT_A_STREAM;
    use std::path::{Path, PathBuf};

    const HASH: &str = "0123456789abcdef0123456789abcdef01234567";

    fn stream(base: &str) -> String {
        format!("{base}/api/v1/torrents/{HASH}/files/3/stream")
    }

    /// An address on another host, or on another route of the same instance, starts nothing: the page can
    /// only make the shell open a file-stream address of the instance it is connected to.
    #[test]
    fn a_foreign_address_starts_nothing() {
        let base = "http://127.0.0.1:53211";
        assert_eq!(
            player_target(base, &stream(base)),
            Ok(url::Url::parse(&stream(base)).expect("the own address parses")),
            "the instance's own file-stream address is opened"
        );
        assert!(player_target(base, &format!("{}?token=abc", stream(base))).is_ok());
        for refused in [
            stream("http://127.0.0.2:53211"),
            stream("http://127.0.0.1:5321"),
            format!("{base}/api/v1/torrents"),
            format!("{base}/api/v1/torrents/{HASH}/files/3"),
            format!("{base}/download/{HASH}"),
        ] {
            assert_eq!(
                player_target(base, &refused),
                Err(NOT_A_STREAM.to_string()),
                "{refused} is not a file-stream address of this instance"
            );
        }
    }

    /// A missing player says what to install in the desktop's words; a playlist or a spawn that failed is
    /// the plain line, and `config.yaml` is not named, because an app user has none.
    #[test]
    fn not_found_is_the_desktop_line() {
        let configured = PlayError::Configured("mpv-legacy".to_string());
        assert!(matches!(
            find_player(&PlayerConfig::default(), &Nowhere),
            Err(PlayError::NotFound)
        ));
        assert_eq!(
            player_error_line(&PlayError::NotFound),
            "No media player found. Install VLC or mpv."
        );
        assert_eq!(
            player_error_line(&configured),
            "No media player found. Install VLC or mpv."
        );
        let playlist = PlayError::Playlist {
            source: std::io::Error::other("no room"),
        };
        let spawn = PlayError::Spawn {
            program: PathBuf::from("/usr/bin/mpv"),
            source: std::io::Error::other("not there"),
        };
        assert_eq!(
            player_error_line(&playlist),
            "The media player could not be started."
        );
        assert_eq!(
            player_error_line(&spawn),
            "The media player could not be started."
        );
    }

    /// A lookup that finds nothing anywhere, so `find_player` answers `NotFound` for the test above.
    struct Nowhere;

    impl fbtt_core::player::Lookup for Nowhere {
        fn find_in_path(&self, _name: &str) -> Option<PathBuf> {
            None
        }

        fn exists(&self, _path: &Path) -> bool {
            false
        }
    }

    /// The playlists live beside the engine's files, in a folder of their own, not in the terminal
    /// client's `play`: two programs with one library would share one folder and one sweep.
    #[test]
    fn the_playlists_folder_beside_the_data_dir() {
        assert_eq!(
            playlist_dir(Path::new("/var/app")),
            PathBuf::from("/var/app/playlists")
        );
    }
}
