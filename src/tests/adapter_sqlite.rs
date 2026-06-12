//! Integration tests for SqliteAdapter (Tier 2).
//!
//! All tests require the `sqlite3` CLI (3.38+ for JSON functions) and are
//! marked `#[ignore]`. Run with: `cargo test -- --ignored`
//!
//! Each test creates its own temporary database file, so no server or
//! environment variable is needed.

use sabiql_app::ports::outbound::{MetadataProvider, QueryExecutor};
use sabiql_infra::adapters::sqlite::SqliteAdapter;

/// A temporary SQLite database seeded via the `sqlite3` CLI.
/// The file is removed on drop.
struct TempDb {
    path: std::path::PathBuf,
}

impl TempDb {
    fn create(seed_sql: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "sabiql_sqlite_it_{}_{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let status = std::process::Command::new("sqlite3")
            .arg(&path)
            .arg(seed_sql)
            .status()
            .expect("sqlite3 CLI must be installed");
        assert!(status.success(), "failed to seed temp database");
        Self { path }
    }

    fn dsn(&self) -> String {
        format!("sqlite://{}", self.path.display())
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

const SEED: &str = r#"
CREATE TABLE users (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    email TEXT
);
CREATE UNIQUE INDEX idx_users_email ON users(email);
CREATE TABLE orders (
    id INTEGER PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    note TEXT
);
CREATE TRIGGER trg_orders_audit AFTER INSERT ON orders BEGIN SELECT 1; END;
INSERT INTO users (id, name, email) VALUES (1, 'Alice', 'a@example.com');
INSERT INTO users (id, name, email) VALUES (2, 'Bob', 'b@example.com');
INSERT INTO orders (id, user_id, note) VALUES (10, 1, 'first');
"#;

mod metadata {
    use super::*;

    #[tokio::test]
    #[ignore = "requires sqlite3 CLI"]
    async fn fetch_metadata_lists_user_tables_under_main_schema() {
        let db = TempDb::create(SEED);
        let adapter = SqliteAdapter::new();

        let metadata = adapter.fetch_metadata(&db.dsn()).await.unwrap();

        assert_eq!(metadata.schemas.len(), 1);
        assert_eq!(metadata.schemas[0].name, "main");
        let names: Vec<&str> = metadata
            .table_summaries
            .iter()
            .map(|t| t.name.as_str())
            .collect();
        assert_eq!(names, vec!["orders", "users"]);
    }

    #[tokio::test]
    #[ignore = "requires sqlite3 CLI"]
    async fn fetch_table_detail_returns_columns_pk_fk_index_and_trigger() {
        let db = TempDb::create(SEED);
        let adapter = SqliteAdapter::new();

        let table = adapter
            .fetch_table_detail(&db.dsn(), "main", "orders")
            .await
            .unwrap();

        assert_eq!(table.schema, "main");
        assert_eq!(table.name, "orders");
        let col_names: Vec<&str> = table.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(col_names, vec!["id", "user_id", "note"]);
        assert_eq!(table.primary_key, Some(vec!["id".to_string()]));

        assert_eq!(table.foreign_keys.len(), 1);
        let fk = &table.foreign_keys[0];
        assert_eq!(fk.to_table, "users");
        assert_eq!(fk.from_columns, vec!["user_id"]);

        assert_eq!(table.triggers.len(), 1);
        assert_eq!(table.triggers[0].name, "trg_orders_audit");
    }

    #[tokio::test]
    #[ignore = "requires sqlite3 CLI"]
    async fn fetch_table_signatures_change_when_schema_changes() {
        let db = TempDb::create(SEED);
        let adapter = SqliteAdapter::new();

        let before = adapter.fetch_table_signatures(&db.dsn()).await.unwrap();
        adapter
            .execute_write(&db.dsn(), "ALTER TABLE users ADD COLUMN age INTEGER", false)
            .await
            .unwrap();
        let after = adapter.fetch_table_signatures(&db.dsn()).await.unwrap();

        let sig = |sigs: &[sabiql_domain::TableSignature]| {
            sigs.iter()
                .find(|s| s.name == "users")
                .unwrap()
                .signature
                .clone()
        };
        assert_ne!(sig(&before), sig(&after));
    }
}

mod query_execution {
    use super::*;

    #[tokio::test]
    #[ignore = "requires sqlite3 CLI"]
    async fn preview_orders_rows_by_primary_key() {
        let db = TempDb::create(SEED);
        let adapter = SqliteAdapter::new();

        let result = adapter
            .execute_preview(&db.dsn(), "main", "users", 10, 0, false)
            .await
            .unwrap();

        assert_eq!(result.columns, vec!["id", "name", "email"]);
        assert_eq!(result.rows.len(), 2);
        assert_eq!(result.rows[0][1], "Alice");
    }

    #[tokio::test]
    #[ignore = "requires sqlite3 CLI"]
    async fn embedded_newline_and_comma_keep_single_row() {
        let db = TempDb::create(SEED);
        let adapter = SqliteAdapter::new();

        let result = adapter
            .execute_adhoc(
                &db.dsn(),
                "SELECT 'line1' || char(10) || 'line2' AS note, 'a,b' AS csvish",
                false,
            )
            .await
            .unwrap();

        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0], vec!["line1\nline2", "a,b"]);
    }

    #[tokio::test]
    #[ignore = "requires sqlite3 CLI"]
    async fn sql_error_is_reported_as_error_result() {
        let db = TempDb::create(SEED);
        let adapter = SqliteAdapter::new();

        let result = adapter
            .execute_adhoc(&db.dsn(), "SELECT * FROM no_such_table", false)
            .await
            .unwrap();

        assert!(result.is_error());
    }

    #[tokio::test]
    #[ignore = "requires sqlite3 CLI"]
    async fn execute_write_returns_affected_rows_via_changes() {
        let db = TempDb::create(SEED);
        let adapter = SqliteAdapter::new();

        let result = adapter
            .execute_write(&db.dsn(), "UPDATE users SET name = 'X'", false)
            .await
            .unwrap();

        assert_eq!(result.affected_rows, 2);
    }

    #[tokio::test]
    #[ignore = "requires sqlite3 CLI"]
    async fn read_only_mode_rejects_writes() {
        let db = TempDb::create(SEED);
        let adapter = SqliteAdapter::new();

        let result = adapter
            .execute_write(&db.dsn(), "DELETE FROM users", true)
            .await;

        assert!(result.is_err(), "write must fail in read-only mode");
    }

    #[tokio::test]
    #[ignore = "requires sqlite3 CLI"]
    async fn count_query_rows_parses_count() {
        let db = TempDb::create(SEED);
        let adapter = SqliteAdapter::new();

        let count = adapter
            .count_query_rows(&db.dsn(), "SELECT COUNT(*) FROM users", false)
            .await
            .unwrap();

        assert_eq!(count, 2);
    }

    #[tokio::test]
    #[ignore = "requires sqlite3 CLI"]
    async fn export_to_csv_writes_file_with_header() {
        let db = TempDb::create(SEED);
        let adapter = SqliteAdapter::new();
        let out =
            std::env::temp_dir().join(format!("sabiql_sqlite_export_{}.csv", std::process::id()));

        let rows = adapter
            .export_to_csv(
                &db.dsn(),
                "SELECT id, name FROM users ORDER BY id",
                &out,
                false,
            )
            .await
            .unwrap();

        assert_eq!(rows, 2);
        let content = std::fs::read_to_string(&out).unwrap();
        assert!(content.starts_with("id,name"));
        let _ = std::fs::remove_file(&out);
    }

    #[tokio::test]
    #[ignore = "requires sqlite3 CLI"]
    async fn missing_database_file_fails_instead_of_creating_it() {
        let adapter = SqliteAdapter::new();
        let dsn = "sqlite:///tmp/sabiql_definitely_missing_db_file.db";

        let result = adapter.execute_adhoc(dsn, "SELECT 1", false).await;

        assert!(result.is_err());
        assert!(!std::path::Path::new("/tmp/sabiql_definitely_missing_db_file.db").exists());
    }
}
