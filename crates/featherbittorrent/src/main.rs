//! featherbittorrent: the binary. Plain `featherbittorrent` runs the local TUI;
//! `featherbittorrent serve` arrives with milestone 2.

#![forbid(unsafe_code)]

use clap::Parser;

/// BitTorrent client for the terminal.
#[derive(Parser)]
#[command(name = "featherbittorrent", version)]
struct Cli {}

fn main() {
    let _cli = Cli::parse();
    println!("The terminal UI is not built yet: it lands later in milestone 1.");
}
