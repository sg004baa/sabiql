use std::process::{ExitStatus, Stdio};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::time::timeout;

use crate::adapters::csv_record_counter::CsvRecordCounter;
use crate::app::ports::outbound::DbOperationError;
use crate::domain::{QueryResult, QuerySource, WriteExecutionResult};

use super::super::SqliteAdapter;

struct SqliteOutput {
    status: ExitStatus,
    stdout: String,
    stderr: String,
}

impl SqliteAdapter {
    /// sqlite3 args for tabular data fetches (preview/adhoc/CSV export): CSV
    /// output with NULLs rendered as the literal `NULL` sentinel so they stay
    /// distinguishable from empty strings, matching the mysql adapter and the
    /// `sql_literal_or_null` sentinel convention.
    const CSV_DATA_ARGS: &[&str] = &["-header", "-csv", "-nullvalue", "NULL"];

    fn db_path_from_dsn(dsn: &str) -> Result<&str, DbOperationError> {
        let path = Self::path_from_dsn(dsn).ok_or_else(|| {
            DbOperationError::ConnectionFailed("Invalid SQLite DSN scheme".into())
        })?;
        if path.is_empty() {
            return Err(DbOperationError::ConnectionFailed(
                "SQLite DSN has no file path".into(),
            ));
        }
        // sqlite3 silently creates a new empty database for unknown paths,
        // which would make a typo look like a successful connection.
        if !std::path::Path::new(path).exists() {
            return Err(DbOperationError::ConnectionFailed(format!(
                "SQLite database file not found: {path}"
            )));
        }
        Ok(path)
    }

    fn build_sqlite_cmd(
        dsn: &str,
        extra_args: &[&str],
        query: &str,
        read_only: bool,
    ) -> Result<Command, DbOperationError> {
        let path = Self::db_path_from_dsn(dsn)?;

        let mut cmd = Command::new("sqlite3");
        cmd.arg("-batch").arg("-bail");

        if read_only {
            cmd.arg("-readonly");
        }

        for arg in extra_args {
            cmd.arg(arg);
        }

        cmd.arg(path).arg(query);

        Ok(cmd)
    }

    async fn collect_output(
        cmd: &mut Command,
        timeout_secs: u64,
    ) -> Result<SqliteOutput, DbOperationError> {
        let mut child = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| DbOperationError::CommandNotFound(e.to_string()))?;

        let mut stdout_handle = child.stdout.take();
        let mut stderr_handle = child.stderr.take();

        let result = timeout(Duration::from_secs(timeout_secs), async {
            let (stdout_result, stderr_result) = tokio::join!(
                async {
                    let mut buf = Vec::new();
                    if let Some(ref mut out) = stdout_handle {
                        out.read_to_end(&mut buf).await?;
                    }
                    Ok::<_, std::io::Error>(String::from_utf8_lossy(&buf).into_owned())
                },
                async {
                    let mut buf = Vec::new();
                    if let Some(ref mut err) = stderr_handle {
                        err.read_to_end(&mut buf).await?;
                    }
                    Ok::<_, std::io::Error>(String::from_utf8_lossy(&buf).into_owned())
                }
            );

            let stdout = stdout_result?;
            let stderr = stderr_result?;
            let status = child.wait().await?;

            Ok::<_, std::io::Error>((status, stdout, stderr))
        })
        .await
        .map_err(|e| DbOperationError::Timeout(e.to_string()))?
        .map_err(|e| DbOperationError::QueryFailed(e.to_string()))?;

        let (status, stdout, stderr) = result;
        Ok(SqliteOutput {
            status,
            stdout,
            stderr,
        })
    }

    /// Execute a metadata query (returns the raw JSON string from a SELECT
    /// that produces a single JSON value).
    pub(in crate::adapters::sqlite) async fn execute_meta_query(
        &self,
        dsn: &str,
        query: &str,
    ) -> Result<String, DbOperationError> {
        let mut cmd = Self::build_sqlite_cmd(dsn, &["-noheader", "-list"], query, false)?;

        let output = Self::collect_output(&mut cmd, self.timeout_secs).await?;

        if !output.status.success() {
            return Err(DbOperationError::QueryFailed(output.stderr));
        }

        Ok(output.stdout)
    }

    /// Execute a data query (preview or adhoc) returning tabular results.
    ///
    /// Uses `sqlite3 -csv -header -nullvalue NULL`: the shell emits RFC 4180
    /// CSV (fields with commas, quotes, or newlines are quoted; rows end in
    /// CRLF), which the `csv` crate parses with its defaults. NULLs arrive as
    /// the bare `NULL` sentinel so they stay distinguishable from `""`.
    pub(in crate::adapters::sqlite) async fn execute_query_raw(
        &self,
        dsn: &str,
        query: &str,
        source: QuerySource,
        read_only: bool,
    ) -> Result<QueryResult, DbOperationError> {
        let start = Instant::now();

        let mut cmd = Self::build_sqlite_cmd(dsn, Self::CSV_DATA_ARGS, query, read_only)?;
        let output = Self::collect_output(&mut cmd, self.timeout_secs).await?;
        let elapsed = start.elapsed().as_millis() as u64;

        if !output.status.success() {
            return Ok(QueryResult::error(
                query.to_string(),
                output.stderr.trim().to_string(),
                elapsed,
                source,
            ));
        }

        if output.stdout.trim().is_empty() {
            return Ok(QueryResult::success(
                query.to_string(),
                Vec::new(),
                Vec::new(),
                elapsed,
                source,
            ));
        }

        let (columns, rows) = parse_csv_output(&output.stdout)?;
        Ok(QueryResult::success(
            query.to_string(),
            columns,
            rows,
            elapsed,
            source,
        ))
    }

    /// Execute a write query (UPDATE, DELETE, INSERT) and return affected rows.
    ///
    /// Appends `SELECT changes()` so the affected-row count is reliably
    /// returned via stdout (sqlite3 prints no status line for DML).
    pub(in crate::adapters::sqlite) async fn execute_write_raw(
        &self,
        dsn: &str,
        query: &str,
        read_only: bool,
    ) -> Result<WriteExecutionResult, DbOperationError> {
        let start = Instant::now();

        let wrapped = format!("{query}; SELECT changes();");
        let mut cmd = Self::build_sqlite_cmd(dsn, &["-noheader", "-list"], &wrapped, read_only)?;

        let output = Self::collect_output(&mut cmd, self.timeout_secs).await?;
        let elapsed = start.elapsed().as_millis() as u64;

        if !output.status.success() {
            return Err(DbOperationError::QueryFailed(
                output.stderr.trim().to_string(),
            ));
        }

        // The write statement may itself emit rows (e.g. RETURNING); the
        // changes() count is always the last line.
        let affected_rows = output
            .stdout
            .trim()
            .lines()
            .last()
            .and_then(|line| line.trim().parse::<usize>().ok())
            .ok_or_else(|| {
                DbOperationError::QueryFailed("Failed to parse affected row count".to_string())
            })?;

        Ok(WriteExecutionResult {
            affected_rows,
            execution_time_ms: elapsed,
        })
    }

    pub(in crate::adapters::sqlite) async fn count_rows(
        &self,
        dsn: &str,
        query: &str,
        read_only: bool,
    ) -> Result<usize, DbOperationError> {
        let mut cmd = Self::build_sqlite_cmd(dsn, &["-noheader", "-list"], query, read_only)?;

        let output = Self::collect_output(&mut cmd, self.timeout_secs).await?;
        if !output.status.success() {
            return Err(DbOperationError::QueryFailed(output.stderr));
        }
        output.stdout.trim().parse::<usize>().map_err(|e| {
            DbOperationError::QueryFailed(format!("Failed to parse COUNT result: {e}"))
        })
    }

    pub(in crate::adapters::sqlite) async fn export_csv_to_file(
        &self,
        dsn: &str,
        query: &str,
        path: &std::path::Path,
        read_only: bool,
    ) -> Result<usize, DbOperationError> {
        let mut cmd = Self::build_sqlite_cmd(dsn, Self::CSV_DATA_ARGS, query, read_only)?;

        let mut child = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| DbOperationError::CommandNotFound(e.to_string()))?;

        let stdout = child.stdout.take();
        let mut stderr_handle = child.stderr.take();

        let file = tokio::fs::File::create(path)
            .await
            .map_err(|e| DbOperationError::QueryFailed(format!("Failed to create file: {e}")))?;
        let mut writer = tokio::io::BufWriter::new(file);

        let result = timeout(Duration::from_secs(self.timeout_secs * 10), async {
            let mut counter = CsvRecordCounter::new();
            if let Some(mut out) = stdout {
                let mut buf = [0u8; 8192];
                loop {
                    let n = out.read(&mut buf).await?;
                    if n == 0 {
                        break;
                    }
                    counter.feed(&buf[..n]);
                    writer.write_all(&buf[..n]).await?;
                }
                writer.flush().await?;
            }

            let stderr = {
                let mut buf = Vec::new();
                if let Some(ref mut err) = stderr_handle {
                    err.read_to_end(&mut buf).await?;
                }
                String::from_utf8_lossy(&buf).into_owned()
            };

            let status = child.wait().await?;
            Ok::<_, std::io::Error>((status, stderr, counter.finish()))
        })
        .await;

        let result = match result {
            Ok(inner) => inner.map_err(|e| DbOperationError::QueryFailed(e.to_string()))?,
            Err(e) => {
                let _ = tokio::fs::remove_file(path).await;
                return Err(DbOperationError::Timeout(e.to_string()));
            }
        };

        let (status, stderr, record_count) = result;
        if !status.success() {
            let _ = tokio::fs::remove_file(path).await;
            return Err(DbOperationError::QueryFailed(stderr.trim().to_string()));
        }

        // Subtract 1 for the CSV header record
        let row_count = record_count.saturating_sub(1);
        Ok(row_count)
    }

    pub(in crate::adapters::sqlite) async fn fetch_preview_order_columns(
        &self,
        dsn: &str,
        table: &str,
    ) -> Result<Vec<String>, DbOperationError> {
        let query = Self::preview_pk_columns_query(table);
        let raw = self.execute_meta_query(dsn, &query).await?;
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed == "null" || trimmed == "NULL" {
            return Ok(vec![]);
        }

        serde_json::from_str(trimmed)
            .map_err(|e| DbOperationError::InvalidJson(std::sync::Arc::new(e)))
    }
}

/// Parse the CSV stdout of `sqlite3 -csv -header` into `(columns, rows)`.
fn parse_csv_output(stdout: &str) -> Result<(Vec<String>, Vec<Vec<String>>), DbOperationError> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(true)
        .from_reader(stdout.as_bytes());

    let columns: Vec<String> = reader
        .headers()
        .map_err(|e| DbOperationError::QueryFailed(format!("CSV parse error: {e}")))?
        .iter()
        .map(ToString::to_string)
        .collect();

    let mut rows = Vec::new();
    for result in reader.records() {
        let record =
            result.map_err(|e| DbOperationError::QueryFailed(format!("CSV parse error: {e}")))?;
        let row: Vec<String> = record.iter().map(ToString::to_string).collect();
        rows.push(row);
    }

    Ok((columns, rows))
}

#[cfg(test)]
mod tests {
    mod db_path_from_dsn {
        use crate::adapters::sqlite::SqliteAdapter;
        use crate::app::ports::outbound::DbOperationError;

        #[test]
        fn invalid_scheme_returns_connection_failed() {
            let result = SqliteAdapter::db_path_from_dsn("postgres://localhost/db");
            assert!(matches!(result, Err(DbOperationError::ConnectionFailed(_))));
        }

        #[test]
        fn empty_path_returns_connection_failed() {
            let result = SqliteAdapter::db_path_from_dsn("sqlite://");
            assert!(matches!(result, Err(DbOperationError::ConnectionFailed(_))));
        }

        #[test]
        fn missing_file_returns_connection_failed() {
            let result = SqliteAdapter::db_path_from_dsn("sqlite:///nonexistent/dir/missing.db");
            assert!(matches!(result, Err(DbOperationError::ConnectionFailed(_))));
        }

        #[test]
        fn existing_file_returns_path() {
            let tmp = std::env::temp_dir().join("sabiql_sqlite_dsn_test.db");
            std::fs::write(&tmp, b"").unwrap();
            let dsn = format!("sqlite://{}", tmp.display());

            let result = SqliteAdapter::db_path_from_dsn(&dsn);

            assert_eq!(result.unwrap(), tmp.to_str().unwrap());
            let _ = std::fs::remove_file(&tmp);
        }
    }

    mod parse_csv_output {
        use super::super::parse_csv_output;

        #[test]
        fn simple_two_column_table_parses() {
            let stdout = "id,name\r\n1,Alice\r\n2,Bob\r\n";
            let (cols, rows) = parse_csv_output(stdout).unwrap();
            assert_eq!(cols, vec!["id", "name"]);
            assert_eq!(rows, vec![vec!["1", "Alice"], vec!["2", "Bob"]]);
        }

        #[test]
        fn quoted_field_with_comma_stays_in_single_field() {
            let stdout = "id,note\r\n1,\"hello, world\"\r\n";
            let (_, rows) = parse_csv_output(stdout).unwrap();
            assert_eq!(rows, vec![vec!["1", "hello, world"]]);
        }

        #[test]
        fn quoted_field_with_newline_stays_in_single_row() {
            let stdout = "id,note\r\n1,\"line1\nline2\"\r\n";
            let (_, rows) = parse_csv_output(stdout).unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0], vec!["1", "line1\nline2"]);
        }

        #[test]
        fn embedded_double_quote_is_unescaped() {
            let stdout = "id,note\r\n1,\"he said \"\"hi\"\"\"\r\n";
            let (_, rows) = parse_csv_output(stdout).unwrap();
            assert_eq!(rows, vec![vec!["1", "he said \"hi\""]]);
        }

        #[test]
        fn header_only_returns_no_rows() {
            let stdout = "id,name\r\n";
            let (cols, rows) = parse_csv_output(stdout).unwrap();
            assert_eq!(cols, vec!["id", "name"]);
            assert!(rows.is_empty());
        }

        #[test]
        fn lf_only_line_endings_also_parse() {
            let stdout = "id,name\n1,Alice\n";
            let (cols, rows) = parse_csv_output(stdout).unwrap();
            assert_eq!(cols, vec!["id", "name"]);
            assert_eq!(rows, vec![vec!["1", "Alice"]]);
        }

        #[test]
        fn null_sentinel_distinguishes_null_from_empty_string() {
            // With `-nullvalue NULL`, sqlite3 emits SQL NULL as the bare
            // sentinel while an empty string stays an empty field.
            let stdout = "id,note\r\n1,NULL\r\n2,\"\"\r\n";
            let (_, rows) = parse_csv_output(stdout).unwrap();
            assert_eq!(rows[0], vec!["1", "NULL"]);
            assert_eq!(rows[1], vec!["2", ""]);
        }
    }

    mod data_query_args {
        use crate::adapters::sqlite::SqliteAdapter;

        #[test]
        fn null_rendered_as_sentinel_in_argv() {
            let file = tempfile::NamedTempFile::new().unwrap();
            let dsn = format!("sqlite://{}", file.path().display());

            let cmd = SqliteAdapter::build_sqlite_cmd(
                &dsn,
                SqliteAdapter::CSV_DATA_ARGS,
                "SELECT 1",
                false,
            )
            .unwrap();

            let args: Vec<String> = cmd
                .as_std()
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            let null_pos = args.iter().position(|arg| arg == "-nullvalue").unwrap();
            assert_eq!(args[null_pos + 1], "NULL");
            assert!(args.contains(&"-csv".to_string()));
        }
    }
}
