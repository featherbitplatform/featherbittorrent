//! featherbittorrent: the binary. Plain `featherbittorrent` runs the local TUI;
//! `serve` runs the library and its API with no screen, and `user` and `device`
//! edit the logins a server needs.

#![forbid(unsafe_code)]
// The binary's whole job is talking to a person: stdout and stderr are the product here.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::io::{BufRead, IsTerminal, Stdout, Write};
use std::path::Path;
use std::process::ExitCode;

use clap::{Args, CommandFactory, Parser, Subcommand};
use fbtt_core::Paths;

mod admin;
mod assets;
mod backend;
mod exit;
#[cfg(test)]
mod fake_store;
mod run;
mod secrets;
mod serve;
mod switch;
mod terminal;
mod wizard;

use crate::exit::Failure;

/// Where the library's files are, what the environment can move them to, and what each exit code
/// means: the three paragraphs that belong beside the flags rather than in any one subcommand's help.
///
/// Every body line starts with two spaces and stays under 75 columns. That is what makes both readers
/// agree: clap prints `after_long_help` without wrapping it, so a long line would run past the
/// terminal, and man keeps a text line that begins with white space on a line of its own (it fills
/// unindented lines into one folded paragraph), so a line over 75 columns would break mid-path at the
/// width man renders at. Raw text so the Windows paths keep their single backslashes; the roff writer
/// escapes them for the page.
const LONG_HELP_AFTER: &str = r#"FILES:
  config.yaml and ui.yaml live in this library's config directory:
  Linux ~/.config/featherbittorrent
  macOS ~/Library/Application Support/io.featherbit.featherbittorrent
  Windows %APPDATA%\featherbit\featherbittorrent\config
  session.state and library.lock live in the data directory:
  Linux ~/.local/share/featherbittorrent
  Windows %APPDATA%\featherbit\featherbittorrent\data
  On macOS the config and data directories are the same folder.

ENVIRONMENT:
  FBTT_CONFIG_DIR and FBTT_DATA_DIR each replace one of those directories,
  which is how a service or a container keeps a library outside the home
  directory. RUST_LOG decides what serve writes to stderr.

EXIT STATUS:
  0 stopped cleanly. 1 something went wrong, and the line on stderr says
  what. 2 clap could not read the command line. 3 another featherbittorrent
  holds this library.
"#;

/// BitTorrent client for the terminal.
#[derive(Parser)]
#[command(name = "featherbittorrent", version, after_long_help = LONG_HELP_AFTER)]
struct Cli {
    /// What to run. With nothing, the terminal client.
    #[command(subcommand)]
    action: Option<Action>,
}

#[derive(Subcommand)]
enum Action {
    /// Run the library and its API with no screen, until Ctrl-C or SIGTERM.
    Serve(serve::ServeDebug),
    /// Create or replace a login, or list the ones `config.yaml` has.
    User(UserArgs),
    /// Register a device key, or list the ones that log in without a password.
    Device(DeviceArgs),
    /// Write the man pages and shell completions the packages install, from this same command.
    #[command(hide = true, name = "generate-assets")]
    GenerateAssets(assets::GenerateAssetsArgs),
}

#[derive(Args)]
struct UserArgs {
    #[command(subcommand)]
    command: UserCommand,
}

#[derive(Subcommand)]
enum UserCommand {
    /// Create or replace one login, asking for the password twice.
    Set(UserSetArgs),
    /// Take one login out of `config.yaml`.
    Remove(UserRemoveArgs),
    /// Print the logins in `config.yaml`: name, then password hash.
    List,
}

#[derive(Args, Default)]
struct UserSetArgs {
    /// The login to create or replace. Typed at the prompt when left out.
    name: Option<String>,
}

#[derive(Args, Default)]
struct UserRemoveArgs {
    /// The login to take out, as `user list` prints it.
    name: String,
}

#[derive(Args)]
struct DeviceArgs {
    #[command(subcommand)]
    command: DeviceCommand,
}

#[derive(Subcommand)]
enum DeviceCommand {
    /// Register a device key under a name: an `ssh-ed25519` public key line, or
    /// the path of a file holding one. With no key argument, it is read from stdin.
    Add(DeviceAddArgs),
    /// Take one device out of `config.yaml`, by the name `device add` gave it.
    Remove(DeviceRemoveArgs),
    /// Print the devices in `config.yaml`: name, then fingerprint.
    List,
}

#[derive(Args, Default)]
struct DeviceAddArgs {
    /// What to call this device, for the devices list.
    name: String,

    /// The key, as `ssh-ed25519 <base64> [comment]`, or a file containing it. Read
    /// from stdin when left out.
    key: Option<String>,
}

#[derive(Args, Default)]
struct DeviceRemoveArgs {
    /// The device to take out, as `device list` prints it.
    name: String,
}

fn main() -> ExitCode {
    match Cli::parse().action {
        // Plain `featherbittorrent`: milestone 1's terminal run, unchanged.
        None => tui(),
        Some(Action::Serve(debug)) => {
            // The same runtime the TUI runs on: `fbtt-server` needs a worker pool,
            // because its save task and fbt-lib's blocking work cannot share one.
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("a tokio runtime");
            let result = runtime.block_on(async move {
                match Paths::resolve() {
                    Ok(paths) => serve::run(&paths, &debug).await,
                    Err(problem) => Err(Failure::problem(problem.to_string())),
                }
            });
            drop(runtime); // Engine threads finish before the process does.
            report(result)
        }
        Some(Action::User(UserArgs { command })) => match command {
            UserCommand::Set(args) => report(admin_command(|paths, input, out, secret_input| {
                admin::set_user(paths, input, out, secret_input, args.name.as_deref())
            })),
            UserCommand::Remove(args) => {
                report(admin_command(|paths, _input, out, _secret_input| {
                    admin::remove_user(paths, out, &args.name)
                }))
            }
            UserCommand::List => report(admin_command(|paths, _input, out, _secret_input| {
                admin::list_users(paths, out)
            })),
        },
        Some(Action::Device(DeviceArgs { command })) => match command {
            DeviceCommand::Add(args) => {
                report(admin_command(|paths, input, out, _secret_input| {
                    admin::add_device(paths, input, out, &args.name, args.key.as_deref())
                }))
            }
            DeviceCommand::Remove(args) => {
                report(admin_command(|paths, _input, out, _secret_input| {
                    admin::remove_device(paths, out, &args.name)
                }))
            }
            DeviceCommand::List => report(admin_command(|paths, _input, out, _secret_input| {
                admin::list_devices(paths, out)
            })),
        },
        Some(Action::GenerateAssets(args)) => report(generate_assets(&args.dir)),
    }
}

/// Writes the packaging assets into `dir` and prints one line per file written, which is what the
/// packaging scripts read to know what to gzip and where to copy it. No library paths are involved:
/// this command touches nothing but the directory it was given.
fn generate_assets(dir: &Path) -> Result<(), Failure> {
    for path in assets::write_assets(Cli::command(), dir)? {
        println!("{}", path.display());
    }
    Ok(())
}

/// One terminal run: the runtime, then `run::run`'s own answer.
fn tui() -> ExitCode {
    // A worker per core: the backend task runs long engine steps, and the frame clock
    // and the event read should not queue up behind one.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("a tokio runtime");
    // A panic on the UI side unwinds out of `run` before its ordered quit, so no last save
    // runs: what the next start finds is whatever was written last, and the tick that
    // writes `session.state` every five minutes is what bounds that loss to re-checking a
    // few minutes of progress. The terminal still comes back, through the panic hook
    // `ratatui::try_init` installs (see `terminal.rs`).
    let code = runtime.block_on(run::run());
    drop(runtime); // Engine threads finish before the process does.
    code
}

/// Runs one `user` or `device` command with the library's paths, and stdin and
/// stdout in hand. No runtime: these commands read a file and hash a password, and
/// nothing in them waits on anything.
fn admin_command(
    do_it: impl FnOnce(&Paths, &mut dyn BufRead, &mut dyn Write, bool) -> Result<(), Failure>,
) -> Result<(), Failure> {
    let paths = Paths::resolve()?;
    // Said before the first prompt: `rpassword` reads the terminal itself, so it is
    // only right to use it when input really is one. Piped answers are read as
    // ordinary lines, which is what a script or a test needs them to be.
    let stdin = std::io::stdin();
    let secret_input = stdin.is_terminal();
    let mut input = stdin.lock();
    let mut out: Stdout = std::io::stdout();
    do_it(&paths, &mut input, &mut out, secret_input)
}

/// Prints the problem and gives the exit code it asked for.
fn report(result: Result<(), Failure>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(problem) => {
            eprintln!("{}", problem.message);
            ExitCode::from(problem.code as u8)
        }
    }
}
