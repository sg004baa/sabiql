use std::borrow::Cow;
use std::collections::BTreeSet;
use std::time::Instant;

use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Cell, Paragraph, Row, Table, Wrap};

use crate::primitives::atoms::{CursorKind, panel_block_highlight, text_cursor_spans_with_kind};

use crate::app::model::app_state::AppState;
use crate::app::model::shared::focused_pane::FocusedPane;
use crate::app::model::shared::ui_state::{RESULT_INNER_OVERHEAD, ResultSelection, YankFlash};
use crate::app::model::shared::viewport::{
    ColumnWidthConfig, ColumnWidthsCache, SelectionContext, ViewportPlan, select_viewport_columns,
};
use crate::domain::{QueryResult, QuerySource};
use crate::primitives::utils::text_utils::{
    MIN_COL_WIDTH, PADDING, calculate_header_min_widths, search_viewport_offset,
    slice_chars_fitting_width,
};
use crate::theme::ThemePalette;

pub struct ResultPane;

impl ResultPane {
    pub fn render(
        frame: &mut Frame,
        area: Rect,
        state: &AppState,
        now: Instant,
        theme: &ThemePalette,
    ) -> (ViewportPlan, ColumnWidthsCache) {
        let is_focused = state.ui.focused_pane == FocusedPane::Result;
        let should_highlight = state
            .query
            .result_highlight_until()
            .is_some_and(|t| now < t);

        let result = state.query.visible_result();
        let title = Self::build_title(result, state);

        let block = panel_block_highlight(&title, is_focused, should_highlight, theme);

        let default_result = || (ViewportPlan::default(), ColumnWidthsCache::default());

        if let Some(result) = result {
            if result.is_error() {
                Self::render_error(frame, area, result, block, theme);
                default_result()
            } else if result.rows.is_empty() {
                Self::render_empty(frame, area, block, theme);
                default_result()
            } else {
                let history_bar = state.query.history_bar();
                Self::render_table(
                    frame,
                    area,
                    result,
                    block,
                    state.result_interaction.scroll_offset,
                    state.result_interaction.horizontal_offset,
                    &state.ui.result_viewport_plan,
                    &state.ui.result_widths_cache,
                    state.query.result_generation(),
                    state.query.history_index(),
                    state.result_interaction.selection(),
                    if state.result_interaction.cell_edit().is_active() {
                        Some((
                            state.result_interaction.cell_edit().row.unwrap_or_default(),
                            state.result_interaction.cell_edit().col.unwrap_or_default(),
                            state.result_interaction.cell_edit().draft_value(),
                            state.input_mode()
                                == crate::app::model::shared::input_mode::InputMode::CellEdit,
                            state.result_interaction.cell_edit().input.cursor(),
                        ))
                    } else {
                        None
                    },
                    state.result_interaction.staged_delete_rows(),
                    history_bar,
                    state.result_interaction.yank_flash,
                    now,
                    theme,
                )
            }
        } else {
            Self::render_placeholder(frame, area, block, theme);
            default_result()
        }
    }

    fn build_title(result: Option<&QueryResult>, state: &AppState) -> String {
        match result {
            None => " [3] Result ".to_string(),
            Some(r) => {
                let name = match r.source {
                    QuerySource::Preview => "Result",
                    QuerySource::Adhoc => "Result Query",
                };

                let history_hint = if state.query.has_history_hint() {
                    " (history: ^H)"
                } else {
                    ""
                };

                if r.is_error() {
                    format!(" [3] {name} ERROR{history_hint} ")
                } else {
                    format!(
                        " [3] {} ({}, {}ms){} ",
                        name,
                        r.row_count_display(),
                        r.execution_time_ms,
                        history_hint,
                    )
                }
            }
        }
    }

    fn render_placeholder(frame: &mut Frame, area: Rect, block: Block, theme: &ThemePalette) {
        let content = Paragraph::new("(select a table to preview)")
            .block(block)
            .style(Style::default().fg(theme.semantic.text.placeholder));
        frame.render_widget(content, area);
    }

    fn render_empty(frame: &mut Frame, area: Rect, block: Block, theme: &ThemePalette) {
        let content = Paragraph::new("No rows returned")
            .block(block)
            .style(Style::default().fg(theme.semantic.text.placeholder));
        frame.render_widget(content, area);
    }

    fn render_error(
        frame: &mut Frame,
        area: Rect,
        result: &QueryResult,
        block: Block,
        theme: &ThemePalette,
    ) {
        let error_msg = result.error.as_deref().unwrap_or("Unknown error");

        let block = block.style(Style::default().fg(theme.semantic.status.error));

        let content = Paragraph::new(error_msg)
            .block(block)
            .wrap(Wrap { trim: false })
            .style(Style::default().fg(theme.semantic.status.error));

        frame.render_widget(content, area);
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "render function requires full viewport context (16 params)"
    )]
    fn render_table(
        frame: &mut Frame,
        area: Rect,
        result: &QueryResult,
        block: Block,
        scroll_offset: usize,
        horizontal_offset: usize,
        stored_plan: &ViewportPlan,
        stored_cache: &ColumnWidthsCache,
        result_generation: u64,
        history_index: Option<usize>,
        selection: &ResultSelection,
        editing_cell: Option<(usize, usize, &str, bool, usize)>,
        staged_delete_rows: &BTreeSet<usize>,
        history_bar: Option<(usize, usize)>,
        yank_flash: Option<YankFlash>,
        now: Instant,
        theme: &ThemePalette,
    ) -> (ViewportPlan, ColumnWidthsCache) {
        let inner = block.inner(area);
        frame.render_widget(block, area);

        if result.columns.is_empty() {
            return (ViewportPlan::default(), ColumnWidthsCache::default());
        }

        let cached = stored_cache.is_valid(result_generation, history_index);
        let fresh_ideal;
        let fresh_min;
        let (ideal_widths, min_widths) = if cached {
            (
                &stored_cache.ideal_widths[..],
                &stored_cache.header_min_widths[..],
            )
        } else {
            fresh_ideal = calculate_ideal_widths(&result.columns, &result.rows);
            fresh_min = calculate_header_min_widths(&result.columns);
            (&fresh_ideal[..], &fresh_min[..])
        };

        let current_min_widths_sum: u16 = min_widths.iter().sum();
        let current_ideal_widths_sum: u16 = ideal_widths.iter().sum();
        let current_ideal_widths_max: u16 = ideal_widths.iter().copied().max().unwrap_or(0);

        let plan = if stored_plan.needs_recalculation(
            ideal_widths.len(),
            inner.width,
            current_min_widths_sum,
            current_ideal_widths_sum,
            current_ideal_widths_max,
        ) {
            ViewportPlan::calculate(ideal_widths, min_widths, inner.width)
        } else {
            stored_plan.clone()
        };

        let widths_cache = if cached {
            stored_cache.clone()
        } else {
            ColumnWidthsCache::new(
                ideal_widths.to_vec(),
                min_widths.to_vec(),
                result_generation,
                history_index,
            )
        };

        let clamped_offset = horizontal_offset.min(plan.max_offset);

        let config = ColumnWidthConfig {
            ideal_widths,
            min_widths,
        };
        let ctx = SelectionContext {
            horizontal_offset: clamped_offset,
            available_width: inner.width,
            fixed_count: Some(plan.column_count),
            max_offset: plan.max_offset,
            slack_policy: plan.slack_policy,
        };
        let (viewport_indices, viewport_widths) = select_viewport_columns(&config, &ctx);

        if viewport_indices.is_empty() {
            return (plan, widths_cache);
        }

        let widths: Vec<Constraint> = viewport_widths
            .iter()
            .map(|&w| Constraint::Length(w))
            .collect();

        let header = Row::new(viewport_indices.iter().map(|&idx| {
            let col_name = result.columns.get(idx).map_or("", String::as_str);
            Cell::from(col_name.to_string())
        }))
        .style(
            Style::default()
                .add_modifier(Modifier::UNDERLINED)
                .add_modifier(Modifier::BOLD)
                .fg(theme.semantic.text.primary),
        )
        .height(1);

        let data_rows_visible = inner.height.saturating_sub(RESULT_INNER_OVERHEAD) as usize;
        let scroll_viewport_size = data_rows_visible;
        let active_row = selection.row();
        let active_cell = selection.cell();

        let yank_flash_active = yank_flash.is_some_and(|f| now < f.until);

        let rows: Vec<Row> = result
            .rows
            .iter()
            .enumerate()
            .skip(scroll_offset)
            .take(data_rows_visible)
            .map(|(abs_row_idx, row)| {
                let is_staged_for_delete = staged_delete_rows.contains(&abs_row_idx);
                let is_active_row = active_row == Some(abs_row_idx);
                // None = no flash; Some(None) = full row; Some(Some(c)) = cell c
                let flash_scope = yank_flash
                    .filter(|f| yank_flash_active && f.row == abs_row_idx)
                    .map(|f| f.col);
                let is_row_flash = flash_scope == Some(None);
                let row_bg = if is_row_flash {
                    Some(theme.component.feedback.yank_flash_bg)
                } else if is_staged_for_delete {
                    Some(theme.component.table.staged_delete_bg)
                } else if is_active_row {
                    Some(theme.component.table.result_row_active_bg)
                } else if (abs_row_idx - scroll_offset) % 2 == 1 {
                    Some(theme.component.table.striped_row_bg)
                } else {
                    None
                };

                let cells: Vec<Cell> = viewport_indices
                    .iter()
                    .zip(viewport_widths.iter())
                    .map(|(&orig_idx, &col_width)| {
                        let val = row.get(orig_idx).map_or("", String::as_str).to_string();
                        let is_editing_cell = editing_cell
                            .is_some_and(|(er, ec, _, _, _)| er == abs_row_idx && ec == orig_idx);
                        let mut cell;
                        if let Some((_, _, draft, actively_editing, cursor_pos)) = editing_cell
                            && is_editing_cell
                        {
                            if actively_editing {
                                let line = cell_edit_line_with_cursor(
                                    draft,
                                    cursor_pos,
                                    col_width as usize,
                                    theme,
                                );
                                cell = Cell::from(line).style(
                                    Style::default()
                                        .bg(theme.component.table.result_cell_active_bg)
                                        .fg(theme.component.table.cell_edit_fg),
                                );
                            } else {
                                let display = truncate_cell(draft, col_width as usize);
                                cell = Cell::from(display).style(
                                    Style::default()
                                        .bg(theme.component.table.result_cell_active_bg)
                                        .fg(theme.semantic.status.pending),
                                );
                            }
                        } else {
                            let display = truncate_cell(&val, col_width as usize);
                            cell = Cell::from(display);
                        }
                        if !is_editing_cell {
                            if is_row_flash || flash_scope == Some(Some(orig_idx)) {
                                cell = cell.style(
                                    Style::default()
                                        .fg(theme.component.feedback.yank_flash_fg)
                                        .bg(theme.component.feedback.yank_flash_bg),
                                );
                            } else if is_staged_for_delete {
                                cell = cell.style(
                                    Style::default().fg(theme.component.table.staged_delete_fg),
                                );
                            } else if is_active_row && active_cell == Some(orig_idx) {
                                cell = cell.style(
                                    Style::default()
                                        .bg(theme.component.table.result_cell_active_bg),
                                );
                            }
                        }
                        cell
                    })
                    .collect();

                let mut r = Row::new(cells);
                if let Some(bg) = row_bg {
                    r = r.style(Style::default().bg(bg));
                }
                r
            })
            .collect();

        let table = Table::new(rows, widths)
            .header(header)
            .style(Style::default().fg(theme.semantic.text.primary));

        frame.render_widget(table, inner);

        // Scroll indicators (pass inner area, not outer with border)
        let total_rows = result.rows.len();
        let total_cols = result.columns.len();

        use crate::primitives::atoms::scroll_indicator::{
            HorizontalScrollParams, VerticalScrollParams, render_horizontal_scroll_indicator,
            render_vertical_scroll_indicator_bar,
        };
        let has_h_scroll = total_cols > plan.column_count;
        render_vertical_scroll_indicator_bar(
            frame,
            inner,
            VerticalScrollParams {
                position: scroll_offset,
                viewport_size: scroll_viewport_size,
                total_items: total_rows,
                has_horizontal_scrollbar: has_h_scroll,
            },
            theme,
        );
        // Split bottom row: history bar on left, h-scroll indicator on right
        let history_bar_width = if let Some((idx, total)) = history_bar {
            let text = format!("\u{25C0} {}/{} \u{25B6}", idx + 1, total);
            let render_width = (text.chars().count() as u16).min(inner.width);
            let bottom_row = inner.y + inner.height.saturating_sub(1);
            frame.render_widget(
                Paragraph::new(Line::from(vec![ratatui::text::Span::styled(
                    text,
                    Style::default().fg(theme.semantic.text.secondary),
                )])),
                Rect::new(inner.x, bottom_row, render_width, 1),
            );
            render_width
        } else {
            0
        };

        // Shift h-scroll indicator right to avoid overlapping the history bar
        let h_scroll_area = Rect::new(
            inner.x + history_bar_width,
            inner.y,
            inner.width.saturating_sub(history_bar_width),
            inner.height,
        );
        render_horizontal_scroll_indicator(
            frame,
            h_scroll_area,
            HorizontalScrollParams {
                position: clamped_offset,
                viewport_size: plan.column_count,
                total_items: total_cols,
            },
            theme,
        );

        (plan, widths_cache)
    }
}

/// Upper bound (in characters) on a result-pane column's ideal width. Stops a
/// single long value (UUID, JSON, free text) from monopolizing the viewport and
/// pushing every other column off-screen behind a horizontal scroll. Cells wider
/// than this are truncated with a trailing ellipsis.
const RESULT_MAX_COL_WIDTH: u16 = 50;

pub(crate) fn calculate_ideal_widths(headers: &[String], rows: &[Vec<String>]) -> Vec<u16> {
    const SAMPLE_ROWS: usize = 50;

    headers
        .iter()
        .enumerate()
        .map(|(col_idx, header)| {
            let mut max_width = header.chars().count();

            let sample_size = rows.len().min(SAMPLE_ROWS);
            for row in rows.iter().take(sample_size) {
                if let Some(cell) = row.get(col_idx) {
                    let escaped = escape_for_display(cell);
                    let cell_width = escaped.chars().count();
                    max_width = max_width.max(cell_width);
                }
            }

            (max_width as u16 + PADDING).clamp(MIN_COL_WIDTH, RESULT_MAX_COL_WIDTH)
        })
        .collect()
}

fn cell_edit_line_with_cursor(
    text: &str,
    cursor: usize,
    max_chars: usize,
    theme: &ThemePalette,
) -> Line<'static> {
    if max_chars == 0 {
        return Line::from(vec![]);
    }

    let visible_width = max_chars.saturating_sub(1);
    let viewport_offset = search_viewport_offset(text, cursor, visible_width);
    let visible = slice_chars_fitting_width(text, viewport_offset, visible_width);
    let relative_cursor = cursor.saturating_sub(viewport_offset);

    Line::from(text_cursor_spans_with_kind(
        &visible,
        relative_cursor,
        0,
        visible.chars().count(),
        CursorKind::Block,
        theme,
    ))
}

/// Render control characters as their backslash-escape form so that values
/// containing `\n` / `\r` / `\t` / `\0` / `\Z` show as readable literals in the
/// fixed-height result grid instead of breaking the row layout or being hidden
/// past the first line.
///
/// Bare backslashes are intentionally not doubled — most stored values
/// (filesystem paths, JSON, etc.) contain literal `\` and rewriting them to
/// `\\` would obscure far more than it would clarify.
fn escape_for_display(s: &str) -> Cow<'_, str> {
    if !s
        .chars()
        .any(|c| matches!(c, '\n' | '\r' | '\t' | '\0' | '\u{001A}'))
    {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\0' => out.push_str("\\0"),
            '\u{001A}' => out.push_str("\\Z"),
            other => out.push(other),
        }
    }
    Cow::Owned(out)
}

fn truncate_cell(s: &str, max_chars: usize) -> String {
    let escaped = escape_for_display(s);
    let char_count = escaped.chars().count();

    if char_count <= max_chars {
        escaped.into_owned()
    } else {
        let truncated: String = escaped.chars().take(max_chars.saturating_sub(3)).collect();
        format!("{truncated}...")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::DEFAULT_THEME;
    use rstest::rstest;
    use unicode_width::UnicodeWidthStr;

    fn line_display_width(line: &Line<'_>) -> usize {
        line.spans
            .iter()
            .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
            .sum()
    }

    fn line_text(line: &Line<'_>) -> String {
        let mut text = String::new();
        for span in &line.spans {
            text.push_str(span.content.as_ref());
        }
        text
    }

    mod calculate_ideal_widths_tests {
        use super::*;

        #[test]
        fn empty_headers_returns_empty_vec() {
            let headers: Vec<String> = vec![];
            let rows: Vec<Vec<String>> = vec![];

            let result = calculate_ideal_widths(&headers, &rows);

            assert_eq!(result.len(), 0);
        }

        #[test]
        fn single_column_uses_header_width_plus_padding() {
            let headers = vec!["name".to_string()];
            let rows: Vec<Vec<String>> = vec![];

            let result = calculate_ideal_widths(&headers, &rows);

            assert_eq!(result.len(), 1);
            // "name" = 4 chars + 2 padding = 6
            assert_eq!(result[0], 6);
        }

        #[test]
        fn uses_max_of_header_and_cell_widths() {
            let headers = vec!["id".to_string(), "name".to_string()];
            let rows = vec![
                vec!["1".to_string(), "Alice".to_string()],
                vec!["2".to_string(), "Bob".to_string()],
            ];

            let result = calculate_ideal_widths(&headers, &rows);

            assert_eq!(result.len(), 2);
            // id: max(2, 1) + 2 = 4
            assert_eq!(result[0], 4);
            // name: max(4, 5) + 2 = 7
            assert_eq!(result[1], 7);
        }

        #[test]
        fn respects_max_width_constraint() {
            let headers = vec!["description".to_string()];
            let long_text = "a".repeat(300);
            let rows = vec![vec![long_text]];

            let result = calculate_ideal_widths(&headers, &rows);

            assert_eq!(result.len(), 1);
            // Should be capped at RESULT_MAX_COL_WIDTH (50)
            assert_eq!(result[0], 50);
        }

        #[test]
        fn handles_multibyte_characters_correctly() {
            let headers = vec!["名前".to_string()];
            let rows = vec![vec!["日本語テスト".to_string()]];

            let result = calculate_ideal_widths(&headers, &rows);

            assert_eq!(result.len(), 1);
            // "日本語テスト" = 6 chars + 2 padding = 8
            assert_eq!(result[0], 8);
        }

        #[test]
        fn newline_in_cell_widens_column_for_escape_literal() {
            // Newlines now render as `\n` literals, so width estimation must
            // account for the escaped form rather than only the first line.
            let headers = vec!["text".to_string()];
            let rows = vec![vec!["short\nlong".to_string()]];

            let result = calculate_ideal_widths(&headers, &rows);

            assert_eq!(result.len(), 1);
            // "short\\nlong" = 11 chars, max(4, 11) + 2 = 13
            assert_eq!(result[0], 13);
        }

        #[test]
        fn handles_multiple_columns_independently() {
            let headers = vec!["id".to_string(), "name".to_string(), "email".to_string()];
            let rows = vec![
                vec![
                    "1".to_string(),
                    "Alice".to_string(),
                    "alice@example.com".to_string(),
                ],
                vec![
                    "22".to_string(),
                    "Bob Smith Jr.".to_string(),
                    "bob@ex.com".to_string(),
                ],
            ];

            let result = calculate_ideal_widths(&headers, &rows);

            assert_eq!(result.len(), 3);
            // id: max(2, 2) + 2 = 4
            assert_eq!(result[0], 4);
            // name: max(4, 13) + 2 = 15
            assert_eq!(result[1], 15);
            // email: max(5, 17) + 2 = 19
            assert_eq!(result[2], 19);
        }
    }

    #[test]
    fn short_string_returns_unchanged() {
        let result = truncate_cell("hello", 10);

        assert_eq!(result, "hello");
    }

    #[test]
    fn exact_length_returns_unchanged() {
        let result = truncate_cell("hello", 5);

        assert_eq!(result, "hello");
    }

    #[test]
    fn long_string_truncates_with_ellipsis() {
        let result = truncate_cell("hello world", 8);

        assert_eq!(result, "hello...");
    }

    #[test]
    fn multibyte_characters_count_correctly() {
        let result = truncate_cell("こんにちは世界", 5);

        assert_eq!(result, "こん...");
    }

    #[rstest]
    #[case("日本語テスト", 10, "日本語テスト")]
    #[case("日本語テスト", 5, "日本...")]
    #[case("日本語テスト", 4, "日...")]
    #[case("日本語テスト", 3, "...")]
    #[case("SELECT * FROM 日本語テーブル", 15, "SELECT * FRO...")]
    fn multibyte_truncation_is_safe(
        #[case] input: &str,
        #[case] max: usize,
        #[case] expected: &str,
    ) {
        let result = truncate_cell(input, max);

        assert_eq!(result, expected);
        assert!(result.chars().count() <= max);
    }

    #[test]
    fn newline_is_rendered_as_escape_literal() {
        let result = truncate_cell("first\nsecond\nthird", 30);

        assert_eq!(result, "first\\nsecond\\nthird");
    }

    #[test]
    fn newline_with_truncation_truncates_escaped_form() {
        let result = truncate_cell("this is a long first line\nsecond", 10);

        assert_eq!(result, "this is...");
    }

    #[test]
    fn control_characters_render_as_backslash_escapes() {
        let raw = "A\0B\nC\tD\r\nE";
        let result = truncate_cell(raw, 40);

        assert_eq!(result, "A\\0B\\nC\\tD\\r\\nE");
    }

    #[test]
    fn sub_character_renders_as_backslash_z() {
        let result = truncate_cell("A\u{001A}B", 10);

        assert_eq!(result, "A\\ZB");
    }

    #[test]
    fn bare_backslash_is_left_untouched() {
        let result = truncate_cell("C:\\Users\\name", 30);

        assert_eq!(result, "C:\\Users\\name");
    }

    #[test]
    fn empty_string_returns_empty() {
        let result = truncate_cell("", 10);

        assert_eq!(result, "");
    }

    #[test]
    fn zero_max_chars_returns_ellipsis_only() {
        let result = truncate_cell("hello", 0);

        assert_eq!(result, "...");
    }

    #[rstest]
    #[case(1, "...")]
    #[case(2, "...")]
    #[case(3, "...")]
    #[case(4, "h...")]
    #[case(5, "he...")]
    fn small_max_chars_handles_edge_cases(#[case] max: usize, #[case] expected: &str) {
        let result = truncate_cell("hello world", max);

        assert_eq!(result, expected);
    }

    #[test]
    fn cell_edit_line_with_wide_chars_keeps_tail_cursor_inside_width() {
        let theme = DEFAULT_THEME;
        let text = "甲乙丙丁戊己庚辛壬癸";
        let cursor = text.chars().count();
        let col_width = 8;

        let line = cell_edit_line_with_cursor(text, cursor, col_width, &theme);

        assert!(line_display_width(&line) <= col_width);
        assert_eq!(
            line.spans.last().map(|span| span.content.as_ref()),
            Some(" ")
        );
        // 先頭の全角文字は省略され、末尾側 (カーソル付近) が表示される
        assert!(!line_text(&line).starts_with('甲'));
        assert!(text.ends_with(line_text(&line).trim_end_matches(' ')));
    }

    #[test]
    fn cell_edit_line_with_wide_chars_scrolls_when_cursor_moves_left() {
        let theme = DEFAULT_THEME;
        let text = "甲乙丙丁戊己庚辛壬癸";
        let col_width = 8;
        let tail_cursor = text.chars().count();
        let left_cursor = tail_cursor - 4;

        let tail = cell_edit_line_with_cursor(text, tail_cursor, col_width, &theme);
        let left = cell_edit_line_with_cursor(text, left_cursor, col_width, &theme);

        assert!(line_display_width(&tail) <= col_width);
        assert!(line_display_width(&left) <= col_width);
        assert_ne!(line_text(&tail), line_text(&left));
    }

    #[test]
    fn cell_edit_line_ascii_tail_behavior_is_unchanged() {
        let theme = DEFAULT_THEME;
        let text = "abcdefghijklmnopqrstuvwxyz";
        let cursor = text.chars().count();
        let col_width = 8;

        let line = cell_edit_line_with_cursor(text, cursor, col_width, &theme);
        let rendered = line_text(&line);

        assert!(rendered.ends_with("z "));
        assert!(line_display_width(&line) <= col_width);
    }

    #[test]
    #[ignore = "local-only dev benchmark, not tied to a CI issue"]
    #[allow(clippy::print_stderr, reason = "benchmark result output")]
    fn bench_ideal_widths_cache_speedup() {
        use crate::app::model::shared::viewport::ColumnWidthsCache;
        use crate::primitives::utils::text_utils::calculate_header_min_widths;
        use std::time::Instant;

        let cols = 20;
        let rows = 50;
        let headers: Vec<String> = (0..cols).map(|i| format!("column_{i}")).collect();
        let data: Vec<Vec<String>> = (0..rows)
            .map(|r| {
                (0..cols)
                    .map(|c| format!("value_r{r}_c{c}_padding"))
                    .collect()
            })
            .collect();

        let iterations = 1000;

        // Baseline: compute both widths every iteration (pre-optimization path)
        let start = Instant::now();
        for _ in 0..iterations {
            std::hint::black_box(calculate_ideal_widths(&headers, &data));
            std::hint::black_box(calculate_header_min_widths(&headers));
        }
        let baseline = start.elapsed();

        // Cached: is_valid check + clone (actual cache-hit path)
        let ideal = calculate_ideal_widths(&headers, &data);
        let min = calculate_header_min_widths(&headers);
        let cache = ColumnWidthsCache::new(ideal, min, 1, None);
        let start = Instant::now();
        for _ in 0..iterations {
            let valid = std::hint::black_box(cache.is_valid(1, None));
            if valid {
                std::hint::black_box(cache.clone());
            }
        }
        let cached = start.elapsed();

        eprintln!(
            "Baseline: {:?} ({:.1} µs/iter), Cached (is_valid+clone): {:?} ({:.1} µs/iter), Speedup: {:.0}x",
            baseline,
            baseline.as_micros() as f64 / iterations as f64,
            cached,
            cached.as_micros() as f64 / iterations as f64,
            baseline.as_secs_f64() / cached.as_secs_f64(),
        );
    }
}
