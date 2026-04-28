use crate::app::ports::DsnBuilder;
use crate::domain::connection::ConnectionProfile;

use super::MySqlAdapter;

impl MySqlAdapter {
    /// Display label used when no database is bound to the DSN.
    pub const ALL_DATABASES_LABEL: &'static str = "(all databases)";

    pub fn extract_database_name(dsn: &str) -> String {
        // mysql://user:pass@host:port[/dbname][?params]
        // No path → no database selected.
        let after_scheme = dsn.strip_prefix("mysql://").unwrap_or(dsn);
        let Some((_, path_and_query)) = after_scheme.split_once('/') else {
            return Self::ALL_DATABASES_LABEL.to_string();
        };
        let db = path_and_query.split('?').next().unwrap_or(path_and_query);
        if db.is_empty() {
            Self::ALL_DATABASES_LABEL.to_string()
        } else {
            db.to_string()
        }
    }

    /// Returns true when the DSN does not bind to a specific database
    /// (i.e. browsing all schemas the user can see).
    pub fn dsn_has_no_database(dsn: &str) -> bool {
        let after_scheme = dsn.strip_prefix("mysql://").unwrap_or(dsn);
        match after_scheme.split_once('/') {
            None => true,
            Some((_, rest)) => rest.split('?').next().unwrap_or(rest).is_empty(),
        }
    }
}

impl DsnBuilder for MySqlAdapter {
    fn build_dsn(&self, profile: &ConnectionProfile) -> String {
        let base = format!(
            "mysql://{}:{}@{}:{}",
            urlencoding::encode(&profile.username),
            urlencoding::encode(&profile.password),
            &profile.host,
            profile.port,
        );
        if profile.database.is_empty() {
            base
        } else {
            format!("{base}/{}", urlencoding::encode(&profile.database))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::connection::{DatabaseType, SslMode};

    fn make_test_profile() -> ConnectionProfile {
        ConnectionProfile::new(
            "Test Connection",
            "localhost",
            3306,
            "testdb",
            "testuser",
            "testpass",
            SslMode::Prefer,
            DatabaseType::MySQL,
        )
        .unwrap()
    }

    mod dsn_builder {
        use super::*;

        #[test]
        fn includes_all_connection_fields() {
            let adapter = MySqlAdapter::new();
            let profile = make_test_profile();
            let dsn = adapter.build_dsn(&profile);
            assert!(dsn.starts_with("mysql://"));
            assert!(dsn.contains("testuser"));
            assert!(dsn.contains("testpass"));
            assert!(dsn.contains("localhost"));
            assert!(dsn.contains("3306"));
            assert!(dsn.contains("testdb"));
        }

        #[test]
        fn encodes_special_chars_in_credentials() {
            let adapter = MySqlAdapter::new();
            let profile = ConnectionProfile::new(
                "Test",
                "localhost",
                3306,
                "my/db",
                "user@org",
                "p@ss:word",
                SslMode::Prefer,
                DatabaseType::MySQL,
            )
            .unwrap();
            let dsn = adapter.build_dsn(&profile);
            assert!(dsn.contains("user%40org"));
            assert!(dsn.contains("p%40ss%3Aword"));
            assert!(dsn.contains("my%2Fdb"));
        }
    }

    mod extract_database_name {
        use super::*;
        use rstest::rstest;

        #[rstest]
        #[case("mysql://user:pass@host:3306/mydb", "mydb")]
        #[case("mysql://localhost/testdb", "testdb")]
        #[case("mysql://user:pass@host:3306/mydb?charset=utf8", "mydb")]
        fn uri_path_returns_dbname(#[case] dsn: &str, #[case] expected: &str) {
            assert_eq!(MySqlAdapter::extract_database_name(dsn), expected);
        }

        #[test]
        fn empty_path() {
            assert_eq!(
                MySqlAdapter::extract_database_name("mysql://localhost/"),
                MySqlAdapter::ALL_DATABASES_LABEL,
            );
        }

        #[test]
        fn no_path_returns_all_databases_label() {
            assert_eq!(
                MySqlAdapter::extract_database_name("mysql://user:pass@host:3306"),
                MySqlAdapter::ALL_DATABASES_LABEL,
            );
        }

        #[test]
        fn roundtrip_build_then_extract_returns_original_dbname() {
            let adapter = MySqlAdapter::new();
            let profile = make_test_profile();
            let dsn = adapter.build_dsn(&profile);

            assert_eq!(MySqlAdapter::extract_database_name(&dsn), "testdb");
        }
    }

    mod build_dsn_empty_database {
        use super::*;

        #[test]
        fn empty_database_omits_path() {
            let adapter = MySqlAdapter::new();
            let profile = ConnectionProfile::new(
                "Test",
                "localhost",
                3306,
                "",
                "testuser",
                "testpass",
                SslMode::Prefer,
                DatabaseType::MySQL,
            )
            .unwrap();
            let dsn = adapter.build_dsn(&profile);

            assert_eq!(dsn, "mysql://testuser:testpass@localhost:3306");
        }

        #[test]
        fn dsn_has_no_database_detects_omitted_path() {
            assert!(MySqlAdapter::dsn_has_no_database(
                "mysql://user:pass@host:3306"
            ));
            assert!(MySqlAdapter::dsn_has_no_database(
                "mysql://user:pass@host:3306/"
            ));
            assert!(!MySqlAdapter::dsn_has_no_database(
                "mysql://user:pass@host:3306/mydb"
            ));
        }
    }
}
