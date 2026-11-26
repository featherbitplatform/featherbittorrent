//! The proxy setting on a live engine, all on loopback: an engine that starts with a proxy that is
//! down, one that never connects directly while a proxy is set, and a proxy that refuses the login.
//!
//! A proxy in these tests is a listener inside the test, on `127.0.0.1` and a port the OS picks: a
//! port with nothing on it for "the proxy is down", and a stand-in SOCKS5 proxy that offers
//! username/password authentication and refuses the credentials for "the login was refused". Nothing
//! here reaches the network, and the engine's proxy retry knobs are turned down so a state that is
//! re-probed with a backoff settles inside a test's own ten seconds rather than a minute.
//!
//! The waits are bounded: [ten seconds](HEALTH_DEADLINE) for a health state, and three seconds for
//! "nobody saw the backend", which is what [`no_direct_connection_while_proxy_is_set`] is.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};

use fbtt_core::proxy::ProxyHealth;
use fbtt_core::{
    AddSource, Client, Command, Config, Connection, LibraryLock, LiveLimits, LocalBackend, Notice,
    PasswordChange, ProxyChange, ProxySetting, ProxyType, SettingsChange, SettingsCommand,
};
use tokio::task::JoinHandle;

mod support;
use support::{
    Library, MIB, POLL, error_text, id_of, info_text, loopback, notice, row_of, sample_for,
    shutdown, torrent_for, wait_for, wait_until_seeding, wire_of, within, write_file,
};

/// How long a test waits for the engine's proxy health to settle, once.
const HEALTH_DEADLINE: Duration = Duration::from_secs(10);
/// How long [`no_direct_connection_while_proxy_is_set`] offers the seed and the witness as peers
/// before looking: long enough that a direct connection would have been made and completed, short
/// enough to sit inside the shared deadline.
const OFFER_EVERY: Duration = Duration::from_millis(50);
/// The offer window from the line above.
const OFFER_WINDOW: Duration = Duration::from_secs(3);

/// A SOCKS5 proxy at `127.0.0.1:port`, with no credentials.
fn socks5(port: u16) -> ProxySetting {
    ProxySetting {
        kind: ProxyType::Socks5,
        host: "127.0.0.1".to_string(),
        port,
        username: None,
        password: None,
    }
}

/// A loopback port with nothing listening on it: bound and released, so a dial to it is refused the
/// way a proxy that is down refuses one.
fn closed_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback listener");
    let port = listener.local_addr().expect("its address").port();
    drop(listener);
    port
}

/// A library whose config names `proxy`, the proxy setting under test.
fn proxied(lib: &Library, proxy: ProxySetting) -> Config {
    Config {
        proxy: Some(proxy),
        ..lib.config.clone()
    }
}

/// Starts a backend on `config`, with the engine's proxy retry knobs turned down.
///
/// The knobs are `Settings::proxy_retry_base` and `proxy_retry_max`, which the engine uses to
/// re-probe a proxy that is unreachable or refused the login. A minute of backoff is right for a
/// real run and useless to a test, so they are 50 ms and 200 ms here: a state a proxy settles into
/// is reachable inside [`HEALTH_DEADLINE`].
async fn start(
    lib: &Library,
    config: Config,
) -> (Client, JoinHandle<Result<(), String>>, fbt::Session) {
    let mut settings = loopback(&config);
    settings.proxy_retry_base = Duration::from_millis(50);
    settings.proxy_retry_max = Duration::from_millis(200);
    let lock = LibraryLock::acquire(&lib.paths.lock_file()).expect("lock the library");
    let (client, task, session, _automation) = within(LocalBackend::start_for_test(
        settings, config, &lib.paths, lock,
    ))
    .await
    .expect("the engine starts whatever the proxy is doing");
    (client, task, session)
}

/// Waits until the engine reports a proxy health `want` accepts, and fails with what it reported
/// instead once [`HEALTH_DEADLINE`] is up. Every wait here goes through this: a health state that
/// never arrives is a failure with a message rather than a hung test job.
async fn health_becomes(
    session: &fbt::Session,
    want: impl Fn(&ProxyHealth) -> bool,
) -> ProxyHealth {
    tokio::time::timeout(HEALTH_DEADLINE, async {
        loop {
            let health = fbtt_core::proxy::health(session);
            if want(&health) {
                return health;
            }
            tokio::time::sleep(POLL).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        let health = fbtt_core::proxy::health(session);
        panic!(
            "the proxy health was still {health:?} after {} seconds",
            HEALTH_DEADLINE.as_secs(),
        )
    })
}

/// A stand-in SOCKS5 proxy that offers username/password authentication (method 0x02) and then
/// refuses the credentials (RFC 1929 status 0x01), so the engine reports `LoginRefused`.
///
/// One plain thread accepts, and one per connection does the reads and writes the handshake is: the
/// client sends a greeting (version 5, then a length-prefixed list of methods, which the read follows
/// by its own length so the methods it offers beside username/password do not matter), this answers
/// `05 02` to say the method is agreed, the client sends the RFC 1929 sub-negotiation, and this
/// answers `01 01`: version 1, status 1, the credentials are not accepted. `Credentials the proxy
/// will not take` is a state the engine has to report, and this is the only way to make one without
/// a real proxy.
fn refusing_login_proxy(listener: TcpListener) {
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            std::thread::spawn(move || {
                // The greeting: version, then a length-prefixed list of methods. The engine offers
                // username/password (0x02) when the proxy has credentials, which is why this one
                // has them; the read is by the list's own length, so a client that offers no-auth
                // beside it is understood too.
                let mut greeting = [0u8; 2];
                if stream.read_exact(&mut greeting).is_err() || greeting[0] != 0x05 {
                    return;
                }
                let mut methods = vec![0u8; usize::from(greeting[1])];
                if stream.read_exact(&mut methods).is_err() {
                    return;
                }
                if !methods.contains(&0x02) {
                    return;
                }
                if stream.write_all(&[0x05, 0x02]).is_err() {
                    return;
                }
                // RFC 1929: version, then a length-prefixed user name and password.
                let mut head = [0u8; 2];
                if stream.read_exact(&mut head).is_err() || head[0] != 0x01 {
                    return;
                }
                let mut user = vec![0u8; usize::from(head[1])];
                if stream.read_exact(&mut user).is_err() {
                    return;
                }
                let mut length = [0u8; 1];
                if stream.read_exact(&mut length).is_err() {
                    return;
                }
                let mut pass = vec![0u8; usize::from(length[0])];
                if stream.read_exact(&mut pass).is_err() {
                    return;
                }
                // Status 1: the credentials are not accepted.
                let _ = stream.write_all(&[0x01, 0x01]);
            });
        }
    });
}

/// A proxy that is down when the engine starts does not stop it: the library opens, a snapshot
/// arrives, and the health settles on "unreachable" rather than the run failing. This is the rule
/// the setting lives or dies by - a proxy nobody can reach must not put the app in a state where it
/// gives up and connects directly instead.
#[tokio::test(flavor = "multi_thread")]
async fn engine_starts_when_the_proxy_is_down() {
    let lib = Library::new("proxy_down");
    let config = proxied(&lib, socks5(closed_port()));

    let (mut client, task, session) = start(&lib, config).await;

    // The engine is up and serving: the screen has a snapshot of this library, and nothing in it
    // came from a proxy that answered because none is listening.
    let snapshot = wait_for(&mut client, |snapshot| {
        matches!(snapshot.connection, Connection::Local)
    })
    .await;
    assert!(
        snapshot.torrents.is_empty(),
        "a fresh library has no torrents"
    );
    assert_eq!(
        session.network_settings().proxy.kind,
        fbt::ProxyKind::Socks5,
        "the session runs on the proxy the config names"
    );

    let health = health_becomes(&session, |health| {
        matches!(health, ProxyHealth::Unreachable { .. })
    })
    .await;
    let ProxyHealth::Unreachable { reason } = &health else {
        unreachable!("the wait above only returns unreachable health");
    };
    assert!(
        !reason.is_empty(),
        "the reason says why the proxy could not be reached: {health:?}"
    );
    assert_eq!(
        health.headline().as_deref(),
        Some(format!("Can't reach the proxy: {reason}").as_str()),
        "the one line a screen shows"
    );
    // The engine's own reason starts with its own "cannot reach the proxy: ", which this product's
    // headline also says; the line must carry the phrase once, not twice.
    assert_eq!(
        reason
            .to_ascii_lowercase()
            .matches("reach the proxy")
            .count(),
        0,
        "the engine's own wording is stripped from the reason: {reason:?}"
    );
    let headline = health.headline().expect("a line for an unreachable proxy");
    assert_eq!(
        headline
            .to_ascii_lowercase()
            .matches("reach the proxy")
            .count(),
        1,
        "the one line says it once: {headline}"
    );

    shutdown(client, task).await;
}

/// While a proxy is set, nothing connects directly to a peer: the backend is handed a seed's address
/// and a witness's, and offers both for three seconds. The seed - on loopback, listening, complete -
/// never sees a connection, nothing of its payload arrives, and the witness never accepts one either.
///
/// The witness is what makes a direct dial *visible* for the whole window: it is a plain listener
/// that accepts and answers nothing, so a connection to it stays open instead of completing a
/// handshake and hanging up in milliseconds. With a real peer only, a build that dialled directly
/// would finish the download and disconnect inside the window, and every read at the end of it would
/// look exactly like a build that never dialled at all.
#[tokio::test(flavor = "multi_thread")]
async fn no_direct_connection_while_proxy_is_set() {
    let lib = Library::new("proxy_no_direct");
    let seed_dir = lib.dir.path().join("seed");
    let seeded = write_file(
        &seed_dir,
        "payload.bin",
        &sample_for(MIB, "proxy_no_direct"),
    );
    let bytes = torrent_for(&seeded).await;
    let metainfo = fbt::Metainfo::from_bytes(&bytes).expect("the torrent we just made");
    let id = id_of(&metainfo.info_hashes);

    // The seed: a second session on loopback, holding the payload complete and listening.
    let seed = fbt::Session::new(loopback(&lib.config))
        .await
        .expect("a session to seed with");
    seed.add_torrent(fbt::AddTorrent::new(metainfo, seed_dir))
        .await
        .expect("seed the torrent");
    wait_until_seeding(&seed).await;
    let seed_addr = seed.local_addr();
    let seed_torrent = seed
        .torrent(&fbt::InfoHashes::from_v1(wire_of(&id)))
        .expect("the seed holds the torrent it added");

    // The witness: a listener that accepts and answers nothing, so a direct dial to it is still a
    // connection at the end of the window rather than a finished handshake nobody can see.
    let witness = TcpListener::bind("127.0.0.1:0").expect("a witness listener");
    witness
        .set_nonblocking(true)
        .expect("the witness never blocks a runtime thread");
    let witness_addr = witness.local_addr().expect("its address");

    // The backend under test, behind a proxy nothing is listening on.
    let config = proxied(&lib, socks5(closed_port()));
    let (mut client, task, session) = start(&lib, config).await;
    assert!(
        client.send(Command::Add {
            source: AddSource::File(bytes),
            dir: Some(lib.config.download_dir.clone()),
        }),
        "the queue takes the torrent"
    );
    wait_for(&mut client, |snapshot| row_of(snapshot, &id).is_some()).await;
    let torrent = session
        .torrent(&fbt::InfoHashes::from_v1(wire_of(&id)))
        .expect("the backend holds the torrent it added");

    // Offer both addresses, over and over, for the offer window: one attempt could be given up on
    // for a reason that has nothing to do with the proxy, and the claim is that *no* attempt goes
    // direct. Every connection the witness accepts is one this test refuses to allow.
    let mut accepted = 0usize;
    let started = Instant::now();
    within(async {
        while started.elapsed() < OFFER_WINDOW {
            torrent
                .add_peer(seed_addr)
                .await
                .expect("the seed is a peer");
            torrent
                .add_peer(witness_addr)
                .await
                .expect("the witness is a peer");
            accepted += drain(&witness);
            tokio::time::sleep(OFFER_EVERY).await;
        }
        // A last look, once the window is past, for a dial that crossed its end.
        tokio::time::sleep(OFFER_EVERY).await;
        accepted += drain(&witness);
    })
    .await;

    assert_eq!(
        accepted, 0,
        "the backend dialled a peer directly while a proxy was set"
    );
    let seen = seed_torrent
        .peers()
        .await
        .expect("the seed can list its peers");
    assert!(
        seen.is_empty(),
        "the seed saw a peer while the backend had a proxy set: {seen:?}"
    );
    let progress = row_of(&client.snapshot(), &id).map_or(1.0, |row| row.progress);
    assert_eq!(
        progress, 0.0,
        "the backend downloaded from the seed while a proxy was set"
    );

    shutdown(client, task).await;
    seed.shutdown().await;
}

/// Every connection the witness has accepted so far, without waiting for one: the listener is
/// non-blocking, so a round with nothing pending returns zero.
fn drain(witness: &TcpListener) -> usize {
    let mut accepted = 0;
    while let Ok((_stream, _addr)) = witness.accept() {
        accepted += 1;
    }
    accepted
}

/// A save never writes over a `config.yaml` whose `proxy:` the run cannot read: the limits a run
/// keeps in memory are not worth dropping the person's proxy line for, and a config left without it
/// would connect directly on the next start. The one line goes to the notice queue and the bytes
/// stay where they can be fixed.
#[tokio::test(flavor = "multi_thread")]
async fn a_limits_save_never_drops_a_proxy_it_cannot_read() {
    let lib = Library::new("proxy_save_refused");
    // The run starts on its own config, which names no proxy. The file it reads back when it saves
    // is the one written under it here, while the run is live.
    let written = "proxy:\n  type: socks4\n  host: proxy.example\n  port: 1080\n";
    let file = write_file(&lib.paths.config_dir, "config.yaml", written.as_bytes());
    let (mut client, task, _session) = start(&lib, lib.config.clone()).await;

    assert!(
        client.send(Command::SetLimits(LiveLimits {
            download_kib: 10,
            upload_kib: 0,
            active_downloads: None,
            active_seeds: None,
            active_total: None,
        })),
        "the queue takes the limits"
    );
    let line = within(async {
        loop {
            match client.notice().await {
                Some(Notice::Info(text) | Notice::Error(text)) => {
                    if text.contains("Unknown proxy type \"socks4\"") {
                        return text;
                    }
                }
                Some(other) => panic!("an unexpected notice: {other:?}"),
                None => panic!("the backend closed its notices"),
            }
        }
    })
    .await;
    assert!(
        line.contains("use none, http or socks5"),
        "the line names what to fix: {line}"
    );
    assert_eq!(
        std::fs::read_to_string(&file).expect("the config file still reads"),
        written,
        "the file was not written over"
    );

    shutdown(client, task).await;
}

/// A proxy that refuses the login is reported as exactly that, so a screen can say so rather than
/// leave a person guessing at "unreachable".
#[tokio::test(flavor = "multi_thread")]
async fn login_refused_is_reported() {
    let lib = Library::new("proxy_login_refused");
    let listener = TcpListener::bind("127.0.0.1:0").expect("a stand-in proxy");
    let port = listener.local_addr().expect("its address").port();
    refusing_login_proxy(listener);

    let config = proxied(
        &lib,
        ProxySetting {
            username: Some("alice".to_string()),
            password: Some("wrong-password".to_string()),
            ..socks5(port)
        },
    );
    let (client, task, session) = start(&lib, config).await;

    let health = health_becomes(&session, |health| *health == ProxyHealth::LoginRefused).await;
    assert_eq!(health, ProxyHealth::LoginRefused);
    assert_eq!(
        health.headline().as_deref(),
        Some("Can't reach the proxy: it refused the login"),
        "the one line a screen shows"
    );

    shutdown(client, task).await;
}

/// A proxy change as the Settings screen sends it: SOCKS5 at `127.0.0.1:port`, no login, the stored
/// password kept.
fn socks5_change(port: u16) -> ProxyChange {
    ProxyChange {
        kind: ProxyType::Socks5,
        host: "127.0.0.1".to_string(),
        port,
        username: None,
        password: PasswordChange::Keep,
    }
}

/// Sends one proxy save and returns the next notice's text, which is the save's answer.
async fn save_proxy(client: &mut Client, change: ProxyChange) -> Notice {
    assert!(
        client.send(Command::Settings(SettingsCommand::Save(
            SettingsChange::Proxy(change)
        ))),
        "the queue takes the save"
    );
    notice(client).await
}

/// A proxy saved on a running backend takes hold at once: the engine's health moves from `Off` to
/// "unreachable" (nothing listens on the port) without a restart, and the file holds the section. Saving
/// `none` takes it away again: the health goes back to `Off`, the switches the proxy turned off come back
/// to what the run started on, and the file has no `proxy:` key.
#[tokio::test(flavor = "multi_thread")]
async fn local_proxy_change_applies_without_restart() {
    let lib = Library::new("proxy_runtime");
    let (mut client, task, session) = start(&lib, lib.config.clone()).await;
    let started_on = session.network_settings();
    assert_eq!(fbtt_core::proxy::health(&session), ProxyHealth::Off);
    assert!(
        started_on.enable_incoming_tcp,
        "a run with no proxy takes incoming connections"
    );

    let answer = save_proxy(&mut client, socks5_change(closed_port())).await;
    assert_eq!(info_text(answer), "Saved.");
    health_becomes(&session, |health| {
        matches!(health, ProxyHealth::Unreachable { .. })
    })
    .await;
    let running = session.network_settings();
    assert_eq!(running.proxy.kind, fbt::ProxyKind::Socks5);
    assert!(
        !running.enable_incoming_tcp,
        "nothing connects in while a proxy is set"
    );
    let written = std::fs::read_to_string(lib.paths.config_file()).expect("the save wrote a file");
    assert!(written.contains("proxy:"), "{written}");
    assert!(written.contains("socks5"), "{written}");

    let none = ProxyChange {
        kind: ProxyType::None,
        host: String::new(),
        port: 0,
        username: None,
        password: PasswordChange::Keep,
    };
    assert_eq!(info_text(save_proxy(&mut client, none).await), "Saved.");
    health_becomes(&session, |health| *health == ProxyHealth::Off).await;
    assert_eq!(
        session.network_settings(),
        started_on,
        "the switches the proxy turned off are back to what the run started on"
    );
    let written = std::fs::read_to_string(lib.paths.config_file()).expect("the file is there");
    assert!(!written.contains("proxy:"), "{written}");

    shutdown(client, task).await;
}

/// A proxy change the checks refuse is answered with the line to show and changes nothing: not the file,
/// not the engine.
#[tokio::test(flavor = "multi_thread")]
async fn local_invalid_proxy_change_changes_nothing() {
    let lib = Library::new("proxy_runtime_invalid");
    let (mut client, task, session) = start(&lib, lib.config.clone()).await;
    let before = session.network_settings();

    let answer = save_proxy(
        &mut client,
        ProxyChange {
            host: String::new(),
            ..socks5_change(1080)
        },
    )
    .await;
    assert_eq!(
        error_text(answer),
        "Enter the proxy's host name or address."
    );
    assert!(
        !lib.paths.config_file().exists(),
        "a refused change writes nothing"
    );
    assert_eq!(session.network_settings(), before, "nor touches the engine");

    shutdown(client, task).await;
}

/// Keeping the stored password while naming another host or another port is refused with the line to
/// show, and nothing is written; a new user name at the same address keeps it.
#[tokio::test(flavor = "multi_thread")]
async fn local_keep_needs_the_same_address() {
    let lib = Library::new("proxy_runtime_keep");
    let proxy_port = closed_port();
    let config = proxied(
        &lib,
        ProxySetting {
            username: Some("bob".to_string()),
            password: Some("local-proxy-password".to_string()),
            ..socks5(proxy_port)
        },
    );
    let (mut client, task, session) = start(&lib, config).await;
    let before = session.network_settings();
    let keep = |host: &str, port: u16, user: &str| ProxyChange {
        username: Some(user.to_string()),
        host: host.to_string(),
        ..socks5_change(port)
    };
    let other_port = if proxy_port == u16::MAX {
        proxy_port - 1
    } else {
        proxy_port + 1
    };

    for change in [
        keep("127.0.0.2", proxy_port, "bob"),
        keep("127.0.0.1", other_port, "bob"),
    ] {
        let answer = save_proxy(&mut client, change).await;
        assert_eq!(
            error_text(answer),
            "Enter the password again when the proxy's host or port changes."
        );
        assert!(
            !lib.paths.config_file().exists(),
            "a refused change writes nothing"
        );
        assert_eq!(session.network_settings(), before, "nor touches the engine");
    }

    let answer = save_proxy(&mut client, keep("127.0.0.1", proxy_port, "carol")).await;
    assert_eq!(info_text(answer), "Saved.");
    let written = std::fs::read_to_string(lib.paths.config_file()).expect("the save wrote a file");
    assert!(written.contains("carol"), "{written}");
    assert!(
        written.contains("local-proxy-password"),
        "the password stayed: {written}"
    );
    assert_eq!(
        session.network_settings().proxy.username.as_deref(),
        Some("carol")
    );

    shutdown(client, task).await;
}

/// A proxy change the engine cannot switch to puts the previous setting back: the engine starts on an
/// HTTP proxy, which binds its BitTorrent port only to learn it and lets it go, and this test takes that
/// port. Removing the proxy needs the engine to listen there again, which the operating system refuses,
/// so the new network stack does not build and the engine stays on the HTTP proxy. The answer says so,
/// and the file, the view and the health still describe the HTTP proxy.
#[tokio::test(flavor = "multi_thread")]
async fn local_failed_apply_rolls_back() {
    let lib = Library::new("proxy_runtime_rollback");
    let proxy_port = closed_port();
    let http = ProxySetting {
        kind: ProxyType::Http,
        ..socks5(proxy_port)
    };
    let (mut client, task, session) = start(&lib, proxied(&lib, http)).await;
    let engine_port = session
        .local_addrs()
        .first()
        .expect("the engine bound its port")
        .port();
    let squatter = TcpListener::bind(("127.0.0.1", engine_port))
        .expect("the engine let its port go under an HTTP proxy");

    let none = ProxyChange {
        kind: ProxyType::None,
        host: String::new(),
        port: 0,
        username: None,
        password: PasswordChange::Keep,
    };
    let line = error_text(save_proxy(&mut client, none).await);
    assert!(
        line.starts_with(
            "The engine could not switch to this proxy, so the previous setting is kept: "
        ),
        "{line}"
    );

    let written = std::fs::read_to_string(lib.paths.config_file()).expect("the file is there");
    assert!(
        written.contains("proxy:"),
        "the file still names the proxy: {written}"
    );
    assert!(written.contains("http"), "{written}");
    assert!(
        client.send(Command::Settings(SettingsCommand::Load)),
        "the queue takes the load"
    );
    let snapshot = wait_for(&mut client, |snapshot| {
        matches!(snapshot.settings, fbtt_core::SettingsState::Loaded(_))
    })
    .await;
    let fbtt_core::SettingsState::Loaded(view) = &snapshot.settings else {
        unreachable!("the wait above only returns a loaded view");
    };
    assert_eq!(
        view.proxy.kind,
        ProxyType::Http,
        "the view still shows the proxy"
    );
    assert_eq!(session.network_settings().proxy.kind, fbt::ProxyKind::Http);
    assert_ne!(fbtt_core::proxy::health(&session), ProxyHealth::Off);

    drop(squatter);
    shutdown(client, task).await;
}
