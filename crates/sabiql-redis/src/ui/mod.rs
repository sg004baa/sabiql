use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Cell, Clear, Paragraph, Wrap};
use sabiql_tui_kit::primitives::atoms::text_cursor_spans;
use sabiql_tui_kit::primitives::molecules::{
    StripedTableConfig, hint_line, render_modal, render_striped_table,
};
use sabiql_tui_kit::theme::{DEFAULT_THEME, StatusTone};

use crate::app::{
    AppState, CommandStatus, ConfirmState, ConnectionFormState, ConnectionStatus, DbOverlayState,
    StatusMessage, ValueEditState, ValueState,
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
    let body = Layout::horizontal([Constraint::Min(40), Constraint::Length(VALUE_PANE_WIDTH)])
        .split(vertical[1]);
    let key_area = Rect {
        width: body[0].width.saturating_add(u16::from(body[1].width > 0)),
        ..body[0]
    };
    let value_area = body[1];

    render_status(frame, vertical[0], state);
    let (key_inner, value_inner) = render_pane_borders(frame, key_area, value_area, state);
    render_keys(frame, key_inner, state);
    render_value_pane(frame, value_inner, state);
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
    if let Some(editor) = &state.value_edit {
        render_value_edit_modal(frame, editor);
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
        .fg(theme.semantic.text.accent)
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
            let is_selected = index == state.selected_index;
            let style = if is_selected {
                selected_style
            } else {
                Style::default().fg(theme.semantic.text.primary)
            };
            let marker = if is_selected { "> " } else { "  " };
            vec![
                Cell::from(format!("{marker}{}", redis_key.key)).style(style),
                Cell::from(redis_key.kind.to_string()).style(style),
            ]
        },
    );
}

fn render_pane_borders(
    frame: &mut Frame<'_>,
    key_area: Rect,
    value_area: Rect,
    state: &AppState,
) -> (Rect, Rect) {
    let theme = DEFAULT_THEME;
    let value_active = state.value_selection.is_some();
    let key_block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.panel_border_style(!value_active, false));
    let value_block = Block::default()
        .title(value_pane_title(state))
        .borders(Borders::ALL)
        .border_style(theme.panel_border_style(value_active, false));
    let key_inner = key_block.inner(key_area);
    let value_inner = value_block.inner(value_area);

    if value_active {
        frame.render_widget(key_block, key_area);
        frame.render_widget(value_block, value_area);
    } else {
        frame.render_widget(value_block, value_area);
        frame.render_widget(key_block, key_area);
    }

    if value_area.width > 0 && value_area.height > 0 {
        let separator_style = theme.panel_border_style(true, false);
        let top_joint = Rect::new(value_area.x, value_area.y, 1, 1);
        frame.render_widget(Paragraph::new("┬").style(separator_style), top_joint);
        if value_area.height > 1 {
            let bottom_joint = Rect::new(
                value_area.x,
                value_area.y.saturating_add(value_area.height - 1),
                1,
                1,
            );
            frame.render_widget(Paragraph::new("┴").style(separator_style), bottom_joint);
        }
    }

    (key_inner, value_inner)
}

fn value_pane_title(state: &AppState) -> String {
    match &state.value_state {
        ValueState::Empty => "value | no key selected".to_string(),
        ValueState::Loading { key } => format!("value | {key} | loading"),
        ValueState::Failed { key, .. } => format!("value | {key} | failed"),
        ValueState::Loaded { key, kind, ttl, .. } => {
            format!("value | {key} | type {kind} | {}", ttl_label(*ttl))
        }
    }
}

fn render_value_pane(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    match &state.value_state {
        ValueState::Empty => {
            frame.render_widget(Paragraph::new("No key selected"), area);
        }
        ValueState::Loading { .. } => {
            frame.render_widget(Paragraph::new("Loading..."), area);
        }
        ValueState::Failed { message, .. } => {
            frame.render_widget(
                Paragraph::new(message.clone())
                    .style(DEFAULT_THEME.status_style(StatusTone::Error)),
                area,
            );
        }
        ValueState::Loaded { value, .. } => {
            let active = state.value_selection.is_some();
            match value {
                RedisValue::String(value) => {
                    render_value_string(frame, area, value, state.value_scroll_offset, active);
                }
                RedisValue::List(_)
                | RedisValue::Set(_)
                | RedisValue::Hash(_)
                | RedisValue::ZSet(_)
                | RedisValue::Stream(_) => {
                    render_value_table(
                        frame,
                        area,
                        value,
                        state.value_scroll_offset,
                        state.value_selection,
                    );
                }
            }
        }
    }
}

fn render_value_table(
    frame: &mut Frame<'_>,
    area: Rect,
    value: &RedisValue,
    offset: usize,
    selection: Option<crate::app::ValueSelection>,
) {
    let theme = DEFAULT_THEME;
    let table = redis_value_table(value);
    let widths = value_widths(table.headers.len());
    let selected_style = Style::default()
        .bg(theme.component.table.result_cell_active_bg)
        .fg(theme.semantic.text.primary)
        .add_modifier(Modifier::BOLD);

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
                .enumerate()
                .map(|(column, value)| {
                    let cell = Cell::from(value.clone());
                    if selection
                        .is_some_and(|selected| selected.row == index && selected.column == column)
                    {
                        cell.style(selected_style)
                    } else {
                        cell
                    }
                })
                .collect()
        },
    );
}

fn render_value_string(
    frame: &mut Frame<'_>,
    area: Rect,
    value: &str,
    offset: usize,
    active: bool,
) {
    let theme = DEFAULT_THEME;
    let scroll_offset = u16::try_from(offset).unwrap_or(u16::MAX);
    let style = if active {
        Style::default()
            .bg(theme.component.table.result_cell_active_bg)
            .fg(theme.semantic.text.primary)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.semantic.text.primary)
    };
    let text = Text::styled(redis_string_display_value(value), style);
    frame.render_widget(
        Paragraph::new(text)
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
    if state.value_selection.is_some() {
        hints.extend([
            ("Esc", "keys"),
            ("j/k", "row"),
            ("h/l", "cell"),
            ("y", "copy"),
            ("e", "edit"),
        ]);
    } else {
        hints.extend([
            ("j/k", "navigate"),
            ("Enter", "value"),
            ("y", "copy key"),
            ("/", "search"),
            (":", "command"),
            ("c", "connect"),
            ("r", "reload"),
            ("e", "export"),
            ("q", "quit"),
        ]);
    }
    frame.render_widget(
        Paragraph::new(hint_line(&hints, &theme))
            .style(Style::default().fg(theme.semantic.text.muted)),
        area,
    );
}

fn render_db_overlay(frame: &mut Frame<'_>, overlay: &DbOverlayState) {
    let theme = DEFAULT_THEME;
    // Never present a guessed database count as truth: when the server denied
    // CONFIG GET databases, say so instead of listing a default range.
    let title = if overlay.database_count_known {
        "Redis Databases"
    } else {
        "Redis Databases (count unknown)"
    };
    let (_, inner) = render_modal(
        frame,
        Constraint::Percentage(50),
        Constraint::Percentage(60),
        title,
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

fn render_value_edit_modal(frame: &mut Frame<'_>, editor: &ValueEditState) {
    let theme = DEFAULT_THEME;
    let (_, inner) = render_modal(
        frame,
        Constraint::Percentage(80),
        Constraint::Length(5),
        "Edit Redis Value",
        " Enter:update  ^E:$EDITOR  Esc:cancel ",
        &theme,
    );
    let char_boundaries = editor
        .input
        .char_indices()
        .map(|(byte_index, _)| byte_index)
        .chain(std::iter::once(editor.input.len()))
        .collect::<Vec<_>>();
    let total_chars = char_boundaries.len().saturating_sub(1);
    let cursor = editor.cursor.min(total_chars);
    let visible_width = inner.width.saturating_sub(1) as usize;
    let max_cursor_width = visible_width.saturating_sub(1);
    let mut viewport = cursor.saturating_sub(max_cursor_width);
    while viewport < cursor
        && Line::from(&editor.input[char_boundaries[viewport]..char_boundaries[cursor]]).width()
            > max_cursor_width
    {
        viewport += 1;
    }
    let view_end = viewport.saturating_add(visible_width).min(total_chars);
    let visible_draft = &editor.input[char_boundaries[viewport]..char_boundaries[view_end]];
    let cursor_width =
        Line::from(&editor.input[char_boundaries[viewport]..char_boundaries[cursor]]).width();

    frame.render_widget(
        Paragraph::new(visible_draft)
            .style(Style::default().fg(theme.semantic.text.primary))
            .wrap(Wrap { trim: false }),
        inner,
    );
    if inner.width > 0 && inner.height > 0 {
        frame.set_cursor_position((inner.x + cursor_width as u16, inner.y));
    }
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
    use ratatui::backend::{Backend, TestBackend};
    use ratatui::buffer::Buffer;
    use ratatui::layout::Position;
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

    fn render_buffer_and_cursor_with_size(
        state: &AppState,
        width: u16,
        height: u16,
    ) -> (Buffer, Position) {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, state)).unwrap();
        let cursor = terminal.backend_mut().get_cursor_position().unwrap();
        (terminal.backend().buffer().clone(), cursor)
    }

    fn state_with_value_edit(input: &str, cursor: usize) -> AppState {
        let mut state = AppState::new("redis://localhost");
        state.value_state = ValueState::Loaded {
            key: "item".to_string(),
            kind: crate::domain::RedisKind::String,
            ttl: None,
            value: RedisValue::String(input.to_string()),
        };
        state.value_selection = Some(crate::app::ValueSelection { row: 0, column: 0 });
        crate::app::reduce(&mut state, crate::app::Action::OpenValueEditor);
        state.value_edit.as_mut().unwrap().cursor = cursor;
        state
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
    fn pane_borders_swap_focus_color_without_doubling_center_separator() {
        let width = 160;
        let height = 12;
        let center_x = width - VALUE_PANE_WIDTH;
        let body_y = 5;
        let focus = DEFAULT_THEME.semantic.surface.focus_border;
        let unfocus = DEFAULT_THEME.semantic.surface.unfocus_border;

        let keys_active =
            render_buffer_with_size(&AppState::new("redis://localhost"), width, height);
        assert_eq!(keys_active.cell((0, body_y)).unwrap().fg, focus);
        assert_eq!(keys_active.cell((width - 1, body_y)).unwrap().fg, unfocus);
        assert_eq!(keys_active.cell((center_x, body_y)).unwrap().symbol(), "│");
        assert_ne!(
            keys_active.cell((center_x - 1, body_y)).unwrap().symbol(),
            "│"
        );
        assert_ne!(
            keys_active.cell((center_x + 1, body_y)).unwrap().symbol(),
            "│"
        );
        assert!(
            !keys_active
                .cell((0, body_y))
                .unwrap()
                .modifier
                .contains(Modifier::BOLD)
        );

        let mut value_active = AppState::new("redis://localhost");
        value_active.value_selection = Some(crate::app::ValueSelection { row: 0, column: 0 });
        let value_active = render_buffer_with_size(&value_active, width, height);
        assert_eq!(value_active.cell((0, body_y)).unwrap().fg, unfocus);
        assert_eq!(value_active.cell((width - 1, body_y)).unwrap().fg, focus);
        assert_eq!(value_active.cell((center_x, body_y)).unwrap().fg, focus);
        assert!(
            !value_active
                .cell((width - 1, body_y))
                .unwrap()
                .modifier
                .contains(Modifier::BOLD)
        );
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

        assert_eq!(lines[2].chars().next(), Some('│'));
        assert!(lines[2].contains("key"));
        assert!(lines[2].contains("type"));
        assert_eq!(lines[3].chars().next(), Some('│'));
        assert_eq!(
            lines[3].chars().nth((1 + KEY_PANE_LEFT_PADDING) as usize),
            Some('─')
        );
        assert!(!lines[3].contains("user:1"));
        assert!(lines[4].contains("user:1"));
    }

    #[test]
    fn key_list_header_uses_separate_separator_line() {
        let mut state = AppState::new("redis://localhost");
        state.keys = vec![crate::domain::RedisKey::unknown("user:1")];
        state.filtered_indices = vec![0];

        let buffer = render_buffer_with_size(&state, 160, 12);
        let rendered = buffer_to_string(&buffer);
        let header = rendered.lines().nth(2).unwrap_or_default();
        let separator = rendered.lines().nth(3).unwrap_or_default();
        let type_start = header.find("type").unwrap();
        let type_end = type_start + "type".len();

        assert_eq!(separator.chars().nth(type_start), Some('─'));
        for x in type_start..type_end {
            let cell = buffer.cell((x as u16, 2)).unwrap();
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
            database_count_known: true,
        });

        let rendered = render_to_string(&state);

        assert!(rendered.contains("Redis Databases"));
        assert!(rendered.contains("db 1"));
        assert!(rendered.contains("0 keys"));
        assert!(rendered.contains("Enter:switch"));
    }

    #[test]
    fn db_overlay_marks_unknown_database_count_in_title() {
        let mut state = AppState::new("redis://localhost");
        state.db_overlay = Some(DbOverlayState {
            entries: vec![(0, Some(2))],
            selected: 0,
            loading: false,
            database_count_known: false,
        });

        let rendered = render_to_string(&state);

        assert!(rendered.contains("Redis Databases (count unknown)"));
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
        assert!(rendered.contains("> user:1"));
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

    #[test]
    fn active_value_title_omits_active_label_and_footer_advertises_inline_edit_only() {
        let mut state = AppState::new("redis://localhost");
        state.value_state = ValueState::Loaded {
            key: "profile".to_string(),
            kind: crate::domain::RedisKind::Hash,
            ttl: None,
            value: RedisValue::Hash(vec![("name".to_string(), "Ada".to_string())]),
        };
        state.value_selection = Some(crate::app::ValueSelection { row: 0, column: 1 });

        let rendered = render_to_string_with_size(&state, 160, 12);

        assert!(rendered.contains("type hash | no expiry"));
        assert!(!rendered.contains("no expiry | active"));
        let footer = rendered.lines().last().unwrap_or_default();
        assert!(footer.contains("Esc"));
        assert!(footer.contains("h/l"));
        assert!(footer.contains("copy"));
        assert!(footer.contains("edit"));
        assert!(!footer.contains("^E"));
        assert!(!footer.contains("$EDITOR"));
        assert!(!footer.contains("export"));
    }

    #[test]
    fn selected_value_cell_uses_active_table_style() {
        let mut state = AppState::new("redis://localhost");
        state.value_state = ValueState::Loaded {
            key: "profile".to_string(),
            kind: crate::domain::RedisKind::Hash,
            ttl: None,
            value: RedisValue::Hash(vec![("name".to_string(), "Ada".to_string())]),
        };
        state.value_selection = Some(crate::app::ValueSelection { row: 0, column: 1 });

        let buffer = render_buffer_with_size(&state, 160, 12);
        let active_bg = DEFAULT_THEME.component.table.result_cell_active_bg;

        let has_selected_cell = (0..buffer.area.height).any(|y| {
            (0..buffer.area.width).any(|x| {
                buffer
                    .cell((x, y))
                    .is_some_and(|cell| !cell.symbol().trim().is_empty() && cell.bg == active_bg)
            })
        });
        assert!(has_selected_cell);
    }

    #[test]
    fn selected_string_highlights_content_without_filling_value_area() {
        let backend = TestBackend::new(12, 3);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                render_value_string(frame, area, "selected", 0, true);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let active_bg = DEFAULT_THEME.component.table.result_cell_active_bg;

        let content_cell = buffer.cell((0, 0)).unwrap();
        assert_eq!(content_cell.symbol(), "s");
        assert_eq!(content_cell.bg, active_bg);
        assert_eq!(content_cell.fg, DEFAULT_THEME.semantic.text.primary);
        assert!(content_cell.modifier.contains(Modifier::BOLD));

        let trailing_cell = buffer.cell((8, 0)).unwrap();
        assert_eq!(trailing_cell.symbol(), " ");
        assert_ne!(trailing_cell.bg, active_bg);

        let lower_cell = buffer.cell((0, 1)).unwrap();
        assert_eq!(lower_cell.symbol(), " ");
        assert_ne!(lower_cell.bg, active_bg);
    }

    #[test]
    fn value_edit_modal_renders_input_and_editor_submit_cancel_hints() {
        let mut state = AppState::new("redis://localhost");
        state.value_state = ValueState::Loaded {
            key: "item".to_string(),
            kind: crate::domain::RedisKind::String,
            ttl: None,
            value: RedisValue::String("old value".to_string()),
        };
        state.value_selection = Some(crate::app::ValueSelection { row: 0, column: 0 });
        crate::app::reduce(&mut state, crate::app::Action::OpenValueEditor);

        let rendered = render_to_string_with_size(&state, 160, 12);

        assert!(rendered.contains("Edit Redis Value"));
        assert!(rendered.contains("old value"));
        assert!(rendered.contains("Enter:update"));
        assert!(rendered.contains("^E:$EDITOR"));
        assert!(rendered.contains("Esc:cancel"));
    }

    #[test]
    fn value_edit_modal_places_real_cursor_at_middle_without_highlighting_draft() {
        let state = state_with_value_edit("abcd", 2);
        let (buffer, cursor) = render_buffer_and_cursor_with_size(&state, 80, 12);
        let draft_start = (0..buffer.area.width)
            .find(|&x| buffer.cell((x, cursor.y)).unwrap().symbol() == "a")
            .unwrap();

        assert_eq!(cursor.x, draft_start + 2);
        for (offset, symbol) in ["a", "b", "c", "d"].into_iter().enumerate() {
            let cell = buffer
                .cell((draft_start + offset as u16, cursor.y))
                .unwrap();
            assert_eq!(cell.symbol(), symbol);
            assert_ne!(cell.bg, DEFAULT_THEME.semantic.cursor.bg);
        }
    }

    #[test]
    fn value_edit_modal_keeps_end_cursor_visible_after_horizontal_scroll() {
        let input = "0123456789".repeat(8);
        let state = state_with_value_edit(&input, input.chars().count());
        let (buffer, cursor) = render_buffer_and_cursor_with_size(&state, 40, 12);

        assert!(cursor.x < buffer.area.width);
        assert_eq!(buffer.cell((cursor.x - 1, cursor.y)).unwrap().symbol(), "9");
        assert_ne!(
            buffer.cell((cursor.x - 1, cursor.y)).unwrap().bg,
            DEFAULT_THEME.semantic.cursor.bg
        );
    }

    #[test]
    fn value_edit_modal_cursor_uses_unicode_display_width_and_character_index() {
        let state = state_with_value_edit("a界b", 2);
        let (buffer, cursor) = render_buffer_and_cursor_with_size(&state, 80, 12);
        let draft_start = (0..buffer.area.width)
            .find(|&x| buffer.cell((x, cursor.y)).unwrap().symbol() == "a")
            .unwrap();

        assert_eq!(
            buffer.cell((draft_start + 1, cursor.y)).unwrap().symbol(),
            "界"
        );
        assert_eq!(
            buffer.cell((draft_start + 3, cursor.y)).unwrap().symbol(),
            "b"
        );
        assert_eq!(cursor.x, draft_start + 3);
    }
}
