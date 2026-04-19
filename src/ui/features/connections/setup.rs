use ratatui::prelude::*;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::app::model::app_state::AppState;
use crate::app::model::connection::setup::{
    CONNECTION_INPUT_VISIBLE_WIDTH, CONNECTION_INPUT_WIDTH, ConnectionField, ConnectionSetupState,
};
use crate::domain::connection::{DatabaseType, SslMode};
use crate::ui::primitives::atoms::text_cursor_spans;
use crate::ui::primitives::molecules::render_modal;
use crate::ui::theme::ThemePalette;

const LABEL_WIDTH: u16 = 12;
const INPUT_WIDTH: u16 = CONNECTION_INPUT_WIDTH;
const ERROR_WIDTH: u16 = 12;
const FIELD_HEIGHT: u16 = 1;
const DROPDOWN_ITEM_COUNT: usize = 6;

fn bracketed_input(content: &str, border_style: Style, theme: &ThemePalette) -> Line<'static> {
    Line::from(vec![
        Span::styled("[", border_style),
        Span::styled(
            format!(" {content} "),
            Style::default().fg(theme.semantic.text.primary),
        ),
        Span::styled("]", border_style),
    ])
}

pub struct ConnectionSetup;

impl ConnectionSetup {
    pub fn render(frame: &mut Frame, state: &AppState, theme: &ThemePalette) {
        let form_state = &state.connection_setup;
        let skip_ssl = form_state.skip_ssl();

        let modal_width = LABEL_WIDTH + INPUT_WIDTH + ERROR_WIDTH + 8;
        let modal_height: u16 = if skip_ssl { 13 } else { 14 };

        let (title, hint) = if form_state.is_edit_mode() {
            (
                " Edit Connection ",
                " Tab: Next │ Shift+Tab: Prev │ Ctrl+S: Save │ Esc: Cancel ",
            )
        } else {
            (
                " New Connection ",
                " Tab: Next │ Shift+Tab: Prev │ Ctrl+S: Connect │ Esc: Cancel ",
            )
        };
        let (_, modal_inner) = render_modal(
            frame,
            Constraint::Length(modal_width),
            Constraint::Length(modal_height),
            title,
            hint,
            theme,
        );

        let inner = modal_inner.inner(Margin::new(2, 1));

        let mut constraints = vec![
            Constraint::Length(FIELD_HEIGHT), // DatabaseType
            Constraint::Length(FIELD_HEIGHT), // Name
            Constraint::Length(FIELD_HEIGHT), // Host
            Constraint::Length(FIELD_HEIGHT), // Port
            Constraint::Length(FIELD_HEIGHT), // Database
            Constraint::Length(FIELD_HEIGHT), // User
            Constraint::Length(FIELD_HEIGHT), // Password
        ];
        if !skip_ssl {
            constraints.push(Constraint::Length(FIELD_HEIGHT)); // SslMode
        }
        constraints.push(Constraint::Length(1)); // spacer
        constraints.push(Constraint::Length(1)); // notice

        let chunks = Layout::vertical(constraints).split(inner);

        // Row indices shift depending on whether SslMode is present
        let mut row = 0;

        Self::render_selector_field(
            frame,
            chunks[row],
            "Type:",
            &form_state.database_type.to_string(),
            form_state.focused_field == ConnectionField::DatabaseType,
            theme,
        );
        row += 1;

        Self::render_text_field(
            frame,
            chunks[row],
            form_state,
            ConnectionField::Name,
            false,
            theme,
        );
        row += 1;
        Self::render_text_field(
            frame,
            chunks[row],
            form_state,
            ConnectionField::Host,
            false,
            theme,
        );
        row += 1;
        Self::render_text_field(
            frame,
            chunks[row],
            form_state,
            ConnectionField::Port,
            false,
            theme,
        );
        row += 1;
        Self::render_text_field(
            frame,
            chunks[row],
            form_state,
            ConnectionField::Database,
            false,
            theme,
        );
        row += 1;
        Self::render_text_field(
            frame,
            chunks[row],
            form_state,
            ConnectionField::User,
            false,
            theme,
        );
        row += 1;
        Self::render_text_field(
            frame,
            chunks[row],
            form_state,
            ConnectionField::Password,
            true,
            theme,
        );
        row += 1;

        let ssl_row = row;
        if !skip_ssl {
            Self::render_selector_field(
                frame,
                chunks[row],
                "SSL Mode:",
                &form_state.ssl_mode.to_string(),
                form_state.focused_field == ConnectionField::SslMode,
                theme,
            );
            row += 1;
        }

        // spacer row is `row`, notice is `row + 1`
        let notice = "Note: Connection info is stored locally in plain text";
        let notice_para =
            Paragraph::new(notice).style(Style::default().fg(theme.component.feedback.note_text));
        frame.render_widget(notice_para, chunks[row + 1]);

        // Dropdowns (rendered last so they overlap fields below)
        if form_state.db_type_dropdown.is_open {
            Self::render_dropdown_items(
                frame,
                chunks[0],
                DatabaseType::ALL.iter().map(ToString::to_string).collect(),
                form_state.db_type_dropdown.selected_index,
                theme,
            );
        } else if !skip_ssl && form_state.ssl_dropdown.is_open {
            Self::render_dropdown_items(
                frame,
                chunks[ssl_row],
                SslMode::all_variants()
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
                form_state.ssl_dropdown.selected_index,
                theme,
            );
        }
    }

    fn render_text_field(
        frame: &mut Frame,
        area: Rect,
        state: &ConnectionSetupState,
        field: ConnectionField,
        mask: bool,
        theme: &ThemePalette,
    ) {
        let is_focused = field == state.focused_field;
        let value = state.field_value(field);
        let error = state.validation_errors.get(&field);

        let chunks = Layout::horizontal([
            Constraint::Length(LABEL_WIDTH),
            Constraint::Length(INPUT_WIDTH),
            Constraint::Length(ERROR_WIDTH),
        ])
        .split(area);

        let label_style = if is_focused {
            Style::default().fg(theme.semantic.text.secondary).bold()
        } else {
            Style::default().fg(theme.semantic.text.secondary)
        };
        let label_para = Paragraph::new(field.label()).style(label_style);
        frame.render_widget(label_para, chunks[0]);

        let display_value = if mask {
            "*".repeat(value.chars().count())
        } else {
            value.to_string()
        };

        let content_width = CONNECTION_INPUT_VISIBLE_WIDTH;

        let border_style = theme.modal_input_border_style(is_focused, error.is_some());

        let input_line = if is_focused {
            let input = state.focused_input().unwrap();
            let viewport = input.viewport_offset();
            let cursor = input.cursor();
            let char_count = display_value.chars().count();

            // same reservation logic as TextInputState::update_viewport
            let effective_width = if cursor >= char_count {
                content_width.saturating_sub(1)
            } else {
                content_width
            };

            let cursor_spans =
                text_cursor_spans(&display_value, cursor, viewport, effective_width, theme);

            // Calculate total display width of cursor spans (including block cursor)
            let used_width: usize = cursor_spans.iter().map(|s| s.content.chars().count()).sum();
            let padding = content_width.saturating_sub(used_width);

            let mut spans = vec![
                Span::styled("[", border_style),
                Span::styled(" ", Style::default().fg(theme.semantic.text.primary)),
            ];
            spans.extend(cursor_spans);
            if padding > 0 {
                spans.push(Span::raw(" ".repeat(padding)));
            }
            spans.push(Span::styled(
                " ",
                Style::default().fg(theme.semantic.text.primary),
            ));
            spans.push(Span::styled("]", border_style));
            Line::from(spans)
        } else {
            let truncated: String = display_value.chars().take(content_width).collect();
            let padding = content_width.saturating_sub(truncated.chars().count());
            bracketed_input(
                &format!("{}{}", truncated, " ".repeat(padding)),
                border_style,
                theme,
            )
        };

        let input_para = Paragraph::new(input_line);
        frame.render_widget(input_para, chunks[1]);

        if let Some(err) = error {
            let err_para = Paragraph::new(format!(" {err}"))
                .style(Style::default().fg(theme.semantic.status.error));
            frame.render_widget(err_para, chunks[2]);
        }
    }

    fn render_selector_field(
        frame: &mut Frame,
        area: Rect,
        label: &str,
        value: &str,
        is_focused: bool,
        theme: &ThemePalette,
    ) {
        let chunks = Layout::horizontal([
            Constraint::Length(LABEL_WIDTH),
            Constraint::Length(INPUT_WIDTH),
            Constraint::Length(ERROR_WIDTH),
        ])
        .split(area);

        let label_style = if is_focused {
            Style::default().fg(theme.semantic.text.secondary).bold()
        } else {
            Style::default().fg(theme.semantic.text.secondary)
        };
        let label_para = Paragraph::new(label.to_string()).style(label_style);
        frame.render_widget(label_para, chunks[0]);

        let content_width = CONNECTION_INPUT_VISIBLE_WIDTH;
        let display_content = format!("{:<1$} ▼", value, content_width - 2);

        let border_style = theme.modal_input_border_style(is_focused, false);

        let input_para = Paragraph::new(bracketed_input(&display_content, border_style, theme));
        frame.render_widget(input_para, chunks[1]);
    }

    fn render_dropdown_items(
        frame: &mut Frame,
        field_area: Rect,
        items: Vec<String>,
        selected_index: usize,
        theme: &ThemePalette,
    ) {
        let chunks = Layout::horizontal([
            Constraint::Length(LABEL_WIDTH),
            Constraint::Length(INPUT_WIDTH),
            Constraint::Length(ERROR_WIDTH),
        ])
        .split(field_area);

        let item_count = items.len().min(DROPDOWN_ITEM_COUNT);
        let dropdown_area = Rect {
            x: chunks[1].x,
            y: chunks[1].y + 1,
            width: INPUT_WIDTH,
            height: item_count as u16 + 2,
        };

        frame.render_widget(Clear, dropdown_area);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.modal_border_style())
            .style(Style::default());
        frame.render_widget(block, dropdown_area);

        let inner = dropdown_area.inner(Margin::new(1, 1));

        for (i, label) in items.iter().enumerate() {
            if i >= DROPDOWN_ITEM_COUNT {
                break;
            }
            let item_area = Rect {
                x: inner.x,
                y: inner.y + i as u16,
                width: inner.width,
                height: 1,
            };

            let is_selected = i == selected_index;
            let item_style = if is_selected {
                theme.picker_selected_style()
            } else {
                Style::default().fg(theme.semantic.text.secondary)
            };

            let item_para = Paragraph::new(label.clone()).style(item_style);
            frame.render_widget(item_para, item_area);
        }
    }
}
