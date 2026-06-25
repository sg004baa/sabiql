use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Cell, Paragraph, Row, Table};

use crate::primitives::atoms::scroll_indicator::{
    VerticalScrollParams, clamp_scroll_offset, render_vertical_scroll_indicator_bar,
};
use crate::theme::ThemePalette;

pub struct StripedTableConfig<'b> {
    pub headers: &'b [&'b str],
    pub widths: &'b [Constraint],
    pub header_bottom_margin: u16,
    pub header_separator: bool,
    pub total_items: usize,
    pub empty_message: &'b str,
}

pub fn render_striped_table<'a>(
    frame: &mut Frame,
    area: Rect,
    config: &StripedTableConfig<'_>,
    scroll_offset: usize,
    theme: &ThemePalette,
    row_fn: impl Fn(usize) -> Vec<Cell<'a>>,
) {
    if config.total_items == 0 {
        frame.render_widget(Paragraph::new(config.empty_message), area);
        return;
    }

    let mut header_style = Style::default()
        .add_modifier(Modifier::BOLD)
        .fg(theme.semantic.text.primary);
    if !config.header_separator {
        header_style = header_style.add_modifier(Modifier::UNDERLINED);
    }

    let header = Row::new(config.headers.iter().map(|&h| Cell::from(h)))
        .style(header_style)
        .height(1)
        .bottom_margin(config.header_bottom_margin);

    // Header (1) + optional header margin + scroll indicator (1).
    let reserved_rows = 2u16.saturating_add(config.header_bottom_margin);
    let visible_rows = area.height.saturating_sub(reserved_rows) as usize;
    let clamped_scroll_offset =
        clamp_scroll_offset(scroll_offset, visible_rows, config.total_items);

    let rows: Vec<Row> = (clamped_scroll_offset..config.total_items)
        .take(visible_rows)
        .enumerate()
        .map(|(visual_idx, item_idx)| {
            let style = if visual_idx % 2 == 1 {
                Style::default().bg(theme.component.table.striped_row_bg)
            } else {
                Style::default()
            };
            Row::new(row_fn(item_idx)).style(style)
        })
        .collect();

    let table_widget = Table::new(rows, config.widths)
        .header(header)
        .style(Style::default().fg(theme.semantic.text.primary));
    frame.render_widget(table_widget, area);

    if config.header_separator && area.height > 1 {
        let separator = "─".repeat(area.width as usize);
        let separator_area = Rect {
            x: area.x,
            y: area.y + 1,
            width: area.width,
            height: 1,
        };
        frame.render_widget(
            Paragraph::new(separator)
                .style(Style::default().fg(theme.semantic.surface.unfocus_border)),
            separator_area,
        );
    }

    render_vertical_scroll_indicator_bar(
        frame,
        area,
        VerticalScrollParams {
            position: clamped_scroll_offset,
            viewport_size: visible_rows,
            total_items: config.total_items,
            has_horizontal_scrollbar: false,
        },
        theme,
    );
}
