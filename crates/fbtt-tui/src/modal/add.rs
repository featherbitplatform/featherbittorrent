//! The add dialog: a magnet link or the path of a `.torrent` file, and where its files go.

use std::io::Read;
use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent};
use fbtt_core::{AddSource, Command};

use crate::format;
use crate::modal::{Answer, Input, field, keys_hint, type_into};
use crate::theme;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

/// The two fields' labels, in the order `Tab` walks them.
const LABELS: [&str; 2] = ["Torrent or magnet", "Save to"];
/// A `.torrent` file this big is not a torrent file. The cap is what keeps a mistyped path from
/// reading a whole video into memory on the thread that also has to draw the screen.
const MAX_TORRENT_BYTES: u64 = 16 * 1024 * 1024;
/// What the dialog says about a file it will not read, in the words the brief specifies.
const TOO_BIG: &str = "File is larger than 16 MiB";

/// What the person is typing into the add dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddForm {
    /// The magnet link, or the path of the file to add.
    pub source: Input,
    /// The directory its files go below, prefilled with the default download folder.
    pub dir: Input,
    /// Why the last try did not work, shown in red under the fields.
    pub error: Option<String>,
    /// Which field takes what is typed: 0 is [`source`](Self::source), 1 is [`dir`](Self::dir).
    focused: usize,
}

impl AddForm {
    /// The dialog as `a` opens it: an empty source, and the folder new torrents go to already in
    /// the second field so that adding one to where they always go needs no typing.
    pub fn new(default_dir: &Path) -> AddForm {
        AddForm {
            source: Input::new(String::new()),
            dir: Input::new(default_dir.display().to_string()),
            error: None,
            focused: 0,
        }
    }

    /// Applies one press. `Tab` moves between the two fields, Enter submits, and anything else
    /// types into the field that has the cursor; Esc is the caller's, since it closes every
    /// dialog. Typing clears the last error: it describes a source that no longer exists.
    pub(crate) fn on_key(&mut self, key: KeyEvent, default_dir: &Path) -> Answer {
        match key.code {
            KeyCode::Tab => {
                self.focused = (self.focused + 1) % LABELS.len();
                Answer::Stay
            }
            KeyCode::BackTab => {
                self.focused = (self.focused + LABELS.len() - 1) % LABELS.len();
                Answer::Stay
            }
            KeyCode::Esc => Answer::Close(Vec::new()),
            KeyCode::Enter => self.submit(default_dir),
            _ => {
                if type_into(self.focused_field_mut(), key) {
                    self.error = None;
                }
                Answer::Stay
            }
        }
    }

    /// Enter: a magnet goes straight to the backend, and a path is read here. It is read on the
    /// thread that draws because it is a file the person has just named on the machine this
    /// screen runs on, and it cannot be bigger than [`MAX_TORRENT_BYTES`] (spec 5.2). A failure
    /// keeps the dialog open with the reason in it, so the path can be corrected.
    fn submit(&mut self, default_dir: &Path) -> Answer {
        let source = self.source.text.trim();
        if source.is_empty() {
            return Answer::Stay;
        }
        let dir = chosen_dir(&self.dir, default_dir);
        let command = if source.starts_with("magnet:") {
            Ok(Command::Add {
                source: AddSource::Magnet(source.to_string()),
                dir,
            })
        } else {
            read_torrent(&expand_tilde(source)).map(|bytes| Command::Add {
                source: AddSource::File(bytes),
                dir,
            })
        };

        match command {
            Ok(command) => Answer::Close(vec![command]),
            Err(error) => {
                self.error = Some(error);
                Answer::Stay
            }
        }
    }

    /// The field that takes what is typed.
    fn focused_field_mut(&mut self) -> &mut Input {
        if self.focused == 0 {
            &mut self.source
        } else {
            &mut self.dir
        }
    }
}

/// The dialog's lines: the two fields, why the last try failed when it did, and what each key
/// does here.
pub(crate) fn lines(form: &AddForm) -> Vec<Line<'static>> {
    let width = LABELS
        .iter()
        .map(|label| label.chars().count())
        .max()
        .unwrap_or_default();

    let mut lines = vec![
        field(LABELS[0], &form.source, width, false),
        field(LABELS[1], &form.dir, width, false),
    ];
    if let Some(error) = &form.error {
        lines.push(Line::from(Span::styled(
            format::safe(error),
            Style::new().fg(theme::RED_500),
        )));
    }
    lines.push(keys_hint(&[
        ("Tab", "switch field"),
        ("Enter", "add"),
        ("Esc", "cancel"),
    ]));
    lines
}

/// The directory to send: nothing when the field still holds the default folder or is empty, so
/// a torrent that is not being moved anywhere is not asked to move. An edited folder is expanded
/// the same way a path is.
fn chosen_dir(dir: &Input, default_dir: &Path) -> Option<PathBuf> {
    let text = dir.text.trim();
    if text.is_empty() || text == default_dir.display().to_string() {
        return None;
    }
    Some(expand_tilde(text))
}

/// A leading `~/` is how a person spells their home directory, and `directories` says what that
/// is on this platform. Nothing else is expanded: a `~` anywhere else is part of a name. A host
/// with no home directory gets the text as typed.
fn expand_tilde(text: &str) -> PathBuf {
    let rest = match text {
        "~" => Some(""),
        _ => text.strip_prefix("~/"),
    };
    match rest {
        Some(rest) => directories::BaseDirs::new()
            .map(|dirs| dirs.home_dir().join(rest))
            .unwrap_or_else(|| PathBuf::from(text)),
        None => PathBuf::from(text),
    }
}

/// The bytes of a `.torrent` file, or the reason it could not be read: the operating system's own
/// words for a path that is not there, plus one check of our own for a file too big to read.
///
/// The size is asked for first because that is the cheap answer, but it is not the one trusted:
/// `/dev/zero` reports a length of nothing and answers forever to a read that is not told where to
/// stop, which would freeze the thread that also has to draw the screen. So the read itself is
/// capped at one byte past the limit, and a file that reaches that many bytes is refused whether
/// or not its own length said so.
fn read_torrent(path: &Path) -> Result<Vec<u8>, String> {
    let size = std::fs::metadata(path)
        .map_err(|error| error.to_string())?
        .len();
    if size > MAX_TORRENT_BYTES {
        return Err(TOO_BIG.to_string());
    }
    let file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    let mut bytes = Vec::new();
    file.take(MAX_TORRENT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_TORRENT_BYTES {
        return Err(TOO_BIG.to_string());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_leading_tilde_becomes_the_home_directory() {
        let Some(home) = directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf())
        else {
            return; // a host with no home directory keeps the text as typed, which is what it gets.
        };
        assert_eq!(
            expand_tilde("~/downloads/x.torrent"),
            home.join("downloads/x.torrent")
        );
        assert_eq!(expand_tilde("~"), home);
        // A `~` that is not leading, and a path that is already absolute, are left alone.
        assert_eq!(
            expand_tilde("/a/~b/x.torrent"),
            PathBuf::from("/a/~b/x.torrent")
        );
    }

    #[test]
    fn only_an_edited_folder_is_sent() {
        let default = Path::new("/downloads");
        assert_eq!(chosen_dir(&Input::new("/downloads"), default), None);
        assert_eq!(chosen_dir(&Input::new(""), default), None);
        assert_eq!(
            chosen_dir(&Input::new("/elsewhere"), default),
            Some(PathBuf::from("/elsewhere"))
        );
    }

    #[test]
    fn a_path_that_is_not_there_reports_the_operating_systems_reason() {
        let error = read_torrent(Path::new("/definitely/not/here.torrent"))
            .expect_err("a path that is not there should not read");
        assert!(!error.is_empty(), "the reason for failing was blank");
    }

    #[test]
    #[cfg(unix)]
    fn a_read_that_would_never_end_is_stopped_by_the_cap() {
        // `/dev/zero` says it is nothing long and would answer forever to an unbounded read, so
        // the cap has to be enforced by how much was actually taken rather than by what the path
        // claims. A read that hung here would hang the thread that draws the screen.
        let error = read_torrent(Path::new("/dev/zero"))
            .expect_err("an endless source should not read as a torrent");
        assert_eq!(error, "File is larger than 16 MiB");
    }
}
