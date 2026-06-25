use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Clear, Paragraph, Wrap};
use sabiql_tui_kit::primitives::atoms::text_cursor_spans;
use sabiql_tui_kit::primitives::molecules::{
    StripedTableConfig, hint_line, render_modal, render_striped_table,
};
use sabiql_tui_kit::theme::{DEFAULT_THEME, StatusTone};

use crate::app::{
    AppState, CommandStatus, ConfirmState, ConnectionFormState, ConnectionStatus, DbOverlayState,
    StatusMessage, ValueState,
};
use crate::domain::{RedisValue, redis_string_display_value, redis_value_table};

const KEY_PANE_LEFT_PADDING: u16 = 1;
const VALUE_PANE_WIDTH: u16 = 96;

pub fn render(frame: &mut Frame<'_>, state: &AppState) {
    let area = frame.area();
    let vertical = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .split(area);
    let body = Layout::horizontal([
        Constraint::Min(40),
        Constraint::Length(1),
        Constraint::Length(VALUE_PANE_WIDTH),
    ])
    .split(vertical[1]);

    render_status(frame, vertical[0], state);
    render_keys(frame, body[0], state);
    render_body_divider(frame, body[1]);
    render_value_pane(frame, body[2], state);
    render_footer(frame, vertical[2], state);
    if state.command_modal.is_open {
        render_command_modal(frame, state);
    }
    if let Some(overlay) = &state.db_overlay {
        render_db_overlay(frame, overlay);
    }
    if let Some(form) = &state.connection_form {
        render_connection_form(frame, form);
    }
    if let Some(confirm_state) = &state.confirm_state {
        render_confirm_dialog(frame, confirm_state);
    }
}

fn render_status(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let theme = DEFAULT_THEME;
    let (mut text, mut style) = match &state.connection_status {
        ConnectionStatus::Disconnected => (
            format!("disconnected | db {} | {}", state.current_db, state.dsn),
            Style::default().fg(theme.semantic.text.secondary),
        ),
        ConnectionStatus::Connecting => (
            format!("connecting | db {} | {}", state.current_db, state.dsn),
            Style::default().fg(theme.semantic.status.pending),
        ),
        ConnectionStatus::Connected => {
            let visible_count = if fuzzy_filter_query(&state.search_pattern).is_empty() {
                state.keys.len().to_string()
            } else {
                format!("{}/{}", state.filtered_indices.len(), state.keys.len())
            };
            let count = match state.dbsize {
                Some(dbsize) => format!("{visible_count} scanned keys | dbsize {dbsize}"),
                None => format!("{visible_count} scanned keys"),
            };
            (
                format!(
                    "connected | db {} | {count} | {}",
                    state.current_db, state.dsn
                ),
                theme.status_style(StatusTone::Success),
            )
        }
        ConnectionStatus::Error(message) => (
            format!(
                "error | db {} | {message} | {}",
                state.current_db, state.dsn
            ),
            theme.status_style(StatusTone::Error),
        ),
    };

    if state.filter_active || matches!(&state.connection_status, ConnectionStatus::Connected) {
        text.push_str(" | search /");
        text.push_str(&state.search_pattern);
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
    let area = Rect {
        x: area.x.saturating_add(KEY_PANE_LEFT_PADDING),
        width: area.width.saturating_sub(KEY_PANE_LEFT_PADDING),
        ..area
    };
    let headers = ["key", "type"];
    let widths = [Constraint::Min(10), Constraint::Length(10)];
    let empty_message = if state.keys.is_empty() {
        "No keys found".to_string()
    } else if fuzzy_filter_query(&state.search_pattern).is_empty() {
        "No matching keys".to_string()
    } else {
        format!("No keys match pattern {}", state.search_pattern)
    };
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
            header_bottom_margin: 1,
            header_separator: true,
            total_items: state.filtered_indices.len(),
            empty_message: empty_message.as_str(),
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

fn render_body_divider(frame: &mut Frame<'_>, area: Rect) {
    let theme = DEFAULT_THEME;
    let divider = (0..area.height).map(|_| "│").collect::<Vec<_>>().join("\n");
    frame.render_widget(
        Paragraph::new(divider).style(Style::default().fg(theme.semantic.surface.unfocus_border)),
        area,
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
            match value {
                RedisValue::String(value) => {
                    render_value_string(frame, chunks[1], value, state.value_scroll_offset);
                }
                RedisValue::List(_)
                | RedisValue::Set(_)
                | RedisValue::Hash(_)
                | RedisValue::ZSet(_)
                | RedisValue::Stream(_) => {
                    render_value_table(frame, chunks[1], value, state.value_scroll_offset);
                }
            }
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

fn render_value_table(frame: &mut Frame<'_>, area: Rect, value: &RedisValue, offset: usize) {
    let theme = DEFAULT_THEME;
    let table = redis_value_table(value);
    let widths = value_widths(table.headers.len());

    render_striped_table(
        frame,
        area,
        &StripedTableConfig {
            headers: &table.headers,
            widths: &widths,
            header_bottom_margin: 0,
            header_separator: false,
            total_items: table.rows.len(),
            empty_message: "No value rows",
        },
        offset,
        &theme,
        |index| {
            table.rows[index]
                .iter()
                .map(|value| Cell::from(value.clone()))
                .collect()
        },
    );
}

fn render_value_string(frame: &mut Frame<'_>, area: Rect, value: &str, offset: usize) {
    let theme = DEFAULT_THEME;
    let scroll_offset = u16::try_from(offset).unwrap_or(u16::MAX);
    frame.render_widget(
        Paragraph::new(redis_string_display_value(value))
            .style(Style::default().fg(theme.semantic.text.primary))
            .wrap(Wrap { trim: false })
            .scroll((scroll_offset, 0)),
        area,
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

fn fuzzy_filter_query(pattern: &str) -> String {
    let query = pattern.trim();
    if query.is_empty() || query == "*" {
        return String::new();
    }

    let mut output = String::new();
    let mut in_bracket_expression = false;
    for ch in query.chars() {
        match ch {
            '[' => in_bracket_expression = true,
            ']' => in_bracket_expression = false,
            '*' | '?' if !in_bracket_expression => {}
            _ if in_bracket_expression => {}
            _ => output.push(ch),
        }
    }
    output
}

fn render_footer(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let theme = DEFAULT_THEME;
    let mut hints = Vec::new();
    if state.read_only {
        hints.push(("[READ-ONLY]", "writes blocked"));
    }
    hints.extend([
        ("j/k", "navigate"),
        ("/", "search"),
        (":", "command"),
        ("c", "connect"),
        ("r", "reload"),
        ("e", "export"),
        ("q", "quit"),
    ]);
    frame.render_widget(
        Paragraph::new(hint_line(&hints, &theme))
            .style(Style::default().fg(theme.semantic.text.muted)),
        area,
    );
}

fn render_db_overlay(frame: &mut Frame<'_>, overlay: &DbOverlayState) {
    let theme = DEFAULT_THEME;
    let (_, inner) = render_modal(
        frame,
        Constraint::Percentage(50),
        Constraint::Percentage(60),
        "Redis Databases",
        " Enter:switch  Esc:cancel  j/k:move ",
        &theme,
    );
    let selected_style = Style::default()
        .bg(theme.component.table.result_row_active_bg)
        .fg(theme.semantic.text.primary)
        .add_modifier(Modifier::BOLD);
    let headers = ["database", "keys"];
    let widths = [Constraint::Length(12), Constraint::Min(10)];
    let visible_rows = inner.height.saturating_sub(2) as usize;
    let scroll_offset = overlay
        .selected
        .saturating_sub(visible_rows.saturating_sub(1));

    render_striped_table(
        frame,
        inner,
        &StripedTableConfig {
            headers: &headers,
            widths: &widths,
            header_bottom_margin: 0,
            header_separator: false,
            total_items: overlay.entries.len(),
            empty_message: if overlay.loading {
                "Loading..."
            } else {
                "No databases found"
            },
        },
        scroll_offset,
        &theme,
        |index| {
            let Some((db, count)) = overlay.entries.get(index) else {
                return vec![Cell::from(""), Cell::from("")];
            };
            let style = if index == overlay.selected {
                selected_style
            } else {
                Style::default().fg(theme.semantic.text.primary)
            };
            let count_label = count
                .map(|count| format!("{count} keys"))
                .unwrap_or_else(|| "...".to_string());

            vec![
                Cell::from(format!("db {db}")).style(style),
                Cell::from(count_label).style(style),
            ]
        },
    );
}

fn render_connection_form(frame: &mut Frame<'_>, form: &ConnectionFormState) {
    let theme = DEFAULT_THEME;
    let (_, inner) = render_modal(
        frame,
        Constraint::Percentage(70),
        Constraint::Length(7),
        "Redis Connection",
        " Enter:connect  Esc:cancel  Tab:read-only ",
        &theme,
    );
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .split(inner);

    render_connection_dsn_input(frame, chunks[0], form);
    render_connection_read_only(frame, chunks[1], form);
    frame.render_widget(
        Paragraph::new(hint_line(
            &[
                ("Tab", "toggle read-only"),
                ("Enter", "reconnect"),
                ("Esc", "cancel"),
            ],
            &theme,
        ))
        .style(Style::default().fg(theme.semantic.text.muted)),
        chunks[2],
    );
}

fn render_connection_dsn_input(frame: &mut Frame<'_>, area: Rect, form: &ConnectionFormState) {
    let theme = DEFAULT_THEME;
    let prompt = "dsn: ";
    let cursor = form.cursor.min(form.dsn.chars().count());
    let visible_width = area
        .width
        .saturating_sub(prompt.len() as u16)
        .saturating_sub(1) as usize;
    let viewport = cursor.saturating_sub(visible_width.saturating_sub(1));
    let cursor_spans = text_cursor_spans(&form.dsn, cursor, viewport, visible_width, &theme);
    let mut spans = vec![Span::styled(
        prompt,
        Style::default().fg(theme.semantic.text.accent),
    )];
    spans.extend(cursor_spans);
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_connection_read_only(frame: &mut Frame<'_>, area: Rect, form: &ConnectionFormState) {
    let theme = DEFAULT_THEME;
    let (label, style) = if form.read_only {
        ("on", theme.status_style(StatusTone::Success))
    } else {
        ("off", Style::default().fg(theme.semantic.text.secondary))
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "read-only: ",
                Style::default().fg(theme.semantic.text.secondary),
            ),
            Span::styled(label, style),
        ])),
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
    render_command_completion(frame, chunks[0], inner, state);
}

fn render_command_input(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let theme = DEFAULT_THEME;
    let prompt = "> ";
    let cursor = state
        .command_modal
        .cursor
        .min(state.command_modal.input.chars().count());
    let visible_width = area
        .width
        .saturating_sub(prompt.len() as u16)
        .saturating_sub(1) as usize;
    let viewport = cursor.saturating_sub(visible_width.saturating_sub(1));
    let cursor_spans = text_cursor_spans(
        &state.command_modal.input,
        cursor,
        viewport,
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

fn render_command_completion(
    frame: &mut Frame<'_>,
    input_area: Rect,
    modal_inner: Rect,
    state: &AppState,
) {
    let completion = &state.command_modal.completion;
    if !completion.visible || completion.candidates.is_empty() {
        return;
    }

    let popup_y = input_area.y.saturating_add(input_area.height);
    let modal_bottom = modal_inner.y.saturating_add(modal_inner.height);
    let available_height = modal_bottom.saturating_sub(popup_y);
    if available_height < 3 {
        return;
    }

    let visible_rows = completion
        .candidates
        .len()
        .min(6)
        .min(usize::from(available_height.saturating_sub(2)));
    if visible_rows == 0 {
        return;
    }

    let theme = DEFAULT_THEME;
    let popup_area = Rect {
        x: input_area.x,
        y: popup_y,
        width: input_area.width,
        height: (visible_rows as u16)
            .saturating_add(2)
            .min(available_height),
    };
    let selected = completion.selected.min(completion.candidates.len() - 1);
    let scroll_offset = selected.saturating_sub(visible_rows.saturating_sub(1));
    let selected_style = Style::default()
        .bg(theme.component.table.result_row_active_bg)
        .fg(theme.semantic.text.primary)
        .add_modifier(Modifier::BOLD);
    let headers = ["command"];
    let widths = [Constraint::Min(10)];

    // Paint an opaque background so the underlying output placeholder does not
    // bleed through the completion popup.
    frame.render_widget(Clear, popup_area);
    render_striped_table(
        frame,
        popup_area,
        &StripedTableConfig {
            headers: &headers,
            widths: &widths,
            header_bottom_margin: 0,
            header_separator: false,
            total_items: completion.candidates.len(),
            empty_message: "",
        },
        scroll_offset,
        &theme,
        |index| {
            let style = if index == selected {
                selected_style
            } else {
                Style::default().fg(theme.semantic.text.primary)
            };
            let candidate = completion
                .candidates
                .get(index)
                .cloned()
                .unwrap_or_default();
            vec![Cell::from(candidate).style(style)]
        },
    );
}

fn render_command_status(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let theme = DEFAULT_THEME;
    let (text, style) = match &state.command_modal.status {
        CommandStatus::Idle if state.read_only => (
            "Read-only mode blocks writes; only allow-listed read commands run".to_string(),
            Style::default().fg(theme.semantic.text.muted),
        ),
        CommandStatus::Idle => (
            "Write commands require confirmation before running.".to_string(),
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

fn render_confirm_dialog(frame: &mut Frame<'_>, confirm_state: &ConfirmState) {
    let theme = DEFAULT_THEME;
    let (_, inner) = render_modal(
        frame,
        Constraint::Percentage(64),
        Constraint::Length(7),
        "Confirm Write",
        " Enter/y:yes  Esc/n:no ",
        &theme,
    );
    let chunks = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(inner);

    frame.render_widget(
        Paragraph::new(confirm_state.prompt.clone())
            .style(Style::default().fg(theme.semantic.text.primary))
            .wrap(Wrap { trim: false }),
        chunks[0],
    );
    frame.render_widget(
        Paragraph::new("[y]es / [n]o").style(Style::default().fg(theme.semantic.text.accent)),
        chunks[1],
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::style::Modifier;

    fn render_to_string(state: &AppState) -> String {
        render_to_string_with_size(state, 120, 24)
    }

    fn render_to_string_with_size(state: &AppState, width: u16, height: u16) -> String {
        buffer_to_string(&render_buffer_with_size(state, width, height))
    }

    fn render_buffer_with_size(state: &AppState, width: u16, height: u16) -> Buffer {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, state)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn buffer_to_string(buffer: &Buffer) -> String {
        let mut output = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                output.push_str(buffer.cell((x, y)).unwrap().symbol());
            }
            if y < buffer.area.height.saturating_sub(1) {
                output.push('\n');
            }
        }
        output
    }

    #[test]
    fn ttl_label_formats_compact_human_readable_values() {
        assert_eq!(ttl_label(None), "no expiry");
        assert_eq!(ttl_label(Some(45)), "45s");
        assert_eq!(ttl_label(Some(3_600)), "1h");
        assert_eq!(ttl_label(Some(5_025)), "1h 23m 45s");
        assert_eq!(ttl_label(Some(172_801)), "2d 1s");
    }

    #[test]
    fn footer_shows_read_only_indicator_when_enabled() {
        let state = AppState::with_read_only("redis://localhost", true);

        let rendered = render_to_string(&state);

        assert!(rendered.contains("[READ-ONLY]"));
        assert!(rendered.contains("writes blocked"));
    }

    #[test]
    fn footer_omits_read_only_indicator_when_disabled() {
        let state = AppState::new("redis://localhost");

        let rendered = render_to_string(&state);

        assert!(!rendered.contains("[READ-ONLY]"));
    }

    #[test]
    fn header_shows_current_db_and_footer_omits_db_hint() {
        let state = AppState::new("redis://localhost:6379/3");

        let rendered = render_to_string(&state);
        let lines = rendered.lines().collect::<Vec<_>>();

        assert!(lines.first().is_some_and(|line| line.contains("db 3")));
        assert!(lines.last().is_some_and(|line| !line.contains("db 3")));
    }

    #[test]
    fn footer_omits_removed_direct_write_hints() {
        let state = AppState::new("redis://localhost");

        let rendered = render_to_string(&state);
        let footer = rendered.lines().last().unwrap_or_default();

        assert!(!footer.contains("x/X"));
        assert!(!footer.contains("expire"));
        assert!(!footer.contains("persist"));
        assert!(footer.contains("reload"));
        assert!(footer.contains("export"));
    }

    #[test]
    fn body_renders_vertical_divider_between_keys_and_value() {
        let state = AppState::new("redis://localhost");
        let width = 160;
        let height = 12;
        let divider_x = (width - VALUE_PANE_WIDTH - 1) as usize;

        let rendered = render_to_string_with_size(&state, width, height);
        let lines = rendered.lines().collect::<Vec<_>>();

        for line in &lines[1..height as usize - 1] {
            assert_eq!(line.chars().nth(divider_x), Some('│'));
        }
    }

    #[test]
    fn key_list_keeps_header_text_separator_and_rows_separate() {
        let mut state = AppState::new("redis://localhost");
        state.keys = vec![crate::domain::RedisKey::unknown("user:1")];
        state.filtered_indices = vec![0];
        let width = 160;
        let height = 12;

        let rendered = render_to_string_with_size(&state, width, height);
        let lines = rendered.lines().collect::<Vec<_>>();

        assert_eq!(lines[1].chars().next(), Some(' '));
        assert!(lines[1].contains("key"));
        assert!(lines[1].contains("type"));
        assert_eq!(lines[2].chars().next(), Some(' '));
        assert_eq!(
            lines[2].chars().nth(KEY_PANE_LEFT_PADDING as usize),
            Some('─')
        );
        assert!(!lines[2].contains("user:1"));
        assert!(lines[3].contains("user:1"));
    }

    #[test]
    fn key_list_header_uses_separate_separator_line() {
        let mut state = AppState::new("redis://localhost");
        state.keys = vec![crate::domain::RedisKey::unknown("user:1")];
        state.filtered_indices = vec![0];

        let buffer = render_buffer_with_size(&state, 160, 12);
        let rendered = buffer_to_string(&buffer);
        let header = rendered.lines().nth(1).unwrap_or_default();
        let separator = rendered.lines().nth(2).unwrap_or_default();
        let type_start = header.find("type").unwrap();
        let type_end = type_start + "type".len();

        assert_eq!(separator.chars().nth(type_start), Some('─'));
        for x in type_start..type_end {
            let cell = buffer.cell((x as u16, 1)).unwrap();
            assert!(!cell.modifier.contains(Modifier::UNDERLINED));
        }
    }

    #[test]
    fn db_overlay_shows_database_rows() {
        let mut state = AppState::new("redis://localhost");
        state.db_overlay = Some(DbOverlayState {
            entries: vec![(0, Some(2)), (1, Some(0))],
            selected: 1,
            loading: false,
        });

        let rendered = render_to_string(&state);

        assert!(rendered.contains("Redis Databases"));
        assert!(rendered.contains("db 1"));
        assert!(rendered.contains("0 keys"));
        assert!(rendered.contains("Enter:switch"));
    }

    #[test]
    fn confirm_dialog_shows_prompt_and_yes_no_hint() {
        let mut state = AppState::new("redis://localhost");
        state.confirm_state = Some(ConfirmState {
            op: crate::app::PendingWrite::Command {
                command: "DEL a".to_string(),
            },
            prompt: "Run this command? DEL a".to_string(),
        });

        let rendered = render_to_string(&state);

        assert!(rendered.contains("Confirm Write"));
        assert!(rendered.contains("Run this command? DEL a"));
        assert!(rendered.contains("[y]es / [n]o"));
    }

    #[test]
    fn connection_form_shows_dsn_read_only_and_hints() {
        let mut state = AppState::new("redis://localhost");
        state.connection_form = Some(ConnectionFormState {
            dsn: "redis://cache.example.com:6380/2".to_string(),
            read_only: true,
            cursor: "redis://cache.example.com:6380/2".chars().count(),
        });

        let rendered = render_to_string(&state);

        assert!(rendered.contains("Redis Connection"));
        assert!(rendered.contains("redis://cache.example.com:6380/2"));
        assert!(rendered.contains("read-only:"));
        assert!(rendered.contains("on"));
        assert!(rendered.contains("Tab:read-only"));
    }

    #[test]
    fn command_modal_shows_read_only_hint_when_enabled() {
        let mut state = AppState::with_read_only("redis://localhost", true);
        state.command_modal.is_open = true;

        let rendered = render_to_string(&state);

        assert!(rendered.contains("Read-only mode blocks writes"));
    }

    #[test]
    fn command_modal_shows_success_status_and_output() {
        let mut state = AppState::new("redis://localhost");
        state.command_modal.is_open = true;
        state.command_modal.status = CommandStatus::Success("PONG\n".to_string());

        let rendered = render_to_string(&state);

        assert!(rendered.contains("OK"));
        assert!(rendered.contains("PONG"));
    }

    #[test]
    fn command_modal_shows_completion_candidates() {
        let mut state = AppState::new("redis://localhost");
        state.command_modal.is_open = true;
        state.command_modal.input = "GET".to_string();
        state.command_modal.cursor = 3;
        state.command_modal.completion.candidates = vec![
            "GET".to_string(),
            "GETBIT".to_string(),
            "GETRANGE".to_string(),
        ];
        state.command_modal.completion.selected = 1;
        state.command_modal.completion.visible = true;

        let rendered = render_to_string(&state);

        assert!(rendered.contains("command"));
        assert!(rendered.contains("GETBIT"));
        assert!(rendered.contains("GETRANGE"));
    }

    #[test]
    fn command_modal_shows_error_status_and_output() {
        let mut state = AppState::new("redis://localhost");
        state.command_modal.is_open = true;
        state.command_modal.status = CommandStatus::Error("ERR unknown command 'NOPE'".to_string());

        let rendered = render_to_string(&state);

        assert!(rendered.contains("Error"));
        assert!(!rendered.contains("OK"));
        assert!(rendered.contains("ERR unknown command 'NOPE'"));
    }

    #[test]
    fn key_list_renders_filtered_keys_and_shows_search_pattern() {
        let mut state = AppState::new("redis://localhost");
        state.connection_status = ConnectionStatus::Connected;
        state.search_pattern = "user:*".to_string();
        state.keys = vec![
            crate::domain::RedisKey::unknown("user:1"),
            crate::domain::RedisKey {
                key: "session:1".to_string(),
                kind: crate::domain::RedisKind::String,
                ttl: None,
            },
        ];
        state.filtered_indices = vec![0];
        state.selected_index = 0;

        let rendered = render_to_string(&state);

        assert!(rendered.contains("search /user:*"));
        assert!(rendered.contains("user:1"));
        assert!(!rendered.contains("session:1"));
        assert!(rendered.contains("1/2 scanned keys"));
    }

    #[test]
    fn empty_search_result_mentions_active_pattern() {
        let mut state = AppState::new("redis://localhost");
        state.connection_status = ConnectionStatus::Connected;
        state.search_pattern = "missing:*".to_string();
        state.keys = vec![crate::domain::RedisKey::unknown("user:1")];
        state.filtered_indices = Vec::new();

        let rendered = render_to_string(&state);

        assert!(rendered.contains("No keys match pattern missing:*"));
    }

    #[test]
    fn value_pane_pretty_prints_json_string() {
        let mut state = AppState::new("redis://localhost");
        state.value_state = ValueState::Loaded {
            key: "json".to_string(),
            kind: crate::domain::RedisKind::String,
            ttl: None,
            value: RedisValue::String(r#"{"items":[1,2]}"#.to_string()),
        };

        let rendered = render_to_string_with_size(&state, 160, 12);

        assert!(rendered.contains("\"items\": ["));
        assert!(rendered.contains("    1,"));
        assert!(rendered.contains("    2"));
    }

    #[test]
    fn value_pane_wraps_long_string_values() {
        let mut state = AppState::new("redis://localhost");
        state.value_state = ValueState::Loaded {
            key: "long".to_string(),
            kind: crate::domain::RedisKind::String,
            ttl: None,
            value: RedisValue::String(
                "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda \
                 alpha beta gamma delta epsilon zeta eta theta iota kappa lambda \
                 alpha beta gamma delta epsilon zeta eta theta iota kappa lambda tail-value"
                    .to_string(),
            ),
        };

        let rendered = render_to_string_with_size(&state, 150, 12);

        assert!(rendered.contains("alpha beta gamma"));
        assert!(rendered.contains("tail-value"));
    }

    #[test]
    fn wide_key_list_keeps_long_keys_visible() {
        let mut state = AppState::new("redis://localhost");
        let long_key = "redis:key:list:primary:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string();
        state.keys = vec![crate::domain::RedisKey::unknown(long_key.clone())];
        state.filtered_indices = vec![0];

        let rendered = render_to_string_with_size(&state, 220, 12);

        assert!(rendered.contains(&long_key));
    }
}
