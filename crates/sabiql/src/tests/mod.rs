mod adapter_mysql;
mod adapter_postgres;
pub mod harness;

use clap::Parser;

use super::{Args, Command};

#[cfg(not(feature = "self-update"))]
use super::self_update_disabled_message;

#[test]
fn no_subcommand_returns_none() {
    let args = Args::parse_from(["sabiql"]);
    assert!(args.command.is_none());
}

#[test]
fn update_subcommand_is_recognized() {
    let args = Args::parse_from(["sabiql", "update"]);
    assert!(args.database.is_none());
    assert!(matches!(args.command, Some(Command::Update)));
}

#[cfg(feature = "self-update")]
mod self_update_selection {
    use super::super::latest_stable_release;

    fn release(version: &str) -> self_update::update::Release {
        self_update::update::Release::builder()
            .version(version)
            .build()
            .unwrap()
    }

    #[test]
    fn selects_latest_stable_release_across_major_versions() {
        let releases = vec![release("1.15.1"), release("2.0.0-rc.1"), release("2.0.0")];

        let selected = latest_stable_release("1.14.0", &releases).unwrap();

        assert_eq!(selected.version(), "2.0.0");
    }
}

#[test]
fn database_positional_is_recognized() {
    let args = Args::parse_from(["sabiql", "/tmp/app.db"]);
    assert_eq!(args.database.as_deref(), Some("/tmp/app.db"));
    assert!(args.command.is_none());
}

mod cli_sqlite_startup {
    use std::fs;
    use std::path::Path;

    use sabiql_app::cmd::cli_sqlite::{activate_cli_sqlite_connection, resolve_cli_sqlite_target};
    use sabiql_app::model::app_state::AppState;
    use sabiql_app::ports::outbound::{AccessMode, QueryExecutor};
    use sabiql_infra::adapters::{FsSqlitePathValidator, SqliteAdapter};
    use tempfile::tempdir;

    #[test]
    fn resolves_existing_sqlite_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("app.db");
        fs::write(&path, b"SQLite format 3\0rest").unwrap();

        assert!(resolve_cli_sqlite_target(path.to_str().unwrap(), &FsSqlitePathValidator).is_ok());
    }

    #[test]
    fn resolves_extensionless_sqlite_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("History");
        fs::write(&path, b"SQLite format 3\0rest").unwrap();

        assert!(resolve_cli_sqlite_target(path.to_str().unwrap(), &FsSqlitePathValidator).is_ok());
    }

    #[test]
    fn rejects_non_sqlite_file_before_startup() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        fs::write(&path, b"not a sqlite database").unwrap();

        let error =
            resolve_cli_sqlite_target(path.to_str().unwrap(), &FsSqlitePathValidator).unwrap_err();

        assert!(error.to_string().contains("not a SQLite database"));
    }

    #[test]
    fn rejects_missing_file_before_startup() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("missing.db");

        let error =
            resolve_cli_sqlite_target(path.to_str().unwrap(), &FsSqlitePathValidator).unwrap_err();

        assert!(error.to_string().contains("not found"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn activation_pins_canonical_target_for_identity_preview_and_write() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::Builder::new()
            .prefix("sabiql-sab-351-")
            .tempdir_in(".")
            .unwrap();
        let database_a = dir.path().join("app-a.db");
        let database_b = dir.path().join("app-b.db");
        let alias = dir.path().join("current.db");
        fs::write(&database_a, b"").unwrap();
        fs::write(&database_b, b"").unwrap();
        let database_a = fs::canonicalize(database_a).unwrap();
        let database_b = fs::canonicalize(database_b).unwrap();

        let adapter = SqliteAdapter::new();
        for (path, value) in [(&database_a, "A"), (&database_b, "B")] {
            let dsn = format!("sqlite://{}", path.display());
            adapter
                .execute_adhoc(
                    &dsn,
                    "CREATE TABLE items (id INTEGER PRIMARY KEY, value TEXT)",
                    AccessMode::ReadWrite,
                )
                .await
                .unwrap();
            adapter
                .execute_adhoc(
                    &dsn,
                    &format!("INSERT INTO items VALUES (1, '{value}')"),
                    AccessMode::ReadWrite,
                )
                .await
                .unwrap();
        }

        symlink(&database_a, &alias).unwrap();
        let relative_database_a = database_a
            .strip_prefix(std::env::current_dir().unwrap())
            .unwrap()
            .to_path_buf();

        let activate = |input: &Path| {
            let target =
                resolve_cli_sqlite_target(input.to_str().unwrap(), &FsSqlitePathValidator).unwrap();
            let mut state = AppState::new("test".to_string());
            activate_cli_sqlite_connection(&mut state, &target, &FsSqlitePathValidator).unwrap();
            (
                state.session.active_connection_id().cloned().unwrap(),
                state.session.dsn().unwrap().to_owned(),
                state.session.active_connection_name().unwrap().to_owned(),
                state,
            )
        };

        let absolute = activate(&database_a);
        let relative = activate(&relative_database_a);
        let symlinked = activate(&alias);
        let canonical_a = database_a.to_str().unwrap();
        let expected_dsn = format!("sqlite://{canonical_a}");

        assert_eq!(absolute.0, relative.0);
        assert_eq!(absolute.0, symlinked.0);
        assert_eq!(absolute.1, expected_dsn);
        assert_eq!(relative.1, absolute.1);
        assert_eq!(symlinked.1, absolute.1);
        assert_eq!(symlinked.2, "current.db");

        fs::remove_file(&alias).unwrap();
        symlink(&database_b, &alias).unwrap();

        let preview = adapter
            .execute_preview(&symlinked.1, "main", "items", 10, 0)
            .await
            .unwrap();
        assert_eq!(preview.display_value_at(0, 1).as_deref(), Some("A"));

        let write = adapter
            .execute_write(
                &symlinked.1,
                "UPDATE items SET value = 'A updated' WHERE id = 1",
                AccessMode::ReadWrite,
            )
            .await
            .unwrap();
        assert_eq!(write.affected_rows, 1);

        let updated_a = adapter
            .execute_preview(&symlinked.1, "main", "items", 10, 0)
            .await
            .unwrap();
        assert_eq!(
            updated_a.display_value_at(0, 1).as_deref(),
            Some("A updated")
        );

        let database_b_dsn = format!("sqlite://{}", database_b.display());
        let unchanged_b = adapter
            .execute_preview(&database_b_dsn, "main", "items", 10, 0)
            .await
            .unwrap();
        assert_eq!(unchanged_b.display_value_at(0, 1).as_deref(), Some("B"));

        assert_eq!(symlinked.3.session.dsn(), Some(expected_dsn.as_str()));
    }
}

#[test]
#[cfg(not(feature = "self-update"))]
fn disabled_message_contains_version_and_upgrade_guidance() {
    let msg = self_update_disabled_message();
    assert!(msg.contains(env!("CARGO_PKG_VERSION")));
    assert!(msg.contains("brew upgrade sabiql"));
    assert!(msg.contains("cargo install sabiql"));
}

mod dispatch_overflow_fallback {
    use sabiql_app::model::app_state::AppState;
    use sabiql_app::update::action::Action;
    use tokio::sync::mpsc;

    use crate::dispatch_overflow_fallback;

    #[test]
    fn requeues_all_actions_and_reports_them_as_deferred() {
        let mut state = AppState::new("test".to_string());
        let (tx, mut rx) = mpsc::channel(8);

        dispatch_overflow_fallback(&mut state, &tx, vec![Action::Render, Action::Render]);

        assert!(rx.try_recv().is_ok());
        assert!(rx.try_recv().is_ok());
        let error = state.messages.last_error().unwrap();
        assert!(error.contains("2 actions deferred"), "got: {error}");
    }

    #[test]
    fn reports_dropped_count_when_channel_is_full() {
        let mut state = AppState::new("test".to_string());
        let (tx, _rx) = mpsc::channel(1);

        dispatch_overflow_fallback(
            &mut state,
            &tx,
            vec![Action::Render, Action::Render, Action::Render],
        );

        let error = state.messages.last_error().unwrap();
        assert!(error.contains("2 actions dropped"), "got: {error}");
    }
}

#[cfg(feature = "self-update")]
mod sha256_digest {
    use crate::{parse_sha256_digest, release_archive_name};

    const DIGEST: &str = "3ceb307a4d3f790abd375a49d74df9d46b99855f76dc790625abe6000775f82e";

    #[test]
    fn parses_sha256sum_style_content() {
        let content = format!("{DIGEST}  sabiql-x86_64-unknown-linux-gnu.tar.gz\n");
        assert_eq!(parse_sha256_digest(&content, "x.sha256").unwrap(), DIGEST);
    }

    #[test]
    fn parses_bare_digest_and_normalizes_case() {
        let content = format!("{}\n", DIGEST.to_uppercase());
        assert_eq!(parse_sha256_digest(&content, "x.sha256").unwrap(), DIGEST);
    }

    #[test]
    fn rejects_empty_content() {
        assert!(parse_sha256_digest("  \n", "x.sha256").is_err());
    }

    #[test]
    fn rejects_non_hex_or_wrong_length_digest() {
        assert!(parse_sha256_digest("nothex", "x.sha256").is_err());
        assert!(parse_sha256_digest(&DIGEST[..40], "x.sha256").is_err());
        let bad = format!("{}zz", &DIGEST[..62]);
        assert!(parse_sha256_digest(&bad, "x.sha256").is_err());
    }

    #[test]
    fn archive_name_matches_cargo_dist_convention() {
        let name = release_archive_name();
        let target = self_update::get_target();
        let extension = if cfg!(target_os = "windows") {
            "zip"
        } else {
            "tar.gz"
        };
        assert_eq!(name, format!("sabiql-{target}.{extension}"));
    }
}

#[cfg(feature = "self-update")]
mod mandatory_update_verification {
    use crate::{fetch_expected_sha256, verified_updater};
    use self_update::update::UpdateConfig;

    #[test]
    fn absent_checksum_asset_stops_before_downloading() {
        let release = self_update::Release::builder()
            .version("9.0.0")
            .build()
            .unwrap();
        let err = fetch_expected_sha256(&release, "sabiql-test.tar.gz").unwrap_err();
        assert!(
            err.to_string()
                .contains("refusing to update without verification")
        );
    }

    struct Response {
        headers: self_update::http_client::HeaderMap,
        body: Vec<u8>,
    }
    impl self_update::http_client::HttpResponse for Response {
        fn headers(&self) -> &self_update::http_client::HeaderMap {
            &self.headers
        }
        fn body(self: Box<Self>) -> Box<dyn std::io::Read> {
            Box::new(std::io::Cursor::new(self.body))
        }
    }
    #[derive(Default)]
    struct Client {
        urls: std::sync::Mutex<Vec<String>>,
    }
    impl self_update::http_client::HttpClient for Client {
        fn get(
            &self,
            url: &str,
            _: &self_update::http_client::HeaderMap,
            _: Option<std::time::Duration>,
        ) -> self_update::Result<Box<dyn self_update::http_client::HttpResponse>> {
            self.urls.lock().unwrap().push(url.to_string());
            let body = if url.ends_with("/releases/tags/v9.0.0") {
                br#"{"tag_name":"v9.0.0","name":"test","created_at":"2026-01-01T00:00:00Z","assets":[
                    {"name":"sabiql-test.tar.gz.sha256","url":"https://example.com/hash"},
                    {"name":"sabiql-test.tar.gz","url":"https://example.com/archive"},
                    {"name":"sabiql-test-installer.sh","url":"https://example.com/installer"}
                ]}"#.to_vec()
            } else {
                assert_eq!(url, "https://example.com/archive");
                b"untrusted payload".to_vec()
            };
            Ok(Box::new(Response {
                headers: self_update::http_client::HeaderMap::default(),
                body,
            }))
        }
    }

    #[test]
    fn mismatched_checksum_stops_before_extraction_and_installation() {
        let client = std::sync::Arc::new(Client::default());
        let updater = verified_updater(
            "1.0.0",
            "v9.0.0".into(),
            "sabiql-test.tar.gz".into(),
            "ab".repeat(32),
        )
        .http_client(client.clone())
        .build()
        .unwrap();
        assert_eq!(updater.release_tag(), Some("v9.0.0"));
        let error = updater.update().unwrap_err();
        assert!(
            matches!(error, self_update::errors::Error::ChecksumMismatch { .. }),
            "{error}"
        );
        assert_eq!(client.urls.lock().unwrap().len(), 2);
    }
}
