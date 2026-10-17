use std::time::Instant;

use crate::cmd::effect::Effect;
use crate::domain::generated_sql::{GenerateSqlKind, generate_sql};
use crate::model::app_state::AppState;
use crate::model::browse::json_detail::JsonDetailMode;
use crate::model::connection::setup::ConnectionField;
use crate::model::shared::input_mode::InputMode;
use crate::model::sql_editor::modal::SqlModalStatus;
use crate::policy::write::write_guardrails::stable_row_identity_for_preview;
use crate::ports::inbound::{Key, KeyCombo, Modifiers};
use crate::update::action::{
    Action, CursorMove, ExternalEditorTarget, InputTarget, ListMotion, ListTarget, ModalKind,
};

pub(super) fn handle_key(combo: KeyCombo, state: &AppState) -> Option<Action> {
    if combo == KeyCombo::ctrl(Key::Char('e')) {
        match state.input_mode() {
            InputMode::SqlModal if matches!(state.sql_modal.status(), SqlModalStatus::Editing) => {
                return Some(Action::OpenExternalEditor(ExternalEditorTarget::SqlEditor));
            }
            InputMode::JsonEdit => {
                return Some(Action::OpenExternalEditor(
                    ExternalEditorTarget::JsonbEditor,
                ));
            }
            InputMode::JsonDetail if state.json_detail.mode() == JsonDetailMode::Editing => {
                return Some(Action::OpenExternalEditor(
                    ExternalEditorTarget::JsonbEditor,
                ));
            }
            _ => {}
        }
    }
    if state.input_mode() == InputMode::ConnectionSetup
        && state.connection_setup.focused_field() == ConnectionField::SqlitePath
        && combo == KeyCombo::ctrl(Key::Char('p'))
    {
        return Some(Action::OpenFilePicker);
    }
    if state.input_mode() == InputMode::GenerateSqlMenu {
        let key = match combo {
            KeyCombo {
                key: Key::Char('p'),
                modifiers: Modifiers::CTRL,
            } => Key::Up,
            KeyCombo {
                key: Key::Char('n'),
                modifiers: Modifiers::CTRL,
            } => Key::Down,
            _ if combo.modifiers.intersects(Modifiers::CTRL | Modifiers::ALT) => {
                return Some(Action::None);
            }
            _ => combo.key,
        };
        return Some(match key {
            Key::Esc | Key::Char('q') => Action::CloseModal(ModalKind::GenerateSqlMenu),
            Key::Up | Key::Char('k') => Action::ListSelect {
                target: ListTarget::GenerateSqlMenu,
                motion: ListMotion::Previous,
            },
            Key::Down | Key::Char('j') => Action::ListSelect {
                target: ListTarget::GenerateSqlMenu,
                motion: ListMotion::Next,
            },
            Key::Enter => Action::GenerateSql(
                GenerateSqlKind::ALL[state.ui.generate_sql_menu.selected().min(3)],
            ),
            _ => Action::None,
        });
    }
    if state.input_mode() == InputMode::FilePicker {
        return Some(match combo.key {
            Key::Esc => Action::CloseModal(ModalKind::FilePicker),
            Key::Enter => Action::FilePickerConfirmSelection,
            Key::Up | Key::Char('p')
                if combo.key == Key::Up || combo.modifiers.contains(Modifiers::CTRL) =>
            {
                Action::ListSelect {
                    target: ListTarget::FilePicker,
                    motion: ListMotion::Previous,
                }
            }
            Key::Down | Key::Char('n')
                if combo.key == Key::Down || combo.modifiers.contains(Modifiers::CTRL) =>
            {
                Action::ListSelect {
                    target: ListTarget::FilePicker,
                    motion: ListMotion::Next,
                }
            }
            Key::Left | Key::Right | Key::Home | Key::End => Action::TextMoveCursor {
                target: InputTarget::FilePickerFilter,
                direction: match combo.key {
                    Key::Left => CursorMove::Left,
                    Key::Right => CursorMove::Right,
                    Key::Home => CursorMove::Home,
                    _ => CursorMove::End,
                },
            },
            Key::Backspace => Action::TextBackspace {
                target: InputTarget::FilePickerFilter,
            },
            Key::Char(ch) if !combo.modifiers.intersects(Modifiers::CTRL | Modifiers::ALT) => {
                Action::TextInput {
                    target: InputTarget::FilePickerFilter,
                    ch,
                }
            }
            _ => Action::None,
        });
    }
    None
}

pub(super) fn reduce(state: &mut AppState, action: &Action, _now: Instant) -> Option<Vec<Effect>> {
    if let Some(effects) = super::connection::file_picker::reduce(state, action) {
        return Some(effects);
    }
    match action {
        Action::ToggleMarkedRow => {
            if let Some(row) = state.result_interaction.selection().row() {
                state.result_interaction.toggle_marked_row(row);
            }
        }
        Action::ClearMarkedRows => state.result_interaction.clear_marked_rows(),
        Action::OpenModal(ModalKind::GenerateSqlMenu) => {
            state.ui.generate_sql_menu.reset();
            state.modal.set_mode(InputMode::GenerateSqlMenu);
        }
        Action::CloseModal(ModalKind::GenerateSqlMenu) => state.modal.set_mode(InputMode::Normal),
        Action::ListSelect {
            target: ListTarget::GenerateSqlMenu,
            motion,
        } => {
            let current = state.ui.generate_sql_menu.selected();
            let selected = match motion {
                ListMotion::Next => (current + 1).min(3),
                ListMotion::Previous => current.saturating_sub(1),
            };
            state.ui.generate_sql_menu.set_selection(selected);
        }
        Action::GenerateSql(kind) => {
            if let Some(sql) = sql_for_selection(state, *kind) {
                state.sql_modal.load_query_for_editing(sql);
                state.modal.set_mode(InputMode::SqlModal);
                state.result_interaction.clear_marked_rows();
            } else {
                state.modal.set_mode(InputMode::Normal);
                state.messages.set_error("SQL generation requires selected table rows and a complete primary key for SELECT, UPDATE and DELETE".to_string());
            }
        }
        Action::OpenExternalEditor(target) => {
            let content = match target {
                ExternalEditorTarget::SqlEditor
                    if state.input_mode() == InputMode::SqlModal
                        && matches!(state.sql_modal.status(), SqlModalStatus::Editing) =>
                {
                    state.sql_modal.editor().content().to_string()
                }
                ExternalEditorTarget::JsonbEditor
                    if state.json_detail.mode() == JsonDetailMode::Editing =>
                {
                    state.json_detail.editor().content().to_string()
                }
                _ => return Some(vec![]),
            };
            return Some(vec![Effect::OpenExternalEditor {
                target: *target,
                content,
            }]);
        }
        Action::ExternalEditorFinished {
            target: ExternalEditorTarget::SqlEditor,
            content,
        } => {
            state.sql_modal.dismiss_completion();
            state.sql_modal.load_query_for_editing(content.clone());
        }
        Action::ExternalEditorFinished {
            target: ExternalEditorTarget::JsonbEditor,
            content,
        } => {
            state.json_detail.editor_mut().set_content(content.clone());
            state.json_detail.validate_editor_content();
        }
        Action::ExternalEditorFailed(error) => state.messages.set_error(error.to_string()),
        _ => return None,
    }
    Some(vec![])
}

fn sql_for_selection(state: &AppState, kind: GenerateSqlKind) -> Option<String> {
    // Ad-hoc or historic results may not represent the table named by pagination.
    if !state.query.can_edit_visible_result() {
        return None;
    }
    let result = state.query.visible_result()?;
    let indices = if state.result_interaction.marked_rows().is_empty() {
        vec![state.result_interaction.selection().row()?]
    } else {
        state
            .result_interaction
            .marked_rows()
            .iter()
            .copied()
            .collect()
    };
    let rows = indices
        .iter()
        .map(|row| result.values().get(*row).cloned())
        .collect::<Option<Vec<_>>>()?;
    let identities = if kind == GenerateSqlKind::Insert {
        Vec::new()
    } else {
        let detail = state.session.table_detail()?;
        if detail.schema != state.query.pagination.schema()
            || detail.name != state.query.pagination.table()
        {
            return None;
        }
        let identity = stable_row_identity_for_preview(detail, result)?;
        indices
            .iter()
            .map(|row| identity.identity_pairs_for_row(result, *row))
            .collect::<Option<Vec<_>>>()?
    };
    let schema = state.query.pagination.schema();
    let table = state.query.pagination.table();
    if table.is_empty() {
        return None;
    }
    generate_sql(
        state.session.active_database_type_or_default(),
        kind,
        schema,
        table,
        &result.columns,
        &rows,
        &identities,
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::domain::{
        QueryResult, QuerySource, QueryValue, Table, TableKindInfo, TableStorageAttributes,
    };

    #[test]
    fn file_picker_moves_filter_cursor_without_changing_selection() {
        let mut state = AppState::new("test".to_string());
        state.modal.set_mode(InputMode::FilePicker);
        assert!(matches!(
            handle_key(KeyCombo::plain(Key::Left), &state),
            Some(Action::TextMoveCursor {
                target: InputTarget::FilePickerFilter,
                direction: CursorMove::Left,
            })
        ));
    }

    #[test]
    fn generate_sql_menu_supports_control_navigation() {
        let mut state = AppState::new("test".to_string());
        state.modal.set_mode(InputMode::GenerateSqlMenu);
        assert!(matches!(
            handle_key(KeyCombo::ctrl(Key::Char('n')), &state),
            Some(Action::ListSelect {
                target: ListTarget::GenerateSqlMenu,
                motion: ListMotion::Next,
            })
        ));
        assert!(matches!(
            handle_key(KeyCombo::ctrl(Key::Char('q')), &state),
            Some(Action::None)
        ));
    }

    fn preview_state() -> AppState {
        let mut state = AppState::new("test".to_string());
        state.query.pagination.reset_for_table("public", "users");
        state
            .query
            .set_current_result(Arc::new(QueryResult::success_with_values(
                "SELECT id, name FROM public.users".to_string(),
                vec!["id".to_string(), "name".to_string()],
                vec![
                    vec![QueryValue::text("1"), QueryValue::text("alice")],
                    vec![QueryValue::text("2"), QueryValue::text("bob")],
                ],
                1,
                QuerySource::Preview,
            )));
        state.session.set_table_detail_raw(Some(Table {
            schema: "public".to_string(),
            name: "users".to_string(),
            owner: None,
            columns: vec![],
            primary_key: Some(vec!["id".to_string()]),
            foreign_keys: vec![],
            indexes: vec![],
            rls: None,
            triggers: vec![],
            row_count_estimate: None,
            comment: None,
            source_ddl: None,
            storage_attributes: TableStorageAttributes::default(),
            kind_info: TableKindInfo::default(),
        }));
        state.result_interaction.activate_cell(0, 0);
        state
    }

    #[test]
    fn marked_rows_generate_editable_sql_without_executing_it() {
        let mut state = preview_state();
        state.result_interaction.toggle_marked_row(0);
        state.result_interaction.toggle_marked_row(1);
        let effects = reduce(
            &mut state,
            &Action::GenerateSql(GenerateSqlKind::Delete),
            Instant::now(),
        )
        .unwrap();
        assert!(effects.is_empty());
        assert_eq!(state.input_mode(), InputMode::SqlModal);
        assert!(state.sql_modal.editor().content().contains("\"id\" = '1'"));
        assert!(state.sql_modal.editor().content().contains("\"id\" = '2'"));
        assert!(state.result_interaction.marked_rows().is_empty());
        assert!(matches!(state.sql_modal.status(), SqlModalStatus::Editing));
    }

    #[test]
    fn adhoc_results_cannot_generate_sql_for_a_stale_table() {
        let mut state = preview_state();
        state
            .query
            .set_current_result(Arc::new(QueryResult::success_with_values(
                "SELECT name FROM unrelated".to_string(),
                vec!["name".to_string()],
                vec![vec![QueryValue::text("other")]],
                1,
                QuerySource::Adhoc,
            )));
        assert!(sql_for_selection(&state, GenerateSqlKind::Insert).is_none());
    }

    #[test]
    fn external_editor_round_trip_keeps_sql_unexecuted_and_validates_json() {
        let mut state = AppState::new("test".to_string());
        state.modal.set_mode(InputMode::SqlModal);
        state
            .sql_modal
            .load_query_for_editing("SELECT 1".to_string());
        let effects = reduce(
            &mut state,
            &Action::OpenExternalEditor(ExternalEditorTarget::SqlEditor),
            Instant::now(),
        )
        .unwrap();
        assert!(
            matches!(effects.as_slice(), [Effect::OpenExternalEditor { content, .. }] if content == "SELECT 1")
        );
        let effects = reduce(
            &mut state,
            &Action::ExternalEditorFinished {
                target: ExternalEditorTarget::SqlEditor,
                content: "DELETE FROM users".to_string(),
            },
            Instant::now(),
        )
        .unwrap();
        assert!(effects.is_empty());
        assert_eq!(state.sql_modal.editor().content(), "DELETE FROM users");
        assert!(matches!(state.sql_modal.status(), SqlModalStatus::Editing));
        state.json_detail.enter_edit();
        reduce(
            &mut state,
            &Action::ExternalEditorFinished {
                target: ExternalEditorTarget::JsonbEditor,
                content: "{invalid".to_string(),
            },
            Instant::now(),
        );
        assert!(state.json_detail.validation_error().is_some());
    }
}
