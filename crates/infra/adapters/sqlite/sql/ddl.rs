use std::fmt::Write as _;

use super::quote_ident;
use crate::app::ports::outbound::DdlGenerator;
use crate::domain::Table;

use super::super::SqliteAdapter;

impl DdlGenerator for SqliteAdapter {
    fn generate_ddl(&self, table: &Table) -> String {
        // SQLite has no COMMENT syntax, so column/table comments are omitted.
        let mut ddl = format!(
            "CREATE TABLE {}.{} (\n",
            quote_ident(&table.schema),
            quote_ident(&table.name)
        );

        for (i, col) in table.columns.iter().enumerate() {
            let nullable = if col.is_nullable() { "" } else { " NOT NULL" };
            let default = col
                .default
                .as_ref()
                .map(|d| format!(" DEFAULT {d}"))
                .unwrap_or_default();

            let _ = write!(
                ddl,
                "  {} {}{}{}",
                quote_ident(&col.name),
                col.data_type,
                nullable,
                default,
            );

            if i + 1 < table.columns.len() || table.primary_key.is_some() {
                ddl.push(',');
            }
            ddl.push('\n');
        }

        if let Some(pk) = &table.primary_key {
            let quoted_cols: Vec<String> = pk.iter().map(|c| quote_ident(c)).collect();
            let _ = writeln!(ddl, "  PRIMARY KEY ({})", quoted_cols.join(", "));
        }

        ddl.push_str(");");

        ddl
    }
}

#[cfg(test)]
mod tests {
    use crate::adapters::sqlite::SqliteAdapter;
    use crate::app::ports::outbound::DdlGenerator;
    use crate::domain::{Column, ColumnAttributes, Table};

    fn make_column(name: &str, data_type: &str, nullable: bool) -> Column {
        Column {
            name: name.to_string(),
            data_type: data_type.to_string(),
            attributes: ColumnAttributes::from_parts(nullable, false, false),
            default: None,
            comment: None,
            ordinal_position: 0,
        }
    }

    fn make_table(columns: Vec<Column>, primary_key: Option<Vec<String>>) -> Table {
        Table {
            schema: "main".to_string(),
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
            let adapter = SqliteAdapter::new();
            let table = make_table(
                vec![
                    make_column("id", "INTEGER", false),
                    make_column("name", "TEXT", true),
                ],
                Some(vec!["id".to_string()]),
            );

            let ddl = adapter.generate_ddl(&table);

            assert!(ddl.contains("CREATE TABLE \"main\".\"test_table\""));
            assert!(ddl.contains("\"id\" INTEGER NOT NULL"));
            assert!(ddl.contains("\"name\" TEXT"));
            assert!(ddl.contains("PRIMARY KEY (\"id\")"));
            assert!(ddl.ends_with(");"));
        }

        #[test]
        fn comments_are_omitted() {
            let adapter = SqliteAdapter::new();
            let mut col = make_column("id", "INTEGER", false);
            col.comment = Some("Primary key".to_string());
            let mut table = make_table(vec![col], None);
            table.comment = Some("User accounts".to_string());

            let ddl = adapter.generate_ddl(&table);

            assert!(!ddl.contains("COMMENT"));
            assert!(!ddl.contains("Primary key"));
            assert!(!ddl.contains("User accounts"));
        }

        #[test]
        fn default_value_is_emitted_verbatim() {
            let adapter = SqliteAdapter::new();
            let mut col = make_column("created_at", "TEXT", true);
            col.default = Some("CURRENT_TIMESTAMP".to_string());
            let table = make_table(vec![col], None);

            let ddl = adapter.generate_ddl(&table);

            assert!(ddl.contains("\"created_at\" TEXT DEFAULT CURRENT_TIMESTAMP"));
        }

        #[test]
        fn default_ddl_line_count_matches_generated_ddl() {
            let adapter = SqliteAdapter::new();
            let table = make_table(vec![make_column("col", "TEXT", true)], None);

            let ddl = adapter.generate_ddl(&table);
            let count = adapter.ddl_line_count(&table);

            assert_eq!(count, ddl.lines().count());
        }
    }
}
