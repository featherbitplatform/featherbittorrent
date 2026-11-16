//! The delete question: forget a torrent, and say whether its files go with it.

use crossterm::event::{KeyCode, KeyEvent};
use fbtt_core::{Command, TorrentId};

use crate::format;
use crate::modal::{Action, Answer};
use crate::theme;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

/// The question and its three answers, each spelled out beside its key: which of them deletes a
/// directory of finished files is not something to guess at.
pub(crate) fn lines(name: &str) -> Vec<Line<'static>> {
    let body = Style::new().fg(theme::INK_300);
    let key = Style::new().fg(theme::VIOLET_400);
    vec![Line::from(vec![
        Span::styled(format!("Remove \"{}\"?", format::safe(name)), body),
        Span::styled("  k", key),
        Span::raw(" keep files"),
        Span::styled("   D", key),
        Span::raw(" delete files"),
        Span::styled("   Esc", key),
        Span::raw(" cancel"),
    ])]
}

/// The two answers this dialog takes and the way out of it. `k` keeps what is on disk, `D` (upper
/// case, because it is the destructive one) deletes it, and Esc sends nothing at all. Any other
/// key is not an answer: the question stays until one of these is pressed.
pub(crate) fn on_key(key: KeyEvent, id: &TorrentId) -> Answer {
    let delete_files = match key.code {
        KeyCode::Char('k') => false,
        KeyCode::Char('D') => true,
        KeyCode::Esc => return Answer::Close(Vec::new()),
        _ => return Answer::Stay,
    };
    Answer::Close(vec![Action::Command(Command::Remove {
        id: id.clone(),
        delete_files,
    })])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn the_question_names_the_torrent_it_is_about() {
        let lines = lines("Big.Buck.Bunny.4K");
        let text = lines[0]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert_eq!(
            text,
            "Remove \"Big.Buck.Bunny.4K\"?  k keep files   D delete files   Esc cancel"
        );
    }

    #[test]
    fn only_the_two_answers_and_escape_are_taken() {
        let id = TorrentId::from_wire_hash([0x11; 20]);
        let Answer::Close(commands) = on_key(press(KeyCode::Char('k')), &id) else {
            panic!("k did not answer the question");
        };
        assert_eq!(
            commands,
            vec![Action::Command(Command::Remove {
                id: id.clone(),
                delete_files: false
            })]
        );

        let Answer::Close(commands) = on_key(press(KeyCode::Char('D')), &id) else {
            panic!("D did not answer the question");
        };
        assert_eq!(
            commands,
            vec![Action::Command(Command::Remove {
                id: id.clone(),
                delete_files: true
            })]
        );

        assert!(
            matches!(on_key(press(KeyCode::Esc), &id), Answer::Close(commands) if commands.is_empty()),
            "escape sent something to the backend"
        );
        assert!(
            matches!(on_key(press(KeyCode::Char('j')), &id), Answer::Stay),
            "a key that is not an answer was taken as one"
        );
    }
}
