//! The settings dialog: the rates and queue counts the session is running under right now.

use crossterm::event::{KeyCode, KeyEvent};
use fbtt_core::{Command, LiveLimits};

use crate::modal::{Action, Answer, Input, field, keys_hint, type_into};
use ratatui::text::Line;

/// The five fields, in the order `Tab` walks them and in the order [`SettingsForm::fields`]
/// holds them. The first two are rates in KiB/s, the rest are counts of torrents.
const LABELS: [&str; 5] = [
    "Download limit KiB/s",
    "Upload limit KiB/s",
    "Active downloads",
    "Active seeds",
    "Active total",
];
/// How many of the fields are rates. They come first, so the queue counts start at this index.
const RATE_FIELDS: usize = 2;

/// What the person is typing into the settings dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsForm {
    /// The five numbers, in the order `LABELS` lists them.
    pub fields: [Input; LABELS.len()],
    /// Which field was refused last and is drawn red, `None` when nothing has been refused.
    /// Editing a field clears its mark, so the warning goes away as soon as it is wrong no more.
    pub invalid: Option<usize>,
    /// Which field takes what is typed.
    focused: usize,
}

impl SettingsForm {
    /// The dialog as `S` opens it, prefilled with the limits the session is running under. An
    /// unlimited rate is spelled `0` and an unlimited queue is left blank, which is how
    /// [`LiveLimits`] tells them apart.
    pub fn new(limits: &LiveLimits) -> SettingsForm {
        let count = |count: Option<usize>| count.map_or(String::new(), |count| count.to_string());
        SettingsForm {
            fields: [
                Input::new(limits.download_kib.to_string()),
                Input::new(limits.upload_kib.to_string()),
                Input::new(count(limits.active_downloads)),
                Input::new(count(limits.active_seeds)),
                Input::new(count(limits.active_total)),
            ],
            invalid: None,
            focused: 0,
        }
    }

    /// Applies one press: `Tab` moves between the fields, Enter sends all five together, and
    /// anything else types. Esc is the caller's.
    pub(crate) fn on_key(&mut self, key: KeyEvent) -> Answer {
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
            KeyCode::Enter => self.apply(),
            _ => {
                if type_into(&mut self.fields[self.focused], key)
                    && self.invalid == Some(self.focused)
                {
                    self.invalid = None;
                }
                Answer::Stay
            }
        }
    }

    /// Enter: parse the five numbers and send them as one change. A field that is not a number is
    /// marked red and nothing is sent, so half-typed text cannot quietly become unlimited.
    fn apply(&mut self) -> Answer {
        let mut values = [0u64; LABELS.len()];
        for (index, field) in self.fields.iter().enumerate() {
            let Some(value) = number(&field.text) else {
                self.invalid = Some(index);
                return Answer::Stay;
            };
            values[index] = value;
        }

        Answer::Close(vec![Action::Command(Command::SetLimits(LiveLimits {
            download_kib: values[0],
            upload_kib: values[1],
            active_downloads: queue_count(values[RATE_FIELDS]),
            active_seeds: queue_count(values[RATE_FIELDS + 1]),
            active_total: queue_count(values[RATE_FIELDS + 2]),
        }))])
    }
}

/// The dialog's lines: the five fields, what `0` means, and what each key does here.
pub(crate) fn lines(form: &SettingsForm) -> Vec<Line<'static>> {
    let width = LABELS
        .iter()
        .map(|label| label.chars().count())
        .max()
        .unwrap_or_default();

    let mut lines: Vec<Line<'static>> = LABELS
        .iter()
        .enumerate()
        .map(|(index, label)| {
            field(
                label,
                &form.fields[index],
                width,
                form.invalid == Some(index),
            )
        })
        .collect();
    lines.push(Line::from("0 or nothing means unlimited"));
    lines.push(keys_hint(&[
        ("Tab", "switch field"),
        ("Enter", "apply"),
        ("Esc", "cancel"),
    ]));
    lines
}

/// One field as a number: nothing typed, or only spaces, is the same as `0`, which every one of
/// the five means as "no limit". Anything else has to be digits and nothing more.
fn number(text: &str) -> Option<u64> {
    let text = text.trim();
    if text.is_empty() {
        return Some(0);
    }
    text.parse().ok()
}

/// A queue count: `0` is no limit at all, which [`LiveLimits`] spells `None`. A number too big
/// for this platform's `usize` is the biggest one it has rather than a wrapped-around small one.
fn queue_count(value: u64) -> Option<usize> {
    if value == 0 {
        return None;
    }
    Some(usize::try_from(value).unwrap_or(usize::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_and_zero_are_the_same_number() {
        assert_eq!(number(""), Some(0));
        assert_eq!(number(" 2048 "), Some(2048));
        assert_eq!(number("1.5"), None);
        assert_eq!(number("abc"), None);
        assert_eq!(number("-3"), None);
    }

    #[test]
    fn an_unlimited_queue_is_no_queue() {
        assert_eq!(queue_count(0), None);
        assert_eq!(queue_count(5), Some(5));
    }

    #[test]
    fn the_prefilled_fields_hold_the_running_limits() {
        let limits = LiveLimits {
            download_kib: 0,
            upload_kib: 512,
            active_downloads: None,
            active_seeds: Some(3),
            active_total: Some(4),
        };
        let form = SettingsForm::new(&limits);
        let texts: Vec<String> = form.fields.iter().map(|field| field.text.clone()).collect();
        assert_eq!(texts, ["0", "512", "", "3", "4"]);
        assert_eq!(form.invalid, None);
    }
}
