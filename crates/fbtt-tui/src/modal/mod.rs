//! Dialogs over the main view: what each one answers, and how it is drawn.
//!
//! A dialog owns the keyboard while it is up: every press goes to it and none to the table, so a
//! `q` typed into a magnet field adds a letter instead of stopping the app. Everything but the
//! filter draws as a box in the middle of the screen, on the surface colour inside a border of the
//! rule colour. The filter is one line along the bottom, where the design puts the key hints, and
//! it changes the table while it is typed rather than when Enter is pressed.

mod add;
mod delete;
mod help;
mod input;
mod settings;

pub use add::AddForm;
pub use input::Input;
pub use settings::SettingsForm;

use crossterm::event::{KeyCode, KeyEvent};
use fbtt_core::{Command, Snapshot, TorrentId, TorrentRow};
use ratatui::Frame;
use ratatui::layout::{Margin, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Fill, Paragraph};

use crate::model::Model;
use crate::theme;

/// Cells between a field's label and what has been typed in it.
const LABEL_GAP: usize = 2;

/// A dialog open on top of the main view.
#[derive(Debug, Clone, PartialEq)]
pub enum Modal {
    /// A magnet link or a `.torrent` file, and where its files go.
    Add(AddForm),
    /// Whether to forget a torrent with its files or without them.
    Delete {
        /// The torrent the question is about.
        id: TorrentId,
        /// Its name, so the question can say which one it means.
        name: String,
    },
    /// The rates and queue counts the session runs under.
    Settings(SettingsForm),
    /// Every key and what pressing it does.
    Help,
    /// A name fragment typed along the bottom, which filters the table as it is typed.
    Filter,
}

impl Modal {
    /// The add dialog, with the folder new torrents go to already in its second field.
    pub fn add(snap: &Snapshot) -> Modal {
        Modal::Add(AddForm::new(&snap.default_dir))
    }

    /// The settings dialog, prefilled with the limits this snapshot says are running.
    pub fn settings(snap: &Snapshot) -> Modal {
        Modal::Settings(SettingsForm::new(&snap.limits))
    }

    /// The delete question about one torrent row.
    pub fn delete(row: &TorrentRow) -> Modal {
        Modal::Delete {
            id: row.id.clone(),
            name: row.name.clone(),
        }
    }

    /// Applies one press to the dialog. `model` is here for the filter, whose text *is* the
    /// model's filter so that the table changes while it is typed; the rest answer themselves.
    fn on_key(&mut self, key: KeyEvent, model: &mut Model, snap: &Snapshot) -> Answer {
        match self {
            Modal::Add(form) => form.on_key(key, &snap.default_dir),
            Modal::Settings(form) => form.on_key(key),
            Modal::Delete { id, .. } => delete::on_key(key, id),
            Modal::Help => help::on_key(key),
            Modal::Filter => filter_on_key(model, key, snap),
        }
    }
}

/// What one press did to a dialog.
#[derive(Debug)]
pub(crate) enum Answer {
    /// The dialog stays up and nothing goes to the backend.
    Stay,
    /// The dialog comes down, with whatever it had to say. Closing without answering sends
    /// nothing, which is what its `Vec` holds then.
    Close(Vec<Command>),
}

/// Applies one press to the open dialog, which owns the keyboard while it is up: none of it
/// reaches the table, a sort, or a quit that is waiting to be confirmed.
pub(crate) fn handle(model: &mut Model, key: KeyEvent, snap: &Snapshot) -> Vec<Command> {
    let Some(mut modal) = model.modal.take() else {
        return Vec::new();
    };
    let answer = modal.on_key(key, model, snap);
    if matches!(answer, Answer::Stay) {
        model.modal = Some(modal);
    }
    match answer {
        Answer::Stay => Vec::new(),
        Answer::Close(commands) => commands,
    }
}

/// The filter field's keys: typing changes the table straight away and Enter keeps what has been
/// typed, while Esc throws it away. The cursor rides at the end of a filter that only ever grows
/// at the end, so this field has no left and right to answer.
fn filter_on_key(model: &mut Model, key: KeyEvent, snap: &Snapshot) -> Answer {
    match key.code {
        KeyCode::Enter => Answer::Close(Vec::new()),
        KeyCode::Esc => {
            model.filter.clear();
            model.sync_selection(snap);
            Answer::Close(Vec::new())
        }
        KeyCode::Backspace => {
            model.filter.pop();
            model.sync_selection(snap);
            Answer::Stay
        }
        KeyCode::Char(c) => {
            model.filter.push(c);
            model.sync_selection(snap);
            Answer::Stay
        }
        _ => Answer::Stay,
    }
}

/// Draws the open dialog over a view that has already been drawn. Only the box it adds is blanked,
/// so the table behind it stays exactly as it was.
pub(crate) fn draw(frame: &mut Frame, model: &Model) {
    let area = frame.area();
    match &model.modal {
        Some(Modal::Add(form)) => draw_box(frame, area, "Add torrent", add::lines(form)),
        Some(Modal::Delete { name, .. }) => {
            draw_box(frame, area, "Delete torrent", delete::lines(name));
        }
        Some(Modal::Settings(form)) => draw_box(frame, area, "Settings", settings::lines(form)),
        Some(Modal::Help) => draw_box(frame, area, "Keys", help::lines(area)),
        Some(Modal::Filter) => draw_filter(frame, area, model),
        None => {}
    }
}

/// The box every dialog but the filter gets: its lines padded one cell inside a border of the rule
/// colour on the surface colour, sized to what they need and never bigger than the screen.
fn draw_box(frame: &mut Frame, area: Rect, title: &str, lines: Vec<Line<'static>>) {
    let widest = lines
        .iter()
        .map(|line| u16::try_from(line.width()).unwrap_or(u16::MAX))
        .max()
        .unwrap_or(0);
    // A cell of padding on each side, plus the two columns of border.
    let width = (widest + 4).min(area.width);
    let height = u16::try_from(lines.len() + 2)
        .unwrap_or(u16::MAX)
        .min(area.height);

    let box_area = centred(area, width, height);
    frame.render_widget(Fill::new(" ").bg(theme::INK_850), box_area);
    frame.render_widget(
        Block::bordered()
            .title(Line::from(Span::styled(
                title,
                Style::new().fg(theme::INK_50),
            )))
            .border_style(Style::new().fg(theme::INK_600)),
        box_area,
    );
    frame.render_widget(Paragraph::new(lines), box_area.inner(Margin::new(1, 1)));
}

/// The filter along the bottom of the screen, where the key hints are and go while it is up. It
/// has no border because it is a line of the screen rather than a box over it.
fn draw_filter(frame: &mut Frame, area: Rect, model: &Model) {
    if area.height == 0 {
        return;
    }
    let row = Rect::new(area.x, area.bottom() - 1, area.width, 1);
    frame.render_widget(Fill::new(" ").bg(theme::INK_850), row);
    frame.render_widget(Paragraph::new(filter_line(model)), row);
}

/// The word `filter`, what has been typed after it, and the cursor after that. The text is the
/// model's own filter drawn as it was typed: spaces are part of what it matches, so they are not
/// collapsed on the way to the screen.
fn filter_line(model: &Model) -> Line<'static> {
    Line::from(vec![
        Span::styled("filter", Style::new().fg(theme::INK_400)),
        Span::raw(" "),
        Span::styled(model.filter.clone(), Style::new().fg(theme::INK_300)),
        Span::styled(" ", Style::new().fg(theme::INK_950).bg(theme::INK_50)),
    ])
}

/// The `width` by `height` area in the middle of `area`. Both are already clamped to the screen,
/// so neither offset can run past it.
fn centred(area: Rect, width: u16, height: u16) -> Rect {
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}

/// A field's line: its label in the colour of text meant to be read second, then what has been
/// typed with the cursor drawn as one inverted cell. `invalid` is the settings dialog marking a
/// field that is not a number, which has to be visible before the person presses Enter again.
pub(crate) fn field(label: &str, input: &Input, width: usize, invalid: bool) -> Line<'static> {
    let text = Style::new().fg(if invalid {
        theme::RED_500
    } else {
        theme::INK_300
    });
    let caret = Style::new().fg(theme::INK_950).bg(theme::INK_50);
    let (before, at_cursor) = input.split_at_cursor();

    let mut spans = vec![
        Span::styled(format!("{label:<width$}"), Style::new().fg(theme::INK_400)),
        Span::raw(" ".repeat(LABEL_GAP)),
        Span::styled(before, text),
    ];
    let mut typed = at_cursor.chars();
    match typed.next() {
        Some(c) => {
            spans.push(Span::styled(c.to_string(), caret));
            spans.push(Span::styled(typed.as_str().to_string(), text));
        }
        // A cursor after the last character has nothing on it, so it marks the next cell.
        None => spans.push(Span::styled(" ", caret)),
    }
    Line::from(spans)
}

/// The line at the bottom of a dialog that says what its keys do here, in the footer's shape: each
/// key in the accent, and its meaning beside it.
pub(crate) fn keys_hint(items: &[(&'static str, &'static str)]) -> Line<'static> {
    let key_style = Style::new().fg(theme::VIOLET_400);
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (index, (name, action)) in items.iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw("   "));
        }
        spans.push(Span::styled(*name, key_style));
        spans.push(Span::raw(format!(" {action}")));
    }
    Line::from(spans)
}

/// The keys every text field answers: typing, deleting and moving the cursor. False means the key
/// belongs to the dialog itself, which is what has to say what Enter and Tab and Esc do.
pub(crate) fn type_into(field: &mut Input, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Char(c) => {
            field.insert(c);
            true
        }
        KeyCode::Backspace => {
            field.backspace();
            true
        }
        KeyCode::Left => {
            field.move_left();
            true
        }
        KeyCode::Right => {
            field.move_right();
            true
        }
        KeyCode::Home => {
            field.home();
            true
        }
        KeyCode::End => {
            field.end();
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// A drawn line as plain text, for checking what a field is made of.
    fn text(line: &Line<'static>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn a_field_shows_its_label_its_text_and_where_the_cursor_is() {
        let line = field("Save to", &Input::new("/downlads"), 17, false);
        // The label column is 17 cells (its seven characters plus ten of padding), then the gap,
        // then what was typed, then the cell the cursor is drawn on.
        assert_eq!(
            text(&line),
            format!("Save to{}  /downlads ", " ".repeat(10))
        );

        // With the cursor before the first character, that character is the one drawn on the
        // caret cell: the text reads the same either way, only the cell it lands on changes. The
        // rest of the field keeps the colour a refused field is drawn in.
        let mut mid = Input::new("ab");
        mid.home();
        let line = field("x", &mid, 1, true);
        assert_eq!(text(&line), "x  ab");
        assert_eq!(
            line.spans[3].style.bg,
            Some(theme::INK_50),
            "the cursor was not drawn on a character"
        );
        assert_eq!(line.spans[4].style.fg, Some(theme::RED_500));
    }

    #[test]
    fn a_box_is_centred_and_never_larger_than_the_screen() {
        assert_eq!(
            centred(Rect::new(0, 0, 80, 24), 40, 10),
            Rect::new(20, 7, 40, 10)
        );
        // Clamped sizes leave the offsets at zero rather than negative.
        assert_eq!(
            centred(Rect::new(0, 0, 10, 5), 10, 5),
            Rect::new(0, 0, 10, 5)
        );
    }

    #[test]
    fn a_dialog_that_closes_without_answering_sends_nothing() {
        let snap = Snapshot {
            connection: fbtt_core::Connection::Local,
            torrents: Vec::new(),
            totals: fbtt_core::Totals {
                down_rate: 0,
                up_rate: 0,
                dht_nodes: None,
            },
            limits: fbtt_core::LiveLimits {
                download_kib: 0,
                upload_kib: 0,
                active_downloads: None,
                active_seeds: None,
                active_total: None,
            },
            detail: None,
            default_dir: std::path::PathBuf::from("/downloads"),
        };

        let mut model = Model::new();
        model.modal = Some(Modal::Help);
        assert_eq!(handle(&mut model, press(KeyCode::Esc), &snap), Vec::new());
        assert!(model.modal.is_none(), "help did not come down");

        // And a key that is not an answer leaves it up.
        model.modal = Some(Modal::Help);
        assert_eq!(
            handle(&mut model, press(KeyCode::Char('j')), &snap),
            Vec::new()
        );
        assert!(matches!(model.modal, Some(Modal::Help)));

        // The filter throws away what was typed; Enter keeps it.
        let mut model = Model::new();
        model.modal = Some(Modal::Filter);
        handle(&mut model, press(KeyCode::Char('d')), &snap);
        assert_eq!(model.filter, "d", "typing did not reach the filter");
        handle(&mut model, press(KeyCode::Esc), &snap);
        assert_eq!(model.filter, "", "escape left the filter behind");
    }
}
