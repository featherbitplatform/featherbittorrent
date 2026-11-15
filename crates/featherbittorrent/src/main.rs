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
    let code = runtime.block_on(run::run());
    drop(runtime); // Engine threads finish before the process does.
    code
}
