//! The first-run setup wizard.
//!
//! Answers come from a `BufRead` and go to a `Write`, so a test can drive the
//! whole conversation in memory while `main` hands in stdin and stdout. Passwords
//! use [`rpassword`] when input is a real terminal and are read as an ordinary
//! line otherwise, which is what lets a script or a test answer them.
//!
//! The wizard only answers questions: it returns the config they describe and the
//! caller writes the file, so nothing is on disk until every answer is good. An
//! answer a question cannot use is said out loud and asked again, up to
//! [`MAX_TRIES`] times; then the run ends with the complaint it last heard rather
//! than writing a config nobody agreed to.

use std::io::{BufRead, Write};
use std::net::SocketAddr;

use crate::exit::Failure;
use fbtt_core::Config;
use fbtt_server::auth::keys::{fingerprint, parse_device_key};
use fbtt_server::auth::password::hash;

/// How long a user or device name may be, matching the limit the API puts on the
/// names it shows: one line wherever it is drawn.
const MAX_NAME_CHARS: usize = 64;

/// How many answers to one question the wizard will listen for before it gives up.
/// Three is enough to fix a typo or re-paste a key that came out wrong, and few
/// enough that somebody who cannot answer is not stuck in a loop nobody mentioned.
const MAX_TRIES: usize = 3;

/// Asks the questions in order and returns the config they describe.
///
/// `base` is what the answers are put on top of: this library's defaults for a
/// first run, or the config just read from the file when only its server section
/// is missing. Nothing else in it is touched.
///
/// `secret_input` says whether stdin is a terminal: passwords are hidden with
/// [`rpassword`] when it is and read as ordinary lines when it is not.
pub fn run(
    base: &Config,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    secret_input: bool,
) -> Result<Config, Failure> {
    // Every bracketed default below is the answer this config already gives, so
    // taking a default never changes anything.
    let mut config = base.clone();

    // 1. Download folder. Any text is a folder here, including one that does not
    //    exist yet: whoever writes into it makes room, and a folder nobody can
    //    create is something the server itself says about, with the operating
    //    system's reason in its own line.
    let default_folder = config.download_dir.display().to_string();
    let folder = ask(
        input,
        out,
        &format!("Download folder [{default_folder}]: "),
        &default_folder,
    )?;
    config.download_dir = folder.into();

    // 2. BitTorrent port. Zero is an answer here and everywhere else in this
    //    product: it means the system picks one, which is what a config file on a
    //    shared machine uses to avoid arguing with another run over a port.
    config.listen_port = ask_checked(input, out, "BitTorrent port [6881]: ", "6881", |answer| {
        answer.parse::<u16>().map_err(|_| {
            format!("{answer:?} is not a port number like 6881 (press enter for the default)")
        })
    })?;

    // 3. API address.
    let default_addr = config.server.bind.to_string();
    config.server.bind = ask_checked(
        input,
        out,
        &format!("API address [{default_addr}]: "),
        &default_addr,
        |answer| {
            answer.parse::<SocketAddr>().map_err(|_| {
                format!(
                    "{answer:?} is not an address and port like 127.0.0.1:8090 \
                     (press enter for the default)"
                )
            })
        },
    )?;

    // 4. Authentication, on by default away from loopback: that is what the server
    //    does with no `auth.enabled` either, so taking the default here leaves the
    //    field unnamed instead of writing a value that says nothing new.
    let default_yes = !config.server.bind.ip().is_loopback();
    let default_answer = if default_yes { "yes" } else { "no" };
    let auth_on = ask_checked(
        input,
        out,
        &format!("Require username and password for non-local devices? [{default_answer}]: "),
        default_answer,
        |answer| {
            answer_is_yes(answer)
                .ok_or("Answer yes or no (press enter for the default).".to_string())
        },
    )?;
    if auth_on != default_yes {
        config.server.auth.enabled = Some(auth_on);
    }

    // 5. The first login, when authentication is on.
    if auth_on {
        match ask_name(input, out, "User name (empty for none): ")? {
            // A server with no logins and no device keys asks everyone to sign in
            // to nothing, so skipping is worth saying out loud.
            None => writeln!(
                out,
                "No login yet. Create one with: featherbittorrent user set"
            )?,
            Some(name) => {
                let password = ask_new_password(input, out, secret_input)?;
                config.server.auth.users.push(fbtt_core::UserEntry {
                    name,
                    password_hash: hash(&password),
                });
            }
        }
    }

    // 6. An optional device key.
    let add_device = ask_checked(input, out, "Add a device key now? [no]: ", "no", |answer| {
        answer_is_yes(answer).ok_or("Answer yes or no (press enter for the default).".to_string())
    })?;
    if add_device {
        let line = ask_device_key(input, out)?;
        if !line.is_empty() {
            // The comment on a key is usually the name the person already gave that
            // machine, so it is used as the device name when it is one this product
            // can show and match exactly. When it is not - a comment 90 characters
            // long, or one with a tab in it - they are told why it was not used and
            // asked for a name of their own.
            let comment = line
                .split_whitespace()
                .skip(2)
                .collect::<Vec<_>>()
                .join(" ");
            let name = match accept_name(&comment) {
                Ok(name) => name,
                Err(problem) => {
                    // An empty comment is not a rejected answer: there was no name to
                    // reject, so the question just gets asked.
                    if !comment.is_empty() {
                        writeln!(
                            out,
                            "The comment on that key is not a usable name: {problem}"
                        )?;
                    }
                    ask_device_name(input, out, &line)?
                }
            };
            config.server.auth.devices.push(fbtt_core::DeviceEntry {
                name,
                public_key: line,
            });
        }
    }

    Ok(config)
}

/// Reads a line. A blank answer, or end of input, takes `default`.
pub(crate) fn ask(
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    prompt: &str,
    default: &str,
) -> Result<String, Failure> {
    write!(out, "{prompt}")?;
    out.flush()?;
    let mut line = String::new();
    if input.read_line(&mut line)? == 0 {
        return Ok(default.to_string());
    }
    let answer = trimmed(&line);
    if answer.is_empty() {
        return Ok(default.to_string());
    }
    Ok(answer)
}

/// Asks `prompt` until `check` accepts the answer, then returns what it made of it.
///
/// A complaint is printed and the question asked again, up to [`MAX_TRIES`] times.
/// The default is always accepted without being checked, because every default here
/// is a value this product already uses, so taking it cannot be wrong.
fn ask_checked<T>(
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    prompt: &str,
    default: &str,
    check: impl Fn(&str) -> Result<T, String>,
) -> Result<T, Failure> {
    let mut problem = String::new();
    for _ in 0..MAX_TRIES {
        let answer = ask(input, out, prompt, default)?;
        match check(&answer) {
            Ok(value) => return Ok(value),
            Err(complaint) => {
                writeln!(out, "{complaint}")?;
                problem = complaint;
            }
        }
    }
    Err(gave_up(&problem))
}

/// Reads a password: hidden through `rpassword` on a terminal, an ordinary line
/// otherwise. Spaces are part of the password and stay in it; only the line ending
/// a read hands along is dropped. Both call sites need a password, so an input that
/// ends before one arrives is a problem to report rather than a blank to keep asking
/// for.
pub fn ask_password(
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    secret_input: bool,
    prompt: &str,
) -> Result<String, Failure> {
    if secret_input {
        return match rpassword::prompt_password(prompt) {
            Ok(line) => Ok(without_line_ending(&line)),
            Err(problem) => Err(Failure::problem(format!(
                "Could not read the password: {problem}"
            ))),
        };
    }
    write!(out, "{prompt}")?;
    out.flush()?;
    let mut line = String::new();
    if input.read_line(&mut line)? == 0 {
        return Err(Failure::problem(
            "The password prompt got no input (stdin is closed).",
        ));
    }
    Ok(without_line_ending(&line))
}

/// The password question, asked as a pair: what was typed has to be given twice and
/// may not be empty. [`MAX_TRIES`] pairs, then the run gives up. Shared with `admin.rs`
/// so that `user set` asks for a password in exactly the way the wizard does.
pub(crate) fn ask_new_password(
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    secret_input: bool,
) -> Result<String, Failure> {
    let mut problem = String::new();
    for _ in 0..MAX_TRIES {
        let first = ask_password(input, out, secret_input, "Password [hidden]: ")?;
        if first.is_empty() {
            problem = "The password must not be empty.".to_string();
            writeln!(out, "{problem}")?;
            continue;
        }
        let again = ask_password(input, out, secret_input, "Repeat password [hidden]: ")?;
        if first != again {
            problem = "The passwords did not match, try again.".to_string();
            writeln!(out, "{problem}")?;
            continue;
        }
        return Ok(first);
    }
    Err(gave_up(&problem))
}

/// Reads the device key line. A blank answer, or an input that ends, means no device
/// and returns an empty `String`; a line that is not an Ed25519 key is reported and
/// asked again, [`MAX_TRIES`] times. Surrounding spaces are not part of a key.
fn ask_device_key(input: &mut dyn BufRead, out: &mut dyn Write) -> Result<String, Failure> {
    let mut problem = String::new();
    for _ in 0..MAX_TRIES {
        write!(out, "Device public key (ssh-ed25519 line): ")?;
        out.flush()?;
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            return Ok(String::new());
        }
        let line = trimmed(&line);
        if line.is_empty() {
            return Ok(String::new());
        }
        match parse_device_key(&line) {
            Ok(_) => return Ok(line),
            Err(complaint) => {
                problem = complaint.to_string();
                writeln!(out, "{problem}")?;
            }
        }
    }
    Err(gave_up(&problem))
}

/// Reads a name that may be left out: a blank line, or an input that ends, is
/// `None`. Anything else has to be a name this product can show and match exactly,
/// so a complaint is printed and the question asked again, [`MAX_TRIES`] times.
fn ask_name(
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    prompt: &str,
) -> Result<Option<String>, Failure> {
    let mut problem = String::new();
    for _ in 0..MAX_TRIES {
        write!(out, "{prompt}")?;
        out.flush()?;
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let name = trimmed(&line);
        if name.is_empty() {
            return Ok(None);
        }
        match accept_name(&name) {
            Ok(name) => return Ok(Some(name)),
            Err(complaint) => {
                problem = complaint;
                writeln!(out, "{problem}")?;
            }
        }
    }
    Err(gave_up(&problem))
}

/// The name question `user set` asks when the command line did not give one. The
/// bracketed default means an empty answer is an answer.
pub(crate) fn ask_user_name(
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<String, Failure> {
    ask_checked(input, out, "User name [alice]: ", "alice", accept_name)
}

/// A key with no usable comment has nothing to name itself, so the person gives it
/// one. The suggestion is the fingerprint's first characters, which is unique enough
/// to tell two devices apart and short enough to be a name.
fn ask_device_name(
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    key_line: &str,
) -> Result<String, Failure> {
    let suggested = match parse_device_key(key_line) {
        Ok(key) => format!("device-{}", short(&fingerprint(&key))),
        Err(_) => "device".to_string(),
    };
    ask_checked(
        input,
        out,
        &format!("Device name [{suggested}]: "),
        &suggested,
        accept_name,
    )
}

/// What is wrong with `name` as a user or device name: the API compares names exactly
/// and shows them on one line, so they may not be empty, too long, or contain
/// something invisible. Shared with `admin.rs`, which asks the same questions from a
/// command line and registers the same kinds of entry.
pub(crate) fn name_problem(name: &str) -> Option<String> {
    if name.is_empty() {
        return Some("A name is at least 1 character.".to_string());
    }
    let chars = name.chars().count();
    if chars > MAX_NAME_CHARS {
        return Some(format!(
            "A name is at most {MAX_NAME_CHARS} characters, this one has {chars}."
        ));
    }
    if name.chars().any(|c| c.is_control()) {
        return Some("A name may not contain a control character.".to_string());
    }
    None
}

/// The name check in the shape `ask_checked` wants: the name back when there is nothing to say about it.
fn accept_name(name: &str) -> Result<String, String> {
    match name_problem(name) {
        None => Ok(name.to_string()),
        Some(problem) => Err(problem),
    }
}

/// The first few characters of a fingerprint (`SHA256:<base64>`), enough to tell two
/// devices apart in a suggested name.
pub(crate) fn short(fingerprint: &str) -> String {
    match fingerprint.find(':') {
        Some(colon) => fingerprint[colon + 1..].chars().take(8).collect(),
        None => fingerprint.chars().take(8).collect(),
    }
}

/// Drops the line ending and any spaces around it. For an answer, a name or a key,
/// surrounding spaces are how text arrives from a paste or a file rather than part of
/// what was typed. A password is the one answer that keeps them (see `ask_password`).
fn trimmed(line: &str) -> String {
    line.trim_end_matches(['\r', '\n']).trim().to_string()
}

/// Drops the line ending and nothing else: for a password, every other character the
/// person typed is part of it, including the spaces at either end.
fn without_line_ending(line: &str) -> String {
    line.trim_end_matches(['\r', '\n']).to_string()
}

/// The problem a question ends on when [`MAX_TRIES`] answers in a row were not usable:
/// the complaint it last heard, and the fact that asking stopped.
fn gave_up(problem: &str) -> Failure {
    Failure::problem(format!(
        "Gave up after {MAX_TRIES} answers. The last one: {problem}"
    ))
}

/// `yes`/`y`/`no`/`n`, in either case. `None` for anything else, which is how a
/// question with a bracketed default says "that is not an answer".
fn answer_is_yes(answer: &str) -> Option<bool> {
    match answer.to_ascii_lowercase().as_str() {
        "yes" | "y" => Some(true),
        "no" | "n" => Some(false),
        _ => None,
    }
}
