fn quote_ident_mysql(name: &str) -> String {
    format!("`{}`", name.replace('`', "``"))
}

fn quote_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

pub(in crate::adapters::mysql) mod ddl;
pub(in crate::adapters::mysql) mod dialect;
pub(in crate::adapters::mysql) mod query;

#[cfg(test)]
mod tests {
    use super::{quote_ident_mysql, quote_literal};

    #[test]
    fn quote_ident_mysql_simple() {
        assert_eq!(quote_ident_mysql("users"), "`users`");
    }

    #[test]
    fn quote_ident_mysql_with_backtick() {
        assert_eq!(quote_ident_mysql("user`name"), "`user``name`");
    }

    #[test]
    fn quote_ident_mysql_empty() {
        assert_eq!(quote_ident_mysql(""), "``");
    }

    #[test]
    fn quote_literal_escapes_embedded_quotes() {
        assert_eq!(quote_literal("O'Reilly"), "'O''Reilly'");
    }

    #[test]
    fn quote_literal_supports_empty_string() {
        assert_eq!(quote_literal(""), "''");
    }
}
