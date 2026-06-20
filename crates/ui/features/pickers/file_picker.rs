use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};

use crate::app::model::app_state::AppState;
use crate::app::model::connection::file_picker::FilteredPath;
use crate::features::pickers::table_picker::filter_visible_width;
use crate::primitives::atoms::text_cursor_spans;
use crate::primitives::molecules::render_modal;
use crate::theme::ThemePalette;

const MIN_INNER: u16 = 6;

pub struct FilePicker;

pub struct FilePickerRenderMetrics {
    pub pane_height: u16,
    pub filter_visible_width: usize,
}

impl FilePicker {
    pub fn render(
        frame: &mut Frame,
        state: &AppState,
        theme: &ThemePalette,
    ) -> FilePickerRenderMetrics {
        let fp = &state.file_picker;
        let filtered = fp.filtered_paths();
        let count = filtered.len();
        let selected_idx = fp.clamped_selected();
        let scroll_offset = fp.picker().scroll_offset();
        let filter_is_empty = fp.filter_text().is_empty();

        let max_height = (frame.area().height * 70 / 100).max(MIN_INNER + 2);
        // border(2) + filter(1) + entries — capped at 70%
        let desired_height = (2 + 1 + (count as u16).max(1)).min(max_height);

        let border_footer = Self::footer_text(fp.is_scanning(), fp.is_truncated(), count);

        let (_, inner) = render_modal(
            frame,
            Constraint::Percentage(70),
            Constraint::Max(desired_height),
            " Select SQLite File ",
            &border_footer,
            theme,
        );

        let [filter_area, list_area] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(inner);
        let raw_width = filter_area.width.saturating_sub(4) as usize;

        let input = fp.picker().filter_input();
        let visible_width = filter_visible_width(raw_width, input.cursor(), input.char_count());
        let filter_line = if input.content().is_empty() {
            Line::from(Span::styled(
                "  type to search",
                Style::default().fg(theme.semantic.text.placeholder),
            ))
        } else {
            let cursor_spans = text_cursor_spans(
                input.content(),
                input.cursor(),
                input.viewport_offset(),
                visible_width,
                theme,
            );
            let mut spans = vec![Span::styled(
                "  > ",
                Style::default().fg(theme.component.modal.title),
            )];
            spans.extend(cursor_spans);
            Line::from(spans)
        };
        frame.render_widget(Paragraph::new(filter_line), filter_area);

        if count == 0 {
            let msg = if fp.is_scanning() {
                "  scanning…"
            } else if filter_is_empty {
                "  type to search for .db / .sqlite files"
            } else {
                "  no matches"
            };
            let empty_line = Line::from(Span::styled(
                msg,
                Style::default().fg(theme.semantic.text.secondary),
            ));
            frame.render_widget(Paragraph::new(empty_line), list_area);
            return FilePickerRenderMetrics {
                pane_height: list_area.height,
                filter_visible_width: visible_width,
            };
        }

        let available_width = list_area.width as usize;
        let items: Vec<ListItem> = filtered
            .iter()
            .enumerate()
            .map(|(i, fpth)| build_list_item(fpth, i, selected_idx, available_width, theme))
            .collect();

        let list = List::new(items)
            .highlight_style(theme.picker_selected_style())
            .highlight_symbol("\u{25b8} ");

        let mut list_state = ListState::default()
            .with_selected(Some(selected_idx))
            .with_offset(scroll_offset);
        frame.render_stateful_widget(list, list_area, &mut list_state);

        FilePickerRenderMetrics {
            pane_height: list_area.height,
            filter_visible_width: visible_width,
        }
    }

    fn footer_text(scanning: bool, truncated: bool, count: usize) -> String {
        if scanning {
            format!(" scanning… {count} found \u{2502} Enter Select ")
        } else if truncated {
            format!(" {count} files (truncated — narrow your filter) \u{2502} Enter Select ")
        } else {
            format!(" {count} files \u{2502} type to filter \u{2502} Enter Select ")
        }
    }
}

fn build_list_item(
    fpth: &FilteredPath<'_>,
    i: usize,
    selected_idx: usize,
    max_width: usize,
    theme: &ThemePalette,
) -> ListItem<'static> {
    let display = &fpth.display;
    let char_len = display.chars().count();
    // Truncate from the left so the file name (the meaningful tail) stays visible.
    let shown: String = if char_len > max_width && max_width > 3 {
        let skip = char_len - (max_width - 1);
        let tail: String = display.chars().skip(skip).collect();
        format!("\u{2026}{tail}")
    } else {
        display.clone()
    };

    if fpth.match_indices.is_empty() {
        let color = if i == selected_idx {
            theme.semantic.text.primary
        } else {
            theme.semantic.text.secondary
        };
        return ListItem::new(Line::from(Span::styled(shown, Style::default().fg(color))));
    }

    // Highlight matched chars. Indices are into the untruncated display; only
    // applies cleanly when not truncated, which is the common case.
    let truncated = shown.chars().count() != char_len;
    let chars: Vec<char> = shown.chars().collect();
    let mut spans = Vec::with_capacity(chars.len());
    for (ci, ch) in chars.iter().enumerate() {
        let is_match = !truncated && fpth.match_indices.contains(&(ci as u32));
        let color = if is_match {
            theme.semantic.text.accent
        } else if i == selected_idx {
            theme.semantic.text.primary
        } else {
            theme.semantic.text.secondary
        };
        let mut style = Style::default().fg(color);
        if is_match {
            style = style.add_modifier(Modifier::BOLD);
        }
        spans.push(Span::styled(ch.to_string(), style));
    }
    ListItem::new(Line::from(spans))
}
