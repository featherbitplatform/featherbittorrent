//! The server run and the two commands that make it usable by someone else: a
//! first-run wizard, `user`, and `device`. Each test drives the real binary against
//! a scratch library, with its own config and data directories.
//!
//! `serve` is tested through the debug-only switches (`--stop-file`, `--offline`)
//! that exist for exactly this: a test stops it by making a file instead of finding
//! its pid, and keeps every socket on loopback so a test run cannot reach the
//! network. A release build has neither switch, so those tests skip there.

use std::fs::{self, File};
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use fbtt_core::LibraryLock;

// The wizard and the exit code it reports are tested in-process too, so a wrong
// answer can be checked without starting a server.
#[path = "../src/exit.rs"]
#[allow(dead_code)]
mod exit;
#[path = "../src/wizard.rs"]
#[allow(dead_code)]
mod wizard;

/// An Ed25519 public key line, the kind `ssh-keygen -t ed25519` makes. Public
/// data, so it may live in a test.
const ED25519_KEY: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIIwtqQz7782qggJ1rY610dkccxudLbzX0ogqN5wu98zj alice@laptop";

/// An RSA public key line, of the kind this product refuses: it is what
/// `device_add_rejects_rsa` sends, and its comment says who it belongs to.
const RSA_KEY: &str = "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAACAQC0WRHtxuxefSJhpIxGq4ibGFgwYnESPm8C3JFM88A1JJLoprenklrd7VJ+VH3Ov/bQwZwLyRU5dRmfR/SWTtIPWs7tToJVayKKDB+/qoXmM5ui/0CU2U4rCdQ6PdaCJdC7yFgpPL8WexjWN06+eSIKYz1AAXbx9rRv1iasslK/KUqtsqzVliagI6jl7FPO2GhRZMcso6LsZGgSxuYf/Lp0D/FcBU8GkeOo1Sx5xEt8H8bJcErtCe4Blb8JxcW6EXO3sReb4z+zcR07gumPgFITZ6hDA8sSNuvo/AlWg0IKTeZSwHHVknWdQqDJ0uczE837caBxyTZllDNIGkBjCIIOFzuTT76HfYc/7CTTGk07uaNkUFXKN79xDiFOX8JQ1ZZMZvGOTwWjuT9CqgdTvQRORbRWwOYv3MH8re9ykw3Ip6lrPifY7s6hOaAKry/nkGPMt40m1TdiW98MTIpooE7W+WXu96ax2l2OJvxX8QR7l+LFlKnkIEEJd/ItF1G22UmOjkVwNASTwza/hlY+8DoVvEmwum/nMgH2TwQT3bTQzF9s9DOJkH4d8p4Mw4gEDjNx0EgUFA91ysCAeUMQQyIvuR8HXXa+VcvhOOO5mmBcVhxJ3qUOJTyDBsT0932Zb4mNtkxdigoVxu+iiwk0vwtvKwGVDYdyMP5EAQeEIP1t0w== bob@example";

/// How long a started server gets to answer, and a stopped one to exit. A cold
/// engine start plus an Argon2 hash is the slowest part of any of these runs.
const DEADLINE: Duration = Duration::from_secs(30);

/// The wizard's answers become a config file, on disk, in the shape the server
/// reads. The server itself then starts on that file and stops cleanly, which is
/// what makes this an end-to-end test of a first run rather than of a prompt.
#[test]
fn wizard_writes_config() {
    if !cfg!(debug_assertions) {
        eprintln!("wizard_writes_config: skipped - a release build has no --stop-file");
        return;
    }
    let scratch = Scratch::new("wizard_writes");
    // A port the OS picks, so this run cannot collide with a featherbittorrent the
    // person running the test already has up.
    let folder = scratch.dir.join("Downloads");
    let answers = format!(
        "{}\n0\n127.0.0.1:0\nyes\nalice\nhunter2\nhunter2\nno\n",
        folder.display()
    );

    let mut run = Run::start(&scratch, &["serve"], &answers, "wizard");
    let stderr = run.wait_for_stderr("listening on ");
    assert!(
        stderr.starts_with("listening on "),
        "the first log line says where it is listening: {stderr}"
    );
    let (code, stdout, _) = run.stop();
    assert_eq!(code, 0, "a stopped server exits cleanly");

    let written =
        fs::read_to_string(scratch.config_file()).expect("the wizard wrote a config file");
    assert!(
        written.contains(&format!("download_dir: {}", folder.display())),
        "the download folder the answer gave is in the file: {written}"
    );
    // The port and API address answers are `0`, which the file records as 0, and
    // the login answer is not: authentication on a loopback address is a choice
    // this product does not make by itself, so it has to be in the file.
    assert!(written.contains("listen_port: 0"), "{written}");
    assert!(written.contains("bind: 127.0.0.1:0"), "{written}");
    assert!(written.contains("enabled: true"), "{written}");
    assert!(written.contains("name: alice"), "{written}");
    assert!(
        written.contains("$argon2id$"),
        "the password went in as a hash, not in the clear: {written}"
    );
    assert!(
        !written.contains("ssh-ed25519"),
        "answering no to a device key leaves it out of the file: {written}"
    );

    for prompt in [
        "Download folder [",
        "BitTorrent port [6881]",
        "API address [127.0.0.1:8090]",
        "Require username and password",
        "User name (empty for none)",
        "Password [hidden]",
        "Repeat password",
        "Add a device key now? [no]",
    ] {
        assert!(
            stdout.contains(prompt),
            "the wizard asked {prompt:?}: {stdout}"
        );
    }
    assert!(
        stdout.contains(&scratch.config_file().display().to_string()),
        "the wizard says where it wrote the file: {stdout}"
    );
    assert!(
        stdout.contains("featherbittorrent device add"),
        "the wizard says how to add a key later: {stdout}"
    );
}

/// The same questions answered in-process, which is how the defaults get checked:
/// a blank answer has to leave this library's own defaults alone, and one bad pasted
/// key has to be reported and skipped rather than end the run.
#[test]
fn wizard_answers_become_config() {
    use std::io::Cursor;

    let base = fbtt_core::Config::default();

    // Every default, and no device key. On a loopback address that means: this
    // host's Downloads folder, port 6881, the API on 127.0.0.1:8090, and nobody
    // asked to sign in - with nothing recorded about it either way, because that is
    // what the server does for a loopback bind without being told.
    let mut input = Cursor::new("\n\n\n\nno\n".to_string());
    let mut out: Vec<u8> = Vec::new();
    let config = answered(&base, &mut input, &mut out);
    assert_eq!(
        config.download_dir, base.download_dir,
        "a blank download folder keeps this host's Downloads"
    );
    assert_eq!(config.listen_port, 6881, "the port default");
    assert_eq!(
        serving(&config).bind.to_string(),
        "127.0.0.1:8090",
        "the API address default"
    );
    assert_eq!(
        serving(&config).auth.enabled,
        None,
        "{:?}",
        serving(&config).auth
    );
    assert!(
        serving(&config).auth.users.is_empty(),
        "authentication off asks for no login"
    );

    // Away from loopback the default flips to yes, so a login is asked for - and a
    // password typed twice differently is retried instead of ending the run.
    let base = fbtt_core::Config {
        server: Some(fbtt_core::ServerConfig {
            bind: "10.0.0.5:8090".parse().expect("an address on this network"),
            ..Default::default()
        }),
        ..Default::default()
    };
    let mut input = Cursor::new("\n\n\n\nalice\nfirst\nsecond\nfirst\nfirst\nno\n".to_string());
    let mut out: Vec<u8> = Vec::new();
    let config = answered(&base, &mut input, &mut out);
    let said = String::from_utf8(out).expect("the prompts are text");
    assert!(
        said.contains("did not match"),
        "a password typed twice differently says so: {said}"
    );
    assert_eq!(
        serving(&config).auth.enabled,
        None,
        "yes was the default here, so nothing new is recorded"
    );
    let users = &serving(&config).auth.users;
    assert_eq!(users.len(), 1, "one login: {users:?}");
    assert_eq!(users[0].name, "alice", "the name as typed");
    assert!(
        users[0].password_hash.starts_with("$argon2id$"),
        "{}",
        users[0].password_hash
    );

    // An empty user name skips the login rather than inventing one, so nobody is
    // told a password until they ask for it - and they are told what to run next.
    let mut input = Cursor::new("\n\n\n\n\nno\n".to_string());
    let mut out: Vec<u8> = Vec::new();
    let config = answered(&base, &mut input, &mut out);
    let said = String::from_utf8(out).expect("the prompts are text");
    assert!(
        serving(&config).auth.users.is_empty(),
        "an empty name asks for no password and creates no login"
    );
    assert!(
        !said.contains("Password"),
        "skipping the login skips its password too: {said}"
    );
    assert!(
        said.contains("featherbittorrent user set"),
        "and says how to make one later: {said}"
    );

    // A key that is not one is reported, and the next line is tried: a person
    // pasting out of a terminal gets one go wrong before they get it right.
    let mut input = Cursor::new(format!(
        "\n\n\n\nbob\npw\npw\nyes\n{RSA_KEY}\n{ED25519_KEY}\n"
    ));
    let mut out: Vec<u8> = Vec::new();
    let config = answered(&base, &mut input, &mut out);
    let said = String::from_utf8(out).expect("the prompts are text");
    assert!(
        said.contains("ed25519"),
        "an RSA key line says what to use instead: {said}"
    );
    let devices = &serving(&config).auth.devices;
    assert_eq!(devices.len(), 1, "the good key went in: {devices:?}");
    assert_eq!(devices[0].public_key, ED25519_KEY);
    assert_eq!(
        devices[0].name, "alice@laptop",
        "the comment on the key names the device"
    );

    // A password keeps the spaces typed around it: they are part of what was typed,
    // and a client that sends them back has to be able to log in.
    let mut input = Cursor::new("\n\n\n\nalice\n hunter2 \n hunter2 \nno\n".to_string());
    let mut out: Vec<u8> = Vec::new();
    let config = answered(&base, &mut input, &mut out);
    let hash = &serving(&config).auth.users[0].password_hash;
    assert!(
        fbtt_server::auth::password::verify(hash, " hunter2 "),
        "the spaces that were typed are in the password that was hashed"
    );
    assert!(
        !fbtt_server::auth::password::verify(hash, "hunter2"),
        "so trimming them would have made a login nobody typed"
    );

    // A comment on the key names the device, unless it is not a name this product can
    // show and match - then the reason is said and the name asked for.
    let (key_body, _) = ED25519_KEY
        .split_once(" alice@laptop")
        .expect("the test key ends with its comment");
    let long_comment = format!("{key_body} {}", "workstation".repeat(7));
    let mut input = Cursor::new(format!(
        "\n\n\n\nbob\npw\npw\nyes\n{long_comment}\nwork laptop\n"
    ));
    let mut out: Vec<u8> = Vec::new();
    let config = answered(&base, &mut input, &mut out);
    let said = String::from_utf8(out).expect("the prompts are text");
    assert!(
        said.contains("not a usable name"),
        "a comment too long to be a name says why it was not used: {said}"
    );
    assert_eq!(
        serving(&config).auth.devices[0].name,
        "work laptop",
        "and the name given instead is what goes in the file"
    );

    // A bad answer is asked again, and a good one after it is taken.
    let base = fbtt_core::Config::default();
    let mut input = Cursor::new("/home/alice/Downloads\nnope\n7000\n\n\nno\n".to_string());
    let mut out: Vec<u8> = Vec::new();
    let config = answered(&base, &mut input, &mut out);
    let said = String::from_utf8(out).expect("the prompts are text");
    assert_eq!(
        config.listen_port, 7000,
        "the second answer is the one used"
    );
    assert_eq!(
        times(&said, "BitTorrent port"),
        2,
        "asked once, complained, asked again: {said}"
    );

    // Three bad answers to one question end the run rather than asking forever.
    let mut input = Cursor::new("/home/alice/Downloads\nnope\nnope\nnope\n".to_string());
    let mut out: Vec<u8> = Vec::new();
    let Err(problem) = wizard::run(&base, &mut input, &mut out, false) else {
        panic!("three answers that are not port numbers cannot make a config");
    };
    let said = String::from_utf8(out).expect("the prompts are text");
    assert_eq!(problem.code, 1);
    assert!(
        problem.message.contains("Gave up after 3 answers"),
        "{}",
        problem.message
    );
    assert!(
        problem.message.contains("port number"),
        "the complaint it gives up on is the last one it heard: {}",
        problem.message
    );
    assert_eq!(
        times(&said, "BitTorrent port"),
        3,
        "asked three times and no more: {said}"
    );
}

/// The server section a wizard run wrote. Coming through the wizard is choosing to serve - these are
/// the questions serving asks - so its answers always come back as a section.
fn serving(config: &fbtt_core::Config) -> &fbtt_core::ServerConfig {
    config
        .server
        .as_ref()
        .expect("the wizard asked the server questions")
}

/// How many times `needle` appears in `haystack`: how many times a question was asked.
fn times(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

/// Answers the wizard from `input` and says the run must have succeeded, so each
/// case below reads as the answer it is about rather than as error plumbing.
fn answered(
    base: &fbtt_core::Config,
    input: &mut dyn BufRead,
    out: &mut Vec<u8>,
) -> fbtt_core::Config {
    match wizard::run(base, input, out, false) {
        Ok(config) => config,
        Err(problem) => panic!("these answers should have been good: {}", problem.message),
    }
}

/// `user set` writes an Argon2id login, `user list` reads it back, and `user
/// remove` takes it out. A password typed twice differently changes nothing on disk.
#[test]
fn user_set_and_list() {
    let scratch = Scratch::new("user_set");
    let run = Run::start(
        &scratch,
        &["user", "set", "alice"],
        "hunter2\nhunter2\n",
        "set",
    );
    let (code, stdout, stderr) = run.wait(0);
    assert_eq!(code, 0, "a login is created: {stderr}");
    assert!(stdout.contains("Saved user alice"), "{stdout}");

    let written =
        fs::read_to_string(scratch.config_file()).expect("user set wrote the config file");
    assert!(written.contains("name: alice"), "{written}");
    assert!(
        written.contains("$argon2id$"),
        "the file holds a hash, not the password: {written}"
    );
    assert!(
        !written.contains("hunter2"),
        "the password itself is not in the file: {written}"
    );

    let run = Run::start(&scratch, &["user", "list"], "", "list");
    let (code, stdout, _) = run.wait(0);
    assert_eq!(code, 0);
    let lines: Vec<&str> = stdout.lines().filter(|line| !line.is_empty()).collect();
    assert_eq!(lines.len(), 1, "one login: {stdout}");
    assert!(lines[0].starts_with("alice "), "{stdout}");

    // The mismatch is caught before anything is written, so the file still holds
    // exactly what the first run put in it.
    let run = Run::start(
        &scratch,
        &["user", "set", "bob"],
        "nope\nother\n",
        "mismatch",
    );
    let (code, stdout, _) = run.wait(1);
    assert_eq!(code, 1, "a mismatched password is a problem");
    assert!(stdout.contains("did not match"), "{stdout}");

    let run = Run::start(&scratch, &["user", "list"], "", "after");
    let (_, stdout, _) = run.wait(0);
    assert!(
        !stdout.contains("bob"),
        "the name that failed to get a password was not saved: {stdout}"
    );

    // Three pairs typed differently end the run: a person who cannot get the two to
    // agree is told to start again rather than asked forever.
    let run = Run::start(
        &scratch,
        &["user", "set", "carol"],
        "a\nb\nc\nd\ne\nf\n",
        "three_pairs",
    );
    let (code, _, stderr) = run.wait(1);
    assert_eq!(code, 1, "three mismatched pairs is a problem");
    assert!(
        stderr.contains("Gave up after 3 answers"),
        "the giving-up line is the problem it reports: {stderr}"
    );

    // `user remove` takes the login out again.
    let run = Run::start(&scratch, &["user", "remove", "alice"], "", "remove");
    let (code, stdout, _) = run.wait(0);
    assert_eq!(code, 0, "a login is taken out: {stdout}");
    assert!(stdout.contains("Removed user alice"), "{stdout}");

    let run = Run::start(&scratch, &["user", "list"], "", "after_remove");
    let (_, stdout, _) = run.wait(0);
    assert_eq!(stdout.trim(), "No users.", "{stdout}");

    // A name that is not in the file is a problem, not a success that did nothing.
    let run = Run::start(&scratch, &["user", "remove", "alice"], "", "again");
    let (code, _, stderr) = run.wait(1);
    assert_eq!(code, 1, "removing a login that is not there says so");
    assert!(stderr.contains("No user named alice"), "{stderr}");
}

/// A device key is an Ed25519 line, and the message for anything else says what to
/// use instead. Nothing is written for a key this product cannot use.
#[test]
fn device_add_rejects_rsa() {
    let scratch = Scratch::new("device_rejects");
    let run = Run::start(&scratch, &["device", "add", "laptop", RSA_KEY], "", "rsa");
    let (code, _, stderr) = run.wait(1);
    assert_eq!(code, 1, "an RSA key is refused");
    assert!(
        stderr.contains("ed25519"),
        "the message names the kind to use: {stderr}"
    );
    assert!(
        !scratch.config_file().exists(),
        "a refused key writes no config file"
    );

    // The same command with a key it can use, and the list that reads it back.
    let run = Run::start(
        &scratch,
        &["device", "add", "laptop", ED25519_KEY],
        "",
        "ed",
    );
    let (code, stdout, stderr) = run.wait(0);
    assert_eq!(code, 0, "an Ed25519 key is registered: {stderr}");
    assert!(
        stdout.contains("Added device laptop (SHA256:"),
        "the answer names the device and its fingerprint: {stdout}"
    );

    let written =
        fs::read_to_string(scratch.config_file()).expect("device add wrote the config file");
    assert!(
        written.contains(ED25519_KEY),
        "the key line is stored as pasted: {written}"
    );
    assert!(written.contains("name: laptop"), "{written}");

    let run = Run::start(&scratch, &["device", "list"], "", "devices");
    let (code, stdout, _) = run.wait(0);
    assert_eq!(code, 0);
    assert!(stdout.contains("laptop  SHA256:"), "{stdout}");

    // A file holding the key works the same way, which is how a
    // `~/.ssh/id_ed25519.pub` path is accepted.
    let key_file = scratch.dir.join("id_ed25519.pub");
    fs::write(&key_file, format!("{ED25519_KEY}\n")).expect("write a key file");
    let run = Run::start(
        &scratch,
        &["device", "add", "desk", &key_file.display().to_string()],
        "",
        "file",
    );
    let (code, stdout, _) = run.wait(0);
    assert_eq!(code, 0, "a key file is read: {stdout}");

    // With no key argument, the line comes from stdin.
    let other = ED25519_KEY.replace("alice@laptop", "carol@desk");
    let run = Run::start(
        &scratch,
        &["device", "add", "phone"],
        format!("{other}\n").as_str(),
        "stdin",
    );
    let (code, stdout, _) = run.wait(0);
    assert_eq!(code, 0, "a key read from stdin is registered: {stdout}");
    assert!(stdout.contains("Added device phone"), "{stdout}");

    // `device remove` takes one out by the name it was added under.
    let run = Run::start(&scratch, &["device", "remove", "laptop"], "", "rm");
    let (code, stdout, _) = run.wait(0);
    assert_eq!(code, 0, "a device is taken out: {stdout}");
    assert!(stdout.contains("Removed device laptop"), "{stdout}");

    let run = Run::start(&scratch, &["device", "list"], "", "after_rm");
    let (_, stdout, _) = run.wait(0);
    assert!(
        !stdout.contains("laptop"),
        "the removed device is out of the list: {stdout}"
    );

    let run = Run::start(&scratch, &["device", "remove", "laptop"], "", "rm_again");
    let (code, _, stderr) = run.wait(1);
    assert_eq!(code, 1, "removing a device that is not there says so");
    assert!(stderr.contains("No device named laptop"), "{stderr}");

    // And a list of nothing says so rather than printing nothing.
    let empty = Scratch::new("device_none");
    let run = Run::start(&empty, &["device", "list"], "", "empty");
    let (_, stdout, _) = run.wait(0);
    assert_eq!(stdout.trim(), "No devices.", "{stdout}");

    let run = Run::start(&empty, &["user", "list"], "", "empty");
    let (_, stdout, _) = run.wait(0);
    assert_eq!(stdout.trim(), "No users.", "{stdout}");
}

/// A command that writes `config.yaml` refuses while a server or TUI holds the
/// library, because racing that file would lose one of them their change. Listing
/// reads nothing, so it answers anyway.
#[test]
fn admin_refuses_while_locked() {
    let scratch = Scratch::new("admin_locked");
    fs::write(scratch.config_file(), "server:\n  bind: 127.0.0.1:0\n")
        .expect("write a config file");
    // The guard outlives the children: the binary must find the lock held while it runs.
    let _held = LibraryLock::acquire(&scratch.lock_file()).expect("this test takes the lock first");

    let run = Run::start(
        &scratch,
        &["user", "set", "bob"],
        "hunter2\nhunter2\n",
        "locked_user",
    );
    let (code, _, stderr) = run.wait(3);
    assert_eq!(code, 3, "a held library is exit code 3: {stderr}");
    assert!(stderr.contains("in use"), "{stderr}");
    assert!(
        stderr.contains("connected TUI"),
        "the message offers the way that needs no restart: {stderr}"
    );

    let run = Run::start(
        &scratch,
        &["device", "add", "laptop", ED25519_KEY],
        "",
        "locked_device",
    );
    let (code, _, stderr) = run.wait(3);
    assert_eq!(code, 3, "the same rule for a device key: {stderr}");
    assert!(stderr.contains("in use"), "{stderr}");

    let written =
        fs::read_to_string(scratch.config_file()).expect("the config file is still there");
    assert!(
        !written.contains("alice")
            && !written.contains(ED25519_KEY.split_whitespace().nth(1).unwrap()),
        "a refused command changed nothing on disk: {written}"
    );

    let run = Run::start(&scratch, &["user", "list"], "", "locked_list");
    let (code, stdout, _) = run.wait(0);
    assert_eq!(
        code, 0,
        "a list reads nothing, so a held library does not stop it"
    );
    assert_eq!(stdout.trim(), "No users.", "{stdout}");
}

/// `serve` refuses a library something else is using *before* it asks anything: the wizard writes
/// `config.yaml`, and writing it while a TUI works from that file would replace what that run is
/// working from for nobody. Nothing is asked, nothing is written, and the answer is exit 3.
#[test]
fn serve_refuses_before_asking() {
    let scratch = Scratch::new("serve_before_asking");
    // No config file at all: this is the case where the wizard would have run.
    let _held = LibraryLock::acquire(&scratch.lock_file()).expect("this test takes the lock first");

    let run = Run::start(&scratch, &["serve"], "hunter2\n", "before_asking");
    let (code, stdout, stderr) = run.wait(3);
    assert_eq!(code, 3, "a held library is exit code 3: {stderr}");
    assert!(stderr.contains("already using this library"), "{stderr}");
    assert!(
        !stdout.contains("Download folder"),
        "no question was asked: {stdout}"
    );
    assert!(
        !scratch.config_file().exists(),
        "and no config file was written over the running library's"
    );
}

/// A `config.yaml` that names a proxy the app cannot use refuses the run: one line
/// saying what is wrong, exit 1, the file left where it is and no `.bad` beside it,
/// because starting on defaults would connect directly, which is the leak this
/// setting exists to close.
#[test]
fn serve_refuses_a_config_whose_proxy_is_unusable() {
    let scratch = Scratch::new("serve_bad_proxy");
    fs::write(
        scratch.config_file(),
        "proxy:\n  type: socks5\n  host: \"\"\n  port: 1080\n",
    )
    .expect("write a config that names an unusable proxy");

    let run = Run::start(&scratch, &["serve"], "", "bad_proxy");
    let (code, stdout, stderr) = run.wait(1);
    assert_eq!(code, 1, "an unusable proxy is refused: {stderr}");
    assert!(
        stderr.contains("Enter the proxy's host name or address."),
        "the line names the problem: {stderr}"
    );
    assert!(
        !stdout.contains("Download folder"),
        "the wizard never ran: {stdout}"
    );
    assert!(
        scratch.config_file().exists(),
        "the config file is left in place"
    );
    assert!(
        !scratch.config_dir.join("config.yaml.bad").exists(),
        "nothing was set aside"
    );
}

/// A server run starts on the config file, answers over HTTP, and stops on its own
/// terms: exit code 0, the library released for whatever starts next.
#[test]
fn serve_stops_cleanly() {
    if !cfg!(debug_assertions) {
        eprintln!("serve_stops_cleanly: skipped - a release build has no --stop-file");
        return;
    }
    let scratch = Scratch::new("serve_stops");
    // Both ports 0, so this run cannot collide with anything else on this machine.
    fs::write(
        scratch.config_file(),
        "listen_port: 0\nserver:\n  bind: 127.0.0.1:0\n",
    )
    .expect("write a config file");

    let mut run = Run::start(&scratch, &["serve"], "", "stops");
    let stderr = run.wait_for_stderr("listening on ");
    let first = stderr.lines().next().unwrap_or_default();
    assert!(
        first.starts_with("listening on 127.0.0.1:"),
        "the first log line is the address, and nothing else came before it: {stderr}"
    );
    let address = first
        .strip_prefix("listening on ")
        .expect("the line starts with what it must")
        .trim()
        .to_string();

    // The API answers on that address. With no users in the file and a loopback
    // bind, this server asks nobody to sign in, so the torrent list is open.
    let status = http_get(&address, "/api/v1/torrents");
    assert_eq!(status, 200, "the API answers on the address it logged");

    let (code, _, stderr) = run.stop();
    assert_eq!(code, 0, "a stopped server exits 0: {stderr}");
    assert!(
        stderr.contains("stopping"),
        "it says it is stopping before it does: {stderr}"
    );

    // The last save ran, which is what a stop is for: the resume file is there.
    assert!(
        scratch.data_dir.join("session.state").exists(),
        "a stopped server left its resume data behind"
    );

    // The library is free again: the whole point of an ordered shutdown.
    let released = LibraryLock::acquire(&scratch.lock_file());
    assert!(released.is_ok(), "the lock went with the server");
}

/// `serve` on a library something else is using says who, and leaves without
/// opening an engine on somebody else's resume data.
#[test]
fn serve_refuses_while_locked() {
    let scratch = Scratch::new("serve_locked");
    fs::write(scratch.config_file(), "server:\n  bind: 127.0.0.1:0\n")
        .expect("write a config file");
    let _held = LibraryLock::acquire(&scratch.lock_file()).expect("this test takes the lock first");

    let run = Run::start(&scratch, &["serve"], "", "locked");
    let (code, _, stderr) = run.wait(3);
    assert_eq!(code, 3, "a held library is exit code 3: {stderr}");
    assert!(stderr.contains("already using this library"), "{stderr}");
    assert!(
        stderr.contains(&scratch.data_dir.display().to_string()),
        "the message names the library: {stderr}"
    );
    assert!(
        !stderr.contains("listening on "),
        "nothing was served, so nothing is listening: {stderr}"
    );
}

/// An HTTP request to this run's own API, answered with its status code. A test
/// that wants the body reads it from the socket itself.
fn http_get(address: &str, path: &str) -> u16 {
    let mut socket = TcpStream::connect(address).expect("the server accepts a connection");
    socket
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("a read timeout");
    write!(
        socket,
        "GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"
    )
    .expect("write a request");
    let mut reader = BufReader::new(socket);
    let mut status_line = String::new();
    reader.read_line(&mut status_line).expect("a status line");
    let code = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .expect("a status code in the status line");
    // The body is read out so the request is finished before the socket goes.
    let mut rest = String::new();
    let _ = reader.read_to_string(&mut rest);
    code
}

/// A scratch library: its own config and data directories, removed at the end.
struct Scratch {
    dir: PathBuf,
    config_dir: PathBuf,
    data_dir: PathBuf,
}

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "featherbittorrent-serve-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or(0),
        ));
        let scratch = Scratch {
            config_dir: dir.join("config"),
            data_dir: dir.join("data"),
            dir,
        };
        fs::create_dir_all(&scratch.config_dir).expect("make the scratch config directory");
        scratch
    }

    fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.yaml")
    }

    fn lock_file(&self) -> PathBuf {
        self.data_dir.join("library.lock")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // Best effort, and a directory already gone is not a failure.
        match fs::remove_dir_all(&self.dir) {
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            other => other.unwrap(),
        }
    }
}

/// One started `featherbittorrent`, with stdout and stderr going to files: a server
/// run has no end, so its output cannot sit in a pipe waiting to be drained.
struct Run {
    child: Child,
    stdout: PathBuf,
    stderr: PathBuf,
    /// The file this run was told to watch for its stop, when it was started as a
    /// `serve` run in a debug build.
    stop_file: PathBuf,
}

impl Run {
    /// Starts the binary on this scratch library with `args`, `answers` piped in as
    /// stdin (and closed, so a prompt that reads gets an end), and the debug-only
    /// switches it needs to run offline and be stopped by a file.
    fn start(scratch: &Scratch, args: &[&str], answers: &str, tag: &str) -> Run {
        let stdout = scratch.dir.join(format!("{tag}-stdout.txt"));
        let stderr = scratch.dir.join(format!("{tag}-stderr.txt"));
        let stop_file = scratch.dir.join(format!("{tag}-stop"));
        let mut command = Command::new(env!("CARGO_BIN_EXE_featherbittorrent"));
        command
            .env("FBTT_CONFIG_DIR", &scratch.config_dir)
            .env("FBTT_DATA_DIR", &scratch.data_dir)
            .stdin(Stdio::piped())
            .stdout(File::create(&stdout).expect("open a file for stdout"))
            .stderr(File::create(&stderr).expect("open a file for stderr"));
        let args: Vec<String> =
            if cfg!(debug_assertions) && args.first().is_some_and(|first| *first == "serve") {
                vec![
                    "serve".to_string(),
                    "--offline".to_string(),
                    "--stop-file".to_string(),
                    stop_file.display().to_string(),
                ]
            } else {
                args.iter().map(|arg| arg.to_string()).collect()
            };
        let mut child = command
            .args(&args)
            .spawn()
            .expect("start featherbittorrent");
        // Writing the answers and dropping the pipe is what closes stdin: a prompt
        // that reaches the end of the answers sees an end of input, like `echo | ...`.
        let mut stdin = child.stdin.take().expect("a piped stdin");
        stdin
            .write_all(answers.as_bytes())
            .expect("write the answers");
        drop(stdin);
        Run {
            child,
            stdout,
            stderr,
            stop_file,
        }
    }

    /// Waits until stderr holds `needle`, and says what the run did instead if it
    /// exited before getting there.
    fn wait_for_stderr(&mut self, needle: &str) -> String {
        let until = Instant::now() + DEADLINE;
        loop {
            let said = read(&self.stderr);
            if said.contains(needle) {
                return said;
            }
            match self.child.try_wait() {
                Ok(Some(status)) => panic!(
                    "the run exited with {status} before saying {needle:?}; it said:\n{}",
                    report(&self.stdout, &self.stderr)
                ),
                Ok(None) => {}
                Err(error) => panic!("cannot wait for the run: {error}"),
            }
            if Instant::now() > until {
                panic!(
                    "no {needle:?} within {}s; it said:\n{}",
                    DEADLINE.as_secs(),
                    report(&self.stdout, &self.stderr)
                );
            }
            thread::sleep(Duration::from_millis(25));
        }
    }

    /// Makes the stop file and waits for the run to end on its own. Only for a
    /// `serve` run, which is the one started with `--stop-file`.
    fn stop(self) -> (i32, String, String) {
        fs::write(&self.stop_file, "").expect("make the stop file");
        self.wait(0)
    }

    /// Waits for the run to exit and checks its code against `expected`.
    fn wait(mut self, expected: i32) -> (i32, String, String) {
        let until = Instant::now() + DEADLINE;
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    let code = status.code().unwrap_or(-1);
                    let (stdout, stderr) = self.output();
                    if code != expected {
                        panic!(
                            "expected exit code {expected}, got {code}; stdout:\n{stdout}\nstderr:\n{stderr}"
                        );
                    }
                    return (code, stdout, stderr);
                }
                Ok(None) => {}
                Err(error) => panic!("cannot wait for the run: {error}"),
            }
            if Instant::now() > until {
                let _ = self.child.kill();
                let (stdout, stderr) = self.output();
                panic!(
                    "still running after {}s; stdout:\n{stdout}\nstderr:\n{stderr}",
                    DEADLINE.as_secs()
                );
            }
            thread::sleep(Duration::from_millis(25));
        }
    }

    fn output(&self) -> (String, String) {
        (read(&self.stdout), read(&self.stderr))
    }
}

/// A file's contents so far, with a missing file reading as nothing yet written.
fn read(path: &Path) -> String {
    match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == ErrorKind::NotFound => String::new(),
        Err(error) => panic!("cannot read {}: {error}", path.display()),
    }
}

/// Everything a run has said, for a failure message that does not need re-running.
fn report(stdout: &Path, stderr: &Path) -> String {
    format!("stdout:\n{}\nstderr:\n{}", read(stdout), read(stderr))
}
