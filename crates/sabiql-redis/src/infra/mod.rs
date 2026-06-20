use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use tokio::process::Command;
use tokio::time::timeout;

use crate::domain::{RedisKey, RedisKind, RedisValue};

// Mirrors crates/app/model/browse/query_execution.rs::PREVIEW_PAGE_SIZE
// without depending on the RDB app crate.
pub const REDIS_VALUE_PREVIEW_LIMIT: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RedisCliError {
    #[error("redis-cli not found: {0}")]
    CommandNotFound(String),
    #[error("{0}")]
    CommandDenied(String),
    #[error("redis-cli failed: {0}")]
    CommandFailed(String),
    #[error("redis-cli timed out: {0}")]
    Timeout(String),
    #[error("failed to parse redis-cli output: {0}")]
    Parse(String),
    #[error("CSV export failed: {0}")]
    CsvExport(String),
}

#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait RedisCli: Send + Sync {
    async fn ping(&self) -> Result<(), RedisCliError>;
    async fn dbsize(&self) -> Result<usize, RedisCliError>;
    async fn scan_keys(&self) -> Result<Vec<RedisKey>, RedisCliError>;
    async fn key_type_and_ttl(&self, key: &str) -> Result<(RedisKind, Option<u64>), RedisCliError>;
    async fn fetch_value(&self, key: &str, kind: RedisKind) -> Result<RedisValue, RedisCliError>;
    async fn execute_command(&self, command: &str) -> Result<String, RedisCliError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedisDsn {
    pub host: String,
    pub port: u16,
    pub db: u32,
}

impl RedisDsn {
    pub fn parse(dsn: &str) -> Result<Self, RedisCliError> {
        let rest = dsn
            .trim()
            .strip_prefix("redis://")
            .ok_or_else(|| RedisCliError::Parse("DSN must start with redis://".to_string()))?;
        let (authority, db_part) = rest.split_once('/').unwrap_or((rest, ""));
        if authority.is_empty() {
            return Err(RedisCliError::Parse("Redis host is required".to_string()));
        }

        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) => {
                if host.is_empty() {
                    return Err(RedisCliError::Parse("Redis host is required".to_string()));
                }
                let parsed_port = port
                    .parse::<u16>()
                    .map_err(|_| RedisCliError::Parse(format!("invalid Redis port: {port}")))?;
                (host.to_string(), parsed_port)
            }
            None => (authority.to_string(), 6379),
        };

        let db = if db_part.is_empty() {
            0
        } else if db_part.contains('/') {
            return Err(RedisCliError::Parse(format!(
                "invalid Redis database path: /{db_part}"
            )));
        } else {
            db_part
                .parse::<u32>()
                .map_err(|_| RedisCliError::Parse(format!("invalid Redis database: {db_part}")))?
        };

        Ok(Self { host, port, db })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanPage {
    pub next_cursor: String,
    pub keys: Vec<String>,
}

pub fn parse_ping_reply(stdout: &str) -> Result<(), RedisCliError> {
    let reply = stdout.trim();
    if reply.eq_ignore_ascii_case("PONG") {
        Ok(())
    } else {
        Err(RedisCliError::Parse(format!(
            "expected PONG, got {reply:?}"
        )))
    }
}

pub fn parse_dbsize_reply(stdout: &str) -> Result<usize, RedisCliError> {
    let reply = stdout.trim();
    reply
        .parse::<usize>()
        .map_err(|_| RedisCliError::Parse(format!("invalid DBSIZE reply: {reply:?}")))
}

pub fn parse_scan_page(stdout: &str) -> Result<ScanPage, RedisCliError> {
    let mut lines = stdout.lines();
    let cursor = lines
        .next()
        .map(str::trim)
        .ok_or_else(|| RedisCliError::Parse("SCAN reply was empty".to_string()))?;
    if cursor.is_empty() || !cursor.bytes().all(|b| b.is_ascii_digit()) {
        return Err(RedisCliError::Parse(format!(
            "invalid SCAN cursor: {cursor:?}"
        )));
    }

    let keys = lines
        .map(|line| line.trim_end_matches('\r').to_string())
        .collect();
    Ok(ScanPage {
        next_cursor: cursor.to_string(),
        keys,
    })
}

pub fn scan_is_complete(page: &ScanPage) -> bool {
    page.next_cursor == "0"
}

pub fn ensure_command_allowed(command: &str) -> Result<(), RedisCliError> {
    let Some(first_token) = command.split_whitespace().next() else {
        return Err(RedisCliError::CommandDenied(
            "Enter a Redis command.".to_string(),
        ));
    };

    let upper = first_token.to_ascii_uppercase();
    match upper.as_str() {
        "FLUSHALL" | "FLUSHDB" | "DEL" | "UNLINK" => Err(RedisCliError::CommandDenied(format!(
            "{upper} is blocked by the destructive-command guard"
        ))),
        _ => Ok(()),
    }
}

fn command_args(command: &str) -> Vec<String> {
    // Whitespace splitting is intentionally simple for 2c. Quoted values with
    // spaces need a later parser slice.
    command
        .split_whitespace()
        .map(ToString::to_string)
        .collect()
}

pub fn serialize_csv(headers: &[String], rows: &[Vec<String>]) -> Result<Vec<u8>, RedisCliError> {
    let mut writer = csv::WriterBuilder::new()
        .terminator(csv::Terminator::Any(b'\n'))
        .from_writer(Vec::new());
    writer
        .write_record(headers)
        .map_err(|e| RedisCliError::CsvExport(e.to_string()))?;
    for row in rows {
        writer
            .write_record(row)
            .map_err(|e| RedisCliError::CsvExport(e.to_string()))?;
    }
    writer
        .into_inner()
        .map_err(|e| RedisCliError::CsvExport(e.to_string()))
}

pub fn write_csv_file(
    stem: &str,
    headers: &[String],
    rows: &[Vec<String>],
) -> Result<PathBuf, RedisCliError> {
    let dir = std::env::current_dir().map_err(|e| {
        RedisCliError::CsvExport(format!("failed to resolve current directory: {e}"))
    })?;
    let path = unique_csv_path(&dir, stem);
    let csv = serialize_csv(headers, rows)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|e| {
            RedisCliError::CsvExport(format!("failed to create {}: {e}", path.display()))
        })?;
    file.write_all(&csv).map_err(|e| {
        RedisCliError::CsvExport(format!("failed to write {}: {e}", path.display()))
    })?;
    Ok(path)
}

fn unique_csv_path(dir: &Path, stem: &str) -> PathBuf {
    let mut suffix = 0usize;
    loop {
        let filename = if suffix == 0 {
            format!("{stem}.csv")
        } else {
            format!("{stem}-{suffix}.csv")
        };
        let path = dir.join(filename);
        if !path.exists() {
            return path;
        }
        suffix = suffix
            .checked_add(1)
            .expect("exhausted usize suffix space for CSV export path");
    }
}

pub fn parse_type_reply(stdout: &str) -> Result<RedisKind, RedisCliError> {
    match stdout.trim() {
        "string" => Ok(RedisKind::String),
        "list" => Ok(RedisKind::List),
        "set" => Ok(RedisKind::Set),
        "hash" => Ok(RedisKind::Hash),
        "zset" => Ok(RedisKind::ZSet),
        "stream" => Ok(RedisKind::Stream),
        "none" => Ok(RedisKind::Unknown),
        other => Err(RedisCliError::Parse(format!(
            "invalid TYPE reply: {other:?}"
        ))),
    }
}

pub fn parse_ttl_reply(stdout: &str) -> Result<Option<u64>, RedisCliError> {
    let reply = stdout.trim();
    let ttl = reply
        .parse::<i64>()
        .map_err(|_| RedisCliError::Parse(format!("invalid TTL reply: {reply:?}")))?;
    if ttl < 0 {
        Ok(None)
    } else {
        Ok(Some(ttl as u64))
    }
}

pub fn parse_string_value(stdout: &str) -> Result<RedisValue, RedisCliError> {
    Ok(RedisValue::String(trim_command_newline(stdout).to_string()))
}

pub fn parse_list_value(stdout: &str, cap: usize) -> Result<RedisValue, RedisCliError> {
    Ok(RedisValue::List(capped_lines(stdout, cap)))
}

pub fn parse_set_value(stdout: &str, cap: usize) -> Result<RedisValue, RedisCliError> {
    Ok(RedisValue::Set(capped_lines(stdout, cap)))
}

pub fn parse_hash_value(stdout: &str, cap: usize) -> Result<RedisValue, RedisCliError> {
    parse_line_pairs(stdout, cap, "hash").map(RedisValue::Hash)
}

pub fn parse_zset_value(stdout: &str, cap: usize) -> Result<RedisValue, RedisCliError> {
    parse_line_pairs(stdout, cap, "zset").map(RedisValue::ZSet)
}

pub fn parse_stream_value(stdout: &str, cap: usize) -> Result<RedisValue, RedisCliError> {
    let mut rows = Vec::new();
    let mut current_id: Option<String> = None;
    let mut current_fields: Vec<(String, String)> = Vec::new();
    let mut pending_field: Option<String> = None;

    for line in stdout.lines().map(clean_line) {
        if is_stream_id(&line) {
            flush_stream_row(&mut rows, &mut current_id, &mut current_fields);
            if rows.len() >= cap {
                break;
            }
            current_id = Some(line);
            pending_field = None;
        } else if current_id.is_none() {
            return Err(RedisCliError::Parse(format!(
                "XRANGE reply started with non-id line: {line:?}"
            )));
        } else if let Some(field) = pending_field.take() {
            current_fields.push((field, line));
        } else {
            pending_field = Some(line);
        }
    }

    if let Some(field) = pending_field.take() {
        current_fields.push((field, String::new()));
    }
    flush_stream_row(&mut rows, &mut current_id, &mut current_fields);
    rows.truncate(cap);

    Ok(RedisValue::Stream(rows))
}

fn flush_stream_row(
    rows: &mut Vec<(String, String)>,
    current_id: &mut Option<String>,
    current_fields: &mut Vec<(String, String)>,
) {
    if let Some(id) = current_id.take() {
        let fields = current_fields
            .drain(..)
            .map(|(field, value)| {
                if value.is_empty() {
                    field
                } else {
                    format!("{field}={value}")
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        rows.push((id, fields));
    }
}

fn is_stream_id(line: &str) -> bool {
    let Some((ms, seq)) = line.split_once('-') else {
        return false;
    };
    !ms.is_empty()
        && !seq.is_empty()
        && ms.bytes().all(|b| b.is_ascii_digit())
        && seq.bytes().all(|b| b.is_ascii_digit())
}

fn trim_command_newline(stdout: &str) -> &str {
    stdout
        .strip_suffix("\r\n")
        .or_else(|| stdout.strip_suffix('\n'))
        .unwrap_or(stdout)
}

fn clean_line(line: &str) -> String {
    line.trim_end_matches('\r').to_string()
}

fn capped_lines(stdout: &str, cap: usize) -> Vec<String> {
    stdout.lines().take(cap).map(clean_line).collect()
}

fn parse_line_pairs(
    stdout: &str,
    cap: usize,
    label: &str,
) -> Result<Vec<(String, String)>, RedisCliError> {
    // Why not: redis-cli --raw line-pairing breaks when fields or values contain
    // embedded newlines. That is an accepted driver-less subprocess tradeoff for
    // this phase, similar to the RDB CSV parsing constraints.
    let lines = capped_lines(stdout, cap.saturating_mul(2));
    if !lines.len().is_multiple_of(2) {
        return Err(RedisCliError::Parse(format!(
            "{label} reply had an odd number of lines"
        )));
    }

    Ok(lines
        .chunks_exact(2)
        .map(|chunk| (chunk[0].clone(), chunk[1].clone()))
        .collect())
}

fn scan_values_body(stdout: &str) -> Result<String, RedisCliError> {
    let mut lines = stdout.lines();
    let cursor = lines
        .next()
        .map(str::trim)
        .ok_or_else(|| RedisCliError::Parse("SCAN-family reply was empty".to_string()))?;
    if cursor.is_empty() || !cursor.bytes().all(|b| b.is_ascii_digit()) {
        return Err(RedisCliError::Parse(format!(
            "invalid SCAN-family cursor: {cursor:?}"
        )));
    }
    Ok(lines.collect::<Vec<_>>().join("\n"))
}

#[derive(Debug, Clone)]
pub struct RedisCliSubprocess {
    dsn: RedisDsn,
    timeout: Duration,
}

impl RedisCliSubprocess {
    const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

    pub fn new(dsn: &str) -> Result<Self, RedisCliError> {
        Self::with_timeout(dsn, Self::DEFAULT_TIMEOUT)
    }

    pub fn with_timeout(dsn: &str, timeout: Duration) -> Result<Self, RedisCliError> {
        Ok(Self {
            dsn: RedisDsn::parse(dsn)?,
            timeout,
        })
    }

    async fn run_command(&self, args: &[String]) -> Result<String, RedisCliError> {
        let mut cmd = Command::new("redis-cli");
        cmd.kill_on_drop(true)
            .arg("-h")
            .arg(&self.dsn.host)
            .arg("-p")
            .arg(self.dsn.port.to_string())
            .arg("-n")
            .arg(self.dsn.db.to_string())
            .arg("--raw");
        for arg in args {
            cmd.arg(arg);
        }

        let output = timeout(self.timeout, cmd.output())
            .await
            .map_err(|e| RedisCliError::Timeout(e.to_string()))?
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    RedisCliError::CommandNotFound(e.to_string())
                } else {
                    RedisCliError::CommandFailed(e.to_string())
                }
            })?;

        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        if !output.status.success() {
            let message = if stderr.trim().is_empty() {
                stdout.trim().to_string()
            } else {
                stderr.trim().to_string()
            };
            return Err(RedisCliError::CommandFailed(message));
        }

        Ok(stdout)
    }
}

#[async_trait]
impl RedisCli for RedisCliSubprocess {
    async fn ping(&self) -> Result<(), RedisCliError> {
        let stdout = self.run_command(&["PING".to_string()]).await?;
        parse_ping_reply(&stdout)
    }

    async fn dbsize(&self) -> Result<usize, RedisCliError> {
        let stdout = self.run_command(&["DBSIZE".to_string()]).await?;
        parse_dbsize_reply(&stdout)
    }

    async fn scan_keys(&self) -> Result<Vec<RedisKey>, RedisCliError> {
        let mut cursor = "0".to_string();
        let mut keys = Vec::new();

        loop {
            let stdout = self
                .run_command(&[
                    "SCAN".to_string(),
                    cursor,
                    "COUNT".to_string(),
                    "1000".to_string(),
                ])
                .await?;
            let page = parse_scan_page(&stdout)?;
            let is_complete = scan_is_complete(&page);
            keys.extend(page.keys.into_iter().map(RedisKey::unknown));

            if is_complete {
                break;
            }
            cursor = page.next_cursor;
        }

        Ok(keys)
    }

    async fn key_type_and_ttl(&self, key: &str) -> Result<(RedisKind, Option<u64>), RedisCliError> {
        let kind = parse_type_reply(
            &self
                .run_command(&["TYPE".to_string(), key.to_string()])
                .await?,
        )?;
        let ttl = parse_ttl_reply(
            &self
                .run_command(&["TTL".to_string(), key.to_string()])
                .await?,
        )?;
        Ok((kind, ttl))
    }

    async fn fetch_value(&self, key: &str, kind: RedisKind) -> Result<RedisValue, RedisCliError> {
        let cap = REDIS_VALUE_PREVIEW_LIMIT;
        match kind {
            RedisKind::String => {
                let stdout = self
                    .run_command(&["GET".to_string(), key.to_string()])
                    .await?;
                parse_string_value(&stdout)
            }
            RedisKind::List => {
                let stdout = self
                    .run_command(&[
                        "LRANGE".to_string(),
                        key.to_string(),
                        "0".to_string(),
                        cap.saturating_sub(1).to_string(),
                    ])
                    .await?;
                parse_list_value(&stdout, cap)
            }
            RedisKind::Set => {
                let stdout = self
                    .run_command(&[
                        "SSCAN".to_string(),
                        key.to_string(),
                        "0".to_string(),
                        "COUNT".to_string(),
                        cap.to_string(),
                    ])
                    .await?;
                parse_set_value(&scan_values_body(&stdout)?, cap)
            }
            RedisKind::Hash => {
                let stdout = self
                    .run_command(&[
                        "HSCAN".to_string(),
                        key.to_string(),
                        "0".to_string(),
                        "COUNT".to_string(),
                        cap.to_string(),
                    ])
                    .await?;
                parse_hash_value(&scan_values_body(&stdout)?, cap)
            }
            RedisKind::ZSet => {
                let stdout = self
                    .run_command(&[
                        "ZRANGE".to_string(),
                        key.to_string(),
                        "0".to_string(),
                        cap.saturating_sub(1).to_string(),
                        "WITHSCORES".to_string(),
                    ])
                    .await?;
                parse_zset_value(&stdout, cap)
            }
            RedisKind::Stream => {
                let stdout = self
                    .run_command(&[
                        "XRANGE".to_string(),
                        key.to_string(),
                        "-".to_string(),
                        "+".to_string(),
                        "COUNT".to_string(),
                        cap.to_string(),
                    ])
                    .await?;
                parse_stream_value(&stdout, cap)
            }
            RedisKind::Unknown => Err(RedisCliError::Parse(format!(
                "unsupported Redis key type for {key:?}"
            ))),
        }
    }

    async fn execute_command(&self, command: &str) -> Result<String, RedisCliError> {
        ensure_command_allowed(command)?;
        self.run_command(&command_args(command)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_default_redis_url() {
        let dsn = RedisDsn::parse("redis://localhost").unwrap();

        assert_eq!(
            dsn,
            RedisDsn {
                host: "localhost".to_string(),
                port: 6379,
                db: 0,
            }
        );
    }

    #[test]
    fn parses_host_port_and_db() {
        let dsn = RedisDsn::parse("redis://redis.example.com:6380/2").unwrap();

        assert_eq!(
            dsn,
            RedisDsn {
                host: "redis.example.com".to_string(),
                port: 6380,
                db: 2,
            }
        );
    }

    #[test]
    fn accepts_pong_reply() {
        assert_eq!(parse_ping_reply("PONG\n"), Ok(()));
    }

    #[test]
    fn rejects_non_pong_reply() {
        let err = parse_ping_reply("NOAUTH Authentication required.\n").unwrap_err();

        assert!(matches!(err, RedisCliError::Parse(_)));
    }

    #[test]
    fn parses_dbsize_reply() {
        assert_eq!(parse_dbsize_reply("42\n"), Ok(42));
    }

    #[test]
    fn parses_scan_page_with_cursor_and_keys() {
        let page = parse_scan_page("17\nsession:1\nuser:2\n").unwrap();

        assert_eq!(
            page,
            ScanPage {
                next_cursor: "17".to_string(),
                keys: vec!["session:1".to_string(), "user:2".to_string()],
            }
        );
        assert!(!scan_is_complete(&page));
    }

    #[test]
    fn parses_terminal_scan_page() {
        let page = parse_scan_page("0\nsettings\n").unwrap();

        assert_eq!(page.next_cursor, "0");
        assert_eq!(page.keys, vec!["settings".to_string()]);
        assert!(scan_is_complete(&page));
    }

    #[test]
    fn parses_type_replies() {
        assert_eq!(parse_type_reply("string\n"), Ok(RedisKind::String));
        assert_eq!(parse_type_reply("list\n"), Ok(RedisKind::List));
        assert_eq!(parse_type_reply("set\n"), Ok(RedisKind::Set));
        assert_eq!(parse_type_reply("hash\n"), Ok(RedisKind::Hash));
        assert_eq!(parse_type_reply("zset\n"), Ok(RedisKind::ZSet));
        assert_eq!(parse_type_reply("stream\n"), Ok(RedisKind::Stream));
        assert_eq!(parse_type_reply("none\n"), Ok(RedisKind::Unknown));
    }

    #[test]
    fn parses_ttl_replies() {
        assert_eq!(parse_ttl_reply("-1\n"), Ok(None));
        assert_eq!(parse_ttl_reply("-2\n"), Ok(None));
        assert_eq!(parse_ttl_reply("120\n"), Ok(Some(120)));
    }

    #[test]
    fn parses_string_value() {
        assert_eq!(
            parse_string_value("hello\n"),
            Ok(RedisValue::String("hello".to_string()))
        );
    }

    #[test]
    fn parses_list_value_with_cap() {
        assert_eq!(
            parse_list_value("a\nb\nc\n", 2),
            Ok(RedisValue::List(vec!["a".to_string(), "b".to_string()]))
        );
    }

    #[test]
    fn parses_set_value_with_cap() {
        assert_eq!(
            parse_set_value("a\nb\nc\n", 2),
            Ok(RedisValue::Set(vec!["a".to_string(), "b".to_string()]))
        );
    }

    #[test]
    fn parses_hash_value_from_flat_field_value_lines() {
        assert_eq!(
            parse_hash_value("field1\nvalue1\nfield2\nvalue2\n", 500),
            Ok(RedisValue::Hash(vec![
                ("field1".to_string(), "value1".to_string()),
                ("field2".to_string(), "value2".to_string()),
            ]))
        );
    }

    #[test]
    fn rejects_odd_hash_field_value_lines() {
        assert!(matches!(
            parse_hash_value("field1\nvalue1\nfield2\n", 500),
            Err(RedisCliError::Parse(_))
        ));
    }

    #[test]
    fn parses_zset_value_from_flat_member_score_lines() {
        assert_eq!(
            parse_zset_value("member1\n1\nmember2\n2.5\n", 500),
            Ok(RedisValue::ZSet(vec![
                ("member1".to_string(), "1".to_string()),
                ("member2".to_string(), "2.5".to_string()),
            ]))
        );
    }

    #[test]
    fn parses_stream_value_by_detecting_entry_ids() {
        assert_eq!(
            parse_stream_value("1-0\nname\nalice\nrole\nadmin\n2-0\nstatus\nactive\n", 500),
            Ok(RedisValue::Stream(vec![
                ("1-0".to_string(), "name=alice, role=admin".to_string()),
                ("2-0".to_string(), "status=active".to_string()),
            ]))
        );
    }

    #[test]
    fn csv_serialization_quotes_commas_quotes_and_newlines() {
        let csv = serialize_csv(
            &["name".to_string(), "note".to_string()],
            &[vec![
                "alice, admin".to_string(),
                "line 1\n\"line 2\"".to_string(),
            ]],
        )
        .unwrap();

        assert_eq!(
            String::from_utf8(csv).unwrap(),
            "name,note\n\"alice, admin\",\"line 1\n\"\"line 2\"\"\"\n"
        );
    }

    #[test]
    fn unique_csv_path_uses_base_name_when_available() {
        let dir = tempfile::tempdir().unwrap();

        let path = unique_csv_path(dir.path(), "user_1");

        assert_eq!(path, dir.path().join("user_1.csv"));
    }

    #[test]
    fn unique_csv_path_uses_next_suffix_when_base_exists() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("user_1.csv"), "").unwrap();

        let path = unique_csv_path(dir.path(), "user_1");

        assert_eq!(path, dir.path().join("user_1-1.csv"));
    }

    #[test]
    fn command_guard_rejects_required_destructive_commands() {
        for command in ["flushall", "  FLUSHDB  ", "Del key1", "\tDeL\tkey1\n"] {
            assert!(
                matches!(
                    ensure_command_allowed(command),
                    Err(RedisCliError::CommandDenied(_))
                ),
                "expected command to be denied: {command:?}"
            );
        }
    }

    #[test]
    fn command_guard_rejects_destructive_sibling_unlink() {
        assert!(matches!(
            ensure_command_allowed("UNLINK key1"),
            Err(RedisCliError::CommandDenied(_))
        ));
    }

    #[test]
    fn command_guard_rejects_empty_or_whitespace_only_input() {
        for command in ["", "   ", "\n\t"] {
            assert!(
                matches!(
                    ensure_command_allowed(command),
                    Err(RedisCliError::CommandDenied(_))
                ),
                "expected command to be denied: {command:?}"
            );
        }
    }

    #[test]
    fn command_guard_allows_non_destructive_reads_and_writes() {
        for command in ["GET foo", "set k v", "  LPUSH list value  "] {
            assert_eq!(ensure_command_allowed(command), Ok(()));
        }
    }

    #[tokio::test]
    #[ignore = "requires Redis and redis-cli; DSN from SABIQL_REDIS_TEST_DSN"]
    async fn subprocess_smoke_test_connects_and_scans() {
        let dsn = std::env::var("SABIQL_REDIS_TEST_DSN")
            .unwrap_or_else(|_| "redis://127.0.0.1:6379/0".to_string());
        let cli = RedisCliSubprocess::new(&dsn).unwrap();

        cli.ping().await.unwrap();
        let _dbsize = cli.dbsize().await.unwrap();
        let _keys = cli.scan_keys().await.unwrap();
    }
}
