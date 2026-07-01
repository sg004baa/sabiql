use std::fmt::Write as _;

use super::{quote_ident_mysql, quote_literal};
use crate::app::ports::outbound::DdlGenerator;
use crate::domain::Table;

use super::super::MySqlAdapter;

fn strip_prefix_ignore_ascii_case<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    let candidate = value.get(..prefix.len())?;
    candidate
        .eq_ignore_ascii_case(prefix)
        .then_some(&value[prefix.len()..])
}

fn strip_keyword_prefix_ignore_ascii_case<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    let remainder = strip_prefix_ignore_ascii_case(value, prefix)?;
    (remainder.is_empty() || remainder.chars().next().is_some_and(char::is_whitespace))
        .then_some(remainder)
}

fn normalize_column_extra(extra: &str) -> Option<String> {
    const DEFAULT_GENERATED: &str = "DEFAULT_GENERATED";

    let trimmed = extra.trim();
    let without_default_generated =
        strip_keyword_prefix_ignore_ascii_case(trimmed, DEFAULT_GENERATED)
            .map_or(trimmed, str::trim_start);

    if without_default_generated.is_empty() {
        return None;
    }

    if without_default_generated.eq_ignore_ascii_case("auto_increment") {
        return Some("AUTO_INCREMENT".to_string());
    }

    if let Some(remainder) =
        strip_keyword_prefix_ignore_ascii_case(without_default_generated, "on update")
    {
        let remainder = remainder.trim_start();
        return Some(if remainder.is_empty() {
            "ON UPDATE".to_string()
        } else {
            format!("ON UPDATE {remainder}")
        });
    }

    Some(without_default_generated.to_string())
}

impl DdlGenerator for MySqlAdapter {
    fn generate_ddl(&self, table: &Table) -> String {
        let mut interior_lines = Vec::new();

        for col in &table.columns {
            let nullable = if col.is_nullable() { "" } else { " NOT NULL" };
            let default = col
                .default
                .as_ref()
                .map(|d| format!(" DEFAULT {d}"))
                .unwrap_or_default();

            let extra = col
                .extra
                .as_deref()
                .and_then(normalize_column_extra)
                .map(|extra| format!(" {extra}"))
                .unwrap_or_default();

            let comment = col
                .comment
                .as_ref()
                .map(|c| format!(" COMMENT {}", quote_literal(c)))
                .unwrap_or_default();

            let mut line = String::new();
            let _ = write!(
                line,
                "  {} {}{}{}{}{}",
                quote_ident_mysql(&col.name),
                col.data_type,
                nullable,
                default,
                extra,
                comment,
            );
            interior_lines.push(line);
        }

        if let Some(pk) = &table.primary_key {
            let quoted_cols: Vec<String> = pk.iter().map(|c| quote_ident_mysql(c)).collect();
            interior_lines.push(format!("  PRIMARY KEY ({})", quoted_cols.join(", ")));
        }

        for index in table
            .indexes
            .iter()
            .filter(|index| index.is_unique() && !index.is_primary())
        {
            let quoted_cols = index
                .columns
                .iter()
                .map(|column| quote_ident_mysql(column))
                .collect::<Vec<_>>()
                .join(", ");
            interior_lines.push(format!(
                "  UNIQUE KEY {} ({quoted_cols})",
                quote_ident_mysql(&index.name)
            ));
        }

        let mut ddl = format!(
            "CREATE TABLE {}.{} (\n{}\n)",
            quote_ident_mysql(&table.schema),
            quote_ident_mysql(&table.name),
            interior_lines.join(",\n")
        );

        if let Some(comment) = &table.comment {
            let _ = write!(ddl, " COMMENT={}", quote_literal(comment));
        }

        ddl.push(';');

        ddl
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_column_extra;
    use crate::adapters::mysql::MySqlAdapter;
    use crate::app::ports::outbound::DdlGenerator;
    use crate::domain::{Column, ColumnAttributes, Index, IndexAttributes, IndexType, Table};

    fn make_column(name: &str, data_type: &str, nullable: bool, extra: Option<&str>) -> Column {
        Column {
            name: name.to_string(),
            data_type: data_type.to_string(),
            attributes: ColumnAttributes::from_parts(nullable, false, false),
            default: None,
            comment: None,
            extra: extra.map(ToString::to_string),
            ordinal_position: 0,
        }
    }

    fn make_table(columns: Vec<Column>, primary_key: Option<Vec<String>>) -> Table {
        Table {
            schema: "mydb".to_string(),
            name: "test_table".to_string(),
            owner: None,
            columns,
            primary_key,
            foreign_keys: vec![],
            indexes: vec![],
            rls: None,
            triggers: vec![],
            row_count_estimate: None,
            comment: None,
        }
    }

    mod column_extra_normalization {
        use super::*;

        #[test]
        fn strips_default_generated_prefix_case_insensitively() {
            assert_eq!(
                normalize_column_extra("default_generated  on update CURRENT_TIMESTAMP"),
                Some("ON UPDATE CURRENT_TIMESTAMP".to_string())
            );
            assert_eq!(normalize_column_extra("DEFAULT_GENERATED"), None);
        }

        #[test]
        fn normalizes_auto_increment_case_insensitively() {
            assert_eq!(
                normalize_column_extra("Auto_Increment"),
                Some("AUTO_INCREMENT".to_string())
            );
        }

        #[test]
        fn preserves_unrecognized_extra_text() {
            assert_eq!(
                normalize_column_extra("STORAGE DISK"),
                Some("STORAGE DISK".to_string())
            );
        }
    }

    mod ddl_generation {
        use super::*;

        #[test]
        fn table_with_pk_returns_valid_ddl() {
            let adapter = MySqlAdapter::new();
            let table = make_table(
                vec![
                    make_column("id", "int", false, None),
                    make_column("name", "varchar(255)", true, None),
                ],
                Some(vec!["id".to_string()]),
            );

            let ddl = adapter.generate_ddl(&table);

            assert!(ddl.contains("CREATE TABLE `mydb`.`test_table`"));
            assert!(ddl.contains("`id` int NOT NULL"));
            assert!(ddl.contains("`name` varchar(255)"));
            assert!(ddl.contains("PRIMARY KEY (`id`)"));
            assert!(ddl.ends_with(';'));
        }

        #[test]
        fn table_comment_inline_after_closing_paren() {
            let adapter = MySqlAdapter::new();
            let mut table = make_table(vec![make_column("id", "int", false, None)], None);
            table.comment = Some("User accounts".to_string());

            let ddl = adapter.generate_ddl(&table);

            assert!(ddl.contains(") COMMENT='User accounts';"));
        }

        #[test]
        fn column_comment_inline() {
            let adapter = MySqlAdapter::new();
            let mut col = make_column("id", "int", false, None);
            col.comment = Some("Primary key".to_string());
            let table = make_table(vec![col], None);

            let ddl = adapter.generate_ddl(&table);

            assert!(ddl.contains("`id` int NOT NULL COMMENT 'Primary key'"));
        }

        #[test]
        fn single_quote_in_comment_is_escaped() {
            let adapter = MySqlAdapter::new();
            let mut table = make_table(vec![make_column("id", "int", false, None)], None);
            table.comment = Some("It's a test".to_string());

            let ddl = adapter.generate_ddl(&table);

            assert!(ddl.contains("COMMENT='It''s a test'"));
        }

        #[test]
        fn no_comment_when_absent() {
            let adapter = MySqlAdapter::new();
            let table = make_table(vec![make_column("id", "int", false, None)], None);

            let ddl = adapter.generate_ddl(&table);

            assert!(!ddl.contains("COMMENT"));
        }

        #[test]
        fn default_ddl_line_count_matches_generated_ddl() {
            let adapter = MySqlAdapter::new();
            let table = make_table(vec![make_column("col", "text", true, None)], None);

            let ddl = adapter.generate_ddl(&table);
            let count = adapter.ddl_line_count(&table);

            assert_eq!(count, ddl.lines().count());
        }

        #[test]
        fn auto_increment_extra_is_emitted() {
            let adapter = MySqlAdapter::new();
            let table = make_table(
                vec![make_column(
                    "id",
                    "bigint unsigned",
                    false,
                    Some("auto_increment"),
                )],
                Some(vec!["id".to_string()]),
            );

            let ddl = adapter.generate_ddl(&table);

            assert!(ddl.contains("`id` bigint unsigned NOT NULL AUTO_INCREMENT"));
        }

        #[test]
        fn generated_on_update_extra_is_sanitized() {
            let adapter = MySqlAdapter::new();
            let mut column = make_column(
                "updated_at",
                "datetime",
                false,
                Some("DEFAULT_GENERATED on update CURRENT_TIMESTAMP"),
            );
            column.default = Some("CURRENT_TIMESTAMP".to_string());
            let table = make_table(vec![column], None);

            let ddl = adapter.generate_ddl(&table);

            assert!(ddl.contains(
                "`updated_at` datetime NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP"
            ));
            assert!(!ddl.contains("DEFAULT_GENERATED"));
        }

        #[test]
        fn unique_non_primary_index_is_emitted() {
            let adapter = MySqlAdapter::new();
            let mut table = make_table(
                vec![
                    make_column("id", "bigint", false, None),
                    make_column("api_key", "varchar(255)", false, None),
                ],
                Some(vec!["id".to_string()]),
            );
            table.indexes.push(Index {
                name: "uq_api_key".to_string(),
                columns: vec!["api_key".to_string()],
                attributes: IndexAttributes::from_parts(true, false),
                index_type: IndexType::BTree,
                definition: None,
            });

            let ddl = adapter.generate_ddl(&table);

            assert!(ddl.contains("UNIQUE KEY `uq_api_key` (`api_key`)"));
        }

        #[test]
        fn unique_key_has_no_trailing_comma_before_closing_paren() {
            let adapter = MySqlAdapter::new();
            let mut table = make_table(
                vec![make_column("api_key", "varchar(255)", false, None)],
                None,
            );
            table.indexes.push(Index {
                name: "uq_api_key".to_string(),
                columns: vec!["api_key".to_string()],
                attributes: IndexAttributes::from_parts(true, false),
                index_type: IndexType::BTree,
                definition: None,
            });

            let ddl = adapter.generate_ddl(&table);

            assert!(!ddl.contains(",\n)"));
            assert!(ddl.contains("UNIQUE KEY `uq_api_key` (`api_key`)\n);"));
        }
    }
}
