use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DatabaseType {
    #[serde(rename = "postgresql", alias = "PostgreSQL")]
    #[default]
    PostgreSQL,
    #[serde(rename = "sqlite", alias = "SQLite")]
    SQLite,
    #[serde(rename = "mysql", alias = "MySQL")]
    MySQL,
}

impl DatabaseType {
    pub const fn all() -> &'static [Self] {
        &[Self::PostgreSQL, Self::SQLite, Self::MySQL]
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::PostgreSQL => "PostgreSQL",
            Self::SQLite => "SQLite",
            Self::MySQL => "MySQL",
        }
    }
}

impl fmt::Display for DatabaseType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case("PostgreSQL", "postgresql", DatabaseType::PostgreSQL)]
    #[case("MySQL", "mysql", DatabaseType::MySQL)]
    #[case("SQLite", "sqlite", DatabaseType::SQLite)]
    fn reads_fork_names_and_writes_canonical_names(
        #[case] legacy: &str,
        #[case] canonical: &str,
        #[case] database_type: DatabaseType,
    ) {
        for name in [legacy, canonical] {
            assert_eq!(
                serde_json::from_value::<DatabaseType>(serde_json::json!(name)).unwrap(),
                database_type,
            );
        }
        assert_eq!(serde_json::to_value(database_type).unwrap(), canonical);
    }
}
