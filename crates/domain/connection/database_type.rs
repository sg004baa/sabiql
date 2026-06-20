use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DatabaseType {
    #[default]
    PostgreSQL,
    MySQL,
    SQLite,
}

impl fmt::Display for DatabaseType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PostgreSQL => write!(f, "PostgreSQL"),
            Self::MySQL => write!(f, "MySQL"),
            Self::SQLite => write!(f, "SQLite"),
        }
    }
}

impl DatabaseType {
    pub const ALL: &'static [Self] = &[Self::PostgreSQL, Self::MySQL, Self::SQLite];

    pub fn default_port(self) -> u16 {
        match self {
            Self::PostgreSQL => 5432,
            Self::MySQL => 3306,
            // SQLite is file-based; no network port. 0 = "not applicable".
            Self::SQLite => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_postgresql() {
        assert_eq!(DatabaseType::default(), DatabaseType::PostgreSQL);
    }

    #[test]
    fn display_formats_correctly() {
        assert_eq!(DatabaseType::PostgreSQL.to_string(), "PostgreSQL");
        assert_eq!(DatabaseType::MySQL.to_string(), "MySQL");
        assert_eq!(DatabaseType::SQLite.to_string(), "SQLite");
    }

    #[test]
    fn default_ports() {
        assert_eq!(DatabaseType::PostgreSQL.default_port(), 5432);
        assert_eq!(DatabaseType::MySQL.default_port(), 3306);
        assert_eq!(DatabaseType::SQLite.default_port(), 0);
    }

    #[test]
    fn all_contains_every_variant() {
        assert_eq!(DatabaseType::ALL.len(), 3);
    }

    #[test]
    fn serde_roundtrip() {
        let json = serde_json::to_string(&DatabaseType::MySQL).unwrap();
        let parsed: DatabaseType = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, DatabaseType::MySQL);
    }
}
