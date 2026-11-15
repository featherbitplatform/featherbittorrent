//! featherbittorrent: the binary. Plain `featherbittorrent` runs the local TUI;
//! `featherbittorrent serve` arrives with milestone 2.

#![forbid(unsafe_code)]

use clap::Parser;

mod run;
mod terminal;

/// BitTorrent client for the terminal.
#[derive(Parser)]
#[command(name = "featherbittorrent", version)]
struct Cli {}

fn main() -> std::process::ExitCode {
    let _cli = Cli::parse();
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
