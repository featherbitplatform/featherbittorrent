//! The files the packages install beside the binary: man pages and shell completions, both written
//! from the same `clap::Command` that prints `--help`, so they cannot drift from the flags.
//!
//! Hidden things stay hidden here. `generate-assets` is itself a hidden subcommand, and a debug build
//! has two hidden `serve` switches. `clap_mangen` skips hidden commands and arguments by itself; the
//! completion generators walk every argument and subcommand they are handed, so they get a command
//! with the hidden parts taken out (see [`prune_hidden`]).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use clap::{Args, Command};
use clap_complete::Shell;

/// `featherbittorrent generate-assets <DIR>`: the arguments the hidden subcommand takes.
#[derive(Args, Debug)]
pub struct GenerateAssetsArgs {
    /// The directory to write `man/` and `completions/` below. It is made when it is not there.
    pub dir: PathBuf,
}

/// The shells a completion script is written for: the four a package installs, and the four an archive
/// carries for the shells that have no package.
const SHELLS: [Shell; 4] = [Shell::Bash, Shell::Zsh, Shell::Fish, Shell::PowerShell];

/// The name every generated file is keyed on, so the build directory or a renamed binary cannot change
/// what a person's shell looks for.
const BIN_NAME: &str = "featherbittorrent";

/// Writes the man pages under `dir/man` and the completion scripts under `dir/completions`, and returns
/// every path written, in the order it was written.
///
/// The pages are uncompressed: the packaging scripts gzip the ones a package installs, and an archive is
/// compressed as a whole.
pub fn write_assets(cmd: Command, dir: &Path) -> io::Result<Vec<PathBuf>> {
    let man_dir = dir.join("man");
    let completions_dir = dir.join("completions");
    fs::create_dir_all(&man_dir)?;
    fs::create_dir_all(&completions_dir)?;

    let mut written = Vec::new();
    write_man_pages(cmd.clone(), &man_dir, &mut written)?;

    for shell in SHELLS {
        // A command of its own per shell: each generator builds the command it is handed, and sharing
        // one across all four would have the second one look at a tree the first one already built.
        let mut cmd = prune_hidden(&cmd);
        written.push(clap_complete::generate_to(
            shell,
            &mut cmd,
            BIN_NAME,
            &completions_dir,
        )?);
    }

    Ok(written)
}

/// Writes `featherbittorrent.1` and one page for every visible subcommand at any depth, named by joining
/// the command path with `-` (`featherbittorrent-user-set.1`).
///
/// Hidden subcommands get no page, and neither does clap's `help`: a person who ran
/// `man featherbittorrent-help` would be reading a page for a command that only prints help.
fn write_man_pages(cmd: Command, out_dir: &Path, written: &mut Vec<PathBuf>) -> io::Result<()> {
    // The built-in `help` subcommand is added while building, so taking it out first is what keeps it
    // out of both the pages and the SUBCOMMANDS list above them.
    let mut cmd = cmd.disable_help_subcommand(true);
    // Building here, before the children are cloned, is what gives each one its `featherbittorrent-serve`
    // display name: that string is both the page title and the filename.
    cmd.build();
    for sub in cmd.get_subcommands().filter(|s| !s.is_hide_set()).cloned() {
        write_man_pages(sub, out_dir, written)?;
    }
    written.push(clap_mangen::Man::new(cmd).generate_to(out_dir)?);
    Ok(())
}

/// The same command without the parts `--help` already keeps out: hidden subcommands, and hidden
/// arguments at every depth.
///
/// clap has no way to take a subcommand or an argument back out of a `Command`, so each command is built
/// again and its visible parts put back onto it. Everything that is kept is the original object cloned,
/// so help text, value parsers and positional order are exactly what `--help` shows; the settings copied
/// below are the ones that change what a completion script contains. What is not listed is covered by
/// `tests::pruning_takes_out_only_what_help_hides`, which compares the two commands' help: a setting
/// added to `Cli` later that this function forgot fails a test instead of quietly changing the packages.
fn prune_hidden(cmd: &Command) -> Command {
    let mut pruned = Command::new(cmd.get_name().to_owned());
    if let Some(author) = cmd.get_author() {
        pruned = pruned.author(author.to_owned());
    }
    if let Some(about) = cmd.get_about() {
        pruned = pruned.about(about.clone());
    }
    if let Some(about) = cmd.get_long_about() {
        pruned = pruned.long_about(about.clone());
    }
    if let Some(text) = cmd.get_before_help() {
        pruned = pruned.before_help(text.clone());
    }
    if let Some(text) = cmd.get_before_long_help() {
        pruned = pruned.before_long_help(text.clone());
    }
    if let Some(text) = cmd.get_after_help() {
        pruned = pruned.after_help(text.clone());
    }
    if let Some(text) = cmd.get_after_long_help() {
        pruned = pruned.after_long_help(text.clone());
    }
    if let Some(version) = cmd.get_version() {
        pruned = pruned.version(version.to_owned());
    }
    if let Some(version) = cmd.get_long_version() {
        pruned = pruned.long_version(version.to_owned());
    }
    // Aliases a person can type are candidates in the scripts; hidden ones were never candidates, so
    // there is nothing to carry over for them.
    let aliases: Vec<String> = cmd.get_visible_aliases().map(str::to_owned).collect();
    pruned = pruned.visible_aliases(aliases);
    // The four switches that decide which candidates exist at all, plus the two a required subcommand
    // group needs so the usage line says `<COMMAND>` rather than `[COMMAND]`.
    if cmd.is_disable_help_flag_set() {
        pruned = pruned.disable_help_flag(true);
    }
    if cmd.is_disable_version_flag_set() {
        pruned = pruned.disable_version_flag(true);
    }
    if cmd.is_disable_help_subcommand_set() {
        pruned = pruned.disable_help_subcommand(true);
    }
    if cmd.is_propagate_version_set() {
        pruned = pruned.propagate_version(true);
    }
    if cmd.is_subcommand_required_set() {
        pruned = pruned.subcommand_required(true);
    }
    if cmd.is_arg_required_else_help_set() {
        pruned = pruned.arg_required_else_help(true);
    }

    for arg in cmd.get_arguments().filter(|a| !a.is_hide_set()) {
        pruned = pruned.arg(arg.clone());
    }
    for sub in cmd.get_subcommands().filter(|s| !s.is_hide_set()) {
        pruned = pruned.subcommand(prune_hidden(sub));
    }
    pruned
}

#[cfg(test)]
mod tests {
    use super::{BIN_NAME, SHELLS, prune_hidden, write_assets};
    use crate::Cli;
    use clap::CommandFactory;
    use fbtt_core::scratch::ScratchDir;
    use std::fs;
    use std::path::Path;

    /// The names the packaging scripts copy out of `completions/`, and the pages they gzip. Asserted
    /// against what was written so a generator renaming a file shows up here rather than in a package
    /// that installs nothing where its metadata says it should.
    const EXPECTED_PAGES: [&str; 10] = [
        "featherbittorrent.1",
        "featherbittorrent-serve.1",
        "featherbittorrent-user.1",
        "featherbittorrent-user-set.1",
        "featherbittorrent-user-remove.1",
        "featherbittorrent-user-list.1",
        "featherbittorrent-device.1",
        "featherbittorrent-device-add.1",
        "featherbittorrent-device-remove.1",
        "featherbittorrent-device-list.1",
    ];

    /// The command prints these paths, and the packaging scripts gzip and copy what they say is there,
    /// so the list returned has to be exactly the files on disk.
    #[test]
    fn write_assets_returns_the_paths_it_wrote() {
        let scratch = ScratchDir::new("write_assets");
        let written =
            write_assets(Cli::command(), scratch.path()).expect("assets in a fresh directory");

        let mut listed: Vec<String> = written
            .iter()
            .map(|path| relative_to(path, scratch.path()))
            .collect();
        listed.sort();
        assert_eq!(
            listed,
            files_under(scratch.path()),
            "the printed list is the files on disk, each once"
        );
        for path in &written {
            let text = fs::read_to_string(path).expect("a generated asset reads as text");
            assert!(!text.trim().is_empty(), "{} came out empty", path.display());
        }

        // The pages, named by the command path, and the four scripts, named for each shell's convention.
        let pages = names_under(&scratch.path().join("man"));
        let mut expected_pages: Vec<String> =
            EXPECTED_PAGES.iter().map(|page| page.to_string()).collect();
        expected_pages.sort();
        assert_eq!(pages, expected_pages, "one page per visible command");
        let scripts = names_under(&scratch.path().join("completions"));
        let mut expected_scripts = vec![
            format!("{BIN_NAME}.bash"),
            format!("_{BIN_NAME}"),
            format!("{BIN_NAME}.fish"),
            format!("_{BIN_NAME}.ps1"),
        ];
        expected_scripts.sort();
        assert_eq!(
            scripts, expected_scripts,
            "one script per shell in `SHELLS`"
        );
        assert_eq!(SHELLS.len(), 4, "the four shells the packages install");
    }

    /// Nothing hidden is in a file somebody installs. A debug build has the two `serve` switches, and
    /// this crate has a packaging subcommand, so this is the check that they stay out: a release build
    /// has no `--stop-file` or `--offline` at all, and its completion script must not offer them.
    #[test]
    fn write_assets_leaves_out_everything_hidden() {
        let scratch = ScratchDir::new("write_assets_hidden");
        for path in
            write_assets(Cli::command(), scratch.path()).expect("assets in a fresh directory")
        {
            let text = fs::read_to_string(&path).expect("a generated asset reads as text");
            for hidden in ["generate-assets", "stop-file", "offline"] {
                assert!(
                    !text.contains(hidden),
                    "{} mentions the hidden `{hidden}`: {}",
                    path.display(),
                    text.lines()
                        .find(|line| line.contains(hidden))
                        .unwrap_or("")
                        .trim()
                );
            }
        }
    }

    /// Pruning is only allowed to take out what `--help` already keeps out, so the two trees must render
    /// the same help. A command-level setting that `prune_hidden` does not copy shows up here as a
    /// different usage line or a missing option.
    #[test]
    fn pruning_takes_out_only_what_help_hides() {
        let original = Cli::command();
        let pruned = prune_hidden(&original);
        assert_same_help(&original, &pruned);

        // And the hidden parts really are gone, at every depth.
        let mut pruned = pruned;
        pruned.build();
        for cmd in walk(&pruned) {
            assert!(
                cmd.get_subcommands().all(|s| !s.is_hide_set()),
                "{} still carries a hidden subcommand",
                cmd.get_name()
            );
            assert!(
                cmd.get_arguments().all(|a| !a.is_hide_set()),
                "{} still carries a hidden argument",
                cmd.get_name()
            );
        }
    }

    /// Renders the long help of two commands with the same path and compares it, then does the same for
    /// each visible subcommand. Colors are off so the comparison is about the text.
    fn assert_same_help(original: &clap::Command, pruned: &clap::Command) {
        let render = |cmd: clap::Command| {
            let mut cmd = cmd.color(clap::ColorChoice::Never);
            cmd.build();
            cmd.render_long_help().to_string()
        };
        assert_eq!(
            render(original.clone()),
            render(pruned.clone()),
            "pruning changed what {} prints",
            original.get_name()
        );
        let mut original = original.clone();
        let mut pruned = pruned.clone();
        original.build();
        pruned.build();
        for sub in original.get_subcommands().filter(|s| !s.is_hide_set()) {
            let name = sub.get_name();
            assert_same_help(
                sub,
                pruned
                    .find_subcommand(name)
                    .unwrap_or_else(|| panic!("pruning dropped the `{name}` subcommand")),
            );
        }
    }

    /// Every command in the tree, including hidden ones.
    fn walk(cmd: &clap::Command) -> Vec<&clap::Command> {
        let mut all = vec![cmd];
        for sub in cmd.get_subcommands() {
            all.extend(walk(sub));
        }
        all
    }

    /// The file names under `dir`, sorted, with the two directories that hold them ignored.
    fn names_under(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .expect("the directory write_assets made")
            .map(|entry| {
                entry
                    .expect("a directory entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }

    /// Every file below `root`, sorted, as its path below `root` with `/` separators.
    fn files_under(root: &Path) -> Vec<String> {
        let mut files = Vec::new();
        collect_files(root, root, &mut files);
        files.sort();
        files
    }

    fn collect_files(root: &Path, dir: &Path, files: &mut Vec<String>) {
        for entry in fs::read_dir(dir).expect("the directory write_assets made") {
            let path = entry.expect("a directory entry").path();
            if path.is_dir() {
                collect_files(root, &path, files);
            } else {
                files.push(relative_to(&path, root));
            }
        }
    }

    /// `path` below `root`, with `/` separators, so the comparison does not depend on the platform.
    fn relative_to(path: &Path, root: &Path) -> String {
        path.strip_prefix(root)
            .expect("a written asset below the directory it was asked for")
            .components()
            .map(|part| part.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/")
    }
}
