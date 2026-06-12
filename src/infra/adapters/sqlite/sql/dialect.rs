use super::{quote_ident, quote_literal};
use crate::app::ports::outbound::SqlDialect;

use super::super::SqliteAdapter;

fn sql_literal_or_null(value: &str) -> String {
    if value == "NULL" {
        "NULL".to_string()
    } else {
        quote_literal(value)
    }
}

impl SqlDialect for SqliteAdapter {
    fn build_explain_sql(&self, query: &str) -> Option<String> {
        Some(format!("EXPLAIN QUERY PLAN {query}"))
    }

    fn build_explain_analyze_sql(&self, _query: &str) -> Option<String> {
        // SQLite has no EXPLAIN ANALYZE equivalent.
        None
    }

    fn build_update_sql(
        &self,
        schema: &str,
        table: &str,
        column: &str,
        new_value: &str,
        pk_pairs: &[(String, String)],
    ) -> String {
        let where_clause = pk_pairs
            .iter()
            .map(|(col, val)| format!("{} = {}", quote_ident(col), quote_literal(val)))
            .collect::<Vec<_>>()
            .join(" AND ");

        format!(
            "UPDATE {}.{}\nSET {} = {}\nWHERE {};",
            quote_ident(schema),
            quote_ident(table),
            quote_ident(column),
            sql_literal_or_null(new_value),
            where_clause
        )
    }

    fn build_bulk_delete_sql(
        &self,
        schema: &str,
        table: &str,
        pk_pairs_per_row: &[Vec<(String, String)>],
    ) -> String {
        assert!(
            !pk_pairs_per_row.is_empty(),
            "pk_pairs_per_row must not be empty"
        );

        let pk_count = pk_pairs_per_row[0].len();

        let where_clause = if pk_count == 1 {
            let col = quote_ident(&pk_pairs_per_row[0][0].0);
            let values = pk_pairs_per_row
                .iter()
                .map(|pairs| sql_literal_or_null(&pairs[0].1))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{col} IN ({values})")
        } else {
            let cols = pk_pairs_per_row[0]
                .iter()
                .map(|(col, _)| quote_ident(col))
                .collect::<Vec<_>>()
                .join(", ");
            let rows = pk_pairs_per_row
                .iter()
                .map(|pairs| {
                    let vals = pairs
                        .iter()
                        .map(|(_, val)| sql_literal_or_null(val))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("({vals})")
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("({cols}) IN ({rows})")
        };

        format!(
            "DELETE FROM {}.{}\nWHERE {};",
            quote_ident(schema),
            quote_ident(table),
            where_clause
        )
    }
}

#[cfg(test)]
mod tests {
    use crate::adapters::sqlite::SqliteAdapter;
    use crate::app::ports::outbound::SqlDialect;

    mod explain {
        use super::*;

        #[test]
        fn explain_uses_query_plan() {
            let adapter = SqliteAdapter::new();
            assert_eq!(
                adapter.build_explain_sql("SELECT 1"),
                Some("EXPLAIN QUERY PLAN SELECT 1".to_string())
            );
        }

        #[test]
        fn explain_analyze_is_unsupported() {
            let adapter = SqliteAdapter::new();
            assert_eq!(adapter.build_explain_analyze_sql("SELECT 1"), None);
        }
    }

    mod sql_dialect_update {
        use super::*;

        #[test]
        fn single_pk_returns_escaped_sql() {
            let adapter = SqliteAdapter::new();

            let sql = adapter.build_update_sql(
                "main",
                "users",
                "name",
                "O'Reilly",
                &[("id".into(), "42".into())],
            );

            assert_eq!(
                sql,
                "UPDATE \"main\".\"users\"\nSET \"name\" = 'O''Reilly'\nWHERE \"id\" = '42';"
            );
        }

        #[test]
        fn null_value_generates_unquoted_null() {
            let adapter = SqliteAdapter::new();

            let sql = adapter.build_update_sql(
                "main",
                "users",
                "name",
                "NULL",
                &[("id".into(), "1".into())],
            );

            assert_eq!(
                sql,
                "UPDATE \"main\".\"users\"\nSET \"name\" = NULL\nWHERE \"id\" = '1';"
            );
        }

        #[test]
        fn backslash_in_value_is_not_escaped() {
            let adapter = SqliteAdapter::new();

            let sql = adapter.build_update_sql(
                "main",
                "users",
                "path",
                r"C:\Users\test",
                &[("id".into(), "1".into())],
            );

            assert_eq!(
                sql,
                "UPDATE \"main\".\"users\"\nSET \"path\" = 'C:\\Users\\test'\nWHERE \"id\" = '1';"
            );
        }
    }

    mod sql_dialect_bulk_delete {
        use super::*;

        #[test]
        fn single_pk_multiple_rows_returns_in_clause_with_all_values() {
            let adapter = SqliteAdapter::new();
            let rows = vec![
                vec![("id".to_string(), "1".to_string())],
                vec![("id".to_string(), "2".to_string())],
            ];

            let sql = adapter.build_bulk_delete_sql("main", "users", &rows);

            assert_eq!(
                sql,
                "DELETE FROM \"main\".\"users\"\nWHERE \"id\" IN ('1', '2');"
            );
        }

        #[test]
        fn composite_pk_returns_row_constructor_in_clause() {
            let adapter = SqliteAdapter::new();
            let rows = vec![
                vec![
                    ("id".to_string(), "1".to_string()),
                    ("tenant_id".to_string(), "a".to_string()),
                ],
                vec![
                    ("id".to_string(), "2".to_string()),
                    ("tenant_id".to_string(), "b".to_string()),
                ],
            ];

            let sql = adapter.build_bulk_delete_sql("main", "t", &rows);

            assert_eq!(
                sql,
                "DELETE FROM \"main\".\"t\"\nWHERE (\"id\", \"tenant_id\") IN (('1', 'a'), ('2', 'b'));"
            );
        }

        #[test]
        fn pk_value_with_quotes_is_escaped() {
            let adapter = SqliteAdapter::new();
            let rows = vec![vec![("id".to_string(), "O'Reilly".to_string())]];

            let sql = adapter.build_bulk_delete_sql("main", "t", &rows);

            assert_eq!(
                sql,
                "DELETE FROM \"main\".\"t\"\nWHERE \"id\" IN ('O''Reilly');"
            );
        }
    }
}
