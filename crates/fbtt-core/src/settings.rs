//! The settings every surface reads, and the change it sends back.
//!
//! One view, so the TUI, the web UI and the desktop app cannot drift into three different pictures of
//! the same engine: the running rates and queue counts under `speed`, the downloads settings under
//! `downloads`. It is also the wire shape - the same field names in the same places are what
//! `GET /api/v1/settings` answers and what the remote backend parses - so a screen that reads the
//! local engine and a screen that reads a server are written against one type.
//!
//! A change comes back the way a form is submitted: the whole section, with one exception. A surface
//! that has no right to edit the program to run at the end of a torrent (a web UI on a server, which
//! reads it from that server's `config.yaml`) leaves the `on_complete` key out altogether, and leaving
//! it out keeps whatever the file has. Naming it with a program sets it; naming it with nothing clears
//! it. That is the difference between "I cannot change this" and "this should be empty", and a form
//! that could not tell them apart would clear the program on every save of a section that never showed
//! it.
//!
//! [`validate_downloads`] is what turns a submitted change into either a write or one line telling the
//! person what to fix, and [`SettingsProblem::message`] is that line. A server runs it with its allowed
//! folders; a run driving its own engine passes `None` and is confined to nothing but its own rules.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::client::LiveLimits;
use crate::config::DownloadsConfig;

/// What a Settings screen shows: the limits the engine is running under, and the downloads settings.
///
/// Both are the engine's own state as this run reads it, never a form's half-edited values, so two
/// surfaces on one engine see the same numbers. The proxy view arrives with the proxy task and sits
/// beside these two.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsView {
    /// The rates and queue counts the running session is under, in the units a person states them in.
    pub speed: LiveLimits,
    /// Where downloads go, and what happens when one finishes.
    pub downloads: DownloadsView,
}

/// The downloads section of the view: the folders, the switch, the program, and the latest complaint
/// about each of the two things that can fail on their own.
///
/// `watch_problem` and `on_complete_problem` are about the last run of those two, not about the
/// settings: a watch folder that was deleted while the app ran, or a program that exited non-zero.
/// They are display-safe text (remote and file-system text goes through `fbt::display_safe` before it
/// gets here), and a surface shows them beside the field they belong to rather than swallowing them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadsView {
    /// Where finished contents go: the config's `download_dir`, already absolute.
    pub default_dir: PathBuf,
    /// Where a torrent's files live until it is whole, `None` for "the default folder".
    pub unfinished_dir: Option<PathBuf>,
    /// Whether a new torrent starts paused.
    pub start_paused: bool,
    /// The folder watched for `.torrent` files, `None` for no watch folder.
    pub watch_dir: Option<PathBuf>,
    /// The program run when a torrent finishes, and whether this surface may change it.
    pub on_complete: OnCompleteView,
    /// What went wrong with the watch folder last time it was read, fit to show.
    pub watch_problem: Option<String>,
    /// What went wrong with the program last time it was run, fit to show.
    pub on_complete_problem: Option<String>,
}

/// The program a finished torrent runs, as a surface sees it.
///
/// `editable` is false on a server: it reads the program from its own `config.yaml` and a browser has
/// no business naming a program to run on a machine it is only visiting. A surface shows the value with
/// that answer, and a change that leaves `on_complete` out keeps the value whatever this says.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnCompleteView {
    /// The program, by its full path, `None` for nothing to run.
    pub program: Option<PathBuf>,
    /// Whether the surface reading this may change the program at all.
    pub editable: bool,
}

/// The downloads section as a person submitted it: every key of the section, so a save writes the
/// section the form showed rather than a diff a surface has to reason about.
///
/// `on_complete` is the one key that may be absent, and absent means "leave it alone" rather than "set
/// it to nothing": see the module docs and [`OnCompleteSet`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadsChange {
    /// The folder finished contents go to.
    pub default_dir: PathBuf,
    /// The folder a torrent works in until it is whole.
    pub unfinished_dir: Option<PathBuf>,
    /// Whether new torrents start paused.
    pub start_paused: bool,
    /// The folder to watch for `.torrent` files.
    pub watch_dir: Option<PathBuf>,
    /// The program to run when a torrent finishes. Absent: keep the one the config has. Present with
    /// no program: run nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_complete: Option<OnCompleteSet>,
}

/// The program a change sets or clears: `program: null` means run nothing, and the whole key being
/// absent (see [`DownloadsChange::on_complete`]) means keep whatever is there.
///
/// The `default` is for a body that spells `on_complete: {}`: a form that submits the key with no
/// program in it has said "nothing to run", not "I meant to send something".
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct OnCompleteSet {
    /// The program to run, by its full path, or `None` to run nothing.
    #[serde(default)]
    pub program: Option<PathBuf>,
}

/// What is wrong with a submitted downloads section, one variant per thing a person can get wrong.
///
/// The variant is what a route answers with and [`message`](Self::message) is the line the surface
/// shows, so the refusal a person reads is decided here once rather than reworded per surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsProblem {
    /// A folder is a full path, but not inside a root this server may name.
    OutsideRoots,
    /// A folder is named by something other than its full path.
    NotAbsolute,
    /// The unfinished folder is the default folder, which would make it the same folder twice.
    UnfinishedIsDefault,
    /// The watch folder is one of the download folders, so a saved torrent would feed itself.
    WatchIsDownloads,
    /// The program is named by something other than its full path.
    ProgramNotAbsolute,
    /// Nothing to run at that path.
    NoProgram,
    /// The program is a Windows batch file, which is a shell script rather than a program to start.
    BatchFile,
    /// This surface may not change this setting: the server's own config file is where it lives.
    NotEditable,
}

impl SettingsProblem {
    /// The one line a person reads, in the plan's exact words.
    ///
    /// Product copy with no emoji and no em-dashes, and the same words on every surface: a refusal a
    /// person saw in the TUI should be the refusal they see in the browser.
    pub fn message(&self) -> &'static str {
        match self {
            SettingsProblem::OutsideRoots => "Choose a folder inside the server's allowed folders.",
            SettingsProblem::NotAbsolute => "Choose a folder by its full path.",
            SettingsProblem::UnfinishedIsDefault => {
                "The unfinished folder must be different from the default folder."
            }
            SettingsProblem::WatchIsDownloads => {
                "The watch folder must be different from the download folders."
            }
            SettingsProblem::ProgramNotAbsolute => "Name the program by its full path.",
            SettingsProblem::NoProgram => "No program at that path.",
            SettingsProblem::BatchFile => "Name a program, not a batch file.",
            SettingsProblem::NotEditable => "Set in the server's config file",
        }
    }
}

impl DownloadsConfig {
    /// Makes this section say what a validated change says.
    ///
    /// The folders and the switch always come across - a change carries the whole section, and a
    /// surface that did not show one of them showed its value, so writing the value back changes
    /// nothing. `on_complete` is applied only when the change named it: a surface with no right to edit
    /// the program (see [`OnCompleteView::editable`]) leaves the key out, and the file's value survives
    /// the save of another field.
    ///
    /// Validation is [`validate_downloads`]'s job: this writes what it approved.
    pub fn apply(&mut self, change: &DownloadsChange) {
        self.unfinished_dir = change.unfinished_dir.clone();
        self.start_paused = change.start_paused;
        self.watch_dir = change.watch_dir.clone();
        if let Some(on_complete) = &change.on_complete {
            self.on_complete = on_complete.program.clone();
        }
    }
}

/// Whether a submitted downloads section may be written, or which one line to answer it with.
///
/// Every folder in it has to be given by its full path: a relative one means different things to the
/// run that saves it and the run that reads it back, which is the mistake
/// [`Config::resolve`](crate::Config::resolve) exists to keep out of a config file. It also has to be
/// inside one of the served folders, when the caller names them. Every folder is folded before any
/// of these comparisons - `.` dropped, `..` resolved against the directory before it, without asking
/// the filesystem - so `/srv/downloads/../etc` cannot pass itself as being under `/srv/downloads` and
/// `/srv/x/../d` cannot pass itself as a folder other than `/srv/d`; the server adds its own
/// canonical check on top of this one when it writes. Then the folders have to be three different
/// places: an unfinished folder that is the default folder splits nothing, and a watch folder that is
/// a download folder is a loop that adds a torrent, saves its `.torrent` in the same folder, and adds
/// it again.
///
/// A program, when the change names one, is a path to start: absolute, and a file that is really there.
/// On Windows it may not be a batch file, because Windows starts those through a shell and a person
/// naming one is asking for something the run does not exec.
///
/// `roots` is `None` for a run that drives its own engine, which serves no folders to anybody else and
/// therefore confines nothing; `Some` for a server, whose allowed folders are the whole point of its
/// `allowed_roots`.
pub fn validate_downloads(
    change: &DownloadsChange,
    roots: Option<&[PathBuf]>,
) -> Result<(), SettingsProblem> {
    // Fold every submitted folder first. A `PathBuf` comparison answers about the spelling, not the
    // folder, so the root list and the "different folder" rules all run on the folded paths or a `..`
    // walks round them. A relative folder is not a folder anybody served: it means "below whatever
    // directory this process was started in", which is the mistake `resolve` exists to keep out of a
    // config file.
    let default_dir = lexical_absolute(&change.default_dir).ok_or(SettingsProblem::NotAbsolute)?;
    let unfinished_dir = change
        .unfinished_dir
        .as_ref()
        .map(|dir| lexical_absolute(dir).ok_or(SettingsProblem::NotAbsolute))
        .transpose()?;
    let watch_dir = change
        .watch_dir
        .as_ref()
        .map(|dir| lexical_absolute(dir).ok_or(SettingsProblem::NotAbsolute))
        .transpose()?;
    for dir in [
        Some(&default_dir),
        unfinished_dir.as_ref(),
        watch_dir.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        if !inside_one_root(dir, roots) {
            return Err(SettingsProblem::OutsideRoots);
        }
    }
    if let Some(unfinished) = &unfinished_dir
        && unfinished == &default_dir
    {
        return Err(SettingsProblem::UnfinishedIsDefault);
    }
    if let Some(watch) = &watch_dir
        && (watch == &default_dir
            || unfinished_dir
                .as_ref()
                .is_some_and(|unfinished| watch == unfinished))
    {
        return Err(SettingsProblem::WatchIsDownloads);
    }
    if let Some(program) = change
        .on_complete
        .as_ref()
        .and_then(|on_complete| on_complete.program.as_ref())
    {
        if !program.is_absolute() {
            return Err(SettingsProblem::ProgramNotAbsolute);
        }
        // Before the existence check, so a batch file is refused for what it is even when nothing is
        // at that path: the person's mistake is the extension, not the location.
        if let Some(problem) = batch_problem(program) {
            return Err(problem);
        }
        if !program.is_file() {
            return Err(SettingsProblem::NoProgram);
        }
    }
    Ok(())
}

/// `path` as absolute components with `.` dropped and every `..` folded against the directory before
/// it, without touching the filesystem: `/srv/downloads/../etc` becomes `/srv/etc`, and `..` above the
/// root stays at the root because there is nowhere above it. `None` for a path that is not absolute.
///
/// Folding lexically is deliberate: the check is about what the person typed, and it has to answer the
/// same way for a folder that does not exist yet. A server canonicalises the folder it actually writes
/// into, which is where a symlink is caught, and that answer cannot depend on this one.
fn lexical_absolute(path: &Path) -> Option<PathBuf> {
    use std::path::Component;

    if !path.is_absolute() {
        return None;
    }
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            // `pop` leaves a root alone, which is the right answer for `..` at the top of a path.
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    Some(out)
}

/// Whether an already-folded directory is one of the roots a server serves, compared by components
/// rather than by text so `/srv/downloads-wide` is not inside `/srv/downloads`. `None` roots is a run
/// driving its own engine, which serves no folders to anybody else and so confines nothing.
///
/// A root that is not absolute (a config the caller never resolved) matches nothing: it says nothing
/// about where this run's files are.
fn inside_one_root(dir: &Path, roots: Option<&[PathBuf]>) -> bool {
    roots.is_none_or(|roots| {
        roots
            .iter()
            .any(|root| lexical_absolute(root).is_some_and(|root| dir.starts_with(&root)))
    })
}

/// On Windows, a batch file is a problem; everywhere else the extension means nothing and the program
/// is whatever is at the path. The split is by platform because what a `.bat` *is* differs: Windows
/// starts one through a shell, and the run here starts a program directly.
#[cfg(windows)]
fn batch_problem(program: &Path) -> Option<SettingsProblem> {
    batch_file(program).then_some(SettingsProblem::BatchFile)
}

/// Everywhere a `.bat` is a file name like any other, so there is nothing to refuse on that account.
#[cfg(not(windows))]
fn batch_problem(_program: &Path) -> Option<SettingsProblem> {
    None
}

/// Whether `program`'s file name ends in a Windows batch file's extension, read case-insensitively
/// because Windows spells extensions whichever way it likes.
#[cfg(any(windows, test))]
fn batch_file(program: &Path) -> bool {
    let Some(name) = program
        .file_name()
        .map(|name| name.to_string_lossy().to_lowercase())
    else {
        return false;
    };
    name.ends_with(".bat") || name.ends_with(".cmd")
}

#[cfg(test)]
mod tests {
    use super::{
        DownloadsChange, DownloadsConfig, DownloadsView, OnCompleteSet, OnCompleteView,
        SettingsProblem, SettingsView, batch_file, validate_downloads,
    };
    use crate::client::LiveLimits;
    use crate::scratch::ScratchDir;
    use std::fs;
    use std::path::{Path, PathBuf};

    /// On Windows a path without a drive letter is not absolute, so tests build absolute paths this way.
    fn abs(p: &str) -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(format!("C:{p}"))
        } else {
            PathBuf::from(p)
        }
    }

    /// The four keys a surface always sends, with the program left to the caller: the shape a change
    /// arrives in, spelled once for these tests.
    fn change(
        default_dir: &Path,
        unfinished_dir: Option<&Path>,
        watch_dir: Option<&Path>,
        program: Option<&Path>,
    ) -> DownloadsChange {
        DownloadsChange {
            default_dir: default_dir.to_path_buf(),
            unfinished_dir: unfinished_dir.map(Path::to_path_buf),
            start_paused: false,
            watch_dir: watch_dir.map(Path::to_path_buf),
            on_complete: program.map(|program| OnCompleteSet {
                program: Some(program.to_path_buf()),
            }),
        }
    }

    /// Each thing a person can get wrong about the downloads section, refused with the plan's exact
    /// line. `roots` is `None` for a run driving its own engine and `Some` for a server, whose allowed
    /// folders every submitted folder has to be inside.
    #[test]
    fn validate_refuses_each_problem() {
        let dir = ScratchDir::new("settings_validate");
        let root = dir.path().join("share");
        fs::create_dir(&root).expect("make the root the server serves");
        let program = dir.path().join("notify");
        fs::write(&program, "a program\n").expect("write the program to run");

        let default_dir = root.join("downloads");
        let unfinished = root.join("incomplete");
        let watch = root.join("watch");
        let roots = [root.clone()];

        let refused =
            |change: &DownloadsChange, roots: Option<&[PathBuf]>, problem: SettingsProblem| {
                assert_eq!(
                    validate_downloads(change, roots),
                    Err(problem),
                    "{problem:?} is what a change of {change:?} gets"
                );
                assert!(!problem.message().is_empty(), "{problem:?} says something");
            };

        // Nothing wrong with it: three different folders under the one root, and a real program.
        let good = change(
            &default_dir,
            Some(&unfinished),
            Some(&watch),
            Some(&program),
        );
        assert_eq!(
            validate_downloads(&good, Some(&roots)),
            Ok(()),
            "three folders inside the served root, with a program that exists"
        );
        assert_eq!(
            validate_downloads(&good, None),
            Ok(()),
            "and a run with its own engine is confined to nothing"
        );
        // Nothing optional set either: the default folder and no program is a section on its own.
        assert_eq!(
            validate_downloads(&change(&default_dir, None, None, None), Some(&roots)),
            Ok(()),
            "the default folder alone is a whole section"
        );
        // `null` for the program is a choice, not a missing key: run nothing.
        let cleared = DownloadsChange {
            on_complete: Some(OnCompleteSet { program: None }),
            ..good.clone()
        };
        assert_eq!(
            validate_downloads(&cleared, Some(&roots)),
            Ok(()),
            "clearing the program is always allowed"
        );

        refused(
            &change(
                &default_dir,
                Some(&default_dir),
                Some(&watch),
                Some(&program),
            ),
            None,
            SettingsProblem::UnfinishedIsDefault,
        );
        refused(
            &change(&default_dir, None, Some(&default_dir), Some(&program)),
            None,
            SettingsProblem::WatchIsDownloads,
        );
        refused(
            &change(
                &default_dir,
                Some(&unfinished),
                Some(&unfinished),
                Some(&program),
            ),
            None,
            SettingsProblem::WatchIsDownloads,
        );
        // A folder outside the one folder this server may name.
        refused(
            &change(&abs("/not-under-the-root/downloads"), None, None, None),
            Some(&roots),
            SettingsProblem::OutsideRoots,
        );
        refused(
            &change(
                &default_dir,
                Some(&abs("/elsewhere/incomplete")),
                None,
                None,
            ),
            Some(&roots),
            SettingsProblem::OutsideRoots,
        );
        // A `..` that walks out of the root has to be folded before it is compared, or
        // `/share/../outside` passes itself as being under `/share`.
        refused(
            &change(
                &default_dir,
                Some(&root.join("..").join("outside")),
                None,
                None,
            ),
            Some(&roots),
            SettingsProblem::OutsideRoots,
        );
        refused(
            &change(
                &default_dir,
                None,
                Some(&root.join("share/../../outside")),
                None,
            ),
            Some(&roots),
            SettingsProblem::OutsideRoots,
        );
        // A folder the run cannot root anywhere: relative means "below whatever the process was
        // started in", which is not a folder anybody served, and the person spelling it relative has
        // not chosen to be outside a root list, so the line they get is about the spelling.
        refused(
            &change(Path::new("dl/downloads"), None, None, None),
            None,
            SettingsProblem::NotAbsolute,
        );
        refused(
            &change(&default_dir, Some(Path::new("incomplete")), None, None),
            Some(&roots),
            SettingsProblem::NotAbsolute,
        );
        // The program: named by something other than a full path, and named at a path with nothing
        // there.
        refused(
            &change(&default_dir, None, None, Some(Path::new("notify"))),
            None,
            SettingsProblem::ProgramNotAbsolute,
        );
        refused(
            &change(&default_dir, None, None, Some(&dir.path().join("gone"))),
            None,
            SettingsProblem::NoProgram,
        );
        // A directory is not a program either.
        refused(
            &change(&default_dir, None, None, Some(&root)),
            None,
            SettingsProblem::NoProgram,
        );

        // The lines a person reads, in the plan's words.
        assert_eq!(
            SettingsProblem::OutsideRoots.message(),
            "Choose a folder inside the server's allowed folders."
        );
        assert_eq!(
            SettingsProblem::NotAbsolute.message(),
            "Choose a folder by its full path."
        );
        assert_eq!(
            SettingsProblem::UnfinishedIsDefault.message(),
            "The unfinished folder must be different from the default folder."
        );
        assert_eq!(
            SettingsProblem::WatchIsDownloads.message(),
            "The watch folder must be different from the download folders."
        );
        assert_eq!(
            SettingsProblem::ProgramNotAbsolute.message(),
            "Name the program by its full path."
        );
        assert_eq!(
            SettingsProblem::NoProgram.message(),
            "No program at that path."
        );
        assert_eq!(
            SettingsProblem::NotEditable.message(),
            "Set in the server's config file"
        );
    }

    /// `/srv/x/../d` and `/srv/d` are two spellings of one folder, and a `PathBuf` comparison answers
    /// about the spelling, not the folder: a person who sets the unfinished folder one `..` away from
    /// the default would keep every torrent's files in the folder they are supposed to be moved out
    /// of, and the check would never notice.
    #[test]
    fn validate_unfinished_equal_after_dotdot_is_refused() {
        let outcome = validate_downloads(
            &change(
                &abs("/srv/d"),
                Some(&abs("/srv/x/../d")),
                None,
                Some(&abs("/usr/bin/tool")),
            ),
            None,
        );
        assert_eq!(
            outcome,
            Err(SettingsProblem::UnfinishedIsDefault),
            "/srv/x/../d is the folder the default already names"
        );
    }

    /// The same spelling trick against the watch folder: spelled with a `..` that walks back, the
    /// watch folder is still the default folder, or still the unfinished one, and a torrent saved
    /// there re-enters the engine that was watching for it.
    #[test]
    fn validate_watch_equal_after_dotdot_is_refused() {
        let outcome = validate_downloads(
            &change(
                &abs("/srv/d"),
                Some(&abs("/srv/x/../e")),
                Some(&abs("/srv/x/../d")),
                Some(&abs("/usr/bin/tool")),
            ),
            None,
        );
        assert_eq!(
            outcome,
            Err(SettingsProblem::WatchIsDownloads),
            "the watch folder spelled with a `..` is the default folder"
        );
        let outcome = validate_downloads(
            &change(
                &abs("/srv/d"),
                Some(&abs("/srv/x/e")),
                Some(&abs("/srv/x/e/../e")),
                Some(&abs("/usr/bin/tool")),
            ),
            None,
        );
        assert_eq!(
            outcome,
            Err(SettingsProblem::WatchIsDownloads),
            "and the same trick against the unfinished folder"
        );
    }

    /// A relative folder means "below whatever directory the process was started in", which is a
    /// different folder on every machine, and it is the spelling that is wrong whether or not a
    /// server has a list of allowed folders to be inside: a person running their own engine has
    /// never seen such a list, and telling them about it would send them looking for a setting that
    /// is not the one they got wrong.
    #[test]
    fn validate_relative_folder_is_not_absolute() {
        let outcome = validate_downloads(
            &change(
                Path::new("Downloads/Shows"),
                None,
                None,
                Some(&abs("/usr/bin/tool")),
            ),
            None,
        );
        let problem = outcome.expect_err("a relative folder is not a folder this run can name");
        assert_eq!(problem, SettingsProblem::NotAbsolute);
        assert_eq!(problem.message(), "Choose a folder by its full path.");

        // A run on a server refuses the same folder the same way: what it fails is the spelling, not
        // the list, and the root-list line is for folders that are full paths going outside it.
        let roots = [abs("/srv")];
        assert_eq!(
            validate_downloads(
                &change(
                    Path::new("Downloads/Shows"),
                    None,
                    None,
                    Some(&abs("/usr/bin/tool")),
                ),
                Some(&roots),
            ),
            Err(SettingsProblem::NotAbsolute),
            "the list is not what the relative path fails"
        );
    }

    /// Windows starts a `.bat` through a shell, so one is not a program this run can start: refused
    /// whatever case it is spelled in, and whatever which of the two extensions it uses. The refusal
    /// comes before the existence check, because the mistake is the kind of file, not where it is.
    #[cfg(windows)]
    #[test]
    fn validate_refuses_batch_files() {
        let dir = ScratchDir::new("settings_batch_files");
        for name in ["nightly.bat", "NIGHTLY.BAT", "nightly.Cmd"] {
            let submitted = change(
                &dir.path().join("downloads"),
                None,
                None,
                Some(&dir.path().join(name)),
            );
            assert_eq!(
                validate_downloads(&submitted, None),
                Err(SettingsProblem::BatchFile),
                "`{name}` is a shell script, not a program to start"
            );
        }
        assert_eq!(
            SettingsProblem::BatchFile.message(),
            "Name a program, not a batch file."
        );
    }

    /// The extension test is the same question on every host - a batch file is a batch file, and
    /// Windows spells the extension whichever way it likes - but only Windows refuses one: on Linux or
    /// macOS a `.bat` is an executable file like any other, and refusing it would turn a person's
    /// program into a mistake the settings screen cannot explain.
    #[test]
    fn batch_file_rule_applies_on_windows_only() {
        assert!(
            batch_file(Path::new("/usr/local/bin/Run.CMD")),
            "an upper-case `.cmd` is still a batch file"
        );
        assert!(
            batch_file(Path::new("/usr/local/bin/nightly.bat")),
            "so is a lower-case `.bat`"
        );
        assert!(
            batch_file(Path::new("nightly.BAT")),
            "and the test is on the file name, not on a whole path"
        );
        assert!(
            !batch_file(Path::new("/usr/local/bin/notify")),
            "a program with no extension is not one"
        );
        assert!(
            !batch_file(Path::new("/usr/local/bin/rotate.sh")),
            "and neither is a script of another kind"
        );

        let dir = ScratchDir::new("settings_batch_rule");
        let program = dir.path().join("nightly.bat");
        fs::write(&program, "a script\n").expect("write a .bat file");
        let submitted = change(&dir.path().join("downloads"), None, None, Some(&program));
        if cfg!(windows) {
            assert_eq!(
                validate_downloads(&submitted, None),
                Err(SettingsProblem::BatchFile),
                "Windows refuses the batch file it has to refuse"
            );
        } else {
            assert_eq!(
                validate_downloads(&submitted, None),
                Ok(()),
                "this host runs a .bat as a program like any other"
            );
        }
    }

    /// A change that leaves `on_complete` out keeps the program the config has. That is what a web UI
    /// on a server sends when it saves the folders and the switch, and a save that read "I cannot edit
    /// this" as "set it to nothing" would empty the field on the first save of an unrelated one.
    #[test]
    fn change_without_on_complete_keeps_it() {
        let kept = PathBuf::from("/usr/local/bin/notify");
        let mut downloads = DownloadsConfig {
            unfinished_dir: Some(PathBuf::from("/srv/incomplete")),
            start_paused: false,
            watch_dir: None,
            on_complete: Some(kept.clone()),
        };
        let left_out = DownloadsChange {
            default_dir: PathBuf::from("/srv/downloads"),
            unfinished_dir: Some(PathBuf::from("/srv/incomplete")),
            start_paused: true,
            watch_dir: Some(PathBuf::from("/srv/watch")),
            on_complete: None,
        };

        downloads.apply(&left_out);
        assert_eq!(
            downloads.on_complete,
            Some(kept.clone()),
            "leaving the program out keeps the one the config has"
        );
        assert!(
            downloads.start_paused,
            "the switch the form showed is applied"
        );
        assert_eq!(
            downloads.watch_dir,
            Some(PathBuf::from("/srv/watch")),
            "and so are the folders it showed"
        );

        let named = DownloadsChange {
            on_complete: Some(OnCompleteSet {
                program: Some(PathBuf::from("/usr/local/bin/tell")),
            }),
            ..left_out.clone()
        };
        downloads.apply(&named);
        assert_eq!(
            downloads.on_complete,
            Some(PathBuf::from("/usr/local/bin/tell")),
            "naming a program sets it"
        );

        let cleared = DownloadsChange {
            on_complete: Some(OnCompleteSet { program: None }),
            ..left_out.clone()
        };
        downloads.apply(&cleared);
        assert_eq!(
            downloads.on_complete, None,
            "naming the key with no program clears it"
        );
    }

    /// The view's JSON shape, which is also the shape `GET /api/v1/settings` answers and the shape a
    /// remote screen parses: snake_case names, the limits under `speed`, the program under
    /// `downloads.on_complete`. A field renamed here is a web page reading `undefined`, so the shape is
    /// written down here as a test.
    #[test]
    fn settings_view_json_shape() {
        let view = SettingsView {
            speed: LiveLimits {
                download_kib: 100,
                upload_kib: 0,
                active_downloads: Some(2),
                active_seeds: None,
                active_total: Some(9),
            },
            downloads: DownloadsView {
                default_dir: PathBuf::from("/srv/downloads/alice"),
                unfinished_dir: Some(PathBuf::from("/srv/incomplete/alice")),
                start_paused: true,
                watch_dir: None,
                on_complete: OnCompleteView {
                    program: Some(PathBuf::from("/usr/local/bin/notify")),
                    editable: false,
                },
                watch_problem: Some("No watch folder at that path.".to_string()),
                on_complete_problem: None,
            },
        };

        let json = serde_json::to_value(&view).expect("a view is JSON");
        assert_eq!(
            json["downloads"]["on_complete"]["editable"], false,
            "a server's program is set in its own config file: {json}"
        );
        assert_eq!(
            json["speed"]["download_kib"], 100,
            "the running limits are under `speed`, in KiB/s: {json}"
        );
        assert_eq!(
            json["speed"]["active_seeds"],
            serde_json::Value::Null,
            "an unlimited queue count is null, as it is in config.yaml: {json}"
        );
        assert_eq!(
            json["downloads"]["default_dir"], "/srv/downloads/alice",
            "the folders are absolute paths in plain text: {json}"
        );
        assert_eq!(
            json["downloads"]["on_complete"]["program"], "/usr/local/bin/notify",
            "the program is under the section it belongs to: {json}"
        );
        assert_eq!(
            json["downloads"]["watch_problem"], "No watch folder at that path.",
            "a problem travels with the field it is about: {json}"
        );
        assert_eq!(
            json["downloads"]["on_complete_problem"],
            serde_json::Value::Null,
            "and an absent one is null rather than missing: {json}"
        );

        // The same view reads back as itself: this is the wire shape the backends parse, not only the
        // one they write.
        let again: SettingsView =
            serde_json::from_value(serde_json::to_value(&view).expect("a view is JSON"))
                .expect("the same shape reads back");
        assert_eq!(again, view, "the view round-trips through JSON");

        // The change that goes back the other way, and the one key that may be absent.
        let change = DownloadsChange {
            default_dir: PathBuf::from("/srv/downloads/alice"),
            unfinished_dir: None,
            start_paused: false,
            watch_dir: Some(PathBuf::from("/srv/watch/alice")),
            on_complete: None,
        };
        let json = serde_json::to_value(&change).expect("a change is JSON");
        assert!(
            json.get("on_complete").is_none(),
            "absent means keep the program, so no key is written for it: {json}"
        );

        let cleared = DownloadsChange {
            on_complete: Some(OnCompleteSet { program: None }),
            ..change.clone()
        };
        let json = serde_json::to_value(&cleared).expect("a change is JSON");
        assert_eq!(
            json["on_complete"]["program"],
            serde_json::Value::Null,
            "an explicit null program means run nothing: {json}"
        );
        let again: DownloadsChange =
            serde_json::from_value(serde_json::to_value(&cleared).expect("a change is JSON"))
                .expect("the same shape reads back");
        assert_eq!(
            again, cleared,
            "a cleared program reads back as a cleared program"
        );
    }
}
