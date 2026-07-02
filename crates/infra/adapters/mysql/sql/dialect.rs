use super::{quote_ident_mysql, quote_literal};
use crate::app::ports::outbound::SqlDialect;

use super::super::MySqlAdapter;

fn sql_literal_or_null(value: &str) -> String {
    if value == "NULL" {
        "NULL".to_string()
    } else {
        quote_literal(value)
    }
}

fn build_pk_where_clause(pk_pairs_per_row: &[Vec<(String, String)>]) -> String {
    assert!(
        !pk_pairs_per_row.is_empty(),
        "pk_pairs_per_row must not be empty"
    );

    let pk_count = pk_pairs_per_row[0].len();

    if pk_count == 1 {
        let col = quote_ident_mysql(&pk_pairs_per_row[0][0].0);
        let values = pk_pairs_per_row
            .iter()
            .map(|pairs| sql_literal_or_null(&pairs[0].1))
            .collect::<Vec<_>>()
            .join(", ");
        format!("{col} IN ({values})")
    } else {
        let cols = pk_pairs_per_row[0]
            .iter()
            .map(|(col, _)| quote_ident_mysql(col))
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
    }
}

impl SqlDialect for MySqlAdapter {
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
        let where_clause = pk_pairs
            .iter()
            .map(|(col, val)| format!("{} = {}", quote_ident_mysql(col), quote_literal(val)))
            .collect::<Vec<_>>()
            .join(" AND ");

        format!(
            "UPDATE {}.{}\nSET {} = {}\nWHERE {};",
            quote_ident_mysql(schema),
            quote_ident_mysql(table),
            quote_ident_mysql(column),
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
        let where_clause = build_pk_where_clause(pk_pairs_per_row);

        format!(
            "DELETE FROM {}.{}\nWHERE {};",
            quote_ident_mysql(schema),
            quote_ident_mysql(table),
            where_clause
        )
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
            .map(|column| quote_ident_mysql(column))
            .collect::<Vec<_>>()
            .join(", ");
        let where_clause = if pk_pairs_per_row.is_empty() {
            String::new()
        } else {
            format!("\nWHERE {}", build_pk_where_clause(pk_pairs_per_row))
        };

        format!(
            "SELECT {columns}\nFROM {}.{}{where_clause};",
            quote_ident_mysql(schema),
            quote_ident_mysql(table)
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
            .map(|column| quote_ident_mysql(column))
            .collect::<Vec<_>>()
            .join(", ");
        let values = rows
            .iter()
            .map(|row| {
                let values = row
                    .iter()
                    .map(|value| sql_literal_or_null(value))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("  ({values})")
            })
            .collect::<Vec<_>>()
            .join(",\n");

        format!(
            "INSERT INTO {}.{} ({columns}) VALUES\n{values};",
            quote_ident_mysql(schema),
            quote_ident_mysql(table)
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
                                    quote_ident_mysql(column),
                                    sql_literal_or_null(value)
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
                                    quote_ident_mysql(pk_column),
                                    sql_literal_or_null(value)
                                )
                            })
                    })
                    .collect::<Vec<_>>()
                    .join(" AND ");

                format!(
                    "UPDATE {}.{}\nSET {set_clause}\nWHERE {where_clause};",
                    quote_ident_mysql(schema),
                    quote_ident_mysql(table)
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[cfg(test)]
mod tests {
    use crate::adapters::mysql::MySqlAdapter;
    use crate::app::ports::outbound::SqlDialect;

    mod sql_dialect_update {
        use super::*;

        #[test]
        fn single_pk_returns_escaped_sql() {
            let adapter = MySqlAdapter::new();

            let sql = adapter.build_update_sql(
                "mydb",
                "users",
                "name",
                "O'Reilly",
                &[("id".into(), "42".into())],
            );

            assert_eq!(
                sql,
                "UPDATE `mydb`.`users`\nSET `name` = 'O''Reilly'\nWHERE `id` = '42';"
            );
        }

        #[test]
        fn composite_pk_returns_where_with_all_keys() {
            let adapter = MySqlAdapter::new();

            let sql = adapter.build_update_sql(
                "s",
                "t",
                "name",
                "new",
                &[("id".into(), "1".into()), ("tenant_id".into(), "7".into())],
            );

            assert_eq!(
                sql,
                "UPDATE `s`.`t`\nSET `name` = 'new'\nWHERE `id` = '1' AND `tenant_id` = '7';"
            );
        }

        #[test]
        fn null_value_generates_unquoted_null() {
            let adapter = MySqlAdapter::new();

            let sql = adapter.build_update_sql(
                "mydb",
                "users",
                "name",
                "NULL",
                &[("id".into(), "1".into())],
            );

            assert_eq!(
                sql,
                "UPDATE `mydb`.`users`\nSET `name` = NULL\nWHERE `id` = '1';"
            );
        }

        #[test]
        fn empty_string_value_generates_empty_literal() {
            let adapter = MySqlAdapter::new();

            let sql =
                adapter.build_update_sql("mydb", "users", "name", "", &[("id".into(), "1".into())]);

            assert_eq!(
                sql,
                "UPDATE `mydb`.`users`\nSET `name` = ''\nWHERE `id` = '1';"
            );
        }

        #[test]
        fn backslash_in_value_is_escaped_for_mysql() {
            let adapter = MySqlAdapter::new();

            let sql = adapter.build_update_sql(
                "mydb",
                "users",
                "path",
                r"C:\Users\test",
                &[("id".into(), "1".into())],
            );

            assert_eq!(
                sql,
                "UPDATE `mydb`.`users`\nSET `path` = 'C:\\\\Users\\\\test'\nWHERE `id` = '1';"
            );
        }

        #[test]
        fn column_name_with_backtick_is_escaped() {
            let adapter = MySqlAdapter::new();

            let sql = adapter.build_update_sql(
                "mydb",
                "users",
                "my`col",
                "val",
                &[("id".into(), "1".into())],
            );

            assert_eq!(
                sql,
                "UPDATE `mydb`.`users`\nSET `my``col` = 'val'\nWHERE `id` = '1';"
            );
        }
    }

    mod sql_dialect_bulk_delete {
        use super::*;

        #[test]
        fn single_pk_single_row_returns_in_clause() {
            let adapter = MySqlAdapter::new();
            let rows = vec![vec![("id".to_string(), "1".to_string())]];

            let sql = adapter.build_bulk_delete_sql("mydb", "users", &rows);

            assert_eq!(sql, "DELETE FROM `mydb`.`users`\nWHERE `id` IN ('1');");
        }

        #[test]
        fn single_pk_multiple_rows_returns_in_clause_with_all_values() {
            let adapter = MySqlAdapter::new();
            let rows = vec![
                vec![("id".to_string(), "1".to_string())],
                vec![("id".to_string(), "2".to_string())],
                vec![("id".to_string(), "3".to_string())],
            ];

            let sql = adapter.build_bulk_delete_sql("mydb", "users", &rows);

            assert_eq!(
                sql,
                "DELETE FROM `mydb`.`users`\nWHERE `id` IN ('1', '2', '3');"
            );
        }

        #[test]
        fn composite_pk_returns_row_constructor_in_clause() {
            let adapter = MySqlAdapter::new();
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

            let sql = adapter.build_bulk_delete_sql("s", "t", &rows);

            assert_eq!(
                sql,
                "DELETE FROM `s`.`t`\nWHERE (`id`, `tenant_id`) IN (('1', 'a'), ('2', 'b'));"
            );
        }

        #[test]
        fn null_pk_value_uses_null_literal() {
            let adapter = MySqlAdapter::new();
            let rows = vec![vec![("id".to_string(), "NULL".to_string())]];

            let sql = adapter.build_bulk_delete_sql("mydb", "t", &rows);

            assert_eq!(sql, "DELETE FROM `mydb`.`t`\nWHERE `id` IN (NULL);");
        }

        #[test]
        fn pk_value_with_quotes_is_escaped() {
            let adapter = MySqlAdapter::new();
            let rows = vec![vec![("id".to_string(), "O'Reilly".to_string())]];

            let sql = adapter.build_bulk_delete_sql("mydb", "t", &rows);

            assert_eq!(sql, "DELETE FROM `mydb`.`t`\nWHERE `id` IN ('O''Reilly');");
        }
    }

    mod sql_dialect_select {
        use super::*;

        #[test]
        fn single_pk_returns_in_clause() {
            let adapter = MySqlAdapter::new();
            let rows = vec![
                vec![("id".to_string(), "1".to_string())],
                vec![("id".to_string(), "2".to_string())],
            ];

            let sql = adapter.build_select_sql(
                "mydb",
                "users",
                &["id".to_string(), "name".to_string()],
                &rows,
            );

            assert_eq!(
                sql,
                "SELECT `id`, `name`\nFROM `mydb`.`users`\nWHERE `id` IN ('1', '2');"
            );
        }

        #[test]
        fn composite_pk_returns_tuple_in_clause() {
            let adapter = MySqlAdapter::new();
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

            let sql = adapter.build_select_sql(
                "s",
                "t",
                &["id".to_string(), "tenant_id".to_string()],
                &rows,
            );

            assert_eq!(
                sql,
                "SELECT `id`, `tenant_id`\nFROM `s`.`t`\nWHERE (`id`, `tenant_id`) IN (('1', 'a'), ('2', 'b'));"
            );
        }

        #[test]
        fn empty_pk_rows_omits_where_clause() {
            let adapter = MySqlAdapter::new();

            let sql = adapter.build_select_sql(
                "mydb",
                "users",
                &["id".to_string(), "name".to_string()],
                &[],
            );

            assert_eq!(sql, "SELECT `id`, `name`\nFROM `mydb`.`users`;");
        }
    }

    mod sql_dialect_insert {
        use super::*;

        #[test]
        fn multiple_rows_and_null_return_values_clause() {
            let adapter = MySqlAdapter::new();
            let rows = vec![
                vec!["1".to_string(), "alice".to_string()],
                vec!["2".to_string(), "NULL".to_string()],
            ];

            let sql = adapter.build_insert_sql(
                "mydb",
                "users",
                &["id".to_string(), "name".to_string()],
                &rows,
            );

            assert_eq!(
                sql,
                "INSERT INTO `mydb`.`users` (`id`, `name`) VALUES\n  ('1', 'alice'),\n  ('2', NULL);"
            );
        }
    }

    mod sql_dialect_row_update {
        use super::*;

        #[test]
        fn multiple_rows_exclude_pk_from_set_and_join_statements() {
            let adapter = MySqlAdapter::new();
            let rows = vec![
                vec!["1".to_string(), "alice".to_string()],
                vec!["2".to_string(), "NULL".to_string()],
            ];

            let sql = adapter.build_row_update_sql(
                "mydb",
                "users",
                &["id".to_string(), "name".to_string()],
                &rows,
                &["id".to_string()],
            );

            assert_eq!(
                sql,
                "UPDATE `mydb`.`users`\nSET `name` = 'alice'\nWHERE `id` = '1';\nUPDATE `mydb`.`users`\nSET `name` = NULL\nWHERE `id` = '2';"
            );
        }
    }

    mod sql_literal_or_null_tests {
        use super::super::sql_literal_or_null;
        use rstest::rstest;

        #[rstest]
        #[case("NULL", "NULL")]
        #[case("null", "'null'")]
        #[case("", "''")]
        #[case("hello", "'hello'")]
        #[case("it's", "'it''s'")]
        #[case("NULL ", "'NULL '")]
        fn formats_sql_literal_or_null(#[case] input: &str, #[case] expected: &str) {
            assert_eq!(sql_literal_or_null(input), expected);
        }
    }
}
