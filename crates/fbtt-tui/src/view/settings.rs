//! Drawing the full-screen settings view: the section list on the left, the open section on the right.
//!
//! Every path drawn here goes through [`format::safe`] (`fbt::display_safe`), whether it came from the
//! snapshot or was typed: the forms keep the raw values, because those are what get sent back.

use fbtt_core::Notice;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::format;
use crate::modal::{Input, keys_hint};
use crate::model::Model;
use crate::settings::{
    DownloadsRow, Focus, SECTIONS, SET_IN_CONFIG, SPEED_SAVE_ROW, SettingsScreen, TOO_OLD,
    TuiSection,
};
use crate::theme;

/// Columns the section list takes, with its rule.
const LIST_WIDTH: u16 = 20;
/// Cells a label is padded to, which is the longest one.
const LABEL_WIDTH: usize = 28;

/// Draws the settings view over the whole of `area`.
pub(crate) fn draw(frame: &mut Frame, area: Rect, model: &Model, screen: &SettingsScreen) {
    if area.height < 4 || area.width < LIST_WIDTH + 4 {
        return;
    }
    let title = Rect::new(area.x, area.y, area.width, 1);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" Settings", Style::new().fg(theme::INK_50)),
            Span::styled("   Back to torrents: Esc", Style::new().fg(theme::INK_400)),
        ])),
        title,
    );
    let rule = Rect::new(area.x, area.y + 1, area.width, 1);
    frame.render_widget(
        Paragraph::new("─".repeat(usize::from(area.width))).style(Style::new().fg(theme::INK_600)),
        rule,
    );

    let body_height = area.height - 4;
    let list = Rect::new(area.x, area.y + 2, LIST_WIDTH, body_height);
    frame.render_widget(Paragraph::new(list_lines(screen)), list);
    let divider = (0..body_height)
        .map(|_| Line::from("│"))
        .collect::<Vec<Line>>();
    frame.render_widget(
        Paragraph::new(divider).style(Style::new().fg(theme::INK_600)),
        Rect::new(area.x + LIST_WIDTH, area.y + 2, 1, body_height),
    );
    let content = Rect::new(
        area.x + LIST_WIDTH + 2,
        area.y + 2,
        area.width - LIST_WIDTH - 3,
        body_height,
    );
    frame.render_widget(Paragraph::new(section_lines(screen)), content);

    // The line above the keys: what the last save was refused with, or what it said.
    let status = Rect::new(area.x + 1, area.y + area.height - 2, area.width - 1, 1);
    frame.render_widget(Paragraph::new(status_line(model, screen)), status);
    let footer = Rect::new(area.x + 1, area.y + area.height - 1, area.width - 1, 1);
    frame.render_widget(Paragraph::new(footer_line(screen)), footer);
}

fn status_line(model: &Model, screen: &SettingsScreen) -> Line<'static> {
    if let Some(question) = screen.question() {
        return Line::from(Span::styled(question, Style::new().fg(theme::AMBER_500)));
    }
    if let Some(error) = &screen.error {
        return Line::from(Span::styled(
            format::safe(error),
            Style::new().fg(theme::RED_500),
        ));
    }
    match model.notice() {
        Some(Notice::Info(text)) => Line::from(Span::styled(
            format::safe(text),
            Style::new().fg(theme::GREEN_500),
        )),
        _ => Line::default(),
    }
}

fn footer_line(screen: &SettingsScreen) -> Line<'static> {
    if screen.asking() {
        return keys_hint(&[
            ("s", "Save changes"),
            ("d", "Discard changes"),
            ("Esc", "keep editing"),
        ]);
    }
    match (screen.focus, screen.editing) {
        (Focus::List, _) => {
            keys_hint(&[("Up/Down", "section"), ("Tab", "fields"), ("Esc", "back")])
        }
        (Focus::Fields, true) => keys_hint(&[("Enter", "done"), ("Ctrl+S", "save")]),
        (Focus::Fields, false) => keys_hint(&[
            ("Tab", "list"),
            ("Up/Down", "field"),
            ("Enter", "edit"),
            ("Space", "toggle"),
            ("Ctrl+S", "save"),
            ("Esc", "back"),
        ]),
    }
}

fn list_lines(screen: &SettingsScreen) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(Span::styled(" Torrents", Style::new().fg(theme::INK_400))),
        Line::default(),
    ];
    for section in SECTIONS {
        let selected = section == screen.section;
        let style = match (selected, screen.focus) {
            (true, Focus::List) => Style::new().fg(theme::VIOLET_400).bg(theme::INK_850),
            (true, Focus::Fields) => Style::new().fg(theme::INK_50),
            (false, _) => Style::new().fg(theme::INK_300),
        };
        let marker = if selected { "> " } else { "  " };
        lines.push(Line::from(Span::styled(
            format!(" {marker}{:<width$}", section.title(), width = 12),
            style,
        )));
    }
    lines
}

fn heading(text: &str) -> Line<'static> {
    Line::from(Span::styled(
        text.to_string(),
        Style::new().fg(theme::INK_50),
    ))
}

fn muted(text: &str) -> Line<'static> {
    Line::from(Span::styled(
        text.to_string(),
        Style::new().fg(theme::INK_400),
    ))
}

/// The label of a row, with the marker that says the cursor is on it.
fn label(text: &str, on_row: bool) -> Vec<Span<'static>> {
    let (marker, colour) = if on_row {
        ("> ", theme::INK_50)
    } else {
        ("  ", theme::INK_400)
    };
    vec![
        Span::styled(marker, Style::new().fg(theme::VIOLET_400)),
        Span::styled(format!("{text:<LABEL_WIDTH$}"), Style::new().fg(colour)),
        Span::raw("  "),
    ]
}

/// A text field's line. The text is drawn through `display_safe`, and the caret is drawn only while
/// the field is being edited.
fn field(text: &str, input: &Input, on_row: bool, editing: bool, invalid: bool) -> Line<'static> {
    let colour = if invalid {
        theme::RED_500
    } else {
        theme::INK_300
    };
    let style = Style::new().fg(colour);
    let mut spans = label(text, on_row);
    if on_row && editing {
        let caret = Style::new().fg(theme::INK_950).bg(theme::INK_50);
        let (before, at_cursor) = input.split_at_cursor();
        spans.push(Span::styled(format::safe(&before), style));
        let mut rest = format::safe(&at_cursor)
            .chars()
            .collect::<Vec<_>>()
            .into_iter();
        match rest.next() {
            Some(c) => {
                spans.push(Span::styled(c.to_string(), caret));
                spans.push(Span::styled(rest.collect::<String>(), style));
            }
            None => spans.push(Span::styled(" ", caret)),
        }
    } else {
        spans.push(Span::styled(format::safe(&input.text), style));
    }
    Line::from(spans)
}

fn switch(text: &str, on: bool, on_row: bool) -> Line<'static> {
    let mut spans = label(text, on_row);
    spans.push(Span::styled(
        if on { "[x]" } else { "[ ]" },
        Style::new().fg(if on_row {
            theme::INK_50
        } else {
            theme::INK_300
        }),
    ));
    Line::from(spans)
}

fn save_row(on_row: bool, saving: bool) -> Line<'static> {
    let style = if on_row {
        Style::new().fg(theme::INK_950).bg(theme::INK_50)
    } else {
        Style::new().fg(theme::VIOLET_400)
    };
    let mut spans = vec![Span::raw("  "), Span::styled(" Save ", style)];
    if saving {
        spans.push(Span::styled("  saving", Style::new().fg(theme::INK_400)));
    }
    Line::from(spans)
}

fn problem(text: &str) -> Line<'static> {
    Line::from(vec![
        Span::raw("  "),
        Span::styled(format::safe(text), Style::new().fg(theme::RED_500)),
    ])
}

fn section_lines(screen: &SettingsScreen) -> Vec<Line<'static>> {
    let mut lines = vec![heading(screen.section.title()), Line::default()];
    if !screen.section_ready() {
        let text = if screen.unsupported && screen.section == TuiSection::Downloads {
            TOO_OLD
        } else {
            "Loading settings."
        };
        lines.push(if screen.unsupported {
            Line::from(Span::styled(text, Style::new().fg(theme::AMBER_500)))
        } else {
            muted(text)
        });
        return lines;
    }
    let fields = screen.focus == Focus::Fields;
    let editing = screen.editing;
    match screen.section {
        TuiSection::Downloads => {
            let form = &screen.downloads;
            let rows = form.rows();
            let at = rows[form.row.min(rows.len() - 1)];
            let on = |row: DownloadsRow| fields && at == row;
            lines.push(field(
                "Default folder",
                &form.default_dir,
                on(DownloadsRow::Default),
                editing,
                false,
            ));
            lines.push(field(
                "Unfinished folder",
                &form.unfinished_dir,
                on(DownloadsRow::Unfinished),
                editing,
                false,
            ));
            lines.push(switch(
                "Start torrents paused",
                form.paused,
                on(DownloadsRow::Paused),
            ));
            lines.push(field(
                "Watch folder",
                &form.watch_dir,
                on(DownloadsRow::Watch),
                editing,
                false,
            ));
            if let Some(text) = &form.watch_problem {
                lines.push(problem(text));
            }
            if form.editable {
                lines.push(field(
                    "Run a program on completion",
                    &form.program,
                    on(DownloadsRow::Program),
                    editing,
                    false,
                ));
            } else {
                let mut spans = label("Run a program on completion", false);
                let shown = if form.program.text.is_empty() {
                    "none".to_string()
                } else {
                    format::safe(&form.program.text)
                };
                spans.push(Span::styled(shown, Style::new().fg(theme::INK_300)));
                lines.push(Line::from(spans));
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(SET_IN_CONFIG, Style::new().fg(theme::INK_400)),
                ]));
            }
            if let Some(text) = &form.on_complete_problem {
                lines.push(problem(text));
            }
            lines.push(Line::default());
            lines.push(save_row(on(DownloadsRow::Save), false));
        }
        TuiSection::Speed => {
            let form = &screen.speed;
            let on = |row: usize| fields && form.row == row;
            let names = [
                "Download limit KiB/s",
                "Upload limit KiB/s",
                "Active downloads",
                "Active seeds",
                "Active total",
            ];
            for (index, name) in names.iter().enumerate() {
                if index == 2 {
                    lines.push(Line::default());
                    lines.push(heading("Queue"));
                }
                lines.push(field(
                    name,
                    &form.fields[index],
                    on(index),
                    editing,
                    form.invalid == Some(index),
                ));
            }
            lines.push(muted("  0 or nothing means unlimited"));
            lines.push(Line::default());
            lines.push(save_row(on(SPEED_SAVE_ROW), false));
        }
    }
    lines
}
