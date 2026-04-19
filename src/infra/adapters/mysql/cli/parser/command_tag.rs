use super::super::super::MySqlAdapter;

impl MySqlAdapter {
    /// Parse MySQL command output like "Query OK, N rows affected"
    /// and return the affected row count.
    pub(in crate::infra::adapters::mysql) fn parse_affected_rows(output: &str) -> Option<usize> {
        for line in output.lines() {
            let n = line
                .trim()
                .strip_prefix("Query OK, ")
                .and_then(|rest| rest.split_whitespace().next())
                .and_then(|n_str| n_str.parse::<usize>().ok());
            if n.is_some() {
                return n;
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use crate::infra::adapters::mysql::MySqlAdapter;

    mod parse_affected_rows {
        use super::*;

        #[test]
        fn update_one_row() {
            assert_eq!(
                MySqlAdapter::parse_affected_rows("Query OK, 1 row affected (0.01 sec)"),
                Some(1)
            );
        }

        #[test]
        fn update_multiple_rows() {
            assert_eq!(
                MySqlAdapter::parse_affected_rows("Query OK, 3 rows affected (0.02 sec)"),
                Some(3)
            );
        }

        #[test]
        fn zero_rows_affected() {
            assert_eq!(
                MySqlAdapter::parse_affected_rows("Query OK, 0 rows affected (0.00 sec)"),
                Some(0)
            );
        }

        #[test]
        fn large_number() {
            assert_eq!(
                MySqlAdapter::parse_affected_rows("Query OK, 1000000 rows affected (1.23 sec)"),
                Some(1_000_000)
            );
        }

        #[test]
        fn no_match_returns_none() {
            assert_eq!(MySqlAdapter::parse_affected_rows("ERROR 1045"), None);
            assert_eq!(MySqlAdapter::parse_affected_rows(""), None);
            assert_eq!(
                MySqlAdapter::parse_affected_rows("some random output"),
                None
            );
        }

        #[test]
        fn multiline_output_finds_query_ok() {
            let output = "Warning: something\nQuery OK, 5 rows affected (0.01 sec)\nRows matched: 5  Changed: 5  Warnings: 0";
            assert_eq!(MySqlAdapter::parse_affected_rows(output), Some(5));
        }
    }
}
