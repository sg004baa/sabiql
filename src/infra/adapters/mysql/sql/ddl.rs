use std::fmt::Write as _;

use crate::app::ports::DdlGenerator;
use crate::domain::Table;
use crate::infra::utils::{quote_ident_mysql, quote_literal};

use super::super::MySqlAdapter;

impl DdlGenerator for MySqlAdapter {
    fn generate_ddl(&self, table: &Table) -> String {
        let mut ddl = format!(
            "CREATE TABLE {}.{} (\n",
            quote_ident_mysql(&table.schema),
            quote_ident_mysql(&table.name)
        );

        for (i, col) in table.columns.iter().enumerate() {
            let nullable = if col.nullable { "" } else { " NOT NULL" };
            let default = col
                .default
                .as_ref()
                .map(|d| format!(" DEFAULT {d}"))
                .unwrap_or_default();

            let comment = col
                .comment
                .as_ref()
                .map(|c| format!(" COMMENT {}", quote_literal(c)))
                .unwrap_or_default();

            let _ = write!(
                ddl,
                "  {} {}{}{}{}",
                quote_ident_mysql(&col.name),
                col.data_type,
                nullable,
                default,
                comment,
            );

            if i + 1 < table.columns.len() || table.primary_key.is_some() {
                ddl.push(',');
            }
            ddl.push('\n');
        }

        if let Some(pk) = &table.primary_key {
            let quoted_cols: Vec<String> = pk.iter().map(|c| quote_ident_mysql(c)).collect();
            let _ = writeln!(ddl, "  PRIMARY KEY ({})", quoted_cols.join(", "));
        }

        ddl.push(')');

        if let Some(comment) = &table.comment {
            let _ = write!(ddl, " COMMENT={}", quote_literal(comment));
        }

        ddl.push(';');

        ddl
    }
}

#[cfg(test)]
mod tests {
    use crate::app::ports::DdlGenerator;
    use crate::domain::{Column, Table};
    use crate::infra::adapters::mysql::MySqlAdapter;

    fn make_column(name: &str, data_type: &str, nullable: bool) -> Column {
        Column {
            name: name.to_string(),
            data_type: data_type.to_string(),
            nullable,
            is_primary_key: false,
            default: None,
            is_unique: false,
            comment: None,
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

    mod ddl_generation {
        use super::*;

        #[test]
        fn table_with_pk_returns_valid_ddl() {
            let adapter = MySqlAdapter::new();
            let table = make_table(
                vec![
                    make_column("id", "int", false),
                    make_column("name", "varchar(255)", true),
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
            let mut table = make_table(vec![make_column("id", "int", false)], None);
            table.comment = Some("User accounts".to_string());

            let ddl = adapter.generate_ddl(&table);

            assert!(ddl.contains(") COMMENT='User accounts';"));
        }

        #[test]
        fn column_comment_inline() {
            let adapter = MySqlAdapter::new();
            let mut col = make_column("id", "int", false);
            col.comment = Some("Primary key".to_string());
            let table = make_table(vec![col], None);

            let ddl = adapter.generate_ddl(&table);

            assert!(ddl.contains("`id` int NOT NULL COMMENT 'Primary key'"));
        }

        #[test]
        fn single_quote_in_comment_is_escaped() {
            let adapter = MySqlAdapter::new();
            let mut table = make_table(vec![make_column("id", "int", false)], None);
            table.comment = Some("It's a test".to_string());

            let ddl = adapter.generate_ddl(&table);

            assert!(ddl.contains("COMMENT='It''s a test'"));
        }

        #[test]
        fn no_comment_when_absent() {
            let adapter = MySqlAdapter::new();
            let table = make_table(vec![make_column("id", "int", false)], None);

            let ddl = adapter.generate_ddl(&table);

            assert!(!ddl.contains("COMMENT"));
        }

        #[test]
        fn default_ddl_line_count_matches_generated_ddl() {
            let adapter = MySqlAdapter::new();
            let table = make_table(vec![make_column("col", "text", true)], None);

            let ddl = adapter.generate_ddl(&table);
            let count = adapter.ddl_line_count(&table);

            assert_eq!(count, ddl.lines().count());
        }
    }
}
