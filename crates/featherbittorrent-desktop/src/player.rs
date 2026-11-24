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
use fbtt_desktop_core::LOCAL_ID;

use crate::commands::DesktopState;
use crate::download::{NOT_A_STREAM, instance_base, stream_target};
use crate::instances::Instances;
use crate::startup::START_WAIT;

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

/// The address to hand the player: `url` with this run's launch secret taken out of its query when that is
/// the credential it carries, and exactly as it came when `secret` is `None`, which is an instance this app
/// did not start - a token of another server is that server's business.
fn player_url(url: url::Url, secret: Option<&str>) -> url::Url {
    match secret {
        Some(secret) => without_launch_secret(url, secret),
        None => url,
    }
}

/// `url` without its `token` when that token is `secret`, and otherwise unchanged, down to the absence of a
/// query: a player that was given an address with nothing on it must not be given a `?` to wonder about.
///
/// The playlist `play` writes waits for its player - and a playlist a killed run left waits for the sweep
/// that is twelve hours away - while the launch secret is the credential this entire run answers with, so a
/// secret can outlive its reason for being in that file by hours. Taking it out costs nothing where it is
/// the secret: an address that carries it came from a server that handed out no stream token for this file,
/// which is a server that will not look for one on the way back in. A stream token is left alone, because
/// that is the credential that opens this one file and nothing else, and a player sent to an address with no
/// credential is refused the file it was sent for.
fn without_launch_secret(url: url::Url, secret: &str) -> url::Url {
    let mut url = url;
    // The pairs, read once and owned: the query is small, and rewriting it wants the rest of them anyway.
    let pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
    if !pairs
        .iter()
        .any(|(name, value)| name == "token" && value == secret)
    {
        return url;
    }
    url.set_query(None);
    for (name, value) in &pairs {
        if name == "token" && value == secret {
            continue;
        }
        url.query_pairs_mut().append_pair(name, value);
    }
    url
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

/// This run's launch secret, when `instance` is the engine this run started: the credential the page was
/// handed for it, and the only credential this shell could be asked to keep out of a playlist. A saved
/// instance is another server, and a token on its address is that server's own, so `None` there says leave
/// it alone rather than "no credential".
async fn own_secret(state: &tauri::State<'_, DesktopState>, instance: &str) -> Option<String> {
    if instance != LOCAL_ID {
        return None;
    }
    // The same address and secret the page's connection was built on. Asking again costs no wait: the
    // start settled before `instance_base` answered, and a start that never settled answers an error,
    // which is an address that came from somewhere else anyway.
    state
        .startup()
        .endpoint(START_WAIT)
        .await
        .ok()
        .map(|(_, secret)| secret)
}

/// Starts the system's media player on `url`, a file-stream address of instance `instance`, and answers
/// the line saying what was opened. Only an address of that instance is opened ([`player_target`]); the
/// address goes in a private playlist, never on the command line, and this run's launch secret is taken out
/// of it first ([`player_url`]) so the credential of the whole run is not left in a file written for one
/// player. A refusal or a failure rejects with the line to show ([`player_error_line`]).
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
    let secret = own_secret(&state, &instance).await;
    let target = player_url(target, secret.as_deref());
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

    use super::{
        player_error_line, player_target, player_url, playlist_dir, without_launch_secret,
    };
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

    /// A launch secret, spelled the way `fbtt_desktop_core::launch_secret` spells one: 43 url-safe
    /// characters, and the same shape a stream token has, which is why the two are compared by value and
    /// not by length.
    const SECRET: &str = "3xQk0VY7m1sTbW9dLpAeRjHgNuCiFoKyBvXlMnZsQwE";
    /// A stream token: the credential the app's own engine issues for one file, and not this run's secret.
    const ISSUED: &str = "8Bq2tY1wLkN4sVp6zRdXeA0mHcJfKgTuOyBnIvZrQ3E";

    /// What the page hands over is an address whose `token` opens this one file. When that token is instead
    /// this run's launch secret, the secret comes back out before a playlist is written: a playlist waits
    /// for its player, and one a killed run left sits until a sweep twelve hours away, while the launch
    /// secret is the credential this whole run answers with. Anything else the query holds is none of this
    /// business, because a player sent to an address with no credential on it is refused the file it was
    /// sent for.
    #[test]
    fn the_launch_secret_never_reaches_a_player() {
        let base = "http://127.0.0.1:53211";
        let url = url::Url::parse(&format!("{}?token={}", stream(base), SECRET))
            .expect("an address carrying a credential");

        let handed = without_launch_secret(url, SECRET);
        assert_eq!(
            handed.query_pairs().count(),
            0,
            "the credential is gone: {handed}"
        );
        assert!(
            !handed.as_str().contains(SECRET),
            "the secret is not in {handed}"
        );
        assert_eq!(
            handed.path(),
            format!("/api/v1/torrents/{HASH}/files/3/stream"),
            "and the file it names is still the same file"
        );

        // A second pair stays where it was: only the credential that is ours is taken out.
        let beside = url::Url::parse(&format!("{}?token={}&start_ms=1200", stream(base), SECRET))
            .expect("an address with a credential and a start");
        let handed = without_launch_secret(beside, SECRET);
        assert_eq!(
            handed.as_str(),
            format!("{base}/api/v1/torrents/{HASH}/files/3/stream?start_ms=1200"),
            "the other pair keeps its value and its order"
        );
    }

    /// The token the app's own engine answers with today is its own stream token: `start_desktop` turns
    /// logins on for the launch secret, so `POST /api/v1/stream-tokens` issues one and the address the page
    /// builds carries that rather than the secret. A stream token is exactly what a player has to keep, and
    /// so is a saved instance's token, which belongs to another server and is never this run's secret. An
    /// address with nothing on it goes as it came, with no `?` left behind.
    #[test]
    fn a_stream_token_is_kept() {
        let base = "http://127.0.0.1:53211";
        let url = url::Url::parse(&format!("{}?token={}", stream(base), ISSUED))
            .expect("an address carrying a stream token");

        assert_eq!(
            without_launch_secret(url.clone(), SECRET),
            url,
            "a stream token goes as it came"
        );
        assert_eq!(
            player_url(url.clone(), None),
            url,
            "an instance we did not start keeps its token"
        );
        assert_eq!(
            player_url(url.clone(), Some(SECRET)),
            url,
            "the secret is offered and the token is not it"
        );
        // A token spelled exactly like our secret on an address of an instance we did not start is that
        // server's credential, and no business of ours to remove.
        let foreign = url::Url::parse(&format!("{}?token={}", stream(base), SECRET))
            .expect("a foreign token");
        assert_eq!(
            player_url(foreign.clone(), None),
            foreign,
            "no secret of ours is offered, so nothing comes off"
        );

        let bare = url::Url::parse(&stream(base)).expect("an address with no credential");
        let handed = without_launch_secret(bare, SECRET);
        assert_eq!(
            handed.query_pairs().count(),
            0,
            "nothing was added: {handed}"
        );
        assert!(
            !handed.as_str().ends_with('?'),
            "no empty query left behind: {handed}"
        );
    }
}
