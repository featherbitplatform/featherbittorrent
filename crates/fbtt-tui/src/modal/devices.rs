//! The devices a server lets sign in: who is on its list, and what to do about it.
//!
//! A device is a name plus an OpenSSH public key, the same shape as SSH's `authorized_keys`, and only a
//! server has one - which is why `K` opens this screen when the torrents are coming from somewhere else
//! and does nothing on a library that runs its own engine. The list itself is the backend's to hold: it
//! arrives in a snapshot and is replaced there whenever the server answers again, so an add or a removal
//! shows up as the list changing rather than as this dialog editing its own rows.

use crossterm::event::{KeyCode, KeyEvent};
use fbtt_core::{Command, DeviceCommand, Snapshot};

use crate::format;
use crate::modal::{Action, Answer, Input, advice, field, keys_hint, problem, type_into};
use crate::theme;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

/// The marker in front of the device the cursor is on, the same one the torrent table uses.
const MARKER: &str = "▌";
/// The two fields of an add, in the order `Tab` walks them.
const NAME_LABEL: &str = "Name";
const KEY_LABEL: &str = "Public key";

/// What the person is doing with the device list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevicesForm {
    mode: Mode,
}

/// Which of the screen's three states is up.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Mode {
    /// The list, with the cursor on one of its rows.
    List { cursor: usize },
    /// A new device: what to call it, and the public key line pasted in beside it.
    Add {
        name: Input,
        public_key: Input,
        focused: usize,
        error: Option<String>,
    },
    /// `x` was pressed on a row, and the removal waits for a `y`. The name is kept here rather than read
    /// from the snapshot again, so what gets removed is the device the question named.
    ConfirmRemove { name: String },
}

impl DevicesForm {
    /// The list as `K` opens it: empty until the server answers the `Devices(List)` that opening sends.
    pub fn new() -> DevicesForm {
        DevicesForm {
            mode: Mode::List { cursor: 0 },
        }
    }

    /// Applies one press. In the list, `a` starts an add and `x` asks about removing the device under the
    /// cursor; the add takes typing and Enter; the question takes `y` and treats anything else as a no.
    pub(crate) fn on_key(&mut self, key: KeyEvent, snap: &Snapshot) -> Answer {
        // The state is taken out, answered, and put back, because the states hold what was typed into
        // them and an answer often means moving one state's text into another.
        match std::mem::replace(&mut self.mode, Mode::List { cursor: 0 }) {
            Mode::List { mut cursor } => {
                let count = snap.devices.as_ref().map_or(0, Vec::len);
                let answer = match key.code {
                    KeyCode::Esc => return Answer::Close(Vec::new()),
                    KeyCode::Char('j') | KeyCode::Down => {
                        cursor = (cursor + 1).min(count.saturating_sub(1));
                        Answer::Stay
                    }
                    KeyCode::Char('k') | KeyCode::Up => {
                        cursor = cursor.saturating_sub(1);
                        Answer::Stay
                    }
                    KeyCode::Char('a') => {
                        self.mode = Mode::Add {
                            name: Input::new(String::new()),
                            public_key: Input::new(String::new()),
                            focused: 0,
                            error: None,
                        };
                        return Answer::Stay;
                    }
                    KeyCode::Char('x') => match row_name(snap, cursor) {
                        Some(name) => {
                            self.mode = Mode::ConfirmRemove { name };
                            return Answer::Stay;
                        }
                        None => Answer::Stay,
                    },
                    _ => Answer::Stay,
                };
                self.mode = Mode::List { cursor };
                answer
            }
            Mode::Add {
                mut name,
                mut public_key,
                mut focused,
                error,
            } => match key.code {
                KeyCode::Esc => {
                    self.mode = Mode::List { cursor: 0 };
                    Answer::Stay
                }
                // Two fields, so either direction between them is the same one step.
                KeyCode::Tab | KeyCode::BackTab => {
                    focused = (focused + 1) % 2;
                    self.mode = Mode::Add {
                        name,
                        public_key,
                        focused,
                        error,
                    };
                    Answer::Stay
                }
                KeyCode::Enter => {
                    let device = name.text.trim().to_string();
                    let key_line = public_key.text.trim().to_string();
                    if device.is_empty() || key_line.is_empty() {
                        self.mode = Mode::Add {
                            name,
                            public_key,
                            focused,
                            error: Some("A device needs a name and its public key.".to_string()),
                        };
                        return Answer::Stay;
                    }
                    self.mode = Mode::List { cursor: 0 };
                    // The server's answer is the list to show, so this stays up and lets the snapshot
                    // bring the new device in. A refusal arrives as a line about itself.
                    Answer::Keep(vec![Action::Command(Command::Devices(
                        DeviceCommand::Add {
                            name: device,
                            public_key: key_line,
                        },
                    ))])
                }
                _ => {
                    let field = if focused == 0 {
                        &mut name
                    } else {
                        &mut public_key
                    };
                    let typed = type_into(field, key);
                    self.mode = Mode::Add {
                        name,
                        public_key,
                        focused,
                        // The reason describes text that is no longer there.
                        error: if typed { None } else { error },
                    };
                    Answer::Stay
                }
            },
            Mode::ConfirmRemove { name } => {
                let confirmed = key.code == KeyCode::Char('y');
                self.mode = Mode::List { cursor: 0 };
                if confirmed {
                    Answer::Keep(vec![Action::Command(Command::Devices(
                        DeviceCommand::Remove(name),
                    ))])
                } else {
                    Answer::Stay
                }
            }
        }
    }
}

impl Default for DevicesForm {
    fn default() -> Self {
        DevicesForm::new()
    }
}

/// The device the cursor is on, by name: what a removal is asked about, and what it will be done to.
fn row_name(snap: &Snapshot, cursor: usize) -> Option<String> {
    let devices = snap.devices.as_ref()?;
    let at = cursor.min(devices.len().saturating_sub(1));
    devices.get(at).map(|device| device.name.clone())
}

/// The dialog's lines: the list with its fingerprints, or the add's two fields, or the question about a
/// removal - and what each key does in whichever of them is up.
pub(crate) fn lines(form: &DevicesForm, snap: &Snapshot) -> Vec<Line<'static>> {
    let muted = Style::new().fg(theme::INK_400);
    match &form.mode {
        Mode::List { cursor } => {
            let mut lines: Vec<Line<'static>> = Vec::new();
            match snap.devices.as_ref() {
                None => lines.push(advice("asking the server...")),
                Some(devices) if devices.is_empty() => lines.push(advice("no devices yet")),
                Some(devices) => {
                    let at = (*cursor).min(devices.len().saturating_sub(1));
                    for (index, device) in devices.iter().enumerate() {
                        // The fingerprint is the part worth checking against the key that was pasted in,
                        // so it goes beside the name rather than under it. Both are server text.
                        lines.push(Line::from(vec![
                            Span::styled(
                                if index == at { MARKER } else { " " },
                                Style::new().fg(theme::VIOLET_500),
                            ),
                            Span::raw(" "),
                            Span::styled(
                                format::safe(&device.name),
                                Style::new().fg(theme::INK_300),
                            ),
                            Span::raw("  "),
                            Span::styled(format::safe(&device.fingerprint), muted),
                        ]));
                    }
                }
            }
            lines.push(keys_hint(&[
                ("a", "add"),
                ("x", "remove"),
                ("Esc", "close"),
            ]));
            lines
        }
        Mode::Add {
            name,
            public_key,
            focused,
            error,
        } => {
            let width = NAME_LABEL.chars().count().max(KEY_LABEL.chars().count());
            let mut lines = vec![
                field(NAME_LABEL, name, width, false),
                field(
                    KEY_LABEL,
                    public_key,
                    width,
                    *focused == 1 && error.is_some(),
                ),
            ];
            if let Some(error) = error {
                lines.push(problem(error));
            }
            lines.push(keys_hint(&[
                ("Tab", "field"),
                ("Enter", "add"),
                ("Esc", "back"),
            ]));
            lines
        }
        Mode::ConfirmRemove { name } => {
            vec![Line::from(vec![
                Span::styled(
                    format!("Remove \"{}\"?", format::safe(name)),
                    Style::new().fg(theme::INK_300),
                ),
                Span::raw("  "),
                Span::styled("y", Style::new().fg(theme::VIOLET_400)),
                Span::raw(" confirm"),
                Span::raw("   "),
                Span::styled("Esc", Style::new().fg(theme::VIOLET_400)),
                Span::raw(" cancel"),
            ])]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;
    use fbtt_core::{Connection, DeviceRow, LiveLimits, Totals};
    use std::path::PathBuf;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// A snapshot whose only content is the device list a test is about.
    fn snap(devices: Option<Vec<DeviceRow>>) -> Snapshot {
        Snapshot {
            connection: Connection::Remote {
                name: "bit.example".to_string(),
                secure: true,
                auth: "password".to_string(),
                state: fbtt_core::LinkState::Connected,
            },
            torrents: Vec::new(),
            totals: Totals {
                down_rate: 0,
                up_rate: 0,
                dht_nodes: None,
            },
            limits: LiveLimits {
                download_kib: 0,
                upload_kib: 0,
                active_downloads: None,
                active_seeds: None,
                active_total: None,
            },
            detail: None,
            default_dir: PathBuf::from("/downloads"),
            dirs: None,
            devices,
            settings: fbtt_core::SettingsState::NotLoaded,
        }
    }

    fn device(name: &str) -> DeviceRow {
        DeviceRow {
            name: name.to_string(),
            fingerprint: format!("SHA256:{name}fingerprint"),
        }
    }

    #[test]
    fn an_add_and_a_removal_send_their_commands_and_wait_for_the_list() {
        let snap = snap(Some(vec![device("laptop"), device("phone")]));

        // The add needs both halves: a name to call the device by and the key line to authorise.
        let mut form = DevicesForm::new();
        assert!(matches!(
            form.on_key(press(KeyCode::Char('a')), &snap),
            Answer::Stay
        ));
        for c in "laptop".chars() {
            form.on_key(press(KeyCode::Char(c)), &snap);
        }
        assert!(matches!(
            form.on_key(press(KeyCode::Enter), &snap),
            Answer::Stay
        ));

        form.on_key(press(KeyCode::Tab), &snap);
        for c in "ssh-ed25519 AAAAC3Nza".chars() {
            form.on_key(press(KeyCode::Char(c)), &snap);
        }
        let actions = form.on_key(press(KeyCode::Enter), &snap);
        assert_eq!(
            actions,
            Answer::Keep(vec![Action::Command(Command::Devices(
                DeviceCommand::Add {
                    name: "laptop".to_string(),
                    public_key: "ssh-ed25519 AAAAC3Nza".to_string(),
                }
            ))])
        );

        // A removal is asked about before it is done, and only `y` answers.
        let mut form = DevicesForm::new();
        assert!(matches!(
            form.on_key(press(KeyCode::Char('x')), &snap),
            Answer::Stay
        ));
        assert!(matches!(
            form.on_key(press(KeyCode::Char('j')), &snap),
            Answer::Stay,
        ));
        let mut asking = DevicesForm::new();
        asking.on_key(press(KeyCode::Char('x')), &snap);
        assert_eq!(
            asking.on_key(press(KeyCode::Char('y')), &snap),
            Answer::Keep(vec![Action::Command(Command::Devices(
                DeviceCommand::Remove("laptop".to_string())
            ))])
        );

        // Cancelling the question sends nothing.
        let mut cancelled = DevicesForm::new();
        cancelled.on_key(press(KeyCode::Char('x')), &snap);
        assert!(matches!(
            cancelled.on_key(press(KeyCode::Esc), &snap),
            Answer::Stay
        ));
    }

    #[test]
    fn the_cursor_only_reaches_devices_that_are_there() {
        let empty = snap(Some(Vec::new()));
        let mut form = DevicesForm::new();
        assert!(matches!(
            form.on_key(press(KeyCode::Char('x')), &empty),
            Answer::Stay
        ));

        // Nothing listed means no name to ask about, so `x` cannot open the question.
        let snap = snap(Some(vec![device("laptop")]));
        let mut form = DevicesForm::new();
        form.on_key(press(KeyCode::Char('j')), &snap);
        form.on_key(press(KeyCode::Char('j')), &snap);
        assert!(matches!(
            form.on_key(press(KeyCode::Char('x')), &snap),
            Answer::Stay
        ));
        assert_eq!(row_name(&snap, 9).as_deref(), Some("laptop"));
        assert_eq!(row_name(&empty, 0), None);
    }
}
