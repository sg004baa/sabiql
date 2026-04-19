use crate::app::ports::DbOperationError;
use crate::domain::{
    Column, FkAction, ForeignKey, Index, IndexType, Schema, TableSignature, TableSummary, Trigger,
    TriggerEvent, TriggerTiming,
};

use super::super::super::MySqlAdapter;

pub(in crate::infra::adapters::mysql) type TableDetailCombined = (
    Vec<Column>,
    Vec<Index>,
    Vec<ForeignKey>,
    Vec<Trigger>,
    TableInfo,
);

fn non_empty_json(raw: &str) -> Option<&str> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "null" || trimmed == "NULL" {
        None
    } else {
        Some(trimmed)
    }
}

pub(in crate::infra::adapters::mysql) struct TableInfo {
    pub owner: Option<String>,
    pub comment: Option<String>,
    pub row_count_estimate: Option<i64>,
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

impl MySqlAdapter {
    pub(in crate::infra::adapters::mysql) fn parse_table_info(
        json: &str,
    ) -> Result<TableInfo, DbOperationError> {
        let Some(trimmed) = non_empty_json(json) else {
            return Ok(TableInfo {
                owner: None,
                comment: None,
                row_count_estimate: None,
            });
        };

        #[derive(serde::Deserialize)]
        struct RawTableInfo {
            owner: Option<String>,
            comment: Option<String>,
            row_count_estimate: Option<i64>,
        }

        let raw: RawTableInfo = serde_json::from_str(trimmed)
            .map_err(|e| DbOperationError::InvalidJson(e.to_string()))?;

        Ok(TableInfo {
            owner: raw.owner,
            comment: raw.comment,
            row_count_estimate: raw.row_count_estimate,
        })
    }

    pub(in crate::infra::adapters::mysql) fn parse_tables(
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
            .map_err(|e| DbOperationError::InvalidJson(e.to_string()))?;

        Ok(raw
            .into_iter()
            .map(|t| TableSummary::new(t.schema, t.name, t.row_count_estimate, t.has_rls))
            .collect())
    }

    pub(in crate::infra::adapters::mysql) fn parse_table_signatures(
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
            .map_err(|e| DbOperationError::InvalidJson(e.to_string()))?;

        Ok(raw
            .into_iter()
            .map(|t| TableSignature {
                schema: t.schema,
                name: t.name,
                signature: t.signature,
            })
            .collect())
    }

    pub(in crate::infra::adapters::mysql) fn parse_schemas(
        json: &str,
    ) -> Result<Vec<Schema>, DbOperationError> {
        let Some(trimmed) = non_empty_json(json) else {
            return Ok(Vec::new());
        };

        #[derive(serde::Deserialize)]
        struct RawSchema {
            name: String,
        }

        let raw: Vec<RawSchema> = serde_json::from_str(trimmed)
            .map_err(|e| DbOperationError::InvalidJson(e.to_string()))?;

        Ok(raw.into_iter().map(|s| Schema::new(s.name)).collect())
    }

    pub(in crate::infra::adapters::mysql) fn parse_columns(
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
            .map_err(|e| DbOperationError::InvalidJson(e.to_string()))?;

        Ok(raw
            .into_iter()
            .map(|c| Column {
                name: c.name,
                data_type: c.data_type,
                nullable: c.nullable,
                default: c.default,
                is_primary_key: c.is_primary_key,
                is_unique: c.is_unique,
                comment: c.comment,
                ordinal_position: c.ordinal_position,
            })
            .collect())
    }

    pub(in crate::infra::adapters::mysql) fn parse_indexes(
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
            .map_err(|e| DbOperationError::InvalidJson(e.to_string()))?;

        Ok(raw
            .into_iter()
            .map(|i| Index {
                name: i.name,
                columns: i.columns,
                is_unique: i.is_unique,
                is_primary: i.is_primary,
                index_type: match i.index_type.as_str() {
                    "BTREE" => IndexType::BTree,
                    "HASH" => IndexType::Hash,
                    "FULLTEXT" => IndexType::Other("FULLTEXT".to_string()),
                    "SPATIAL" => IndexType::Other("SPATIAL".to_string()),
                    other => IndexType::Other(other.to_string()),
                },
                definition: i.definition,
            })
            .collect())
    }

    pub(in crate::infra::adapters::mysql) fn parse_foreign_keys(
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
            .map_err(|e| DbOperationError::InvalidJson(e.to_string()))?;

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

    pub(in crate::infra::adapters::mysql) fn parse_triggers(
        json: &str,
    ) -> Result<Vec<Trigger>, DbOperationError> {
        let Some(trimmed) = non_empty_json(json) else {
            return Ok(Vec::new());
        };

        #[derive(serde::Deserialize)]
        struct RawTrigger {
            name: String,
            timing: String,
            events: Vec<String>,
            function_name: String,
            security_definer: bool,
        }

        let raw: Vec<RawTrigger> = serde_json::from_str(trimmed)
            .map_err(|e| DbOperationError::InvalidJson(e.to_string()))?;

        Ok(raw
            .into_iter()
            .map(|t| Trigger {
                name: t.name,
                timing: match t.timing.as_str() {
                    "BEFORE" => TriggerTiming::Before,
                    _ => TriggerTiming::After,
                },
                events: t
                    .events
                    .iter()
                    .filter_map(|e| match e.as_str() {
                        "INSERT" => Some(TriggerEvent::Insert),
                        "UPDATE" => Some(TriggerEvent::Update),
                        "DELETE" => Some(TriggerEvent::Delete),
                        _ => None,
                    })
                    .collect(),
                function_name: t.function_name,
                security_definer: t.security_definer,
            })
            .collect())
    }

    pub(in crate::infra::adapters::mysql) fn parse_table_detail_combined(
        json: &str,
    ) -> Result<TableDetailCombined, DbOperationError> {
        let Some(trimmed) = non_empty_json(json) else {
            return Err(DbOperationError::InvalidJson(
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
            table_info: serde_json::Value,
        }

        let combined: CombinedDetail = serde_json::from_str(trimmed)
            .map_err(|e| DbOperationError::InvalidJson(e.to_string()))?;

        let columns = Self::parse_columns(&combined.columns.to_string())?;
        let indexes = Self::parse_indexes(&combined.indexes.to_string())?;
        let foreign_keys = Self::parse_foreign_keys(&combined.foreign_keys.to_string())?;
        let triggers = Self::parse_triggers(&combined.triggers.to_string())?;
        let table_info = Self::parse_table_info(&combined.table_info.to_string())?;

        Ok((columns, indexes, foreign_keys, triggers, table_info))
    }

    pub(in crate::infra::adapters::mysql) fn parse_table_columns_and_fks(
        json: &str,
    ) -> Result<(Vec<Column>, Vec<ForeignKey>), DbOperationError> {
        let Some(trimmed) = non_empty_json(json) else {
            return Err(DbOperationError::InvalidJson(
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
            .map_err(|e| DbOperationError::InvalidJson(e.to_string()))?;

        let columns = Self::parse_columns(&light.columns.to_string())?;
        let foreign_keys = Self::parse_foreign_keys(&light.foreign_keys.to_string())?;

        Ok((columns, foreign_keys))
    }
}

#[cfg(test)]
mod tests {
    use crate::app::ports::DbOperationError;
    use crate::infra::adapters::mysql::MySqlAdapter;

    mod table_parsing {
        use super::*;
        use rstest::rstest;

        #[rstest]
        #[case("")]
        #[case("null")]
        #[case("NULL")]
        #[case("   ")]
        fn empty_or_null_input_returns_empty_vec(#[case] input: &str) {
            let result = MySqlAdapter::parse_tables(input).unwrap();
            assert!(result.is_empty());
        }

        #[test]
        fn valid_single_table_parses_all_fields() {
            let json = r#"[{
                "schema": "mydb",
                "name": "users",
                "row_count_estimate": 100,
                "has_rls": false
            }]"#;

            let result = MySqlAdapter::parse_tables(json).unwrap();

            assert_eq!(result.len(), 1);
            assert_eq!(result[0].schema, "mydb");
            assert_eq!(result[0].name, "users");
        }

        #[test]
        fn malformed_json_returns_invalid_json_error() {
            let result = MySqlAdapter::parse_tables("{not valid}");
            assert!(matches!(result, Err(DbOperationError::InvalidJson(_))));
        }
    }

    mod column_parsing {
        use super::*;

        #[test]
        fn valid_column_parses_all_fields() {
            let json = r#"[{
                "name": "id",
                "data_type": "int",
                "nullable": false,
                "default": null,
                "is_primary_key": true,
                "is_unique": false,
                "comment": null,
                "ordinal_position": 1
            }]"#;

            let result = MySqlAdapter::parse_columns(json).unwrap();

            assert_eq!(result.len(), 1);
            assert_eq!(result[0].name, "id");
            assert_eq!(result[0].data_type, "int");
            assert!(!result[0].nullable);
            assert!(result[0].is_primary_key);
        }

        #[test]
        fn empty_returns_empty_vec() {
            assert!(MySqlAdapter::parse_columns("").unwrap().is_empty());
            assert!(MySqlAdapter::parse_columns("null").unwrap().is_empty());
        }
    }

    mod foreign_key_parsing {
        use super::*;
        use crate::domain::FkAction;

        #[test]
        fn valid_fk_parses_mysql_action_strings() {
            let json = r#"[{
                "name": "orders_user_fk",
                "from_schema": "mydb",
                "from_table": "orders",
                "from_columns": ["user_id"],
                "to_schema": "mydb",
                "to_table": "users",
                "to_columns": ["id"],
                "on_delete": "CASCADE",
                "on_update": "NO ACTION"
            }]"#;

            let result = MySqlAdapter::parse_foreign_keys(json).unwrap();
            let fk = &result[0];

            assert_eq!(fk.on_delete, FkAction::Cascade);
            assert_eq!(fk.on_update, FkAction::NoAction);
        }

        #[test]
        fn set_null_action_maps_correctly() {
            let json = r#"[{
                "name": "fk_1",
                "from_schema": "mydb",
                "from_table": "t1",
                "from_columns": ["id"],
                "to_schema": "mydb",
                "to_table": "t2",
                "to_columns": ["id"],
                "on_delete": "SET NULL",
                "on_update": "RESTRICT"
            }]"#;

            let result = MySqlAdapter::parse_foreign_keys(json).unwrap();

            assert_eq!(result[0].on_delete, FkAction::SetNull);
            assert_eq!(result[0].on_update, FkAction::Restrict);
        }

        #[test]
        fn empty_returns_empty_vec() {
            assert!(MySqlAdapter::parse_foreign_keys("").unwrap().is_empty());
        }
    }

    mod index_parsing {
        use super::*;
        use crate::domain::IndexType;

        #[test]
        fn btree_index_maps_correctly() {
            let json = r#"[{
                "name": "PRIMARY",
                "columns": ["id"],
                "is_unique": true,
                "is_primary": true,
                "index_type": "BTREE",
                "definition": null
            }]"#;

            let result = MySqlAdapter::parse_indexes(json).unwrap();

            assert_eq!(result[0].index_type, IndexType::BTree);
            assert!(result[0].is_primary);
        }

        #[test]
        fn hash_index_maps_correctly() {
            let json = r#"[{
                "name": "idx_hash",
                "columns": ["col"],
                "is_unique": false,
                "is_primary": false,
                "index_type": "HASH",
                "definition": null
            }]"#;

            let result = MySqlAdapter::parse_indexes(json).unwrap();

            assert_eq!(result[0].index_type, IndexType::Hash);
        }

        #[test]
        fn fulltext_index_maps_to_other() {
            let json = r#"[{
                "name": "idx_ft",
                "columns": ["content"],
                "is_unique": false,
                "is_primary": false,
                "index_type": "FULLTEXT",
                "definition": null
            }]"#;

            let result = MySqlAdapter::parse_indexes(json).unwrap();

            assert_eq!(
                result[0].index_type,
                IndexType::Other("FULLTEXT".to_string())
            );
        }
    }

    mod trigger_parsing {
        use super::*;
        use crate::domain::{TriggerEvent, TriggerTiming};

        #[test]
        fn valid_trigger_parses_all_fields() {
            let json = r#"[{
                "name": "before_insert_audit",
                "timing": "BEFORE",
                "events": ["INSERT"],
                "function_name": "SET NEW.created_at = NOW()",
                "security_definer": true
            }]"#;

            let result = MySqlAdapter::parse_triggers(json).unwrap();
            let trigger = &result[0];

            assert_eq!(trigger.name, "before_insert_audit");
            assert_eq!(trigger.timing, TriggerTiming::Before);
            assert_eq!(trigger.events, vec![TriggerEvent::Insert]);
            assert!(trigger.security_definer);
        }

        #[test]
        fn empty_returns_empty_vec() {
            assert!(MySqlAdapter::parse_triggers("").unwrap().is_empty());
        }
    }

    mod table_detail_combined_parsing {
        use super::*;

        fn build_combined_json(
            columns: &str,
            indexes: &str,
            fks: &str,
            triggers: &str,
            table_info: &str,
        ) -> String {
            format!(
                r#"{{
                    "columns": {columns},
                    "indexes": {indexes},
                    "foreign_keys": {fks},
                    "triggers": {triggers},
                    "table_info": {table_info}
                }}"#
            )
        }

        #[test]
        fn valid_combined_json_parses_all_categories() {
            let json = build_combined_json(
                r#"[{"name":"id","data_type":"int","nullable":false,"default":null,"is_primary_key":true,"is_unique":false,"comment":null,"ordinal_position":1}]"#,
                "null",
                "null",
                "null",
                r#"{"owner":null,"comment":null,"row_count_estimate":42}"#,
            );

            let (columns, indexes, fks, triggers, table_info) =
                MySqlAdapter::parse_table_detail_combined(&json).unwrap();

            assert_eq!(columns.len(), 1);
            assert_eq!(columns[0].name, "id");
            assert!(indexes.is_empty());
            assert!(fks.is_empty());
            assert!(triggers.is_empty());
            assert_eq!(table_info.row_count_estimate, Some(42));
        }

        #[test]
        fn empty_input_returns_error() {
            let result = MySqlAdapter::parse_table_detail_combined("");
            assert!(matches!(result, Err(DbOperationError::InvalidJson(_))));
        }

        #[test]
        fn null_input_returns_error() {
            let result = MySqlAdapter::parse_table_detail_combined("null");
            assert!(matches!(result, Err(DbOperationError::InvalidJson(_))));
        }
    }

    mod table_columns_and_fks_parsing {
        use super::*;

        #[test]
        fn valid_light_json_parses_columns_and_fks() {
            let json = r#"{"columns": [{"name":"id","data_type":"int","nullable":false,"default":null,"is_primary_key":true,"is_unique":false,"comment":null,"ordinal_position":1}], "foreign_keys": null}"#;

            let (columns, fks) = MySqlAdapter::parse_table_columns_and_fks(json).unwrap();

            assert_eq!(columns.len(), 1);
            assert!(fks.is_empty());
        }

        #[test]
        fn empty_input_returns_error() {
            let result = MySqlAdapter::parse_table_columns_and_fks("");
            assert!(matches!(result, Err(DbOperationError::InvalidJson(_))));
        }
    }
}
