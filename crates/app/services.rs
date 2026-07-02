use std::sync::Arc;

use super::ports::outbound::{DdlGenerator, SqlDialect};
use crate::model::shared::db_capabilities::DbCapabilities;

pub struct AppServices {
    pub ddl_generator: Arc<dyn DdlGenerator>,
    pub sql_dialect: Arc<dyn SqlDialect>,
    pub db_capabilities: DbCapabilities,
}

impl AppServices {
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn stub() -> Self {
        struct StubDdlGenerator;
        impl DdlGenerator for StubDdlGenerator {
            fn generate_ddl(&self, _table: &crate::domain::Table) -> String {
                unimplemented!("inject a real DdlGenerator via AppServices")
            }
            fn ddl_line_count(&self, _table: &crate::domain::Table) -> usize {
                0
            }
        }

        struct StubSqlDialect;
        impl StubSqlDialect {
            fn quote_ident(value: &str) -> String {
                format!("\"{}\"", value.replace('"', "\"\""))
            }

            fn sql_literal_or_null(value: &str) -> String {
                if value == "NULL" {
                    "NULL".to_string()
                } else {
                    format!("'{}'", value.replace('\'', "''"))
                }
            }

            fn pk_where_clause(pk_pairs_per_row: &[Vec<(String, String)>]) -> String {
                let pk_count = pk_pairs_per_row[0].len();
                if pk_count == 1 {
                    let column = Self::quote_ident(&pk_pairs_per_row[0][0].0);
                    let values = pk_pairs_per_row
                        .iter()
                        .map(|pairs| Self::sql_literal_or_null(&pairs[0].1))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("{column} IN ({values})")
                } else {
                    let columns = pk_pairs_per_row[0]
                        .iter()
                        .map(|(column, _)| Self::quote_ident(column))
                        .collect::<Vec<_>>()
                        .join(", ");
                    let rows = pk_pairs_per_row
                        .iter()
                        .map(|pairs| {
                            let values = pairs
                                .iter()
                                .map(|(_, value)| Self::sql_literal_or_null(value))
                                .collect::<Vec<_>>()
                                .join(", ");
                            format!("({values})")
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("({columns}) IN ({rows})")
                }
            }
        }

        impl SqlDialect for StubSqlDialect {
            fn build_explain_sql(&self, query: &str) -> Option<String> {
                Some(format!("EXPLAIN {query}"))
            }

            fn build_explain_analyze_sql(&self, query: &str) -> Option<String> {
                Some(format!("EXPLAIN ANALYZE {query}"))
            }

            fn build_update_sql(
                &self,
                schema: &str,
                table: &str,
                column: &str,
                new_value: &str,
                pk_pairs: &[(String, String)],
            ) -> String {
                let set_clause = format!("\"{column}\" = '{new_value}'");
                let where_clause = pk_pairs
                    .iter()
                    .map(|(key, value)| format!("\"{key}\" = '{value}'"))
                    .collect::<Vec<_>>()
                    .join(" AND ");
                format!("UPDATE \"{schema}\".\"{table}\" SET {set_clause} WHERE {where_clause}")
            }
            fn build_bulk_delete_sql(
                &self,
                schema: &str,
                table: &str,
                pk_pairs_per_row: &[Vec<(String, String)>],
            ) -> String {
                let where_clause = pk_pairs_per_row
                    .iter()
                    .map(|pk_pairs| {
                        pk_pairs
                            .iter()
                            .map(|(key, value)| format!("\"{key}\" = '{value}'"))
                            .collect::<Vec<_>>()
                            .join(" AND ")
                    })
                    .collect::<Vec<_>>()
                    .join(" OR ");
                format!("DELETE FROM \"{schema}\".\"{table}\" WHERE {where_clause}")
            }

            fn build_select_sql(
                &self,
                schema: &str,
                table: &str,
                columns: &[String],
                pk_pairs_per_row: &[Vec<(String, String)>],
            ) -> String {
                let columns = columns
                    .iter()
                    .map(|column| Self::quote_ident(column))
                    .collect::<Vec<_>>()
                    .join(", ");
                let where_clause = if pk_pairs_per_row.is_empty() {
                    String::new()
                } else {
                    format!("\nWHERE {}", Self::pk_where_clause(pk_pairs_per_row))
                };
                format!(
                    "SELECT {columns}\nFROM {}.{}{where_clause};",
                    Self::quote_ident(schema),
                    Self::quote_ident(table)
                )
            }

            fn build_insert_sql(
                &self,
                schema: &str,
                table: &str,
                columns: &[String],
                rows: &[Vec<String>],
            ) -> String {
                let columns = columns
                    .iter()
                    .map(|column| Self::quote_ident(column))
                    .collect::<Vec<_>>()
                    .join(", ");
                let values = rows
                    .iter()
                    .map(|row| {
                        let values = row
                            .iter()
                            .map(|value| Self::sql_literal_or_null(value))
                            .collect::<Vec<_>>()
                            .join(", ");
                        format!("  ({values})")
                    })
                    .collect::<Vec<_>>()
                    .join(",\n");
                format!(
                    "INSERT INTO {}.{} ({columns}) VALUES\n{values};",
                    Self::quote_ident(schema),
                    Self::quote_ident(table)
                )
            }

            fn build_row_update_sql(
                &self,
                schema: &str,
                table: &str,
                columns: &[String],
                rows: &[Vec<String>],
                pk_columns: &[String],
            ) -> String {
                rows.iter()
                    .map(|row| {
                        let set_clause = columns
                            .iter()
                            .enumerate()
                            .filter_map(|(index, column)| {
                                if pk_columns.contains(column) {
                                    None
                                } else {
                                    row.get(index).map(|value| {
                                        format!(
                                            "{} = {}",
                                            Self::quote_ident(column),
                                            Self::sql_literal_or_null(value)
                                        )
                                    })
                                }
                            })
                            .collect::<Vec<_>>()
                            .join(", ");
                        let where_clause = pk_columns
                            .iter()
                            .filter_map(|pk_column| {
                                columns
                                    .iter()
                                    .position(|column| column == pk_column)
                                    .and_then(|index| row.get(index))
                                    .map(|value| {
                                        format!(
                                            "{} = {}",
                                            Self::quote_ident(pk_column),
                                            Self::sql_literal_or_null(value)
                                        )
                                    })
                            })
                            .collect::<Vec<_>>()
                            .join(" AND ");
                        format!(
                            "UPDATE {}.{}\nSET {set_clause}\nWHERE {where_clause};",
                            Self::quote_ident(schema),
                            Self::quote_ident(table)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        }

        Self {
            ddl_generator: Arc::new(StubDdlGenerator),
            sql_dialect: Arc::new(StubSqlDialect),
            db_capabilities: DbCapabilities::postgres_like(),
        }
    }
}
