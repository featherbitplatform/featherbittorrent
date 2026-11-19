//! Dialogs over the main view: what each one answers, and how it is drawn.
//!
//! A dialog owns the keyboard while it is up: every press goes to it and none to the table, so a
//! `q` typed into a magnet field adds a letter instead of stopping the app. Everything but the
//! filter draws as a box in the middle of the screen, on the surface colour inside a border of the
//! rule colour. The filter is one line along the bottom, where the design puts the key hints, and
//! it changes the table while it is typed rather than when Enter is pressed.
//!
//! A dialog that shows something the backend reported - the folders to pick from, the devices a
//! server lets sign in - is drawn with the newest snapshot beside it, because that listing is not
//! the dialog's to hold: the backend replaces it whenever it answers again.

mod add;
mod connect;
mod delete;
mod devices;
mod folders;
mod help;
mod input;
mod settings;

pub use add::AddForm;
pub use connect::{ConnectForm, ConnectRequest, ConnectTarget, SecretInput};
pub use devices::DevicesForm;
pub use folders::FolderPicker;
pub use input::{Input, Masked};
pub use settings::SettingsForm;

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent};
use fbtt_core::{Snapshot, TorrentId, TorrentRow};
use ratatui::Frame;
use ratatui::layout::{Margin, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Fill, Paragraph};

use crate::format;
use crate::graphics::{LogoSize, LogoView, Placement};
use crate::model::{Action, Model};
use crate::theme;

/// Cells between a field's label and what has been typed in it.
const LABEL_GAP: usize = 2;

/// A dialog open on top of the main view.
#[derive(Debug, Clone, PartialEq)]
pub enum Modal {
    /// A magnet link or a `.torrent` file, and where its files go.
    Add(AddForm),
    /// Which backend to run on, and how to sign in to it.
    Connect(ConnectForm),
    /// The devices a server lets sign in, and what to do with them.
    Devices(DevicesForm),
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

    /// The connect screen, prefilled with `last` - the connection this library was last used on, or
    /// the server running on its own library - and pointed at a server so that there is something to
    /// change with Left and Right.
    pub fn connect(last: Option<&fbtt_core::ui_state::RemoteTarget>) -> Modal {
        Modal::Connect(ConnectForm::new(last))
    }

    /// The device list, empty until the server answers the `Devices(List)` that opening it sends.
    pub fn devices() -> Modal {
        Modal::Devices(DevicesForm::new())
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
            Modal::Add(form) => form.on_key(key, snap),
            Modal::Connect(form) => form.on_key(key),
            Modal::Devices(form) => form.on_key(key, snap),
            Modal::Settings(form) => form.on_key(key),
            Modal::Delete { id, .. } => delete::on_key(key, id),
            Modal::Help => help::on_key(key),
            Modal::Filter => filter_on_key(model, key, snap),
        }
    }
}

/// What one press did to a dialog.
#[derive(Debug, PartialEq)]
pub(crate) enum Answer {
    /// The dialog stays up and nothing goes out.
    Stay,
    /// The actions go out and the dialog stays up for their answer: what happens next comes back from
    /// the backend - as a notice that closes an add, or as a snapshot that redraws a list - rather than
    /// from this press. See [`Model::push_notice`](crate::model::Model::push_notice).
    Keep(Vec<Action>),
    /// The dialog comes down, with whatever it had to say. Closing without answering sends nothing,
    /// which is what its `Vec` holds then.
    Close(Vec<Action>),
}

/// Applies one press to the open dialog, which owns the keyboard while it is up: none of it reaches
/// the table, a sort, or a quit that is waiting to be confirmed. A dialog that sent something and is
/// waiting for its answer stays up too.
pub(crate) fn handle(model: &mut Model, key: KeyEvent, snap: &Snapshot) -> Vec<Action> {
    let Some(mut modal) = model.modal.take() else {
        return Vec::new();
    };
    match modal.on_key(key, model, snap) {
        Answer::Stay => {
            model.modal = Some(modal);
            Vec::new()
        }
        Answer::Keep(actions) => {
            model.modal = Some(modal);
            actions
        }
        Answer::Close(actions) => actions,
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
/// so the table behind it stays exactly as it was. `snap` is the newest snapshot: a dialog that lists
/// something the backend reported draws from it rather than keeping its own copy.
///
/// The box's rect comes back, or `None` for the filter and for no dialog at all: a box over a mark
/// takes the mark off the screen for this frame, and the caller is what decides that.
pub(crate) fn draw(
    frame: &mut Frame,
    model: &Model,
    snap: &Snapshot,
    logos: LogoView<'_>,
) -> Option<Rect> {
    let area = frame.area();
    match &model.modal {
        Some(Modal::Add(form)) => draw_box(frame, area, "Add torrent", add::lines(form, snap)),
        Some(Modal::Connect(form)) => draw_box(frame, area, "Connect", connect::lines(form)),
        Some(Modal::Devices(form)) => draw_box(frame, area, "Devices", devices::lines(form, snap)),
        Some(Modal::Delete { name, .. }) => {
            draw_box(frame, area, "Delete torrent", delete::lines(name))
        }
        Some(Modal::Settings(form)) => draw_box(frame, area, "Settings", settings::lines(form)),
        Some(Modal::Help) => {
            // The mark the box has room for, when it does: the box and the mark are sized together,
            // so the box leaves the mark's rows at its top and the mark stands in them, above the
            // keys. A box too short for both draws the keys alone.
            let mark: Option<Arc<crate::graphics::place::Prepared>> =
                help_placement(area, logos).and_then(|placement| placement.prepared());
            let mark = mark.as_deref();
            draw_box(frame, area, "Keys", help::lines(area, logos, mark))
        }
        Some(Modal::Filter) => {
            draw_filter(frame, area, model);
            None
        }
        None => None,
    }
}

/// The box every dialog but the filter gets: its lines padded one cell inside a border of the rule
/// colour on the surface colour, sized to what they need and never bigger than the screen. The rect
/// comes back, because a box over a mark takes the mark off the screen for this frame.
fn draw_box(frame: &mut Frame, area: Rect, title: &str, lines: Vec<Line<'static>>) -> Option<Rect> {
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
    Some(box_area)
}

/// The help screen's mark, when there is one to place: the tallest of the prepared large marks the
/// help box has room for, standing at the top of the box above its keys (decision 12).
///
/// The box and the mark are sized together: the box holds the keys, the close hint, and - when it
/// can hold the mark and every key together - `mark.rows + 1` rows left at its top for the mark.
/// The rect the mark is placed over is exactly the mark's own cells, `dims.cols` by `dims.rows`,
/// centred horizontally in the box, with the one row between it and the first key left as the
/// list's own blank line.
pub(crate) fn help_placement(area: Rect, logos: LogoView<'_>) -> Option<Placement> {
    let logos_ref = logos.logos?;
    // The keys' own width, as `help::lines` draws them with no mark: that is the widest line the
    // box holds, since the rows the mark stands in are empty.
    let widest = help::lines(area, logos, None)
        .iter()
        .map(|line| u16::try_from(line.width()).unwrap_or(u16::MAX))
        .max()
        .unwrap_or(0);
    // The tallest mark the box can hold together with every key, tried from tallest first.
    logos_ref.large.iter().find_map(|prepared| {
        let rows = usize::from(prepared.dims.rows);
        if prepared.dims.cols > area.width {
            return None;
        }
        let width = (widest.max(prepared.dims.cols) + 4).min(area.width);
        // The rows the list takes: one column when the keys and the mark both fit in it, two
        // columns otherwise. The box is the list's rows, the close hint, the mark's rows plus its
        // blank row, and the border.
        let list_rows = {
            let keys = crate::keys::KEYS.len();
            let half = keys.div_ceil(2);
            let rows_available = usize::from(area.height).saturating_sub(3);
            if rows_available > keys + rows {
                keys
            } else {
                half
            }
        };
        let height = u16::try_from(list_rows + rows + 1 + 2).unwrap_or(u16::MAX);
        if height > area.height {
            return None;
        }
        let (box_x, box_y) = (
            area.x + (area.width.saturating_sub(width)) / 2,
            area.y + (area.height.saturating_sub(height)) / 2,
        );
        // The mark stands in the box's inner top, above the keys: the list leaves the mark's rows
        // and one blank row at its top, so the rect begins two rows below the box's top, and it is
        // the mark's own cells, centred in the box's width.
        let x = box_x + (width.saturating_sub(prepared.dims.cols)) / 2;
        let y = box_y + 2;
        Some(Placement {
            size: LogoSize::Large(prepared.dims.rows),
            rect: Rect::new(x, y, prepared.dims.cols, prepared.dims.rows),
            dims: prepared.dims,
            prepared: Some(Arc::new(prepared.clone())),
        })
    })
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
    // Field text is drawn the way everything else on the screen is drawn, through `format::safe`:
    // the folder field starts life as a path that came from the snapshot rather than one this
    // person typed, and text from somewhere else cannot be trusted to stay inside its cells.
    let before = format::safe(&before);
    let at_cursor = format::safe(&at_cursor);

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

/// A secret's field: its label, then one `*` for every character typed, with the cursor drawn as one
/// inverted cell in the same place [`field`] draws it. The counts are all that leave the field, so a
/// password is never copied on its way to the screen; what shows is how long it is, which a person
/// typing it already knows.
pub(crate) fn masked_field(label: &str, secret: &Masked, width: usize) -> Line<'static> {
    let text = Style::new().fg(theme::INK_300);
    let caret = Style::new().fg(theme::INK_950).bg(theme::INK_50);
    let (before, at_cursor) = secret.split_at_cursor();

    let mut spans = vec![
        Span::styled(format!("{label:<width$}"), Style::new().fg(theme::INK_400)),
        Span::raw(" ".repeat(LABEL_GAP)),
        Span::styled("*".repeat(before), text),
    ];
    if at_cursor > 0 {
        // The character under the cursor is a star like the others, only inverted: the field says where
        // the next keystroke lands without saying which letter is there.
        spans.push(Span::styled("*", caret));
        spans.push(Span::styled("*".repeat(at_cursor - 1), text));
    } else {
        spans.push(Span::styled(" ", caret));
    }
    Line::from(spans)
}

/// A row whose value is chosen rather than typed - the target, the login method - drawn like a field,
/// with the whole value inverted while the cursor is on it. The inversion is what tells a person that
/// Left and Right do something here: a caret inside a word they cannot edit would say nothing.
pub(crate) fn choice(label: &str, value: &str, width: usize, focused: bool) -> Line<'static> {
    let style = if focused {
        Style::new().fg(theme::INK_950).bg(theme::INK_50)
    } else {
        Style::new().fg(theme::INK_300)
    };
    Line::from(vec![
        Span::styled(format!("{label:<width$}"), Style::new().fg(theme::INK_400)),
        Span::raw(" ".repeat(LABEL_GAP)),
        Span::styled(value.to_string(), style),
    ])
}

/// A row that is a yes or a no: the box, and the tick inside it drawn as the cursor cell when this row
/// has the cursor. The label follows the box rather than sitting in the field column beside it, because
/// these labels are longer than any field's and would push every field on the screen to the right.
pub(crate) fn toggle(label: &str, on: bool, focused: bool) -> Line<'static> {
    let caret = Style::new().fg(theme::INK_950).bg(theme::INK_50);
    let box_style = Style::new().fg(theme::INK_400);
    let tick = if on { "x" } else { " " };
    Line::from(vec![
        Span::styled("[", box_style),
        Span::styled(tick, if focused { caret } else { box_style }),
        Span::styled("] ", box_style),
        Span::styled(label.to_string(), Style::new().fg(theme::INK_300)),
    ])
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

/// A line of muted advice under a dialog's fields: what the keys do not make obvious, in the colour of
/// text meant to be read second.
pub(crate) fn advice(text: &str) -> Line<'static> {
    Line::from(Span::styled(
        text.to_string(),
        Style::new().fg(theme::INK_400),
    ))
}

/// A line saying what went wrong, in the error colour, fit to draw however it was worded.
pub(crate) fn problem(text: &str) -> Line<'static> {
    Line::from(Span::styled(
        format::safe(text),
        Style::new().fg(theme::RED_500),
    ))
}

/// A line saying something the person should know before going ahead, in the warning colour: not a
/// failure, and not something to skim past either.
pub(crate) fn caution(text: &str) -> Line<'static> {
    Line::from(Span::styled(
        text.to_string(),
        Style::new().fg(theme::AMBER_500),
    ))
}

/// The keys every text field answers: typing, deleting and moving the cursor. False means the key
/// belongs to the dialog itself, which is what has to say what Enter and Tab and Esc do. A key with a
/// modifier on it is not typing either: `Ctrl+O` means the folder picker wherever a field is up, and a
/// field that swallowed it would type its letter as well.
pub(crate) fn type_into(field: &mut Input, key: KeyEvent) -> bool {
    if !matches!(
        key.modifiers,
        crossterm::event::KeyModifiers::NONE | crossterm::event::KeyModifiers::SHIFT
    ) {
        return false;
    }
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

/// The same keys for a masked field.
pub(crate) fn type_into_masked(field: &mut Masked, key: KeyEvent) -> bool {
    if !matches!(
        key.modifiers,
        crossterm::event::KeyModifiers::NONE | crossterm::event::KeyModifiers::SHIFT
    ) {
        return false;
    }
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
    fn a_field_draws_text_that_came_from_elsewhere_clean() {
        // The folder field starts life holding a path from the snapshot, not one this person typed,
        // so what it holds is remote text: a character that would reorder or escape a row has to be
        // gone before it is drawn, the same as everywhere else on the screen.
        let line = field("Save to", &Input::new("/down\u{202e}loads"), 17, false);
        assert_eq!(
            text(&line),
            format!("Save to{}  /downloads ", " ".repeat(10))
        );
    }

    #[test]
    fn a_masked_field_draws_stars_and_never_the_password() {
        let mut secret = Masked::new();
        for c in "hunter2".chars() {
            secret.insert(c);
        }
        // Seven characters, so seven stars, and nothing of what was typed among them. The cursor is past
        // the last one, so it marks the next cell - the same mark a typed-into field draws there.
        let line = masked_field("Password", &secret, 8);
        assert_eq!(text(&line), "Password  ******* ");
        assert_eq!(line.spans[3].style.bg, Some(theme::INK_50));

        // The cursor moved; the stars did not.
        secret.home();
        let line = masked_field("Password", &secret, 8);
        assert_eq!(text(&line), "Password  *******");
        assert_eq!(line.spans[3].style.bg, Some(theme::INK_50));

        // Nothing typed is one inverted cell and nothing else.
        secret.clear();
        assert_eq!(text(&masked_field("Password", &secret, 8)), "Password   ");
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
            dirs: None,
            devices: None,
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
