use std::time::Instant;

use crate::cmd::effect::Effect;
use crate::model::app_state::AppState;
use crate::model::browse::generate_sql::GenerateSqlKind;
use crate::model::shared::input_mode::InputMode;
use crate::policy::write::write_update::build_pk_pairs;
use crate::services::AppServices;
use crate::update::action::Action;

pub fn reduce(
    state: &mut AppState,
    action: &Action,
    services: &AppServices,
    now: Instant,
) -> Option<Vec<Effect>> {
    let Action::GenerateSql(kind) = action else {
        return None;
    };

    state.modal.set_mode(InputMode::Normal);

    let row_indices = if state.result_interaction.marked_rows().is_empty() {
        state
            .result_interaction
            .selection()
            .row()
            .into_iter()
            .collect::<Vec<_>>()
    } else {
        state
            .result_interaction
            .marked_rows()
            .iter()
            .copied()
            .collect::<Vec<_>>()
    };
    if row_indices.is_empty() {
        state
            .messages
            .set_error_at("No row selected".to_string(), now);
        return Some(vec![]);
    }

    let Some(result) = state.query.visible_result() else {
        state
            .messages
            .set_error_at("No result available".to_string(), now);
        return Some(vec![]);
    };
    if result.columns.is_empty() {
        state
            .messages
            .set_error_at("No result columns available".to_string(), now);
        return Some(vec![]);
    }

    let columns = result.columns.clone();
    let selected_rows = row_indices
        .iter()
        .map(|row_index| result.rows.get(*row_index).cloned())
        .collect::<Option<Vec<_>>>();
    let Some(selected_rows) = selected_rows else {
        state
            .messages
            .set_error_at("Selected row is unavailable".to_string(), now);
        return Some(vec![]);
    };

    let schema = state.query.pagination.schema.clone();
    let table = state.query.pagination.table.clone();
    if schema.is_empty() || table.is_empty() {
        state
            .messages
            .set_error_at("Unknown table".to_string(), now);
        return Some(vec![]);
    }

    let pk_columns = if *kind == GenerateSqlKind::Insert {
        Vec::new()
    } else {
        let primary_key = state
            .session
            .table_detail()
            .and_then(|table_detail| table_detail.primary_key.clone())
            .filter(|columns| !columns.is_empty());
        let Some(primary_key) = primary_key else {
            state
                .messages
                .set_error_at("This action requires a PRIMARY KEY.".to_string(), now);
            return Some(vec![]);
        };
        primary_key
    };

    let pk_pairs_per_row = if *kind == GenerateSqlKind::Insert {
        Vec::new()
    } else {
        let pairs = selected_rows
            .iter()
            .map(|row| build_pk_pairs(&columns, row, &pk_columns))
            .collect::<Option<Vec<_>>>();
        let Some(pairs) = pairs else {
            state.messages.set_error_at(
                "PRIMARY KEY columns are missing from the result.".to_string(),
                now,
            );
            return Some(vec![]);
        };
        pairs
    };

    let sql = match kind {
        GenerateSqlKind::Select => {
            services
                .sql_dialect
                .build_select_sql(&schema, &table, &columns, &pk_pairs_per_row)
        }
        GenerateSqlKind::Insert => {
            services
                .sql_dialect
                .build_insert_sql(&schema, &table, &columns, &selected_rows)
        }
        GenerateSqlKind::Update => services.sql_dialect.build_row_update_sql(
            &schema,
            &table,
            &columns,
            &selected_rows,
            &pk_columns,
        ),
        GenerateSqlKind::Delete => {
            services
                .sql_dialect
                .build_bulk_delete_sql(&schema, &table, &pk_pairs_per_row)
        }
    };

    state.modal.set_mode(InputMode::SqlModal);
    state.sql_modal.load_query_for_editing(sql);
    state.result_interaction.clear_marked_rows();

    Some(vec![])
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::domain::{Column, ColumnAttributes, QueryResult, QuerySource, Table};
    use crate::model::shared::text_input::TextInputLike;
    use crate::model::sql_editor::modal::{SqlModalStatus, SqlModalTab};

    fn state_with_rows(primary_key: Option<Vec<String>>) -> AppState {
        let now = Instant::now();
        let mut state = AppState::new("test".to_string());
        state.query.pagination.schema = "public".to_string();
        state.query.pagination.table = "users".to_string();
        state.query.set_current_result(Arc::new(QueryResult {
            query: "SELECT id, name FROM public.users".to_string(),
            columns: vec!["id".to_string(), "name".to_string()],
            rows: vec![
                vec!["1".to_string(), "alice".to_string()],
                vec!["2".to_string(), "bob".to_string()],
            ],
            row_count: 2,
            execution_time_ms: 1,
            executed_at: now,
            source: QuerySource::Preview,
            error: None,
            command_tag: None,
        }));
        state.session.set_table_detail_raw(Some(Table {
            schema: "public".to_string(),
            name: "users".to_string(),
            owner: None,
            columns: vec![
                Column {
                    name: "id".to_string(),
                    data_type: "integer".to_string(),
                    default: None,
                    attributes: ColumnAttributes::PRIMARY_KEY,
                    comment: None,
                    extra: None,
                    ordinal_position: 1,
                },
                Column {
                    name: "name".to_string(),
                    data_type: "text".to_string(),
                    default: None,
                    attributes: ColumnAttributes::empty(),
                    comment: None,
                    extra: None,
                    ordinal_position: 2,
                },
            ],
            primary_key,
            foreign_keys: vec![],
            indexes: vec![],
            rls: None,
            triggers: vec![],
            row_count_estimate: None,
            comment: None,
        }));
        state
    }

    mod insert {
        use super::*;

        #[test]
        fn selected_row_opens_editing_sql_modal() {
            let mut state = state_with_rows(Some(vec!["id".to_string()]));
            state.result_interaction.activate_cell(0, 0);

            let effects = reduce(
                &mut state,
                &Action::GenerateSql(GenerateSqlKind::Insert),
                &AppServices::stub(),
                Instant::now(),
            )
            .unwrap();

            assert!(effects.is_empty());
            assert_eq!(state.input_mode(), InputMode::SqlModal);
            assert_eq!(state.sql_modal.status(), &SqlModalStatus::Editing);
            assert_eq!(state.sql_modal.active_tab(), SqlModalTab::Sql);
            assert!(state.sql_modal.editor.content().contains("INSERT"));
        }
    }

    mod select {
        use super::*;

        #[test]
        fn missing_primary_key_sets_error_without_opening_modal() {
            let mut state = state_with_rows(None);
            state.result_interaction.activate_cell(0, 0);

            reduce(
                &mut state,
                &Action::GenerateSql(GenerateSqlKind::Select),
                &AppServices::stub(),
                Instant::now(),
            );

            assert_eq!(state.input_mode(), InputMode::Normal);
            assert_eq!(
                state.messages.last_error.as_deref(),
                Some("This action requires a PRIMARY KEY.")
            );
        }

        #[test]
        fn marked_rows_generate_multi_row_predicate_and_are_consumed() {
            let mut state = state_with_rows(Some(vec!["id".to_string()]));
            state.result_interaction.toggle_marked_row(0);
            state.result_interaction.toggle_marked_row(1);

            reduce(
                &mut state,
                &Action::GenerateSql(GenerateSqlKind::Select),
                &AppServices::stub(),
                Instant::now(),
            );

            let sql = state.sql_modal.editor.content();
            assert!(sql.contains("\"id\" IN ('1', '2')"));
            assert!(state.result_interaction.marked_rows().is_empty());
        }
    }
}
