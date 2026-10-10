mod sqlite_diagnostics_binding {
    use crate::app::model::app_state::AppState;
    use crate::app::model::shared::focused_pane::FocusedPane;
    use crate::app::ports::inbound::InputEvent;
    use crate::app::update::action::{
        Action, ModalKind, ScrollAmount, ScrollDirection, ScrollTarget,
    };
    use crate::app::update::input::handle_event;
    use crate::domain::{ConnectionId, DatabaseType};
    use crate::event::key_translator::translate;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn sqlite_connected_state() -> AppState {
        let mut state = AppState::new("test".to_string());
        state.session.activate_connection_with_dsn(
            &ConnectionId::new(),
            "sqlite",
            DatabaseType::SQLite,
            "sqlite:///tmp/app.db",
        );
        state
    }

    #[test]
    fn translated_ctrl_shift_uppercase_d_opens_diagnostics() {
        let state = sqlite_connected_state();
        let combo = translate(KeyEvent::new(
            KeyCode::Char('D'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ));

        let result = handle_event(InputEvent::Key(combo), &state);

        assert!(matches!(
            result,
            Action::OpenModal(ModalKind::SqliteDiagnostics)
        ));
    }

    #[test]
    fn translated_ctrl_d_still_half_page_scrolls_on_result() {
        let mut state = sqlite_connected_state();
        state.ui.set_focused_pane(FocusedPane::Result);
        let combo = translate(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));

        let result = handle_event(InputEvent::Key(combo), &state);

        assert!(matches!(
            result,
            Action::Scroll {
                target: ScrollTarget::Result,
                direction: ScrollDirection::Down,
                amount: ScrollAmount::HalfPage
            }
        ));
    }
}
