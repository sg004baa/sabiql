use std::process::{ExitStatus, Stdio};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::time::timeout;

use crate::app::ports::DbOperationError;
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
            .arg(&parts.user)
            .arg("-D")
            .arg(&parts.database);

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
    pub(in crate::infra::adapters::mysql) async fn execute_meta_query(
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
    /// mysql --batch outputs tab-separated values with column headers.
    pub(in crate::infra::adapters::mysql) async fn execute_query_raw(
        &self,
        dsn: &str,
        query: &str,
        source: QuerySource,
        read_only: bool,
    ) -> Result<QueryResult, DbOperationError> {
        let start = Instant::now();

        let mut cmd = Self::build_mysql_cmd(
            dsn,
            &["--batch", "--raw", "--column-names"],
            query,
            read_only,
        )?;

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

        // mysql --batch outputs tab-separated values
        let mut reader = csv::ReaderBuilder::new()
            .has_headers(true)
            .delimiter(b'\t')
            .flexible(true)
            .from_reader(output.stdout.as_bytes());

        let columns: Vec<String> = reader
            .headers()
            .map_err(|e| DbOperationError::QueryFailed(format!("TSV parse error: {e}")))?
            .iter()
            .map(ToString::to_string)
            .collect();

        let mut rows = Vec::new();
        for result in reader.records() {
            let record = result
                .map_err(|e| DbOperationError::QueryFailed(format!("TSV parse error: {e}")))?;
            let row: Vec<String> = record.iter().map(ToString::to_string).collect();
            rows.push(row);
        }

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
    /// Appends `SELECT ROW_COUNT()` so the affected-row count is reliably
    /// returned via stdout, regardless of `--batch` or pipe-mode suppression
    /// of the "Query OK" info message.
    pub(in crate::infra::adapters::mysql) async fn execute_write_raw(
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

        let affected_rows = output
            .stdout
            .trim()
            .parse::<usize>()
            .map_err(|_| {
                DbOperationError::QueryFailed(
                    "Failed to parse affected row count".to_string(),
                )
            })?;

        Ok(WriteExecutionResult {
            affected_rows,
            execution_time_ms: elapsed,
        })
    }

    pub(in crate::infra::adapters::mysql) async fn count_rows(
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

    pub(in crate::infra::adapters::mysql) async fn export_csv_to_file(
        &self,
        dsn: &str,
        query: &str,
        path: &std::path::Path,
        read_only: bool,
    ) -> Result<usize, DbOperationError> {
        let mut cmd = Self::build_mysql_cmd(
            dsn,
            &["--batch", "--raw", "--column-names"],
            query,
            read_only,
        )?;

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

    pub(in crate::infra::adapters::mysql) async fn fetch_preview_order_columns(
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

        serde_json::from_str(trimmed).map_err(|e| DbOperationError::InvalidJson(e.to_string()))
    }
}

/// Convert a TSV line to a CSV line (tab → comma, with proper quoting).
fn tsv_line_to_csv(line: &str) -> String {
    line.split('\t')
        .map(|field| {
            if field.contains(',') || field.contains('"') || field.contains('\n') {
                format!("\"{}\"", field.replace('"', "\"\""))
            } else {
                field.to_string()
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
    }
}
