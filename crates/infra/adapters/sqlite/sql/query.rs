use super::{quote_ident, quote_literal};

use super::super::SqliteAdapter;

impl SqliteAdapter {
    /// The single schema SQLite exposes for the main database file.
    pub const MAIN_SCHEMA: &'static str = "main";

    /// Filter excluding SQLite internal tables (sqlite_sequence, sqlite_stat1, …).
    const USER_TABLES_FILTER: &'static str =
        r"type = 'table' AND name NOT LIKE 'sqlite\_%' ESCAPE '\'";

    pub(in crate::adapters::sqlite) fn tables_query() -> String {
        format!(
            r"
            SELECT json_group_array(json_object(
                'schema', 'main',
                'name', m.name,
                'row_count_estimate', NULL,
                'has_rls', json('false')
            ))
            FROM (
                SELECT name FROM sqlite_master
                WHERE {filter}
                ORDER BY name
            ) m
            ",
            filter = Self::USER_TABLES_FILTER,
        )
    }

    pub(in crate::adapters::sqlite) fn table_signatures_query() -> String {
        format!(
            r#"
            SELECT json_group_array(json_object(
                'schema', 'main',
                'name', m.name,
                'signature', COALESCE((
                    SELECT group_concat(
                        ti.name || ':' || ti.type || ':' || ti."notnull" || ':' || COALESCE(ti.dflt_value, ''),
                        '|'
                    )
                    FROM pragma_table_info(m.name) ti
                ), '') || '##FK##' || COALESCE((
                    SELECT group_concat(
                        fk.id || ':' || fk."table" || ':' || fk."from",
                        '|'
                    )
                    FROM pragma_foreign_key_list(m.name) fk
                ), '')
            ))
            FROM (
                SELECT name FROM sqlite_master
                WHERE {filter}
                ORDER BY name
            ) m
            "#,
            filter = Self::USER_TABLES_FILTER,
        )
    }

    pub(in crate::adapters::sqlite) fn columns_query(table: &str) -> String {
        format!(
            r#"
            SELECT json_group_array(json_object(
                'name', ti.name,
                'data_type', ti.type,
                'nullable', json(CASE WHEN ti."notnull" = 0 THEN 'true' ELSE 'false' END),
                'default', ti.dflt_value,
                'is_primary_key', json(CASE WHEN ti.pk > 0 THEN 'true' ELSE 'false' END),
                'is_unique', json('false'),
                'comment', NULL,
                'ordinal_position', ti.cid + 1
            ))
            FROM (SELECT * FROM pragma_table_info({table}) ORDER BY cid) ti
            "#,
            table = quote_literal(table),
        )
    }

    pub(in crate::adapters::sqlite) fn indexes_query(table: &str) -> String {
        format!(
            r#"
            SELECT json_group_array(json_object(
                'name', il.name,
                'columns', (
                    SELECT json_group_array(COALESCE(ii.name, '<expr>'))
                    FROM (SELECT * FROM pragma_index_info(il.name) ORDER BY seqno) ii
                ),
                'is_unique', json(CASE WHEN il."unique" = 1 THEN 'true' ELSE 'false' END),
                'is_primary', json(CASE WHEN il.origin = 'pk' THEN 'true' ELSE 'false' END),
                'index_type', 'BTREE',
                'definition', (
                    SELECT sm.sql FROM sqlite_master sm
                    WHERE sm.type = 'index' AND sm.name = il.name
                )
            ))
            FROM (SELECT * FROM pragma_index_list({table}) ORDER BY name) il
            "#,
            table = quote_literal(table),
        )
    }

    pub(in crate::adapters::sqlite) fn foreign_keys_query(table: &str) -> String {
        format!(
            r#"
            SELECT json_group_array(json_object(
                'name', 'fk_' || g.id,
                'from_schema', 'main',
                'from_table', {table},
                'from_columns', json(g.from_cols),
                'to_schema', 'main',
                'to_table', g.ref_table,
                'to_columns', json(g.to_cols),
                'on_delete', g.on_delete,
                'on_update', g.on_update
            ))
            FROM (
                SELECT
                    fk.id AS id,
                    fk."table" AS ref_table,
                    fk.on_delete AS on_delete,
                    fk.on_update AS on_update,
                    json_group_array(fk."from") AS from_cols,
                    json_group_array(COALESCE(fk."to", '')) AS to_cols
                FROM (SELECT * FROM pragma_foreign_key_list({table}) ORDER BY id, seq) fk
                GROUP BY fk.id, fk."table", fk.on_delete, fk.on_update
            ) g
            "#,
            table = quote_literal(table),
        )
    }

    pub(in crate::adapters::sqlite) fn triggers_query(table: &str) -> String {
        format!(
            r"
            SELECT json_group_array(json_object(
                'name', t.name,
                'sql', t.sql
            ))
            FROM (
                SELECT name, sql FROM sqlite_master
                WHERE type = 'trigger' AND tbl_name = {table}
                ORDER BY name
            ) t
            ",
            table = quote_literal(table),
        )
    }

    pub(in crate::adapters::sqlite) fn table_columns_and_fks_query(table: &str) -> String {
        format!(
            r"
            SELECT json_object(
                'columns', ({columns}),
                'foreign_keys', ({fks})
            )
            ",
            columns = Self::columns_query(table).trim(),
            fks = Self::foreign_keys_query(table).trim(),
        )
    }

    pub(in crate::adapters::sqlite) fn table_detail_query(table: &str) -> String {
        format!(
            r"
            SELECT json_object(
                'columns', ({columns}),
                'indexes', ({indexes}),
                'foreign_keys', ({fks}),
                'triggers', ({triggers})
            )
            ",
            columns = Self::columns_query(table).trim(),
            indexes = Self::indexes_query(table).trim(),
            fks = Self::foreign_keys_query(table).trim(),
            triggers = Self::triggers_query(table).trim(),
        )
    }

    pub(in crate::adapters::sqlite) fn preview_pk_columns_query(table: &str) -> String {
        format!(
            r"
            SELECT json_group_array(pk.name) FROM (
                SELECT ti.name FROM pragma_table_info({table}) ti
                WHERE ti.pk > 0
                ORDER BY ti.pk
            ) pk
            ",
            table = quote_literal(table),
        )
    }

    pub(in crate::adapters::sqlite) fn build_preview_query(
        schema: &str,
        table: &str,
        order_columns: &[String],
        limit: usize,
        offset: usize,
    ) -> String {
        let order_clause = if order_columns.is_empty() {
            String::new()
        } else {
            let cols = order_columns
                .iter()
                .map(|col| quote_ident(col))
                .collect::<Vec<_>>()
                .join(", ");
            format!(" ORDER BY {cols}")
        };

        format!(
            "SELECT * FROM {}.{}{} LIMIT {} OFFSET {}",
            quote_ident(schema),
            quote_ident(table),
            order_clause,
            limit,
            offset
        )
    }
}

#[cfg(test)]
mod tests {
    use crate::adapters::sqlite::SqliteAdapter;

    mod preview_query {
        use super::*;

        #[test]
        fn with_primary_key_columns_returns_ordered_preview_query() {
            let sql = SqliteAdapter::build_preview_query(
                "main",
                "users",
                &["id".to_string(), "tenant_id".to_string()],
                100,
                200,
            );

            assert_eq!(
                sql,
                "SELECT * FROM \"main\".\"users\" ORDER BY \"id\", \"tenant_id\" LIMIT 100 OFFSET 200"
            );
        }

        #[test]
        fn without_primary_key_columns_returns_unordered_preview_query() {
            let sql = SqliteAdapter::build_preview_query("main", "users", &[], 100, 0);

            assert_eq!(sql, "SELECT * FROM \"main\".\"users\" LIMIT 100 OFFSET 0");
        }

        #[test]
        fn primary_key_query_uses_pragma_table_info() {
            let sql = SqliteAdapter::preview_pk_columns_query("users");

            assert!(sql.contains("pragma_table_info('users')"));
            assert!(sql.contains("pk > 0"));
        }
    }

    mod table_detail_query {
        use super::*;

        #[test]
        fn wraps_all_four_categories_in_json_object() {
            let sql = SqliteAdapter::table_detail_query("users");

            assert!(sql.contains("json_object("));
            for key in ["'columns'", "'indexes'", "'foreign_keys'", "'triggers'"] {
                assert!(sql.contains(key), "Missing key: {key}");
            }
        }

        #[test]
        fn table_detail_uses_quoted_table_literal() {
            let sql = SqliteAdapter::table_detail_query("my_table");

            assert!(sql.contains("'my_table'"));
        }
    }

    mod table_columns_and_fks_query {
        use super::*;

        #[test]
        fn wraps_columns_and_fks_only_in_json_object() {
            let sql = SqliteAdapter::table_columns_and_fks_query("users");

            assert!(sql.contains("json_object("));
            assert!(sql.contains("'columns'"));
            assert!(sql.contains("'foreign_keys'"));
            assert!(!sql.contains("'indexes'"));
            assert!(!sql.contains("'triggers'"));
        }
    }

    mod metadata_query_injection {
        use super::*;
        use rstest::rstest;

        const HOSTILE: &str = "'; DROP TABLE users; --";
        const ESCAPED: &str = "'''; DROP TABLE users; --'";

        #[rstest]
        #[case("columns_query", SqliteAdapter::columns_query(HOSTILE))]
        #[case("indexes_query", SqliteAdapter::indexes_query(HOSTILE))]
        #[case("foreign_keys_query", SqliteAdapter::foreign_keys_query(HOSTILE))]
        #[case("triggers_query", SqliteAdapter::triggers_query(HOSTILE))]
        #[case("table_detail_query", SqliteAdapter::table_detail_query(HOSTILE))]
        #[case(
            "table_columns_and_fks_query",
            SqliteAdapter::table_columns_and_fks_query(HOSTILE)
        )]
        #[case(
            "preview_pk_columns_query",
            SqliteAdapter::preview_pk_columns_query(HOSTILE)
        )]
        fn hostile_input_is_escaped(#[case] _label: &str, #[case] sql: String) {
            assert!(
                sql.contains(ESCAPED),
                "Hostile input must be quote_literal-escaped in: {sql}"
            );
        }
    }

    mod system_table_filter {
        use super::*;

        #[test]
        fn tables_query_excludes_sqlite_internal_tables() {
            let sql = SqliteAdapter::tables_query();
            assert!(sql.contains(r"NOT LIKE 'sqlite\_%' ESCAPE '\'"));
        }

        #[test]
        fn table_signatures_query_excludes_sqlite_internal_tables() {
            let sql = SqliteAdapter::table_signatures_query();
            assert!(sql.contains(r"NOT LIKE 'sqlite\_%' ESCAPE '\'"));
        }
    }

    mod preview_query_edge_cases {
        use super::*;

        #[test]
        fn table_name_with_double_quote_is_escaped() {
            let sql = SqliteAdapter::build_preview_query("main", "my\"table", &[], 100, 0);

            assert_eq!(
                sql,
                "SELECT * FROM \"main\".\"my\"\"table\" LIMIT 100 OFFSET 0"
            );
        }

        #[test]
        fn order_by_column_with_double_quote_is_escaped() {
            let sql = SqliteAdapter::build_preview_query(
                "main",
                "users",
                &["my\"col".to_string()],
                100,
                0,
            );

            assert_eq!(
                sql,
                "SELECT * FROM \"main\".\"users\" ORDER BY \"my\"\"col\" LIMIT 100 OFFSET 0"
            );
        }
    }
}
