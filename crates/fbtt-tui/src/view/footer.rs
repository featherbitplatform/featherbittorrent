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
    let line = if model.quit_armed() {
        confirmation()
    } else if let Some(notice) = model.notice() {
        notice_line(notice)
    } else if model.files_focused {
        hints_for(keys::FILE_LIST_KEYS)
    } else {
        hints_for(keys::FOOTER_KEYS)
    };
    frame.render_widget(Paragraph::new(line), area);
}

/// The hints for one set of keys, in the order they are wanted: each key in the lifted accent, and
/// what it does beside it. Every word on this line comes from the key table, which a test keeps
/// true, so a hint cannot promise a key that does nothing.
fn hints_for(wanted: &[&'static str]) -> Line<'static> {
    let mut spans = Vec::with_capacity(wanted.len() * 2);
    for key in wanted {
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

/// The last thing the backend said, in the colour that says whether it was good news or bad news.
/// How long it stays is the model's business; this line only shows it while it has one.
fn notice_line(notice: &Notice) -> Line<'static> {
    let (text, style) = match notice {
        Notice::Info(text) => (text, Style::new().fg(theme::INK_300)),
        Notice::Error(text) => (text, Style::new().fg(theme::RED_500)),
    };
    Line::from(Span::styled(format::safe(text), style))
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
        // Seven hints is already tight on an eighty-column terminal: adding one more should mean
        // moving one out of the list, not wrapping the footer.
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

    #[test]
    fn a_notice_is_drawn_as_one_line() {
        let text = "a tracker refused an announce";
        let line = notice_line(&Notice::Error(text.to_string()));
        assert_eq!(line.width(), text.chars().count());
    }
}
