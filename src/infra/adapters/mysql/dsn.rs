use crate::app::ports::DsnBuilder;
use crate::domain::connection::ConnectionProfile;

use super::MySqlAdapter;

impl MySqlAdapter {
    pub fn extract_database_name(dsn: &str) -> String {
        // mysql://user:pass@host:port/dbname?params
        if let Some(db) = dsn
            .rsplit('/')
            .next()
            .map(|s| s.split('?').next().unwrap_or(s))
            .filter(|s| !s.is_empty() && !s.contains('='))
        {
            return db.to_string();
        }
        "unknown".to_string()
    }
}

impl DsnBuilder for MySqlAdapter {
    fn build_dsn(&self, profile: &ConnectionProfile) -> String {
        format!(
            "mysql://{}:{}@{}:{}/{}",
            urlencoding::encode(&profile.username),
            urlencoding::encode(&profile.password),
            &profile.host,
            profile.port,
            urlencoding::encode(&profile.database),
        )
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
                "unknown"
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
}
