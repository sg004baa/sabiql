use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Paragraph, Wrap};
use sabiql_tui_kit::primitives::atoms::text_cursor_spans;
use sabiql_tui_kit::primitives::molecules::{
    StripedTableConfig, hint_line, render_modal, render_striped_table,
};
use sabiql_tui_kit::theme::{DEFAULT_THEME, StatusTone};

use crate::app::{AppState, CommandStatus, ConnectionStatus, StatusMessage, ValueState};
use crate::domain::{RedisValue, redis_value_table};

pub fn render(frame: &mut Frame<'_>, state: &AppState) {
    let area = frame.area();
    let vertical = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .split(area);
    let body = Layout::horizontal([
        Constraint::Percentage(38),
        Constraint::Length(1),
        Constraint::Min(20),
    ])
    .split(vertical[1]);

    render_status(frame, vertical[0], state);
    render_keys(frame, body[0], state);
    render_value_pane(frame, body[2], state);
    render_footer(frame, vertical[2]);
    if state.command_modal.is_open {
        render_command_modal(frame, state);
    }
}

fn render_status(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let theme = DEFAULT_THEME;
    let (mut text, mut style) = match &state.connection_status {
        ConnectionStatus::Disconnected => (
            format!("disconnected | {}", state.dsn),
            Style::default().fg(theme.semantic.text.secondary),
        ),
        ConnectionStatus::Connecting => (
            format!("connecting | {}", state.dsn),
            Style::default().fg(theme.semantic.status.pending),
        ),
        ConnectionStatus::Connected => {
            let visible_count = if state.filter_query.is_empty() {
                state.keys.len().to_string()
            } else {
                format!("{}/{}", state.filtered_indices.len(), state.keys.len())
            };
            let count = match state.dbsize {
                Some(dbsize) => format!("{visible_count} scanned keys | dbsize {dbsize}"),
                None => format!("{visible_count} scanned keys"),
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

    if state.filter_active || !state.filter_query.is_empty() {
        text.push_str(" | filter /");
        text.push_str(&state.filter_query);
    }

    if let Some(status_message) = &state.status_message {
        let status_text = match status_message {
            StatusMessage::Info(message) => message,
            StatusMessage::Success(message) => {
                style = theme.status_style(StatusTone::Success);
                message
            }
            StatusMessage::Error(message) => {
                style = theme.status_style(StatusTone::Error);
                message
            }
        };
        text.push_str(" | ");
        text.push_str(status_text);
    }

    frame.render_widget(Paragraph::new(Line::from(text)).style(style), area);
}

fn render_keys(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
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
            total_items: state.filtered_indices.len(),
            empty_message: if state.keys.is_empty() {
                "No keys found"
            } else {
                "No matching keys"
            },
        },
        state.scroll_offset,
        &theme,
        |index| {
            let Some(redis_key) = state
                .filtered_indices
                .get(index)
                .and_then(|full_index| state.keys.get(*full_index))
            else {
                return vec![Cell::from(""), Cell::from("")];
            };
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

fn render_value_pane(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let chunks = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(area);
    match &state.value_state {
        ValueState::Empty => {
            render_value_title(frame, chunks[0], "value | no key selected");
            frame.render_widget(Paragraph::new("No key selected"), chunks[1]);
        }
        ValueState::Loading { key } => {
            render_value_title(frame, chunks[0], &format!("value | {key} | loading"));
            frame.render_widget(Paragraph::new("Loading..."), chunks[1]);
        }
        ValueState::Failed { key, message } => {
            render_value_title(frame, chunks[0], &format!("value | {key} | failed"));
            frame.render_widget(
                Paragraph::new(message.clone())
                    .style(DEFAULT_THEME.status_style(StatusTone::Error)),
                chunks[1],
            );
        }
        ValueState::Loaded {
            key,
            kind,
            ttl,
            value,
        } => {
            render_value_title(
                frame,
                chunks[0],
                &format!("value | {key} | type {kind} | {}", ttl_label(*ttl)),
            );
            render_value_table(frame, chunks[1], value);
        }
    }
}

fn render_value_title(frame: &mut Frame<'_>, area: Rect, title: &str) {
    let theme = DEFAULT_THEME;
    frame.render_widget(
        Paragraph::new(Line::from(title.to_string()))
            .style(Style::default().fg(theme.semantic.text.secondary)),
        area,
    );
}

fn render_value_table(frame: &mut Frame<'_>, area: Rect, value: &RedisValue) {
    let theme = DEFAULT_THEME;
    let table = redis_value_table(value);
    let widths = value_widths(table.headers.len());

    render_striped_table(
        frame,
        area,
        &StripedTableConfig {
            headers: &table.headers,
            widths: &widths,
            total_items: table.rows.len(),
            empty_message: "No value rows",
        },
        0,
        &theme,
        |index| {
            table.rows[index]
                .iter()
                .map(|value| Cell::from(value.clone()))
                .collect()
        },
    );
}

fn value_widths(column_count: usize) -> Vec<Constraint> {
    match column_count {
        0 | 1 => vec![Constraint::Min(10)],
        2 => vec![Constraint::Percentage(35), Constraint::Percentage(65)],
        _ => vec![Constraint::Min(10); column_count],
    }
}

fn ttl_label(ttl: Option<u64>) -> String {
    let Some(seconds) = ttl else {
        return "no expiry".to_string();
    };

    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let seconds = seconds % 60;
    let mut parts = Vec::new();
    if days > 0 {
        parts.push(format!("{days}d"));
    }
    if hours > 0 {
        parts.push(format!("{hours}h"));
    }
    if minutes > 0 {
        parts.push(format!("{minutes}m"));
    }
    if seconds > 0 || parts.is_empty() {
        parts.push(format!("{seconds}s"));
    }
    parts.join(" ")
}

fn render_footer(frame: &mut Frame<'_>, area: Rect) {
    let theme = DEFAULT_THEME;
    let hints = [
        ("j/k", "navigate"),
        ("/", "filter"),
        (":", "command"),
        ("e", "export"),
        ("q", "quit"),
    ];
    frame.render_widget(
        Paragraph::new(hint_line(&hints, &theme))
            .style(Style::default().fg(theme.semantic.text.muted)),
        area,
    );
}

fn render_command_modal(frame: &mut Frame<'_>, state: &AppState) {
    let theme = DEFAULT_THEME;
    let (_, inner) = render_modal(
        frame,
        Constraint::Percentage(80),
        Constraint::Percentage(45),
        "Redis Command",
        " Enter run | Esc close ",
        &theme,
    );
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .split(inner);

    render_command_input(frame, chunks[0], state);
    render_command_status(frame, chunks[1], state);
    render_command_output(frame, chunks[2], state);
}

fn render_command_input(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let theme = DEFAULT_THEME;
    let prompt = "> ";
    let visible_width = area
        .width
        .saturating_sub(prompt.len() as u16)
        .saturating_sub(1) as usize;
    let cursor_spans = text_cursor_spans(
        &state.command_modal.input,
        state.command_modal.input.chars().count(),
        0,
        visible_width,
        &theme,
    );
    let mut spans = vec![Span::styled(
        prompt,
        Style::default().fg(theme.semantic.text.accent),
    )];
    spans.extend(cursor_spans);
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_command_status(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let theme = DEFAULT_THEME;
    let (text, style) = match &state.command_modal.status {
        CommandStatus::Idle => (
            "Blocked: FLUSHALL, FLUSHDB, DEL, UNLINK".to_string(),
            Style::default().fg(theme.semantic.text.muted),
        ),
        CommandStatus::Running => (
            "Running...".to_string(),
            Style::default().fg(theme.semantic.status.pending),
        ),
        CommandStatus::Success(_) => ("OK".to_string(), theme.status_style(StatusTone::Success)),
        CommandStatus::Error(_) => ("Error".to_string(), theme.status_style(StatusTone::Error)),
    };
    frame.render_widget(Paragraph::new(Line::from(text)).style(style), area);
}

fn render_command_output(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let theme = DEFAULT_THEME;
    let (text, style) = match &state.command_modal.status {
        CommandStatus::Idle | CommandStatus::Running => (
            "Output appears here after a command runs.".to_string(),
            Style::default().fg(theme.semantic.text.secondary),
        ),
        CommandStatus::Success(output) => (
            output.clone(),
            Style::default().fg(theme.semantic.text.primary),
        ),
        CommandStatus::Error(message) => (message.clone(), theme.status_style(StatusTone::Error)),
    };
    frame.render_widget(
        Paragraph::new(text).style(style).wrap(Wrap { trim: false }),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ttl_label_formats_compact_human_readable_values() {
        assert_eq!(ttl_label(None), "no expiry");
        assert_eq!(ttl_label(Some(45)), "45s");
        assert_eq!(ttl_label(Some(3_600)), "1h");
        assert_eq!(ttl_label(Some(5_025)), "1h 23m 45s");
        assert_eq!(ttl_label(Some(172_801)), "2d 1s");
    }
}
