use std::path::Path;

use crate::app::ports::outbound::DsnBuilder;
use crate::domain::connection::ConnectionProfile;

use super::SqliteAdapter;

pub(in crate::adapters::sqlite) const SQLITE_DSN_PREFIX: &str = "sqlite://";

/// Expand `~` to the home directory and absolutize relative paths against
/// the current working directory.
///
/// The form input is typed by hand, where shell conveniences like `~/` don't
/// exist; without this a `~/data/app.db` profile would fail the existence
/// check with a literal `~` directory. Paths that cannot be resolved
/// (no home dir / no cwd) are returned unchanged so the connect attempt
/// produces a normal "file not found" error instead of panicking.
fn normalize_path(path: &str, home: Option<&Path>, cwd: Option<&Path>) -> String {
    if path == "~" || path.starts_with("~/") {
        let Some(home) = home else {
            return path.to_string();
        };
        let Some(rest) = path.strip_prefix("~/") else {
            return home.display().to_string();
        };
        return home.join(rest).display().to_string();
    }

    if !path.is_empty() && Path::new(path).is_relative() {
        if let Some(cwd) = cwd {
            return cwd.join(path).display().to_string();
        }
    }

    path.to_string()
}

impl SqliteAdapter {
    /// Extract the database file path from a `sqlite://<path>` pseudo-DSN.
    /// The path is stored verbatim (no percent-encoding) so it round-trips
    /// arbitrary filesystem paths.
    pub fn path_from_dsn(dsn: &str) -> Option<&str> {
        dsn.strip_prefix(SQLITE_DSN_PREFIX)
    }

    /// Display name for the connection: the file name component of the path.
    pub fn extract_database_name(dsn: &str) -> String {
        let path = Self::path_from_dsn(dsn).unwrap_or(dsn);
        std::path::Path::new(path)
            .file_name()
            .map_or_else(|| path.to_string(), |f| f.to_string_lossy().into_owned())
    }
}

impl DsnBuilder for SqliteAdapter {
    fn build_dsn(&self, profile: &ConnectionProfile) -> String {
        // The form stores the database file path in `database`; host, port,
        // user, and password are meaningless for a file-based database.
        // The stored profile keeps the path as typed (`~/...` stays portable);
        // expansion happens here, on every connect.
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(std::path::PathBuf::from);
        let cwd = std::env::current_dir().ok();
        let path = normalize_path(&profile.database, home.as_deref(), cwd.as_deref());
        format!("{SQLITE_DSN_PREFIX}{path}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::connection::{DatabaseType, SslMode};

    fn make_test_profile(path: &str) -> ConnectionProfile {
        ConnectionProfile::new(
            "Test SQLite",
            "",
            0,
            path,
            "",
            "",
            SslMode::Prefer,
            DatabaseType::SQLite,
        )
        .unwrap()
    }

    mod dsn_builder {
        use super::*;

        #[test]
        fn wraps_file_path_in_sqlite_scheme() {
            let adapter = SqliteAdapter::new();
            let dsn = adapter.build_dsn(&make_test_profile("/home/me/app.db"));
            assert_eq!(dsn, "sqlite:///home/me/app.db");
        }

        #[test]
        fn path_with_spaces_is_kept_verbatim() {
            let adapter = SqliteAdapter::new();
            let dsn = adapter.build_dsn(&make_test_profile("/data/my db.sqlite"));
            assert_eq!(dsn, "sqlite:///data/my db.sqlite");
        }
    }

    mod normalize_path {
        use super::super::normalize_path;
        use std::path::Path;

        fn home() -> Option<&'static Path> {
            Some(Path::new("/home/me"))
        }

        fn cwd() -> Option<&'static Path> {
            Some(Path::new("/work/project"))
        }

        #[test]
        fn tilde_slash_expands_to_home() {
            assert_eq!(
                normalize_path("~/data/app.db", home(), cwd()),
                "/home/me/data/app.db"
            );
        }

        #[test]
        fn bare_tilde_expands_to_home() {
            assert_eq!(normalize_path("~", home(), cwd()), "/home/me");
        }

        #[test]
        fn relative_path_is_absolutized_against_cwd() {
            assert_eq!(
                normalize_path("data/app.db", home(), cwd()),
                "/work/project/data/app.db"
            );
        }

        #[test]
        fn absolute_path_is_unchanged() {
            assert_eq!(
                normalize_path("/var/db/app.db", home(), cwd()),
                "/var/db/app.db"
            );
        }

        #[test]
        fn tilde_without_home_is_unchanged() {
            assert_eq!(normalize_path("~/app.db", None, cwd()), "~/app.db");
        }

        #[test]
        fn relative_without_cwd_is_unchanged() {
            assert_eq!(normalize_path("app.db", home(), None), "app.db");
        }

        #[test]
        fn empty_path_is_unchanged() {
            assert_eq!(normalize_path("", home(), cwd()), "");
        }

        #[test]
        fn tilde_prefixed_file_name_is_not_expanded() {
            // `~backup.db` is a literal file name, not a home reference.
            assert_eq!(
                normalize_path("~backup.db", home(), cwd()),
                "/work/project/~backup.db"
            );
        }
    }

    mod path_from_dsn {
        use super::*;

        #[test]
        fn strips_scheme_prefix() {
            assert_eq!(
                SqliteAdapter::path_from_dsn("sqlite:///home/me/app.db"),
                Some("/home/me/app.db")
            );
        }

        #[test]
        fn non_sqlite_dsn_returns_none() {
            assert_eq!(
                SqliteAdapter::path_from_dsn("postgres://localhost/db"),
                None
            );
        }

        #[test]
        fn roundtrip_build_then_extract_returns_original_path() {
            let adapter = SqliteAdapter::new();
            let dsn = adapter.build_dsn(&make_test_profile("/tmp/x.db"));
            assert_eq!(SqliteAdapter::path_from_dsn(&dsn), Some("/tmp/x.db"));
        }
    }

    mod extract_database_name {
        use super::*;

        #[test]
        fn returns_file_name_component() {
            assert_eq!(
                SqliteAdapter::extract_database_name("sqlite:///home/me/app.db"),
                "app.db"
            );
        }

        #[test]
        fn bare_file_name_is_returned_as_is() {
            assert_eq!(
                SqliteAdapter::extract_database_name("sqlite://app.db"),
                "app.db"
            );
        }
    }
}
