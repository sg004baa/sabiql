use crate::cmd::effect::Effect;
use crate::domain::connection::DatabaseType;
use crate::model::app_state::AppState;
use crate::model::connection::file_picker::WalkOptions;
use crate::model::connection::setup::ConnectionField;
use crate::model::shared::input_mode::InputMode;
use crate::update::action::{Action, InputTarget, ListMotion, ListTarget, ModalKind};

/// Reduce file-picker actions. Returns `None` for unrelated actions so the
/// parent reducer can fall through.
pub fn reduce(state: &mut AppState, action: &Action) -> Option<Vec<Effect>> {
    match action {
        // Open only for SQLite while the File field is focused. Scanning starts
        // eagerly here so the candidate list is already populating when the
        // picker appears.
        Action::OpenFilePicker => {
            if state.connection_setup.database_type != DatabaseType::SQLite
                || state.connection_setup.focused_field != ConnectionField::Database
            {
                return Some(vec![]);
            }
            state.file_picker.open();
            state.modal.push_mode(InputMode::FilePicker);
            let field = state.connection_setup.database.content().to_string();
            let generation = state.file_picker.generation();
            Some(vec![Effect::StartFilePickerWalk {
                field,
                options: WalkOptions::default(),
                generation,
            }])
        }

        Action::TextInput {
            target: InputTarget::FilePickerFilter,
            ch,
        } => {
            state.file_picker.insert_filter_char(*ch);
            Some(vec![])
        }

        Action::Paste(text) if state.modal.active_mode() == InputMode::FilePicker => {
            state.file_picker.insert_filter_str(text);
            Some(vec![])
        }

        Action::TextBackspace {
            target: InputTarget::FilePickerFilter,
        } => {
            state.file_picker.backspace_filter();
            Some(vec![])
        }

        Action::TextMoveCursor {
            target: InputTarget::FilePickerFilter,
            direction,
        } => {
            state.file_picker.move_filter_cursor(*direction);
            Some(vec![])
        }

        Action::ListSelect {
            target: ListTarget::FilePicker,
            motion,
        } => {
            match motion {
                ListMotion::Next => state.file_picker.select_next(),
                ListMotion::Previous => state.file_picker.select_previous(),
            }
            Some(vec![])
        }

        Action::FilePickerChunk { generation, paths } => {
            if *generation == state.file_picker.generation() {
                state.file_picker.append_paths(paths);
            }
            Some(vec![])
        }

        Action::FilePickerWalkDone {
            generation,
            truncated,
        } => {
            if *generation == state.file_picker.generation() {
                state.file_picker.finish_walk(*truncated);
            }
            Some(vec![])
        }

        Action::FilePickerConfirmSelection => {
            if let Some(path) = state.file_picker.selected_path() {
                set_database_field(state, &path.to_string_lossy());
            }
            state.file_picker.invalidate();
            state.modal.pop_mode();
            Some(vec![])
        }

        Action::CloseModal(ModalKind::FilePicker) => {
            state.file_picker.invalidate();
            state.modal.pop_mode();
            Some(vec![])
        }

        _ => None,
    }
}

/// Replace the `File:` field contents with `value`, cursor at the end.
fn set_database_field(state: &mut AppState, value: &str) {
    use crate::model::shared::text_input::TextInputState;
    let len = value.chars().count();
    state.connection_setup.database = TextInputState::new(value, len);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::connection::setup::ConnectionField;
    use crate::model::shared::text_input::TextInputState;
    use std::path::PathBuf;

    fn sqlite_setup_state() -> AppState {
        let mut state = AppState::new("test".to_string());
        state.connection_setup.database_type = DatabaseType::SQLite;
        state.connection_setup.focused_field = ConnectionField::Database;
        state.modal.set_mode(InputMode::ConnectionSetup);
        state
    }

    fn dispatch(state: &mut AppState, action: &Action) -> Option<Vec<Effect>> {
        reduce(state, action)
    }

    #[test]
    fn open_for_sqlite_file_field_starts_walk() {
        let mut state = sqlite_setup_state();

        let effects = dispatch(&mut state, &Action::OpenFilePicker).unwrap();

        assert_eq!(state.modal.active_mode(), InputMode::FilePicker);
        assert_eq!(state.modal.return_destination(), InputMode::ConnectionSetup);
        assert!(state.file_picker.is_scanning());
        assert_eq!(effects.len(), 1);
        assert!(matches!(
            effects[0],
            Effect::StartFilePickerWalk { generation, .. }
                if generation == state.file_picker.generation()
        ));
    }

    #[test]
    fn open_ignored_for_non_sqlite() {
        let mut state = sqlite_setup_state();
        state.connection_setup.database_type = DatabaseType::PostgreSQL;

        dispatch(&mut state, &Action::OpenFilePicker);

        assert_eq!(state.modal.active_mode(), InputMode::ConnectionSetup);
    }

    #[test]
    fn open_ignored_when_not_on_file_field() {
        let mut state = sqlite_setup_state();
        state.connection_setup.focused_field = ConnectionField::Name;

        dispatch(&mut state, &Action::OpenFilePicker);

        assert_eq!(state.modal.active_mode(), InputMode::ConnectionSetup);
    }

    #[test]
    fn filter_keystroke_does_not_emit_walk() {
        let mut state = sqlite_setup_state();
        dispatch(&mut state, &Action::OpenFilePicker);

        let effects = dispatch(
            &mut state,
            &Action::TextInput {
                target: InputTarget::FilePickerFilter,
                ch: 'a',
            },
        )
        .unwrap();

        assert!(
            effects.is_empty(),
            "filtering reuses the eager walk's cache"
        );
    }

    #[test]
    fn chunk_with_current_generation_is_appended() {
        let mut state = sqlite_setup_state();
        dispatch(&mut state, &Action::OpenFilePicker);
        let g = state.file_picker.generation();

        dispatch(
            &mut state,
            &Action::FilePickerChunk {
                generation: g,
                paths: vec![PathBuf::from("/a.db"), PathBuf::from("/b.db")],
            },
        );

        assert_eq!(state.file_picker.path_count(), 2);
    }

    #[test]
    fn chunk_with_stale_generation_is_discarded() {
        let mut state = sqlite_setup_state();
        dispatch(&mut state, &Action::OpenFilePicker);
        let g = state.file_picker.generation();

        dispatch(
            &mut state,
            &Action::FilePickerChunk {
                generation: g.wrapping_sub(1),
                paths: vec![PathBuf::from("/stale.db")],
            },
        );

        assert_eq!(state.file_picker.path_count(), 0);
    }

    #[test]
    fn walk_done_sets_truncated_for_current_generation() {
        let mut state = sqlite_setup_state();
        dispatch(&mut state, &Action::OpenFilePicker);
        let g = state.file_picker.generation();

        dispatch(
            &mut state,
            &Action::FilePickerWalkDone {
                generation: g,
                truncated: true,
            },
        );

        assert!(state.file_picker.is_truncated());
        assert!(!state.file_picker.is_scanning());
    }

    #[test]
    fn confirm_writes_selected_path_to_database_field_and_returns() {
        let mut state = sqlite_setup_state();
        dispatch(&mut state, &Action::OpenFilePicker);
        let g = state.file_picker.generation();
        dispatch(
            &mut state,
            &Action::FilePickerChunk {
                generation: g,
                paths: vec![PathBuf::from("/home/me/app.db")],
            },
        );
        state.file_picker.picker_mut().pane_height = 10;

        dispatch(&mut state, &Action::FilePickerConfirmSelection);

        assert_eq!(state.connection_setup.database.content(), "/home/me/app.db");
        assert_eq!(state.modal.active_mode(), InputMode::ConnectionSetup);
    }

    #[test]
    fn confirm_with_no_results_just_returns() {
        let mut state = sqlite_setup_state();
        state.connection_setup.database = TextInputState::new("orig", 4);
        dispatch(&mut state, &Action::OpenFilePicker);

        dispatch(&mut state, &Action::FilePickerConfirmSelection);

        assert_eq!(state.connection_setup.database.content(), "orig");
        assert_eq!(state.modal.active_mode(), InputMode::ConnectionSetup);
    }

    #[test]
    fn close_returns_to_setup_without_changing_field() {
        let mut state = sqlite_setup_state();
        state.connection_setup.database = TextInputState::new("orig", 4);
        dispatch(&mut state, &Action::OpenFilePicker);
        let g = state.file_picker.generation();

        dispatch(&mut state, &Action::CloseModal(ModalKind::FilePicker));

        assert_eq!(state.connection_setup.database.content(), "orig");
        assert_eq!(state.modal.active_mode(), InputMode::ConnectionSetup);
        // generation bumped so any in-flight chunks are now stale
        assert_ne!(state.file_picker.generation(), g);
    }

    #[test]
    fn navigation_moves_selection() {
        let mut state = sqlite_setup_state();
        dispatch(&mut state, &Action::OpenFilePicker);
        let g = state.file_picker.generation();
        dispatch(
            &mut state,
            &Action::FilePickerChunk {
                generation: g,
                paths: vec![
                    PathBuf::from("/a.db"),
                    PathBuf::from("/b.db"),
                    PathBuf::from("/c.db"),
                ],
            },
        );
        state.file_picker.picker_mut().pane_height = 10;

        dispatch(
            &mut state,
            &Action::ListSelect {
                target: ListTarget::FilePicker,
                motion: ListMotion::Next,
            },
        );

        assert_eq!(
            state.file_picker.selected_path(),
            Some(PathBuf::from("/b.db"))
        );
    }
}
