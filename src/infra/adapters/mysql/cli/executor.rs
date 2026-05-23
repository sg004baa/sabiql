use std::process::{ExitStatus, Stdio};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::time::timeout;

use crate::app::ports::outbound::DbOperationError;
use crate::domain::{QueryResult, QuerySource, WriteExecutionResult};

use super::super::MySqlAdapter;

struct DsnParts {
    host: String,
    port: u16,
    user: String,
    password: String,
    database: String,
}

fn parse_dsn(dsn: &str) -> Result<DsnParts, DbOperationError> {
    // mysql://user:pass@host:port/database
    let without_scheme = dsn
        .strip_prefix("mysql://")
        .ok_or_else(|| DbOperationError::ConnectionFailed("Invalid MySQL DSN scheme".into()))?;

    let (userinfo, hostpath) = without_scheme
        .split_once('@')
        .ok_or_else(|| DbOperationError::ConnectionFailed("Invalid MySQL DSN: missing @".into()))?;

    let (user_raw, pass_raw) = userinfo.split_once(':').unwrap_or((userinfo, ""));

    let (hostport, dbpath) = hostpath.split_once('/').unwrap_or((hostpath, ""));

    let database = dbpath.split('?').next().unwrap_or(dbpath);

    let (host, port_str) = hostport.rsplit_once(':').unwrap_or((hostport, "3306"));

    let port = port_str.parse::<u16>().unwrap_or(3306);

    let user = urlencoding::decode(user_raw)
        .map_err(|e| DbOperationError::ConnectionFailed(e.to_string()))?
        .into_owned();
    let password = urlencoding::decode(pass_raw)
        .map_err(|e| DbOperationError::ConnectionFailed(e.to_string()))?
        .into_owned();
    let database = urlencoding::decode(database)
        .map_err(|e| DbOperationError::ConnectionFailed(e.to_string()))?
        .into_owned();

    Ok(DsnParts {
        host: host.to_string(),
        port,
        user,
        password,
        database,
    })
}

struct MysqlOutput {
    status: ExitStatus,
    stdout: String,
    stderr: String,
}

impl MySqlAdapter {
    fn build_mysql_cmd(
        dsn: &str,
        extra_args: &[&str],
        query: &str,
        read_only: bool,
    ) -> Result<Command, DbOperationError> {
        let parts = parse_dsn(dsn)?;

        let mut cmd = Command::new("mysql");
        cmd.arg("-h")
            .arg(&parts.host)
            .arg("-P")
            .arg(parts.port.to_string())
            .arg("-u")
            .arg(&parts.user);

        // Empty database means "browse all schemas" — connect without binding
        // to a specific database so DATABASE() is NULL and SHOW DATABASES works.
        if !parts.database.is_empty() {
            cmd.arg("-D").arg(&parts.database);
        }

        // Pass password via environment variable to avoid it showing in process list
        cmd.env("MYSQL_PWD", &parts.password);

        if read_only {
            cmd.arg("--init-command=SET SESSION TRANSACTION READ ONLY");
        }

        for arg in extra_args {
            cmd.arg(arg);
        }

        cmd.arg("-e").arg(query);

        Ok(cmd)
    }

    async fn collect_output(
        cmd: &mut Command,
        timeout_secs: u64,
    ) -> Result<MysqlOutput, DbOperationError> {
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
        Ok(MysqlOutput {
            status,
            stdout,
            stderr,
        })
    }

    /// Execute a metadata query (returns raw JSON string from a SELECT that
    /// produces a single JSON value).
    pub(in crate::adapters::mysql) async fn execute_meta_query(
        &self,
        dsn: &str,
        query: &str,
    ) -> Result<String, DbOperationError> {
        let mut cmd = Self::build_mysql_cmd(
            dsn,
            &["--batch", "--raw", "--skip-column-names"],
            query,
            false,
        )?;

        let output = Self::collect_output(&mut cmd, self.timeout_secs).await?;

        if !output.status.success() {
            return Err(DbOperationError::QueryFailed(output.stderr));
        }

        Ok(output.stdout)
    }

    /// Execute a data query (preview or adhoc) returning tabular results.
    ///
    /// Runs `mysql --batch` (no `--raw`) so the client escapes embedded tabs,
    /// newlines, carriage returns, NUL, backslash, and SUB inside each field —
    /// preventing row/column misalignment. When the local client supports
    /// `--binary-as-hex` (MySQL 8.0.19+) it is added so `BINARY/VARBINARY/BLOB`
    /// values render as `0xABCD…` instead of corrupted UTF-8 bytes.
    pub(in crate::adapters::mysql) async fn execute_query_raw(
        &self,
        dsn: &str,
        query: &str,
        source: QuerySource,
        read_only: bool,
    ) -> Result<QueryResult, DbOperationError> {
        let start = Instant::now();

        let output = self
            .run_query_with_binary_as_hex_fallback(
                dsn,
                &["--batch", "--column-names"],
                query,
                read_only,
            )
            .await?;
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

        let (columns, rows) = parse_tsv_output(&output.stdout)?;
        Ok(QueryResult::success(
            query.to_string(),
            columns,
            rows,
            elapsed,
            source,
        ))
    }

    /// Run a data query with `--binary-as-hex` when supported, transparently
    /// falling back if the local mysql client rejects the option.
    async fn run_query_with_binary_as_hex_fallback(
        &self,
        dsn: &str,
        base_args: &[&str],
        query: &str,
        read_only: bool,
    ) -> Result<MysqlOutput, DbOperationError> {
        use std::sync::atomic::Ordering;

        if !self.binary_as_hex_disabled.load(Ordering::Relaxed) {
            let mut args: Vec<&str> = Vec::with_capacity(base_args.len() + 1);
            args.push("--binary-as-hex");
            args.extend_from_slice(base_args);
            let mut cmd = Self::build_mysql_cmd(dsn, &args, query, read_only)?;
            let output = Self::collect_output(&mut cmd, self.timeout_secs).await?;
            if output.status.success() || !looks_like_unknown_binary_as_hex(&output.stderr) {
                return Ok(output);
            }
            // One-shot fallback: client predates 8.0.19 (or is MariaDB).
            self.binary_as_hex_disabled.store(true, Ordering::Relaxed);
        }

        let mut cmd = Self::build_mysql_cmd(dsn, base_args, query, read_only)?;
        Self::collect_output(&mut cmd, self.timeout_secs).await
    }

    /// Execute a write query (UPDATE, DELETE, INSERT) and return affected rows.
    ///
    /// Appends `SELECT ROW_COUNT()` so the affected-row count is reliably
    /// returned via stdout, regardless of `--batch` or pipe-mode suppression
    /// of the "Query OK" info message.
    pub(in crate::adapters::mysql) async fn execute_write_raw(
        &self,
        dsn: &str,
        query: &str,
        read_only: bool,
    ) -> Result<WriteExecutionResult, DbOperationError> {
        let start = Instant::now();

        let wrapped = format!("{query}; SELECT ROW_COUNT()");
        let mut cmd = Self::build_mysql_cmd(
            dsn,
            &["--batch", "--skip-column-names"],
            &wrapped,
            read_only,
        )?;

        let output = Self::collect_output(&mut cmd, self.timeout_secs).await?;
        let elapsed = start.elapsed().as_millis() as u64;

        if !output.status.success() {
            return Err(DbOperationError::QueryFailed(
                output.stderr.trim().to_string(),
            ));
        }

        let affected_rows = output.stdout.trim().parse::<usize>().map_err(|_| {
            DbOperationError::QueryFailed("Failed to parse affected row count".to_string())
        })?;

        Ok(WriteExecutionResult {
            affected_rows,
            execution_time_ms: elapsed,
        })
    }

    pub(in crate::adapters::mysql) async fn count_rows(
        &self,
        dsn: &str,
        query: &str,
        read_only: bool,
    ) -> Result<usize, DbOperationError> {
        let mut cmd = Self::build_mysql_cmd(
            dsn,
            &["--batch", "--raw", "--skip-column-names"],
            query,
            read_only,
        )?;

        let output = Self::collect_output(&mut cmd, self.timeout_secs).await?;
        if !output.status.success() {
            return Err(DbOperationError::QueryFailed(output.stderr));
        }
        output.stdout.trim().parse::<usize>().map_err(|e| {
            DbOperationError::QueryFailed(format!("Failed to parse COUNT result: {e}"))
        })
    }

    pub(in crate::adapters::mysql) async fn export_csv_to_file(
        &self,
        dsn: &str,
        query: &str,
        path: &std::path::Path,
        read_only: bool,
    ) -> Result<usize, DbOperationError> {
        use std::sync::atomic::Ordering;

        // Mirrors `execute_query_raw`: drop `--raw` so embedded tabs/newlines
        // are escaped, and prefer `--binary-as-hex` when the local client
        // supports it. We reuse the adapter-wide `binary_as_hex_disabled`
        // flag rather than retrying inside the streaming write — in the
        // unlikely case the very first DB interaction is a CSV export against
        // an old client, the user sees a normal `unknown option` error and
        // the partial file is cleaned up below.
        let mut args: Vec<&str> = Vec::with_capacity(3);
        if !self.binary_as_hex_disabled.load(Ordering::Relaxed) {
            args.push("--binary-as-hex");
        }
        args.push("--batch");
        args.push("--column-names");
        let mut cmd = Self::build_mysql_cmd(dsn, &args, query, read_only)?;

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
            let mut newline_count: usize = 0;
            if let Some(mut out) = stdout {
                // Read tab-separated output and convert to CSV
                let mut buf = [0u8; 8192];
                let mut line_buf = Vec::new();
                let mut is_first_byte = true;

                loop {
                    let n = out.read(&mut buf).await?;
                    if n == 0 {
                        // Flush remaining line
                        if !line_buf.is_empty() {
                            let line = String::from_utf8_lossy(&line_buf);
                            let csv_line = tsv_line_to_csv(&line);
                            writer.write_all(csv_line.as_bytes()).await?;
                            writer.write_all(b"\n").await?;
                            newline_count += 1;
                        }
                        break;
                    }

                    for &byte in &buf[..n] {
                        if byte == b'\n' {
                            let line = String::from_utf8_lossy(&line_buf);
                            let csv_line = tsv_line_to_csv(&line);
                            writer.write_all(csv_line.as_bytes()).await?;
                            writer.write_all(b"\n").await?;
                            newline_count += 1;
                            line_buf.clear();
                            is_first_byte = true;
                        } else {
                            is_first_byte = false;
                            line_buf.push(byte);
                        }
                    }

                    let _ = is_first_byte; // suppress unused warning
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
            Ok::<_, std::io::Error>((status, stderr, newline_count))
        })
        .await;

        let result = match result {
            Ok(inner) => inner.map_err(|e| DbOperationError::QueryFailed(e.to_string()))?,
            Err(e) => {
                let _ = tokio::fs::remove_file(path).await;
                return Err(DbOperationError::Timeout(e.to_string()));
            }
        };

        let (status, stderr, newline_count) = result;
        if !status.success() {
            let _ = tokio::fs::remove_file(path).await;
            return Err(DbOperationError::QueryFailed(stderr.trim().to_string()));
        }

        // Subtract 1 for the header line
        let row_count = newline_count.saturating_sub(1);
        Ok(row_count)
    }

    pub(in crate::adapters::mysql) async fn fetch_preview_order_columns(
        &self,
        dsn: &str,
        schema: &str,
        table: &str,
    ) -> Result<Vec<String>, DbOperationError> {
        let query = Self::preview_pk_columns_query(schema, table);
        let raw = self.execute_meta_query(dsn, &query).await?;
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed == "null" || trimmed == "NULL" {
            return Ok(vec![]);
        }

        serde_json::from_str(trimmed)
            .map_err(|e| DbOperationError::InvalidJson(std::sync::Arc::new(e)))
    }
}

/// Parse the TSV stdout of `mysql --batch --column-names` into
/// `(columns, rows)`. Each field is unescaped to restore embedded
/// tabs/newlines/etc. that the client encoded.
///
/// CSV quoting is disabled because mysql does not quote fields; a bare `"`
/// inside a text value would otherwise be misinterpreted as a quote opener.
///
/// The terminator is pinned to `\n` because the mysql client emits a raw `\r`
/// byte for stored carriage returns instead of the `\r` escape form. With the
/// csv crate's default `CRLF` terminator, that lone `\r` would split a single
/// logical row in two.
fn parse_tsv_output(stdout: &str) -> Result<(Vec<String>, Vec<Vec<String>>), DbOperationError> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .delimiter(b'\t')
        .flexible(true)
        .quoting(false)
        .terminator(csv::Terminator::Any(b'\n'))
        .from_reader(stdout.as_bytes());

    let columns: Vec<String> = reader
        .headers()
        .map_err(|e| DbOperationError::QueryFailed(format!("TSV parse error: {e}")))?
        .iter()
        .map(unescape_mysql_tsv)
        .collect();

    let mut rows = Vec::new();
    for result in reader.records() {
        let record =
            result.map_err(|e| DbOperationError::QueryFailed(format!("TSV parse error: {e}")))?;
        let row: Vec<String> = record.iter().map(unescape_mysql_tsv).collect();
        rows.push(row);
    }

    Ok((columns, rows))
}

/// Decode the escape sequences that `mysql --batch` (non-`--raw`) emits inside
/// each TSV field.
///
/// Why: without `--raw`, the mysql client escapes any byte in a value that
/// would otherwise corrupt the TSV layout (tabs, newlines, carriage returns,
/// NUL, backslash, SUB). Reversing those escapes after parsing restores the
/// original bytes while keeping the row/column structure intact.
fn unescape_mysql_tsv(field: &str) -> String {
    let mut result = String::with_capacity(field.len());
    let mut chars = field.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            result.push(c);
            continue;
        }
        match chars.next() {
            Some('0') => result.push('\0'),
            Some('\\') => result.push('\\'),
            Some('n') => result.push('\n'),
            Some('r') => result.push('\r'),
            Some('t') => result.push('\t'),
            Some('Z') => result.push('\u{001A}'),
            Some(other) => {
                result.push('\\');
                result.push(other);
            }
            None => result.push('\\'),
        }
    }
    result
}

/// Whether the mysql client rejected `--binary-as-hex` because the local
/// binary predates 8.0.19 (or is MariaDB). Used to drive a one-shot fallback.
fn looks_like_unknown_binary_as_hex(stderr: &str) -> bool {
    let lower = stderr.to_lowercase();
    lower.contains("binary-as-hex") && (lower.contains("unknown") || lower.contains("unrecognized"))
}

/// Convert a TSV line to a CSV line (tab → comma, with proper quoting).
///
/// Fields are first unescaped from MySQL's `--batch` escape form so embedded
/// tabs/newlines round-trip into the CSV via RFC 4180 quoting instead of
/// staying as literal `\n` / `\t` text.
fn tsv_line_to_csv(line: &str) -> String {
    line.split('\t')
        .map(unescape_mysql_tsv)
        .map(|field| {
            if field.contains(',')
                || field.contains('"')
                || field.contains('\n')
                || field.contains('\r')
            {
                format!("\"{}\"", field.replace('"', "\"\""))
            } else {
                field
            }
        })
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    mod parse_dsn {
        use super::super::parse_dsn;

        #[test]
        fn standard_dsn_parses_all_fields() {
            let parts = parse_dsn("mysql://user:pass@localhost:3306/mydb").unwrap();
            assert_eq!(parts.host, "localhost");
            assert_eq!(parts.port, 3306);
            assert_eq!(parts.user, "user");
            assert_eq!(parts.password, "pass");
            assert_eq!(parts.database, "mydb");
        }

        #[test]
        fn encoded_credentials_are_decoded() {
            let parts =
                parse_dsn("mysql://user%40org:p%40ss%3Aword@localhost:3306/my%2Fdb").unwrap();
            assert_eq!(parts.user, "user@org");
            assert_eq!(parts.password, "p@ss:word");
            assert_eq!(parts.database, "my/db");
        }

        #[test]
        fn missing_port_defaults_to_3306() {
            let parts = parse_dsn("mysql://user:pass@localhost/mydb").unwrap();
            assert_eq!(parts.port, 3306);
        }

        #[test]
        fn query_params_are_stripped_from_database() {
            let parts = parse_dsn("mysql://user:pass@localhost:3306/mydb?charset=utf8").unwrap();
            assert_eq!(parts.database, "mydb");
        }

        #[test]
        fn invalid_scheme_returns_error() {
            assert!(parse_dsn("postgres://user:pass@localhost/mydb").is_err());
        }

        #[test]
        fn missing_database_path_yields_empty_database() {
            let parts = parse_dsn("mysql://user:pass@localhost:3306").unwrap();
            assert_eq!(parts.host, "localhost");
            assert_eq!(parts.port, 3306);
            assert_eq!(parts.database, "");
        }
    }

    mod parse_tsv_output {
        use super::super::parse_tsv_output;

        #[test]
        fn simple_two_column_table_parses() {
            let stdout = "id\tname\n1\tAlice\n2\tBob\n";
            let (cols, rows) = parse_tsv_output(stdout).unwrap();
            assert_eq!(cols, vec!["id", "name"]);
            assert_eq!(rows, vec![vec!["1", "Alice"], vec!["2", "Bob"]]);
        }

        #[test]
        fn embedded_newline_stays_in_single_row() {
            // Without `--raw`, mysql emits the newline inside the value as
            // the two chars `\n`. The TSV layout must report one row, and
            // the value must contain a real newline after unescape.
            let stdout = "id\tnote\n1\thello\\nworld\n";
            let (cols, rows) = parse_tsv_output(stdout).unwrap();
            assert_eq!(cols, vec!["id", "note"]);
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0], vec!["1", "hello\nworld"]);
        }

        #[test]
        fn embedded_tab_stays_in_single_field() {
            // Embedded tab encoded as `\t`; must not split the field.
            let stdout = "id\tnote\n1\tcol\\ta\n";
            let (cols, rows) = parse_tsv_output(stdout).unwrap();
            assert_eq!(cols, vec!["id", "note"]);
            assert_eq!(rows, vec![vec!["1", "col\ta"]]);
        }

        #[test]
        fn binary_as_hex_value_passes_through() {
            // With `--binary-as-hex`, BINARY values arrive as `0x…` literals.
            let stdout = "id\tdata\n1\t0xDEADBEEF\n";
            let (cols, rows) = parse_tsv_output(stdout).unwrap();
            assert_eq!(cols, vec!["id", "data"]);
            assert_eq!(rows, vec![vec!["1", "0xDEADBEEF"]]);
        }

        #[test]
        fn bare_double_quote_in_field_is_not_treated_as_quote_delimiter() {
            // mysql does not quote fields; the csv reader must not interpret
            // a stray `"` as opening a quoted region.
            let stdout = "id\tnote\n1\the said \"hi\"\n2\tplain\n";
            let (cols, rows) = parse_tsv_output(stdout).unwrap();
            assert_eq!(cols, vec!["id", "note"]);
            assert_eq!(rows, vec![vec!["1", "he said \"hi\""], vec!["2", "plain"]]);
        }

        #[test]
        fn newlines_and_binary_mixed_keeps_columns_aligned() {
            let stdout = "id\tnote\tblob\n1\tline1\\nline2\t0xCAFE\n2\tplain\t0xBABE\n";
            let (cols, rows) = parse_tsv_output(stdout).unwrap();
            assert_eq!(cols, vec!["id", "note", "blob"]);
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0], vec!["1", "line1\nline2", "0xCAFE"]);
            assert_eq!(rows[1], vec!["2", "plain", "0xBABE"]);
        }

        #[test]
        fn header_only_returns_no_rows() {
            let stdout = "id\tname\n";
            let (cols, rows) = parse_tsv_output(stdout).unwrap();
            assert_eq!(cols, vec!["id", "name"]);
            assert!(rows.is_empty());
        }

        #[test]
        fn raw_carriage_return_inside_field_does_not_split_row() {
            // mysql --batch leaks a stored `\r` as a raw byte rather than the
            // `\r` escape form. The TSV must still be one logical row; the
            // unescape pass is a no-op for raw `\r`, so it stays in the field.
            let stdout = "id\ttext\n15\tA\rB\n";
            let (cols, rows) = parse_tsv_output(stdout).unwrap();
            assert_eq!(cols, vec!["id", "text"]);
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0], vec!["15", "A\rB"]);
        }
    }

    mod unescape_mysql_tsv {
        use super::super::unescape_mysql_tsv;

        #[test]
        fn plain_text_passes_through_unchanged() {
            assert_eq!(unescape_mysql_tsv("hello world"), "hello world");
        }

        #[test]
        fn escaped_newline_becomes_lf() {
            assert_eq!(unescape_mysql_tsv("line1\\nline2"), "line1\nline2");
        }

        #[test]
        fn escaped_tab_becomes_tab() {
            assert_eq!(unescape_mysql_tsv("a\\tb"), "a\tb");
        }

        #[test]
        fn escaped_carriage_return_becomes_cr() {
            assert_eq!(unescape_mysql_tsv("a\\rb"), "a\rb");
        }

        #[test]
        fn escaped_null_becomes_nul_byte() {
            assert_eq!(unescape_mysql_tsv("a\\0b"), "a\0b");
        }

        #[test]
        fn double_backslash_becomes_single_backslash() {
            assert_eq!(unescape_mysql_tsv("a\\\\b"), "a\\b");
        }

        #[test]
        fn escaped_sub_becomes_ascii_1a() {
            assert_eq!(unescape_mysql_tsv("a\\Zb"), "a\u{001A}b");
        }

        #[test]
        fn unknown_escape_is_kept_literal() {
            // Unknown escape (e.g. \q) is preserved as-is so we never silently
            // drop bytes when MySQL adds a new escape sequence in the future.
            assert_eq!(unescape_mysql_tsv("a\\qb"), "a\\qb");
        }

        #[test]
        fn trailing_backslash_is_kept() {
            assert_eq!(unescape_mysql_tsv("abc\\"), "abc\\");
        }

        #[test]
        fn empty_input_returns_empty() {
            assert_eq!(unescape_mysql_tsv(""), "");
        }

        #[test]
        fn multiple_escapes_in_sequence_all_decode() {
            assert_eq!(unescape_mysql_tsv("\\n\\t\\\\\\0"), "\n\t\\\0");
        }
    }

    mod looks_like_unknown_binary_as_hex {
        use super::super::looks_like_unknown_binary_as_hex;

        #[test]
        fn unknown_option_message_matches() {
            assert!(looks_like_unknown_binary_as_hex(
                "mysql: unknown option '--binary-as-hex'"
            ));
        }

        #[test]
        fn unrecognized_option_message_matches() {
            assert!(looks_like_unknown_binary_as_hex(
                "mysql: unrecognized option '--binary-as-hex'"
            ));
        }

        #[test]
        fn case_insensitive_match() {
            assert!(looks_like_unknown_binary_as_hex(
                "MySQL: Unknown Option '--BINARY-AS-HEX'"
            ));
        }

        #[test]
        fn unrelated_error_does_not_match() {
            assert!(!looks_like_unknown_binary_as_hex(
                "ERROR 1146 (42S02): Table 'foo' doesn't exist"
            ));
        }

        #[test]
        fn unknown_about_another_option_does_not_match() {
            assert!(!looks_like_unknown_binary_as_hex(
                "mysql: unknown option '--some-other'"
            ));
        }
    }

    mod tsv_to_csv {
        use super::super::tsv_line_to_csv;

        #[test]
        fn simple_fields() {
            assert_eq!(tsv_line_to_csv("a\tb\tc"), "a,b,c");
        }

        #[test]
        fn field_with_comma_is_quoted() {
            assert_eq!(tsv_line_to_csv("hello, world\tb"), "\"hello, world\",b");
        }

        #[test]
        fn field_with_quote_is_escaped() {
            assert_eq!(tsv_line_to_csv("a\"b\tc"), "\"a\"\"b\",c");
        }

        #[test]
        fn single_field() {
            assert_eq!(tsv_line_to_csv("value"), "value");
        }

        #[test]
        fn escaped_newline_is_unescaped_then_csv_quoted() {
            // MySQL emits embedded newlines as the two chars `\n`; the CSV
            // output must restore the real newline and wrap the field in
            // quotes per RFC 4180.
            assert_eq!(tsv_line_to_csv("a\\nb\tc"), "\"a\nb\",c");
        }

        #[test]
        fn escaped_tab_is_unescaped_in_place() {
            assert_eq!(tsv_line_to_csv("a\\tb\tc"), "a\tb,c");
        }

        #[test]
        fn escaped_backslash_round_trips() {
            assert_eq!(tsv_line_to_csv("a\\\\b\tc"), "a\\b,c");
        }

        #[test]
        fn escaped_carriage_return_is_quoted_per_rfc4180() {
            // RFC 4180 requires CR-bearing fields to be quoted; otherwise a
            // strict reader (or Excel on Windows) would split the row.
            assert_eq!(tsv_line_to_csv("a\\rb\tc"), "\"a\rb\",c");
        }
    }
}
