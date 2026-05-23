//! Integration tests for MySqlAdapter (Tier 2).
//!
//! All tests require a running MySQL instance and are marked `#[ignore]`.
//! Run with: `cargo test -- --ignored` or `mise run test:ignored`
//!
//! DSN is read from `SABIQL_MYSQL_TEST_DSN` env var.
//! Default: `mysql://root:root@localhost:3306/sabiql_test`

use sabiql_app::ports::outbound::QueryExecutor;
use sabiql_infra::adapters::mysql::MySqlAdapter;

fn test_dsn() -> String {
    std::env::var("SABIQL_MYSQL_TEST_DSN")
        .unwrap_or_else(|_| "mysql://root:root@localhost:3306/sabiql_test".to_string())
}

mod binary_and_newline_rendering {
    use super::*;

    #[tokio::test]
    #[ignore = "requires MySQL 8.0.19+ with --binary-as-hex support"]
    async fn binary_literal_renders_as_hex_string() {
        let adapter = MySqlAdapter::new();
        let dsn = test_dsn();

        let result = adapter
            .execute_adhoc(&dsn, "SELECT 0xDEADBEEF AS data", false)
            .await
            .unwrap();

        assert_eq!(result.columns, vec!["data"]);
        assert_eq!(result.rows.len(), 1);
        let cell = &result.rows[0][0];
        assert!(
            cell.starts_with("0x") && cell.to_uppercase().contains("DEADBEEF"),
            "binary literal should render as 0x... hex, got: {cell:?}"
        );
    }

    #[tokio::test]
    #[ignore = "requires MySQL"]
    async fn embedded_newline_keeps_single_row() {
        let adapter = MySqlAdapter::new();
        let dsn = test_dsn();

        // `CHAR(10)` is used instead of `'\n'` so the byte is produced
        // deterministically even when the server runs with
        // `NO_BACKSLASH_ESCAPES`. The parser must keep this as one row with
        // the newline preserved.
        let result = adapter
            .execute_adhoc(
                &dsn,
                "SELECT CONCAT('line1', CHAR(10), 'line2') AS note",
                false,
            )
            .await
            .unwrap();

        assert_eq!(result.columns, vec!["note"]);
        assert_eq!(
            result.rows.len(),
            1,
            "embedded newline must not split into multiple rows"
        );
        assert_eq!(result.rows[0], vec!["line1\nline2"]);
    }

    #[tokio::test]
    #[ignore = "requires MySQL"]
    async fn embedded_tab_keeps_single_field() {
        let adapter = MySqlAdapter::new();
        let dsn = test_dsn();

        let result = adapter
            .execute_adhoc(
                &dsn,
                "SELECT CONCAT('col', CHAR(9), 'a') AS note, 'x' AS sentinel",
                false,
            )
            .await
            .unwrap();

        assert_eq!(result.columns, vec!["note", "sentinel"]);
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0], vec!["col\ta", "x"]);
    }

    #[tokio::test]
    #[ignore = "requires MySQL 8.0.19+ with --binary-as-hex support"]
    async fn mixed_binary_and_newline_keeps_columns_aligned() {
        let adapter = MySqlAdapter::new();
        let dsn = test_dsn();

        let result = adapter
            .execute_adhoc(
                &dsn,
                "SELECT 1 AS id, CONCAT('a', CHAR(10), 'b') AS note, 0xCAFE AS blob_col \
                 UNION ALL SELECT 2, 'plain', 0xBABE",
                false,
            )
            .await
            .unwrap();

        assert_eq!(result.columns, vec!["id", "note", "blob_col"]);
        assert_eq!(result.rows.len(), 2);
        assert_eq!(result.rows[0][0], "1");
        assert_eq!(result.rows[0][1], "a\nb");
        assert!(
            result.rows[0][2].starts_with("0x"),
            "binary column should render as hex, got: {:?}",
            result.rows[0][2]
        );
        assert_eq!(result.rows[1][0], "2");
        assert_eq!(result.rows[1][1], "plain");
        assert!(result.rows[1][2].starts_with("0x"));
    }
}
