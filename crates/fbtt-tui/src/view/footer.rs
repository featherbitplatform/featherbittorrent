//! The key hints along the bottom, and the one question the model can ask there.

use fbtt_core::Notice;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::format;
use crate::keys;
use crate::model::Model;
use crate::theme;

/// Draws the one line along the bottom: whichever of these the person needs to know right now.
/// A `q` waiting to be confirmed comes first, then what the backend just said, then the keys that
/// work where the cursor is, and last the hints everyone ends up reading anyway. A filter being
/// typed draws its own line over this one.
pub(crate) fn draw(frame: &mut Frame, area: Rect, model: &Model) {
    if area.is_empty() {
        return;
    }
    let row_cells = usize::from(area.width);
    let line = if model.quit_armed() {
        confirmation()
    } else if let Some(notice) = model.notice() {
        notice_line(notice).unwrap_or_else(|| {
            if model.files_focused {
                hints_from(&fit_hints(keys::FILE_LIST_HINTS, row_cells))
            } else {
                hints_for(keys::FOOTER_KEYS, row_cells)
            }
        })
    } else if model.files_focused {
        hints_from(&fit_hints(keys::FILE_LIST_HINTS, row_cells))
    } else {
        hints_for(keys::FOOTER_KEYS, row_cells)
    };
    frame.render_widget(Paragraph::new(line), area);
}

/// Cells the footer line has to hold: the hints, two cells between each of them.
fn line_cells(described: &[(&'static str, &'static str)]) -> usize {
    described
        .iter()
        .map(|(key, action)| key.chars().count() + action.chars().count() + 1)
        .sum::<usize>()
        + described.len().saturating_sub(1) * 2
}

/// The hints for one set of keys, in the order they are wanted. Everything on this line comes from
/// the key table, which a test keeps true, so a hint cannot promise a key that does nothing.
/// `width` is the cells the row has: when the hints do not fit they are dropped from the back,
/// but `? help` and `q quit` always stay at the end, in that order - help is where a person finds
/// every key the footer cannot show, and `q quit` is the one a person looks for when something
/// goes wrong.
fn hints_for(wanted: &[&'static str], width: usize) -> Line<'static> {
    let described: Vec<(&'static str, &'static str)> = wanted
        .iter()
        .filter_map(|key| {
            keys::KEYS
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, action)| (*key, *action))
        })
        .collect();
    let shown = fit_hints(&described, width);
    hints_from(&shown)
}

/// The hints that fit in `limit` cells, in their original order. What does not fit is dropped
/// from the back, one at a time, but the last two hints are never dropped: `?` opens the list
/// of every key, so a person can always find the dropped ones, and `q` is the one a person
/// reaches for when something goes wrong.
fn fit_hints(
    described: &[(&'static str, &'static str)],
    limit: usize,
) -> Vec<(&'static str, &'static str)> {
    let n = described.len();
    if n <= 2 || line_cells(described) <= limit {
        return described.to_vec();
    }
    let (droppable, kept) = described.split_at(n - 2);
    for count in (0..=droppable.len()).rev() {
        let mut shown: Vec<(&'static str, &'static str)> = droppable[..count].to_vec();
        shown.extend_from_slice(kept);
        if line_cells(&shown) <= limit {
            return shown;
        }
    }
    vec![described[n - 2], described[n - 1]]
}

/// The hints given as a key and what it does: each key in the lifted accent, and its meaning beside
/// it. This is the shape for a hint that has to say something other than what the table says, which
/// is what the file list does with `Esc`: there it leaves the list, it does not close a dialog.
fn hints_from(described: &[(&'static str, &'static str)]) -> Line<'static> {
    let mut spans = Vec::with_capacity(described.len() * 2);
    for (key, action) in described {
        if !spans.is_empty() {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(*key, Style::new().fg(theme::VIOLET_400)));
        spans.push(Span::raw(format!(" {action}")));
    }
    Line::from(spans)
}

/// The last thing the backend said, in the colour that says whether it was good news or bad news.
/// How long it stays is the model's business; this line only shows it while it has one. A refused sign-in is
/// bad news about the connection like any other error: who has to act on it is the run's business, not the
/// footer's, so it is drawn in the same colour with the same text. A file to play is drawn as nothing at all:
/// it is a job the run does, and the line that lands on the footer is the one the player reports.
fn notice_line(notice: &Notice) -> Option<Line<'static>> {
    let (text, style) = match notice {
        Notice::Info(text) => (text, Style::new().fg(theme::INK_300)),
        Notice::Error(text) | Notice::SignInRefused(text) => {
            (text, Style::new().fg(theme::RED_500))
        }
        // Not drawn; the player's own line is what the footer gets.
        Notice::Play(_) => return None,
    };
    Some(Line::from(Span::styled(format::safe(text), style)))
}

/// A `q` that found something still downloading: what each answer means, in the warning colour so
/// it cannot be read as another hint.
fn confirmation() -> Line<'static> {
    let key = Style::new().fg(theme::VIOLET_400);
    let warning = Style::new().fg(theme::AMBER_500);
    Line::from(vec![
        Span::styled("something is still downloading: ", warning),
        Span::styled("q", key),
        Span::styled(" again quits and saves, ", warning),
        Span::styled("y", key),
        Span::styled(" agrees; any other key keeps going", warning),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hints_line_stays_one_line_of_its_own() {
        // Nine hints does not all fit an eighty-column row, and the rule for what does not fit
        // is to drop from the back, one at a time, keeping `? help` and `q quit` at the end:
        // help is where a person finds the dropped keys, and `q` is the one a person reaches
        // for when something goes wrong. What is drawn fits the row, and keeps its order.
        let wanted: Vec<(&'static str, &'static str)> = keys::FOOTER_KEYS
            .iter()
            .filter_map(|key| {
                keys::KEYS
                    .iter()
                    .find(|(name, _)| name == key)
                    .map(|(_, action)| (*key, *action))
            })
            .collect();
        let row_cells = 80;
        assert!(
            line_cells(&wanted) > row_cells,
            "all nine hints fit, so there is nothing to drop: {} cells",
            line_cells(&wanted)
        );
        let shown = fit_hints(&wanted, row_cells);
        assert!(
            line_cells(&shown) <= row_cells,
            "the footer wrapped the row"
        );
        assert!(shown.len() < wanted.len(), "no hint needed dropping");
        // The two kept hints are the last two of the list, in order.
        assert_eq!(
            shown.last().map(|(key, _)| *key),
            Some("q"),
            "`q quit` is not at the end"
        );
        assert_eq!(
            shown.get(shown.len() - 2).map(|(key, _)| *key),
            Some("?"),
            "`? help` is not before `q quit`"
        );
        // What is shown is a prefix of the list followed by the two kept hints: no gaps.
        let (droppable, kept) = wanted.split_at(wanted.len() - 2);
        let prefix_len = shown.len() - kept.len();
        assert_eq!(
            &shown[..prefix_len],
            &droppable[..prefix_len],
            "a dropped hint left a gap in the order"
        );
    }

    #[test]
    fn a_notice_is_drawn_as_one_line() {
        let text = "a tracker refused an announce";
        let line = notice_line(&Notice::Error(text.to_string())).expect("an error is drawn");
        assert_eq!(line.width(), text.chars().count());
    }

    /// A refused sign-in is drawn exactly like the error it is: the same words in the same colour, on one
    /// line. What somebody does about it is the run's business; a screen only has to show it as bad news.
    #[test]
    fn a_refused_sign_in_is_drawn_like_an_error() {
        let text = "sign-in was refused: the password was wrong";
        let refusal =
            notice_line(&Notice::SignInRefused(text.to_string())).expect("a refusal is drawn");
        assert_eq!(
            notice_line(&Notice::Error(text.to_string())).as_ref(),
            Some(&refusal)
        );
        assert_eq!(refusal.width(), text.chars().count());
    }

    /// A file to play is not a line the footer shows: the run does the opening, and the line that lands is
    /// the player's. The footer falls back to its hints.
    #[test]
    fn a_play_notice_is_not_drawn() {
        use fbtt_core::{PlaySource, PlayTarget, StreamUrl};
        let play = Notice::Play(PlayTarget::new(
            "a film",
            PlaySource::Url(StreamUrl::new("http://h/x?token=abc".into())),
        ));
        assert!(notice_line(&play).is_none(), "a play notice draws nothing");
    }
}
