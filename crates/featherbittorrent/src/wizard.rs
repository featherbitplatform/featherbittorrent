//! The first-run setup wizard.
//!
//! Answers come from a `BufRead` and go to a `Write`, so a test can drive the
//! whole conversation in memory while `main` hands in stdin and stdout. Passwords
//! use [`rpassword`] when input is a real terminal and are read as an ordinary
//! line otherwise, which is what lets a script or a test answer them.
//!
//! The wizard only answers questions: it returns the config they describe and the
//! caller writes the file, so nothing is on disk until every answer is good.

use std::io::{BufRead, Write};
use std::net::SocketAddr;

use crate::exit::Failure;
use fbtt_core::Config;
use fbtt_server::auth::keys::{fingerprint, parse_device_key};
use fbtt_server::auth::password::hash;

/// How long a user name may be, matching the limit the API puts on the names it
/// shows: one line wherever it is drawn.
const MAX_NAME_CHARS: usize = 64;

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

    // 1. Download folder.
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
    let port_text = ask(input, out, "BitTorrent port [6881]: ", "6881")?;
    match port_text.parse::<u16>() {
        Ok(port) => config.listen_port = port,
        Err(_) => {
            return Err(Failure::problem(format!(
                "{port_text:?} is not a port number like 6881 \
                 (press enter for the default)"
            )));
        }
    }

    // 3. API address.
    let default_addr = config.server.bind.to_string();
    let addr_text = ask(
        input,
        out,
        &format!("API address [{default_addr}]: "),
        &default_addr,
    )?;
    match addr_text.parse::<SocketAddr>() {
        Ok(bind) => config.server.bind = bind,
        Err(_) => {
            return Err(Failure::problem(format!(
                "{addr_text:?} is not an address and port like 127.0.0.1:8090 \
                 (press enter for the default)"
            )));
        }
    }

    // 4. Authentication, on by default away from loopback: that is what the
    //    server does with no `auth.enabled` either, so taking the default here
    //    leaves the field unnamed instead of writing a value that says nothing new.
    let default_yes = !config.server.bind.ip().is_loopback();
    let auth_prompt = format!(
        "Require username and password for non-local devices? [{}]: ",
        if default_yes { "yes" } else { "no" }
    );
    let answer = ask(
        input,
        out,
        &auth_prompt,
        if default_yes { "yes" } else { "no" },
    )?;
    let Some(auth_on) = answer_is_yes(&answer) else {
        return Err(Failure::problem(
            "Answer yes or no (press enter for the default).",
        ));
    };
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
                let password = loop {
                    let first = ask_password(input, out, secret_input, "Password [hidden]: ")?;
                    if first.is_empty() {
                        writeln!(out, "The password must not be empty.")?;
                        continue;
                    }
                    let again =
                        ask_password(input, out, secret_input, "Repeat password [hidden]: ")?;
                    if first != again {
                        writeln!(out, "The passwords did not match, try again.")?;
                        continue;
                    }
                    break first;
                };
                config.server.auth.users.push(fbtt_core::UserEntry {
                    name,
                    password_hash: hash(&password),
                });
            }
        }
    }

    // 6. An optional device key.
    let answer = ask(input, out, "Add a device key now? [no]: ", "no")?;
    let Some(add_device) = answer_is_yes(&answer) else {
        return Err(Failure::problem(
            "Answer yes or no (press enter for the default).",
        ));
    };
    if add_device {
        let line = loop {
            write!(out, "Device public key (ssh-ed25519 line): ")?;
            out.flush()?;
            let mut line = String::new();
            if input.read_line(&mut line)? == 0 {
                break String::new(); // nothing more to read: skip the device
            }
            let line = line.trim().to_string();
            if line.is_empty() {
                break line;
            }
            match parse_device_key(&line) {
                Ok(_) => break line,
                Err(problem) => writeln!(out, "{problem}")?,
            }
        };
        if !line.is_empty() {
            let comment = line
                .split_whitespace()
                .skip(2)
                .collect::<Vec<_>>()
                .join(" ");
            let name = match comment.is_empty() {
                false => comment,
                true => ask_device_name(input, out, &line)?,
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
    let answer = line.trim().to_string();
    if answer.is_empty() {
        return Ok(default.to_string());
    }
    Ok(answer)
}

/// Reads a password: hidden through `rpassword` on a terminal, an ordinary line
/// otherwise. Both call sites need a password, so an input that ends before one
/// arrives is a problem to report rather than a blank to keep asking for.
pub fn ask_password(
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    secret_input: bool,
    prompt: &str,
) -> Result<String, Failure> {
    if secret_input {
        return match rpassword::prompt_password(prompt) {
            Ok(line) => Ok(trimmed(&line)),
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
    Ok(trimmed(&line))
}

/// Reads a name that may be left out: a blank line, or an input that ends, is
/// `None`. Anything else has to be a name this product can show and match exactly,
/// so a complaint is printed and the question asked again.
fn ask_name(
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    prompt: &str,
) -> Result<Option<String>, Failure> {
    loop {
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
        match name_problem(&name) {
            None => return Ok(Some(name)),
            Some(problem) => writeln!(out, "{problem}")?,
        }
    }
}

/// A key with no comment has nothing to name itself, so the person gives it one.
fn ask_device_name(
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    key_line: &str,
) -> Result<String, Failure> {
    let suggested = match parse_device_key(key_line) {
        Ok(key) => format!("device-{}", short(&fingerprint(&key))),
        Err(_) => "device".to_string(),
    };
    loop {
        let candidate = ask(
            input,
            out,
            &format!("Device name [{suggested}]: "),
            &suggested,
        )?;
        match name_problem(&candidate) {
            None => return Ok(candidate),
            Some(problem) => writeln!(out, "{problem}")?,
        }
    }
}

/// What is wrong with `name` as a user or device name: the API compares names
/// exactly and shows them on one line, so they may not be empty, too long, or
/// contain something invisible. Shared with `admin.rs`, which asks the same
/// questions from a command line.
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

/// The first few characters of a fingerprint (`SHA256:<base64>`), enough to tell
/// two devices apart in a suggested name.
pub(crate) fn short(fingerprint: &str) -> String {
    match fingerprint.find(':') {
        Some(colon) => fingerprint[colon + 1..].chars().take(8).collect(),
        None => fingerprint.chars().take(8).collect(),
    }
}

/// Drops the line ending and any spaces around it: pasted text arrives with both.
fn trimmed(line: &str) -> String {
    line.trim_end_matches(['\r', '\n']).trim().to_string()
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
