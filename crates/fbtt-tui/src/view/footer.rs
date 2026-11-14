//! The key hints along the bottom, and the one question the model can ask there.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::keys;
use crate::model::Model;
use crate::theme;

/// Draws the hints, or the confirmation while a `q` is waiting to be answered. Task 6 adds
/// the last notice to this line too.
pub(crate) fn draw(frame: &mut Frame, area: Rect, model: &Model) {
    if area.is_empty() {
        return;
    }
    let line = if model.quit_armed() {
        confirmation()
    } else {
        hints()
    };
    frame.render_widget(Paragraph::new(line), area);
}

/// The actions a person reaches for without looking, in the order they are wanted: the key
/// in the lifted accent, and what it does beside it. Everything on this line is a real entry
/// of the key table, which its own test keeps true.
fn hints() -> Line<'static> {
    let mut spans = Vec::with_capacity(keys::FOOTER_KEYS.len() * 2);
    for key in keys::FOOTER_KEYS {
        let Some((_, action)) = keys::KEYS.iter().find(|(name, _)| name == key) else {
            continue;
        };
        if !spans.is_empty() {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(*key, Style::new().fg(theme::VIOLET_400)));
        spans.push(Span::raw(format!(" {action}")));
    }
    Line::from(spans)
}

/// A `q` that found something still downloading: what each answer means, in the warning
/// colour so it cannot be read as another hint.
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
        // Seven hints is already tight on an eighty-column terminal: adding one more should
        // mean moving one out of FOOTER_KEYS, not wrapping the footer.
        let width: usize = keys::FOOTER_KEYS
            .iter()
            .map(|key| {
                let action = keys::KEYS
                    .iter()
                    .find(|(name, _)| name == key)
                    .map_or("", |(_, action)| *action);
                key.chars().count() + action.chars().count() + 1
            })
            .sum::<usize>()
            + (keys::FOOTER_KEYS.len() - 1) * 2;
        assert!(width <= 80, "the footer needs {width} cells");
    }
}
