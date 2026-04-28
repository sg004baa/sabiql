use crate::infra::utils::{quote_ident_mysql, quote_literal};

use super::super::MySqlAdapter;

impl MySqlAdapter {
    /// Filter excluding MySQL/InnoDB internal schemas. Used when no database
    /// is bound to the connection so users only see their own schemas.
    const NON_SYSTEM_SCHEMAS_FILTER: &'static str =
        "TABLE_SCHEMA NOT IN ('mysql', 'information_schema', 'performance_schema', 'sys')";

    pub(in crate::infra::adapters::mysql) fn tables_query() -> &'static str {
        r"
        SELECT JSON_ARRAYAGG(JSON_OBJECT(
            'schema', t.TABLE_SCHEMA,
            'name', t.TABLE_NAME,
            'row_count_estimate', t.TABLE_ROWS,
            'has_rls', CAST('false' AS JSON)
        ))
        FROM information_schema.TABLES t
        WHERE t.TABLE_SCHEMA = DATABASE()
          AND t.TABLE_TYPE = 'BASE TABLE'
        ORDER BY t.TABLE_NAME
        "
    }

    pub(in crate::infra::adapters::mysql) fn tables_query_all() -> String {
        format!(
            r"
        SELECT JSON_ARRAYAGG(JSON_OBJECT(
            'schema', t.TABLE_SCHEMA,
            'name', t.TABLE_NAME,
            'row_count_estimate', t.TABLE_ROWS,
            'has_rls', CAST('false' AS JSON)
        ))
        FROM information_schema.TABLES t
        WHERE t.{filter}
          AND t.TABLE_TYPE = 'BASE TABLE'
        ORDER BY t.TABLE_SCHEMA, t.TABLE_NAME
        ",
            filter = Self::NON_SYSTEM_SCHEMAS_FILTER,
        )
    }

    pub(in crate::infra::adapters::mysql) fn table_signatures_query() -> &'static str {
        r"
        SELECT JSON_ARRAYAGG(JSON_OBJECT(
            'schema', t.TABLE_SCHEMA,
            'name', t.TABLE_NAME,
            'signature', MD5(CONCAT(
                COALESCE((
                    SELECT GROUP_CONCAT(
                        CONCAT(c.COLUMN_NAME, ':', c.COLUMN_TYPE, ':', c.IS_NULLABLE, ':', COALESCE(c.COLUMN_DEFAULT, ''))
                        ORDER BY c.ORDINAL_POSITION SEPARATOR '|'
                    )
                    FROM information_schema.COLUMNS c
                    WHERE c.TABLE_SCHEMA = t.TABLE_SCHEMA
                      AND c.TABLE_NAME = t.TABLE_NAME
                ), ''),
                '##FK##',
                COALESCE((
                    SELECT GROUP_CONCAT(
                        CONCAT(rc.CONSTRAINT_NAME, ':', rc.REFERENCED_TABLE_NAME)
                        ORDER BY rc.CONSTRAINT_NAME SEPARATOR '|'
                    )
                    FROM information_schema.REFERENTIAL_CONSTRAINTS rc
                    WHERE rc.CONSTRAINT_SCHEMA = t.TABLE_SCHEMA
                      AND rc.TABLE_NAME = t.TABLE_NAME
                ), '')
            ))
        ))
        FROM information_schema.TABLES t
        WHERE t.TABLE_SCHEMA = DATABASE()
          AND t.TABLE_TYPE = 'BASE TABLE'
        ORDER BY t.TABLE_NAME
        "
    }

    pub(in crate::infra::adapters::mysql) fn table_signatures_query_all() -> String {
        format!(
            r"
        SELECT JSON_ARRAYAGG(JSON_OBJECT(
            'schema', t.TABLE_SCHEMA,
            'name', t.TABLE_NAME,
            'signature', MD5(CONCAT(
                COALESCE((
                    SELECT GROUP_CONCAT(
                        CONCAT(c.COLUMN_NAME, ':', c.COLUMN_TYPE, ':', c.IS_NULLABLE, ':', COALESCE(c.COLUMN_DEFAULT, ''))
                        ORDER BY c.ORDINAL_POSITION SEPARATOR '|'
                    )
                    FROM information_schema.COLUMNS c
                    WHERE c.TABLE_SCHEMA = t.TABLE_SCHEMA
                      AND c.TABLE_NAME = t.TABLE_NAME
                ), ''),
                '##FK##',
                COALESCE((
                    SELECT GROUP_CONCAT(
                        CONCAT(rc.CONSTRAINT_NAME, ':', rc.REFERENCED_TABLE_NAME)
                        ORDER BY rc.CONSTRAINT_NAME SEPARATOR '|'
                    )
                    FROM information_schema.REFERENTIAL_CONSTRAINTS rc
                    WHERE rc.CONSTRAINT_SCHEMA = t.TABLE_SCHEMA
                      AND rc.TABLE_NAME = t.TABLE_NAME
                ), '')
            ))
        ))
        FROM information_schema.TABLES t
        WHERE t.{filter}
          AND t.TABLE_TYPE = 'BASE TABLE'
        ORDER BY t.TABLE_SCHEMA, t.TABLE_NAME
        ",
            filter = Self::NON_SYSTEM_SCHEMAS_FILTER,
        )
    }

    pub(in crate::infra::adapters::mysql) fn schemas_query() -> &'static str {
        r"
        SELECT JSON_ARRAYAGG(JSON_OBJECT('name', s.SCHEMA_NAME))
        FROM information_schema.SCHEMATA s
        WHERE s.SCHEMA_NAME = DATABASE()
        "
    }

    pub(in crate::infra::adapters::mysql) fn schemas_query_all() -> &'static str {
        r"
        SELECT JSON_ARRAYAGG(JSON_OBJECT('name', s.SCHEMA_NAME))
        FROM information_schema.SCHEMATA s
        WHERE s.SCHEMA_NAME NOT IN ('mysql', 'information_schema', 'performance_schema', 'sys')
        ORDER BY s.SCHEMA_NAME
        "
    }

    pub(in crate::infra::adapters::mysql) fn columns_query(schema: &str, table: &str) -> String {
        format!(
            r"
            SELECT JSON_ARRAYAGG(j.col) FROM (
                SELECT JSON_OBJECT(
                    'name', c.COLUMN_NAME,
                    'data_type', c.COLUMN_TYPE,
                    'nullable', IF(c.IS_NULLABLE = 'YES', CAST('true' AS JSON), CAST('false' AS JSON)),
                    'default', c.COLUMN_DEFAULT,
                    'is_primary_key', IF(c.COLUMN_KEY = 'PRI', CAST('true' AS JSON), CAST('false' AS JSON)),
                    'is_unique', IF(c.COLUMN_KEY = 'UNI', CAST('true' AS JSON), CAST('false' AS JSON)),
                    'comment', NULLIF(c.COLUMN_COMMENT, ''),
                    'ordinal_position', c.ORDINAL_POSITION
                ) AS col
                FROM information_schema.COLUMNS c
                WHERE c.TABLE_SCHEMA = {schema}
                  AND c.TABLE_NAME = {table}
                ORDER BY c.ORDINAL_POSITION
            ) j
            ",
            schema = quote_literal(schema),
            table = quote_literal(table),
        )
    }

    pub(in crate::infra::adapters::mysql) fn preview_pk_columns_query(
        schema: &str,
        table: &str,
    ) -> String {
        format!(
            r"
            SELECT JSON_ARRAYAGG(pk.COLUMN_NAME) FROM (
                SELECT s.COLUMN_NAME
                FROM information_schema.STATISTICS s
                WHERE s.TABLE_SCHEMA = {schema}
                  AND s.TABLE_NAME = {table}
                  AND s.INDEX_NAME = 'PRIMARY'
                ORDER BY s.SEQ_IN_INDEX
            ) pk
            ",
            schema = quote_literal(schema),
            table = quote_literal(table),
        )
    }

    pub(in crate::infra::adapters::mysql) fn build_preview_query(
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
                .map(|col| quote_ident_mysql(col))
                .collect::<Vec<_>>()
                .join(", ");
            format!(" ORDER BY {cols}")
        };

        format!(
            "SELECT * FROM {}.{}{} LIMIT {} OFFSET {}",
            quote_ident_mysql(schema),
            quote_ident_mysql(table),
            order_clause,
            limit,
            offset
        )
    }

    pub(in crate::infra::adapters::mysql) fn indexes_query(schema: &str, table: &str) -> String {
        format!(
            r"
            SELECT JSON_ARRAYAGG(JSON_OBJECT(
                'name', idx.INDEX_NAME,
                'columns', idx.cols,
                'is_unique', IF(idx.NON_UNIQUE = 0, CAST('true' AS JSON), CAST('false' AS JSON)),
                'is_primary', IF(idx.INDEX_NAME = 'PRIMARY', CAST('true' AS JSON), CAST('false' AS JSON)),
                'index_type', idx.INDEX_TYPE,
                'definition', NULL
            ))
            FROM (
                SELECT
                    s.INDEX_NAME,
                    s.NON_UNIQUE,
                    s.INDEX_TYPE,
                    CAST(CONCAT('[', GROUP_CONCAT(JSON_QUOTE(s.COLUMN_NAME) ORDER BY s.SEQ_IN_INDEX SEPARATOR ','), ']') AS JSON) AS cols
                FROM information_schema.STATISTICS s
                WHERE s.TABLE_SCHEMA = {schema}
                  AND s.TABLE_NAME = {table}
                GROUP BY s.INDEX_NAME, s.NON_UNIQUE, s.INDEX_TYPE
                ORDER BY s.INDEX_NAME
            ) idx
            ",
            schema = quote_literal(schema),
            table = quote_literal(table),
        )
    }

    pub(in crate::infra::adapters::mysql) fn foreign_keys_query(
        schema: &str,
        table: &str,
    ) -> String {
        format!(
            r"
            SELECT JSON_ARRAYAGG(JSON_OBJECT(
                'name', fk.CONSTRAINT_NAME,
                'from_schema', fk.TABLE_SCHEMA,
                'from_table', fk.TABLE_NAME,
                'from_columns', fk.from_cols,
                'to_schema', fk.REFERENCED_TABLE_SCHEMA,
                'to_table', fk.REFERENCED_TABLE_NAME,
                'to_columns', fk.to_cols,
                'on_delete', fk.DELETE_RULE,
                'on_update', fk.UPDATE_RULE
            ))
            FROM (
                SELECT
                    kcu.CONSTRAINT_NAME,
                    kcu.TABLE_SCHEMA,
                    kcu.TABLE_NAME,
                    CAST(CONCAT('[', GROUP_CONCAT(JSON_QUOTE(kcu.COLUMN_NAME) ORDER BY kcu.ORDINAL_POSITION SEPARATOR ','), ']') AS JSON) AS from_cols,
                    kcu.REFERENCED_TABLE_SCHEMA,
                    kcu.REFERENCED_TABLE_NAME,
                    CAST(CONCAT('[', GROUP_CONCAT(JSON_QUOTE(kcu.REFERENCED_COLUMN_NAME) ORDER BY kcu.ORDINAL_POSITION SEPARATOR ','), ']') AS JSON) AS to_cols,
                    rc.DELETE_RULE,
                    rc.UPDATE_RULE
                FROM information_schema.KEY_COLUMN_USAGE kcu
                JOIN information_schema.REFERENTIAL_CONSTRAINTS rc
                  ON rc.CONSTRAINT_SCHEMA = kcu.CONSTRAINT_SCHEMA
                 AND rc.CONSTRAINT_NAME = kcu.CONSTRAINT_NAME
                WHERE kcu.TABLE_SCHEMA = {schema}
                  AND kcu.TABLE_NAME = {table}
                  AND kcu.REFERENCED_TABLE_NAME IS NOT NULL
                GROUP BY kcu.CONSTRAINT_NAME, kcu.TABLE_SCHEMA, kcu.TABLE_NAME,
                         kcu.REFERENCED_TABLE_SCHEMA, kcu.REFERENCED_TABLE_NAME,
                         rc.DELETE_RULE, rc.UPDATE_RULE
            ) fk
            ",
            schema = quote_literal(schema),
            table = quote_literal(table),
        )
    }

    pub(in crate::infra::adapters::mysql) fn triggers_query(schema: &str, table: &str) -> String {
        format!(
            r"
            SELECT JSON_ARRAYAGG(j.trigger_obj) FROM (
                SELECT JSON_OBJECT(
                    'name', t.TRIGGER_NAME,
                    'timing', t.ACTION_TIMING,
                    'events', JSON_ARRAY(t.EVENT_MANIPULATION),
                    'function_name', t.ACTION_STATEMENT,
                    'security_definer', IF(t.DEFINER IS NOT NULL, CAST('true' AS JSON), CAST('false' AS JSON))
                ) AS trigger_obj
                FROM information_schema.TRIGGERS t
                WHERE t.TRIGGER_SCHEMA = {schema}
                  AND t.EVENT_OBJECT_TABLE = {table}
                ORDER BY t.TRIGGER_NAME
            ) j
            ",
            schema = quote_literal(schema),
            table = quote_literal(table),
        )
    }

    pub(in crate::infra::adapters::mysql) fn table_info_query(schema: &str, table: &str) -> String {
        format!(
            r"
            SELECT JSON_OBJECT(
                'owner', NULL,
                'comment', NULLIF(t.TABLE_COMMENT, ''),
                'row_count_estimate', t.TABLE_ROWS
            )
            FROM information_schema.TABLES t
            WHERE t.TABLE_SCHEMA = {schema}
              AND t.TABLE_NAME = {table}
            ",
            schema = quote_literal(schema),
            table = quote_literal(table),
        )
    }

    pub(in crate::infra::adapters::mysql) fn table_columns_and_fks_query(
        schema: &str,
        table: &str,
    ) -> String {
        format!(
            r"
            SELECT JSON_OBJECT(
                'columns', ({columns}),
                'foreign_keys', ({fks})
            )
            ",
            columns = Self::columns_query(schema, table).trim(),
            fks = Self::foreign_keys_query(schema, table).trim(),
        )
    }

    pub(in crate::infra::adapters::mysql) fn table_detail_query(
        schema: &str,
        table: &str,
    ) -> String {
        format!(
            r"
            SELECT JSON_OBJECT(
                'columns', ({columns}),
                'indexes', ({indexes}),
                'foreign_keys', ({fks}),
                'triggers', ({triggers}),
                'table_info', ({table_info})
            )
            ",
            columns = Self::columns_query(schema, table).trim(),
            indexes = Self::indexes_query(schema, table).trim(),
            fks = Self::foreign_keys_query(schema, table).trim(),
            triggers = Self::triggers_query(schema, table).trim(),
            table_info = Self::table_info_query(schema, table).trim(),
        )
    }
}

#[cfg(test)]
mod tests {
    use crate::infra::adapters::mysql::MySqlAdapter;

    mod preview_query {
        use super::*;

        #[test]
        fn with_primary_key_columns_returns_ordered_preview_query() {
            let sql = MySqlAdapter::build_preview_query(
                "mydb",
                "users",
                &["id".to_string(), "tenant_id".to_string()],
                100,
                200,
            );

            assert_eq!(
                sql,
                "SELECT * FROM `mydb`.`users` ORDER BY `id`, `tenant_id` LIMIT 100 OFFSET 200"
            );
        }

        #[test]
        fn without_primary_key_columns_returns_unordered_preview_query() {
            let sql = MySqlAdapter::build_preview_query("mydb", "users", &[], 100, 0);

            assert_eq!(sql, "SELECT * FROM `mydb`.`users` LIMIT 100 OFFSET 0");
        }

        #[test]
        fn primary_key_query_contains_statistics_reference() {
            let sql = MySqlAdapter::preview_pk_columns_query("mydb", "users");

            assert!(sql.contains("information_schema.STATISTICS"));
            assert!(sql.contains("'mydb'"));
            assert!(sql.contains("'users'"));
            assert!(sql.contains("INDEX_NAME = 'PRIMARY'"));
        }
    }

    mod table_detail_query {
        use super::*;

        #[test]
        fn wraps_all_five_categories_in_json_object() {
            let sql = MySqlAdapter::table_detail_query("mydb", "users");

            assert!(sql.contains("JSON_OBJECT("));
            for key in [
                "'columns'",
                "'indexes'",
                "'foreign_keys'",
                "'triggers'",
                "'table_info'",
            ] {
                assert!(sql.contains(key), "Missing key: {key}");
            }
        }

        #[test]
        fn table_detail_uses_quoted_schema_and_table() {
            let sql = MySqlAdapter::table_detail_query("my_schema", "my_table");

            assert!(sql.contains("'my_schema'"));
            assert!(sql.contains("'my_table'"));
        }
    }

    mod table_columns_and_fks_query {
        use super::*;

        #[test]
        fn wraps_columns_and_fks_only_in_json_object() {
            let sql = MySqlAdapter::table_columns_and_fks_query("mydb", "users");

            assert!(sql.contains("JSON_OBJECT("));
            assert!(sql.contains("'columns'"));
            assert!(sql.contains("'foreign_keys'"));
            assert!(!sql.contains("'indexes'"));
            assert!(!sql.contains("'triggers'"));
            assert!(!sql.contains("'table_info'"));
        }
    }

    mod metadata_query_injection {
        use super::*;
        use rstest::rstest;

        const HOSTILE: &str = "'; DROP TABLE users; --";
        const ESCAPED: &str = "'''; DROP TABLE users; --'";

        #[rstest]
        #[case("columns_query", MySqlAdapter::columns_query(HOSTILE, "t"))]
        #[case("columns_query_table", MySqlAdapter::columns_query("mydb", HOSTILE))]
        #[case("indexes_query", MySqlAdapter::indexes_query(HOSTILE, "t"))]
        #[case("foreign_keys_query", MySqlAdapter::foreign_keys_query(HOSTILE, "t"))]
        #[case("triggers_query", MySqlAdapter::triggers_query(HOSTILE, "t"))]
        #[case("table_detail_query", MySqlAdapter::table_detail_query(HOSTILE, "t"))]
        #[case(
            "table_detail_query_table",
            MySqlAdapter::table_detail_query("mydb", HOSTILE)
        )]
        #[case(
            "table_columns_and_fks_query",
            MySqlAdapter::table_columns_and_fks_query(HOSTILE, "t")
        )]
        #[case(
            "table_columns_and_fks_query_table",
            MySqlAdapter::table_columns_and_fks_query("mydb", HOSTILE)
        )]
        fn hostile_input_is_escaped(#[case] _label: &str, #[case] sql: String) {
            assert!(
                sql.contains(ESCAPED),
                "Hostile input must be quote_literal-escaped in: {sql}"
            );
        }
    }

    mod all_databases_queries {
        use super::*;

        #[test]
        fn schemas_query_all_excludes_system_schemas() {
            let sql = MySqlAdapter::schemas_query_all();
            assert!(sql.contains("'mysql'"));
            assert!(sql.contains("'information_schema'"));
            assert!(sql.contains("'performance_schema'"));
            assert!(sql.contains("'sys'"));
            assert!(sql.contains("NOT IN"));
        }

        #[test]
        fn tables_query_all_uses_non_system_filter_instead_of_database_call() {
            let sql = MySqlAdapter::tables_query_all();
            assert!(!sql.contains("DATABASE()"));
            assert!(sql.contains("TABLE_SCHEMA NOT IN"));
        }

        #[test]
        fn table_signatures_query_all_uses_non_system_filter_instead_of_database_call() {
            let sql = MySqlAdapter::table_signatures_query_all();
            assert!(!sql.contains("t.TABLE_SCHEMA = DATABASE()"));
            assert!(sql.contains("TABLE_SCHEMA NOT IN"));
        }
    }

    mod preview_query_edge_cases {
        use super::*;

        #[test]
        fn schema_name_with_backtick_is_escaped() {
            let sql = MySqlAdapter::build_preview_query("my`db", "users", &[], 100, 0);

            assert_eq!(sql, "SELECT * FROM `my``db`.`users` LIMIT 100 OFFSET 0");
        }

        #[test]
        fn table_name_with_backtick_is_escaped() {
            let sql = MySqlAdapter::build_preview_query("mydb", "my`table", &[], 100, 0);

            assert_eq!(sql, "SELECT * FROM `mydb`.`my``table` LIMIT 100 OFFSET 0");
        }

        #[test]
        fn order_by_column_with_backtick_is_escaped() {
            let sql =
                MySqlAdapter::build_preview_query("mydb", "users", &["my`col".to_string()], 100, 0);

            assert_eq!(
                sql,
                "SELECT * FROM `mydb`.`users` ORDER BY `my``col` LIMIT 100 OFFSET 0"
            );
        }
    }
}
