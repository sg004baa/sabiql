use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Cell, Paragraph};
use sabiql_tui_kit::primitives::molecules::{StripedTableConfig, render_striped_table};
use sabiql_tui_kit::theme::{DEFAULT_THEME, StatusTone};

use crate::app::{AppState, ConnectionStatus};

pub fn render(frame: &mut Frame<'_>, state: &AppState) {
    let area = frame.area();
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .split(area);

    render_status(frame, chunks[0], state);
    render_keys(frame, chunks[1], state);
    render_footer(frame, chunks[2]);
}

fn render_status(frame: &mut Frame<'_>, area: ratatui::layout::Rect, state: &AppState) {
    let theme = DEFAULT_THEME;
    let (text, style) = match &state.connection_status {
        ConnectionStatus::Disconnected => (
            format!("disconnected | {}", state.dsn),
            Style::default().fg(theme.semantic.text.secondary),
        ),
        ConnectionStatus::Connecting => (
            format!("connecting | {}", state.dsn),
            Style::default().fg(theme.semantic.status.pending),
        ),
        ConnectionStatus::Connected => {
            let count = match state.dbsize {
                Some(dbsize) => format!("{} scanned keys | dbsize {dbsize}", state.keys.len()),
                None => format!("{} scanned keys", state.keys.len()),
            };
            (
                format!("connected | {count} | {}", state.dsn),
                theme.status_style(StatusTone::Success),
            )
        }
        ConnectionStatus::Error(message) => (
            format!("error | {message} | {}", state.dsn),
            theme.status_style(StatusTone::Error),
        ),
    };

    frame.render_widget(Paragraph::new(Line::from(text)).style(style), area);
}

fn render_keys(frame: &mut Frame<'_>, area: ratatui::layout::Rect, state: &AppState) {
    let theme = DEFAULT_THEME;
    let headers = ["key", "type"];
    let widths = [Constraint::Min(10), Constraint::Length(10)];
    let selected_style = Style::default()
        .bg(theme.component.table.result_row_active_bg)
        .fg(theme.semantic.text.primary)
        .add_modifier(Modifier::BOLD);

    render_striped_table(
        frame,
        area,
        &StripedTableConfig {
            headers: &headers,
            widths: &widths,
            total_items: state.keys.len(),
            empty_message: "No keys found",
        },
        state.scroll_offset,
        &theme,
        |index| {
            let redis_key = &state.keys[index];
            let style = if index == state.selected_index {
                selected_style
            } else {
                Style::default().fg(theme.semantic.text.primary)
            };
            vec![
                Cell::from(redis_key.key.clone()).style(style),
                Cell::from(redis_key.kind.to_string()).style(style),
            ]
        },
    );
}

fn render_footer(frame: &mut Frame<'_>, area: ratatui::layout::Rect) {
    let theme = DEFAULT_THEME;
    frame.render_widget(
        Paragraph::new(Line::from("j/k or arrows move | q quit"))
            .style(Style::default().fg(theme.semantic.text.muted)),
        area,
    );
}
