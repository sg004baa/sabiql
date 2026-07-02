use ratatui::Frame;
use ratatui::layout::Constraint;
use ratatui::style::Style;
use ratatui::widgets::{List, ListItem};

use crate::app::model::app_state::AppState;
use crate::app::model::browse::generate_sql::GenerateSqlKind;
use crate::primitives::molecules::render_modal;
use crate::theme::ThemePalette;

pub struct GenerateSqlMenu;

impl GenerateSqlMenu {
    pub fn render(frame: &mut Frame, state: &AppState, theme: &ThemePalette) {
        let (_, inner) = render_modal(
            frame,
            Constraint::Percentage(40),
            Constraint::Percentage(30),
            " Generate SQL ",
            " Enter Select │ Esc/q Close ",
            theme,
        );

        let items = GenerateSqlKind::ALL
            .iter()
            .enumerate()
            .map(|(index, kind)| {
                let style = if index == state.ui.generate_sql_menu.selected() {
                    theme.picker_selected_style()
                } else {
                    Style::default().fg(theme.semantic.text.secondary)
                };
                ListItem::new(format!("  {}", kind.label())).style(style)
            })
            .collect::<Vec<_>>();

        frame.render_widget(List::new(items), inner);
    }
}
