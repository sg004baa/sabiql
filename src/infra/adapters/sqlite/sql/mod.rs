fn quote_ident(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn quote_literal(value: &str) -> String {
    // SQLite never treats `\` as an escape character inside string literals,
    // so only embedded single quotes need doubling (PostgreSQL-style).
    format!("'{}'", value.replace('\'', "''"))
}

pub(in crate::adapters::sqlite) mod ddl;
pub(in crate::adapters::sqlite) mod dialect;
pub(in crate::adapters::sqlite) mod query;

#[cfg(test)]
mod tests {
    use super::{quote_ident, quote_literal};

    #[test]
    fn quote_ident_escapes_embedded_quotes() {
        assert_eq!(quote_ident(r#"user"name"#), r#""user""name""#);
    }

    #[test]
    fn quote_ident_supports_empty_string() {
        assert_eq!(quote_ident(""), "\"\"");
    }

    #[test]
    fn quote_literal_escapes_embedded_quotes() {
        assert_eq!(quote_literal("O'Reilly"), "'O''Reilly'");
    }

    #[test]
    fn quote_literal_supports_empty_string() {
        assert_eq!(quote_literal(""), "''");
    }

    #[test]
    fn quote_literal_keeps_backslashes_untouched() {
        assert_eq!(quote_literal(r"C:\temp\new"), r"'C:\temp\new'");
    }
}
