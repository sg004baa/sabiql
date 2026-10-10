use crate::{DatabaseType, QueryValue, mysql_sql, postgres_sql, sqlite_sql};

type QuoteIdentifier = fn(&str) -> String;
type FormatValue = fn(&QueryValue) -> String;
type EqualityPredicate = fn(&str, &QueryValue) -> String;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerateSqlKind {
    Select,
    Insert,
    Update,
    Delete,
}

impl GenerateSqlKind {
    pub const ALL: [Self; 4] = [Self::Select, Self::Insert, Self::Update, Self::Delete];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Select => "SELECT",
            Self::Insert => "INSERT",
            Self::Update => "UPDATE",
            Self::Delete => "DELETE",
        }
    }
}

/// Builds editable SQL from typed result values without executing it.
pub fn generate_sql(
    database_type: DatabaseType,
    kind: GenerateSqlKind,
    schema: &str,
    table: &str,
    columns: &[String],
    rows: &[Vec<QueryValue>],
    identities: &[Vec<(String, QueryValue)>],
) -> Option<String> {
    if rows.is_empty() || columns.is_empty() || rows.iter().any(|row| row.len() != columns.len()) {
        return None;
    }
    if kind != GenerateSqlKind::Insert
        && (identities.len() != rows.len() || identities.iter().any(Vec::is_empty))
    {
        return None;
    }
    let (quote, literal, predicate): (QuoteIdentifier, FormatValue, EqualityPredicate) =
        match database_type {
            DatabaseType::PostgreSQL => (
                postgres_sql::quote_ident,
                postgres_sql::sql_literal,
                postgres_sql::equality_predicate,
            ),
            DatabaseType::MySQL => (
                mysql_sql::quote_identifier,
                mysql_sql::sql_literal,
                mysql_sql::equality_predicate,
            ),
            DatabaseType::SQLite => (
                sqlite_sql::quote_ident,
                sqlite_sql::sql_literal,
                sqlite_sql::equality_predicate,
            ),
        };
    let qualified = if database_type == DatabaseType::SQLite {
        quote(table)
    } else {
        format!("{}.{}", quote(schema), quote(table))
    };
    let column_list = columns
        .iter()
        .map(|column| quote(column))
        .collect::<Vec<_>>()
        .join(", ");
    let where_row = |pairs: &[(String, QueryValue)]| {
        pairs
            .iter()
            .map(|(column, value)| predicate(column, value))
            .collect::<Vec<_>>()
            .join(" AND ")
    };
    Some(match kind {
        GenerateSqlKind::Select => {
            let filter = identities
                .iter()
                .map(|pairs| format!("({})", where_row(pairs)))
                .collect::<Vec<_>>()
                .join(" OR ");
            format!("SELECT {column_list}\nFROM {qualified}\nWHERE {filter};")
        }
        GenerateSqlKind::Insert => {
            let values = rows
                .iter()
                .map(|row| {
                    format!(
                        "({})",
                        row.iter().map(literal).collect::<Vec<_>>().join(", ")
                    )
                })
                .collect::<Vec<_>>()
                .join(",\n");
            format!("INSERT INTO {qualified} ({column_list})\nVALUES {values};")
        }
        GenerateSqlKind::Update => {
            let mut statements = Vec::new();
            for (row, identity) in rows.iter().zip(identities) {
                let assignments = columns
                    .iter()
                    .zip(row)
                    .filter(|(column, _)| !identity.iter().any(|(key, _)| key == *column))
                    .map(|(column, value)| format!("{} = {}", quote(column), literal(value)))
                    .collect::<Vec<_>>();
                if assignments.is_empty() {
                    return None;
                }
                statements.push(format!(
                    "UPDATE {qualified}\nSET {}\nWHERE {};",
                    assignments.join(", "),
                    where_row(identity)
                ));
            }
            statements.join("\n")
        }
        GenerateSqlKind::Delete => match database_type {
            DatabaseType::PostgreSQL => {
                postgres_sql::build_bulk_delete_sql(schema, table, identities)
            }
            DatabaseType::MySQL => mysql_sql::build_bulk_delete_sql(schema, table, identities),
            DatabaseType::SQLite => sqlite_sql::build_bulk_delete_sql(table, identities),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_preserves_null_text_numbers_and_binary_values() {
        let columns = ["nothing", "text", "number", "bytes"].map(str::to_string);
        let rows = vec![vec![
            QueryValue::Null,
            QueryValue::text("NULL's\\value"),
            QueryValue::SqlLiteral("42".to_string()),
            QueryValue::Blob(vec![0, 255]),
        ]];
        for database_type in [
            DatabaseType::PostgreSQL,
            DatabaseType::MySQL,
            DatabaseType::SQLite,
        ] {
            let sql = generate_sql(
                database_type,
                GenerateSqlKind::Insert,
                "public",
                "users",
                &columns,
                &rows,
                &[],
            )
            .unwrap();
            assert!(sql.contains("NULL, "));
            assert!(sql.contains("NULL''s") || sql.contains("NULL\\'s"));
            assert!(sql.contains(", 42, "));
            assert!(sql.to_lowercase().contains("00ff"));
            assert!(!sql.contains("BLOB ("));
        }
    }

    #[test]
    fn update_targets_hidden_composite_key_and_skips_key_assignments() {
        let columns = ["id", "name"].map(str::to_string);
        let rows = vec![vec![
            QueryValue::SqlLiteral("9".to_string()),
            QueryValue::text("O'Reilly"),
        ]];
        let identities = vec![vec![
            ("id".to_string(), QueryValue::SqlLiteral("9".to_string())),
            ("tenant".to_string(), QueryValue::Null),
        ]];
        let sql = generate_sql(
            DatabaseType::PostgreSQL,
            GenerateSqlKind::Update,
            "pub\"lic",
            "users",
            &columns,
            &rows,
            &identities,
        )
        .unwrap();
        assert!(sql.contains("UPDATE \"pub\"\"lic\".\"users\""));
        assert!(sql.contains("SET \"name\" = 'O''Reilly'"));
        assert!(sql.contains("WHERE \"id\" = 9 AND \"tenant\" IS NULL"));
        assert!(!sql.contains("SET \"id\""));
    }

    #[test]
    fn destructive_templates_require_an_identity_for_every_row() {
        let columns = vec!["name".to_string()];
        let rows = vec![
            vec![QueryValue::text("alice")],
            vec![QueryValue::text("bob")],
        ];
        for kind in [
            GenerateSqlKind::Select,
            GenerateSqlKind::Update,
            GenerateSqlKind::Delete,
        ] {
            assert!(
                generate_sql(
                    DatabaseType::SQLite,
                    kind,
                    "main",
                    "users",
                    &columns,
                    &rows,
                    &[]
                )
                .is_none()
            );
            assert!(
                generate_sql(
                    DatabaseType::SQLite,
                    kind,
                    "main",
                    "users",
                    &columns,
                    &rows,
                    &[vec![("id".to_string(), QueryValue::text("1"))]]
                )
                .is_none()
            );
        }
    }

    #[test]
    fn select_combines_multiple_key_predicates() {
        let columns = vec!["name".to_string()];
        let rows = vec![
            vec![QueryValue::text("alice")],
            vec![QueryValue::text("bob")],
        ];
        let identities = vec![
            vec![("id".to_string(), QueryValue::text("1"))],
            vec![("id".to_string(), QueryValue::text("2"))],
        ];
        let sql = generate_sql(
            DatabaseType::SQLite,
            GenerateSqlKind::Select,
            "main",
            "users",
            &columns,
            &rows,
            &identities,
        )
        .unwrap();
        assert_eq!(
            sql,
            "SELECT \"name\"\nFROM \"users\"\nWHERE (\"id\" = '1') OR (\"id\" = '2');"
        );
    }
}
