use crate::app::ports::outbound::DbOperationError;
use crate::domain::{
    Column, ColumnAttributes, FkAction, ForeignKey, Index, IndexAttributes, IndexType, Schema,
    TableSignature, TableSummary, Trigger, TriggerEvent, TriggerTiming,
};

use super::super::super::SqliteAdapter;

pub(in crate::adapters::sqlite) type TableDetailCombined =
    (Vec<Column>, Vec<Index>, Vec<ForeignKey>, Vec<Trigger>);

fn non_empty_json(raw: &str) -> Option<&str> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "null" || trimmed == "NULL" {
        None
    } else {
        Some(trimmed)
    }
}

fn parse_fk_action(rule: &str) -> FkAction {
    match rule {
        "RESTRICT" => FkAction::Restrict,
        "CASCADE" => FkAction::Cascade,
        "SET NULL" => FkAction::SetNull,
        "SET DEFAULT" => FkAction::SetDefault,
        _ => FkAction::NoAction,
    }
}

/// Derive trigger timing and events from the trigger's `CREATE TRIGGER` SQL.
///
/// SQLite stores triggers only as raw SQL in `sqlite_master`, so the header
/// (everything before the first `BEGIN`) is scanned for the timing and event
/// keywords. The body is excluded so statements inside the trigger don't leak
/// into the event list.
fn parse_trigger_header(sql: &str) -> (TriggerTiming, Vec<TriggerEvent>) {
    let upper = sql.to_uppercase();
    let header = upper.find("BEGIN").map_or(upper.as_str(), |i| &upper[..i]);

    let timing = if header.contains("BEFORE") {
        TriggerTiming::Before
    } else {
        TriggerTiming::After
    };

    let mut events = Vec::new();
    if header.contains("INSERT") {
        events.push(TriggerEvent::Insert);
    }
    if header.contains("UPDATE") {
        events.push(TriggerEvent::Update);
    }
    if header.contains("DELETE") {
        events.push(TriggerEvent::Delete);
    }

    (timing, events)
}

impl SqliteAdapter {
    pub(in crate::adapters::sqlite) fn parse_schemas() -> Vec<Schema> {
        // SQLite exposes exactly one schema for the opened file.
        vec![Schema::new(Self::MAIN_SCHEMA.to_string())]
    }

    pub(in crate::adapters::sqlite) fn parse_tables(
        json: &str,
    ) -> Result<Vec<TableSummary>, DbOperationError> {
        let Some(trimmed) = non_empty_json(json) else {
            return Ok(Vec::new());
        };

        #[derive(serde::Deserialize)]
        struct RawTable {
            schema: String,
            name: String,
            row_count_estimate: Option<i64>,
            has_rls: bool,
        }

        let raw: Vec<RawTable> = serde_json::from_str(trimmed)
            .map_err(|e| DbOperationError::InvalidJson(std::sync::Arc::new(e)))?;

        Ok(raw
            .into_iter()
            .map(|t| TableSummary::new(t.schema, t.name, t.row_count_estimate, t.has_rls))
            .collect())
    }

    pub(in crate::adapters::sqlite) fn parse_table_signatures(
        json: &str,
    ) -> Result<Vec<TableSignature>, DbOperationError> {
        let Some(trimmed) = non_empty_json(json) else {
            return Ok(Vec::new());
        };

        #[derive(serde::Deserialize)]
        struct RawTableSignature {
            schema: String,
            name: String,
            signature: String,
        }

        let raw: Vec<RawTableSignature> = serde_json::from_str(trimmed)
            .map_err(|e| DbOperationError::InvalidJson(std::sync::Arc::new(e)))?;

        Ok(raw
            .into_iter()
            .map(|t| TableSignature {
                schema: t.schema,
                name: t.name,
                signature: t.signature,
            })
            .collect())
    }

    pub(in crate::adapters::sqlite) fn parse_columns(
        json: &str,
    ) -> Result<Vec<Column>, DbOperationError> {
        let Some(trimmed) = non_empty_json(json) else {
            return Ok(Vec::new());
        };

        #[derive(serde::Deserialize)]
        struct RawColumn {
            name: String,
            data_type: String,
            nullable: bool,
            default: Option<String>,
            is_primary_key: bool,
            is_unique: bool,
            comment: Option<String>,
            ordinal_position: i32,
        }

        let raw: Vec<RawColumn> = serde_json::from_str(trimmed)
            .map_err(|e| DbOperationError::InvalidJson(std::sync::Arc::new(e)))?;

        Ok(raw
            .into_iter()
            .map(|c| Column {
                name: c.name,
                data_type: c.data_type,
                default: c.default,
                attributes: ColumnAttributes::from_parts(c.nullable, c.is_primary_key, c.is_unique),
                comment: c.comment,
                ordinal_position: c.ordinal_position,
            })
            .collect())
    }

    pub(in crate::adapters::sqlite) fn parse_indexes(
        json: &str,
    ) -> Result<Vec<Index>, DbOperationError> {
        let Some(trimmed) = non_empty_json(json) else {
            return Ok(Vec::new());
        };

        #[derive(serde::Deserialize)]
        struct RawIndex {
            name: String,
            columns: Vec<String>,
            is_unique: bool,
            is_primary: bool,
            index_type: String,
            definition: Option<String>,
        }

        let raw: Vec<RawIndex> = serde_json::from_str(trimmed)
            .map_err(|e| DbOperationError::InvalidJson(std::sync::Arc::new(e)))?;

        Ok(raw
            .into_iter()
            .map(|i| Index {
                name: i.name,
                columns: i.columns,
                attributes: IndexAttributes::from_parts(i.is_unique, i.is_primary),
                index_type: match i.index_type.as_str() {
                    "BTREE" => IndexType::BTree,
                    other => IndexType::Other(other.to_string()),
                },
                definition: i.definition,
            })
            .collect())
    }

    pub(in crate::adapters::sqlite) fn parse_foreign_keys(
        json: &str,
    ) -> Result<Vec<ForeignKey>, DbOperationError> {
        let Some(trimmed) = non_empty_json(json) else {
            return Ok(Vec::new());
        };

        #[derive(serde::Deserialize)]
        struct RawForeignKey {
            name: String,
            from_schema: String,
            from_table: String,
            from_columns: Vec<String>,
            to_schema: String,
            to_table: String,
            to_columns: Vec<String>,
            on_delete: String,
            on_update: String,
        }

        let raw: Vec<RawForeignKey> = serde_json::from_str(trimmed)
            .map_err(|e| DbOperationError::InvalidJson(std::sync::Arc::new(e)))?;

        Ok(raw
            .into_iter()
            .map(|fk| ForeignKey {
                name: fk.name,
                from_schema: fk.from_schema,
                from_table: fk.from_table,
                from_columns: fk.from_columns,
                to_schema: fk.to_schema,
                to_table: fk.to_table,
                to_columns: fk.to_columns,
                on_delete: parse_fk_action(&fk.on_delete),
                on_update: parse_fk_action(&fk.on_update),
            })
            .collect())
    }

    pub(in crate::adapters::sqlite) fn parse_triggers(
        json: &str,
    ) -> Result<Vec<Trigger>, DbOperationError> {
        let Some(trimmed) = non_empty_json(json) else {
            return Ok(Vec::new());
        };

        #[derive(serde::Deserialize)]
        struct RawTrigger {
            name: String,
            sql: Option<String>,
        }

        let raw: Vec<RawTrigger> = serde_json::from_str(trimmed)
            .map_err(|e| DbOperationError::InvalidJson(std::sync::Arc::new(e)))?;

        Ok(raw
            .into_iter()
            .map(|t| {
                let sql = t.sql.unwrap_or_default();
                let (timing, events) = parse_trigger_header(&sql);
                Trigger {
                    name: t.name,
                    timing,
                    events,
                    function_name: sql,
                    security_definer: false,
                }
            })
            .collect())
    }

    pub(in crate::adapters::sqlite) fn parse_table_detail_combined(
        json: &str,
    ) -> Result<TableDetailCombined, DbOperationError> {
        let Some(trimmed) = non_empty_json(json) else {
            return Err(DbOperationError::EmptyResponse(
                "table_detail_combined: empty response".to_string(),
            ));
        };

        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct CombinedDetail {
            columns: serde_json::Value,
            indexes: serde_json::Value,
            foreign_keys: serde_json::Value,
            triggers: serde_json::Value,
        }

        let combined: CombinedDetail = serde_json::from_str(trimmed)
            .map_err(|e| DbOperationError::InvalidJson(std::sync::Arc::new(e)))?;

        let columns = Self::parse_columns(&combined.columns.to_string())?;
        let indexes = Self::parse_indexes(&combined.indexes.to_string())?;
        let foreign_keys = Self::parse_foreign_keys(&combined.foreign_keys.to_string())?;
        let triggers = Self::parse_triggers(&combined.triggers.to_string())?;

        Ok((columns, indexes, foreign_keys, triggers))
    }

    pub(in crate::adapters::sqlite) fn parse_table_columns_and_fks(
        json: &str,
    ) -> Result<(Vec<Column>, Vec<ForeignKey>), DbOperationError> {
        let Some(trimmed) = non_empty_json(json) else {
            return Err(DbOperationError::EmptyResponse(
                "table_columns_and_fks: empty response".to_string(),
            ));
        };

        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct LightDetail {
            columns: serde_json::Value,
            foreign_keys: serde_json::Value,
        }

        let light: LightDetail = serde_json::from_str(trimmed)
            .map_err(|e| DbOperationError::InvalidJson(std::sync::Arc::new(e)))?;

        let columns = Self::parse_columns(&light.columns.to_string())?;
        let foreign_keys = Self::parse_foreign_keys(&light.foreign_keys.to_string())?;

        Ok((columns, foreign_keys))
    }
}

#[cfg(test)]
mod tests {
    use crate::adapters::sqlite::SqliteAdapter;
    use crate::app::ports::outbound::DbOperationError;

    mod schema_parsing {
        use super::*;

        #[test]
        fn always_returns_single_main_schema() {
            let schemas = SqliteAdapter::parse_schemas();
            assert_eq!(schemas.len(), 1);
            assert_eq!(schemas[0].name, "main");
        }
    }

    mod table_parsing {
        use super::*;
        use rstest::rstest;

        #[rstest]
        #[case("")]
        #[case("null")]
        #[case("NULL")]
        #[case("   ")]
        fn empty_or_null_input_returns_empty_vec(#[case] input: &str) {
            let result = SqliteAdapter::parse_tables(input).unwrap();
            assert!(result.is_empty());
        }

        #[test]
        fn valid_single_table_parses_all_fields() {
            let json = r#"[{
                "schema": "main",
                "name": "users",
                "row_count_estimate": null,
                "has_rls": false
            }]"#;

            let result = SqliteAdapter::parse_tables(json).unwrap();

            assert_eq!(result.len(), 1);
            assert_eq!(result[0].schema, "main");
            assert_eq!(result[0].name, "users");
        }

        #[test]
        fn malformed_json_returns_invalid_json_error() {
            let result = SqliteAdapter::parse_tables("{not valid}");
            assert!(matches!(result, Err(DbOperationError::InvalidJson(_))));
        }
    }

    mod column_parsing {
        use super::*;

        #[test]
        fn valid_column_parses_all_fields() {
            let json = r#"[{
                "name": "id",
                "data_type": "INTEGER",
                "nullable": false,
                "default": null,
                "is_primary_key": true,
                "is_unique": false,
                "comment": null,
                "ordinal_position": 1
            }]"#;

            let result = SqliteAdapter::parse_columns(json).unwrap();

            assert_eq!(result.len(), 1);
            assert_eq!(result[0].name, "id");
            assert_eq!(result[0].data_type, "INTEGER");
            assert!(!result[0].is_nullable());
            assert!(result[0].is_primary_key());
        }

        #[test]
        fn typeless_column_parses_with_empty_data_type() {
            let json = r#"[{
                "name": "v",
                "data_type": "",
                "nullable": true,
                "default": null,
                "is_primary_key": false,
                "is_unique": false,
                "comment": null,
                "ordinal_position": 1
            }]"#;

            let result = SqliteAdapter::parse_columns(json).unwrap();

            assert_eq!(result[0].data_type, "");
        }

        #[test]
        fn empty_returns_empty_vec() {
            assert!(SqliteAdapter::parse_columns("").unwrap().is_empty());
            assert!(SqliteAdapter::parse_columns("null").unwrap().is_empty());
        }
    }

    mod foreign_key_parsing {
        use super::*;
        use crate::domain::FkAction;

        #[test]
        fn valid_fk_parses_sqlite_action_strings() {
            let json = r#"[{
                "name": "fk_0",
                "from_schema": "main",
                "from_table": "orders",
                "from_columns": ["user_id"],
                "to_schema": "main",
                "to_table": "users",
                "to_columns": ["id"],
                "on_delete": "CASCADE",
                "on_update": "NO ACTION"
            }]"#;

            let result = SqliteAdapter::parse_foreign_keys(json).unwrap();
            let fk = &result[0];

            assert_eq!(fk.on_delete, FkAction::Cascade);
            assert_eq!(fk.on_update, FkAction::NoAction);
        }

        #[test]
        fn empty_returns_empty_vec() {
            assert!(SqliteAdapter::parse_foreign_keys("").unwrap().is_empty());
        }
    }

    mod index_parsing {
        use super::*;
        use crate::domain::IndexType;

        #[test]
        fn btree_index_maps_correctly() {
            let json = r#"[{
                "name": "idx_users_email",
                "columns": ["email"],
                "is_unique": true,
                "is_primary": false,
                "index_type": "BTREE",
                "definition": "CREATE UNIQUE INDEX idx_users_email ON users(email)"
            }]"#;

            let result = SqliteAdapter::parse_indexes(json).unwrap();

            assert_eq!(result[0].index_type, IndexType::BTree);
            assert!(result[0].is_unique());
            assert!(!result[0].is_primary());
        }

        #[test]
        fn pk_origin_index_is_primary() {
            let json = r#"[{
                "name": "sqlite_autoindex_users_1",
                "columns": ["id"],
                "is_unique": true,
                "is_primary": true,
                "index_type": "BTREE",
                "definition": null
            }]"#;

            let result = SqliteAdapter::parse_indexes(json).unwrap();

            assert!(result[0].is_primary());
        }
    }

    mod trigger_parsing {
        use super::*;
        use crate::domain::{TriggerEvent, TriggerTiming};

        #[test]
        fn before_insert_trigger_parses_timing_and_event() {
            let json = r#"[{
                "name": "audit_insert",
                "sql": "CREATE TRIGGER audit_insert BEFORE INSERT ON users BEGIN SELECT 1; END"
            }]"#;

            let result = SqliteAdapter::parse_triggers(json).unwrap();
            let trigger = &result[0];

            assert_eq!(trigger.name, "audit_insert");
            assert_eq!(trigger.timing, TriggerTiming::Before);
            assert_eq!(trigger.events, vec![TriggerEvent::Insert]);
            assert!(!trigger.security_definer);
        }

        #[test]
        fn after_delete_trigger_parses_event() {
            let json = r#"[{
                "name": "cleanup",
                "sql": "CREATE TRIGGER cleanup AFTER DELETE ON users BEGIN SELECT 1; END"
            }]"#;

            let result = SqliteAdapter::parse_triggers(json).unwrap();

            assert_eq!(result[0].timing, TriggerTiming::After);
            assert_eq!(result[0].events, vec![TriggerEvent::Delete]);
        }

        #[test]
        fn body_statements_do_not_leak_into_events() {
            // The body contains an UPDATE, but the trigger fires on INSERT only.
            let json = r#"[{
                "name": "t",
                "sql": "CREATE TRIGGER t AFTER INSERT ON users BEGIN UPDATE counts SET n = n + 1; END"
            }]"#;

            let result = SqliteAdapter::parse_triggers(json).unwrap();

            assert_eq!(result[0].events, vec![TriggerEvent::Insert]);
        }

        #[test]
        fn empty_returns_empty_vec() {
            assert!(SqliteAdapter::parse_triggers("").unwrap().is_empty());
        }
    }

    mod table_detail_combined_parsing {
        use super::*;

        #[test]
        fn valid_combined_json_parses_all_categories() {
            let json = r#"{
                "columns": [{"name":"id","data_type":"INTEGER","nullable":false,"default":null,"is_primary_key":true,"is_unique":false,"comment":null,"ordinal_position":1}],
                "indexes": null,
                "foreign_keys": null,
                "triggers": null
            }"#;

            let (columns, indexes, fks, triggers) =
                SqliteAdapter::parse_table_detail_combined(json).unwrap();

            assert_eq!(columns.len(), 1);
            assert_eq!(columns[0].name, "id");
            assert!(indexes.is_empty());
            assert!(fks.is_empty());
            assert!(triggers.is_empty());
        }

        #[test]
        fn empty_input_returns_error() {
            let result = SqliteAdapter::parse_table_detail_combined("");
            assert!(matches!(result, Err(DbOperationError::EmptyResponse(_))));
        }
    }

    mod table_columns_and_fks_parsing {
        use super::*;

        #[test]
        fn valid_light_json_parses_columns_and_fks() {
            let json = r#"{"columns": [{"name":"id","data_type":"INTEGER","nullable":false,"default":null,"is_primary_key":true,"is_unique":false,"comment":null,"ordinal_position":1}], "foreign_keys": null}"#;

            let (columns, fks) = SqliteAdapter::parse_table_columns_and_fks(json).unwrap();

            assert_eq!(columns.len(), 1);
            assert!(fks.is_empty());
        }

        #[test]
        fn empty_input_returns_error() {
            let result = SqliteAdapter::parse_table_columns_and_fks("");
            assert!(matches!(result, Err(DbOperationError::EmptyResponse(_))));
        }
    }
}
