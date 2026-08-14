use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
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
    #[error("{0}")]
    InvalidCommand(String),
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
    fn select_db(&self, db: u8);
    async fn db_overview(&self) -> Result<DbOverview, RedisCliError>;
    async fn scan_keys(&self, pattern: &str) -> Result<Vec<RedisKey>, RedisCliError>;
    async fn key_type_and_ttl(&self, key: &str) -> Result<(RedisKind, Option<u64>), RedisCliError>;
    async fn fetch_value(&self, key: &str, kind: RedisKind) -> Result<RedisValue, RedisCliError>;
    async fn execute_command(&self, command: &str) -> Result<String, RedisCliError>;
}

#[cfg_attr(test, mockall::automock)]
pub trait RedisCliFactory: Send + Sync {
    fn create(&self, dsn: &str, read_only: bool) -> Result<Arc<dyn RedisCli>, RedisCliError>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct RedisCliSubprocessFactory;

impl RedisCliFactory for RedisCliSubprocessFactory {
    fn create(&self, dsn: &str, read_only: bool) -> Result<Arc<dyn RedisCli>, RedisCliError> {
        Ok(Arc::new(RedisCliSubprocess::with_read_only(
            dsn, read_only,
        )?))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedisDsn {
    pub host: String,
    pub port: u16,
    pub db: u8,
    pub tls: bool,
    pub username: Option<String>,
    pub password: Option<String>,
}

impl RedisDsn {
    pub fn parse(dsn: &str) -> Result<Self, RedisCliError> {
        let url = url::Url::parse(dsn.trim())
            .map_err(|error| RedisCliError::Parse(format!("invalid Redis DSN: {error}")))?;
        let tls = match url.scheme() {
            "redis" => false,
            "rediss" => true,
            scheme => {
                return Err(RedisCliError::Parse(format!(
                    "unsupported Redis DSN scheme: {scheme}"
                )));
            }
        };

        if url.query().is_some() {
            return Err(RedisCliError::Parse(
                "Redis DSN queries are not supported".to_string(),
            ));
        }
        if url.fragment().is_some() {
            return Err(RedisCliError::Parse(
                "Redis DSN fragments are not supported".to_string(),
            ));
        }

        let host = match url.host() {
            Some(url::Host::Domain(host)) if !host.is_empty() => host.to_string(),
            Some(url::Host::Ipv4(host)) => host.to_string(),
            Some(url::Host::Ipv6(host)) => host.to_string(),
            _ => return Err(RedisCliError::Parse("Redis host is required".to_string())),
        };
        let port = url.port().unwrap_or(6379);

        let db_part = url.path().strip_prefix('/').unwrap_or_else(|| url.path());
        let db = if db_part.is_empty() {
            0
        } else if db_part.contains('/') {
            return Err(RedisCliError::Parse(format!(
                "invalid Redis database path: {}",
                url.path()
            )));
        } else {
            db_part
                .parse::<u8>()
                .map_err(|_| RedisCliError::Parse(format!("invalid Redis database: {db_part}")))?
        };

        let decode_credential = |value: &str, name: &str| {
            let mut bytes = value.bytes();
            while let Some(byte) = bytes.next() {
                if byte == b'%'
                    && (!matches!(bytes.next(), Some(byte) if byte.is_ascii_hexdigit())
                        || !matches!(bytes.next(), Some(byte) if byte.is_ascii_hexdigit()))
                {
                    return Err(RedisCliError::Parse(format!(
                        "invalid percent-encoded Redis {name}"
                    )));
                }
            }

            urlencoding::decode(value)
                .map(String::from)
                .map_err(|error| {
                    RedisCliError::Parse(format!("invalid percent-encoded Redis {name}: {error}"))
                })
        };
        let (username, password) = match (url.username(), url.password()) {
            (username, Some(password)) => {
                let username = decode_credential(username, "username")?;
                let password = decode_credential(password, "password")?;
                ((!username.is_empty()).then_some(username), Some(password))
            }
            ("", None) => (None, None),
            (password, None) => (None, Some(decode_credential(password, "password")?)),
        };

        Ok(Self {
            host,
            port,
            db,
            tls,
            username,
            password,
        })
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbOverview {
    pub entries: Vec<(u8, usize)>,
    /// `false` when the server rejected `CONFIG GET databases` (e.g. an ACL
    /// that denies CONFIG); `entries` then only cover databases observed via
    /// `INFO keyspace` plus the currently selected one.
    pub database_count_known: bool,
}

pub fn parse_config_databases_reply(stdout: &str) -> Result<u8, RedisCliError> {
    let lines = stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    if lines.len() != 2 || !lines[0].eq_ignore_ascii_case("databases") {
        return Err(RedisCliError::Parse(format!(
            "invalid CONFIG GET databases reply: {stdout:?}"
        )));
    }

    let databases = lines[1]
        .parse::<u8>()
        .map_err(|_| RedisCliError::Parse(format!("invalid databases count: {:?}", lines[1])))?;
    if databases == 0 {
        return Err(RedisCliError::Parse(
            "databases count must be greater than zero".to_string(),
        ));
    }

    Ok(databases)
}

/// Interprets a `CONFIG GET databases` reply, mapping a server-side rejection
/// (e.g. `NOPERM` from an ACL that denies CONFIG) to `Ok(None)` so callers can
/// surface the database count as unknown. Any other malformed reply is an
/// error.
fn parse_config_databases_or_denied(stdout: &str) -> Result<Option<u8>, RedisCliError> {
    let first_line = stdout.lines().find(|line| !line.trim().is_empty());
    if first_line.is_some_and(is_redis_error_line) {
        return Ok(None);
    }
    parse_config_databases_reply(stdout).map(Some)
}

fn build_db_overview(
    database_count: Option<u8>,
    key_counts: &HashMap<u8, usize>,
    current_db: u8,
) -> DbOverview {
    let databases: Vec<u8> = database_count.map_or_else(
        || {
            let mut databases: Vec<u8> = key_counts.keys().copied().collect();
            if !key_counts.contains_key(&current_db) {
                databases.push(current_db);
            }
            databases.sort_unstable();
            databases
        },
        |count| (0..count).collect(),
    );

    DbOverview {
        entries: databases
            .into_iter()
            .map(|db| (db, key_counts.get(&db).copied().unwrap_or_default()))
            .collect(),
        database_count_known: database_count.is_some(),
    }
}

pub fn parse_info_keyspace(stdout: &str) -> HashMap<u8, usize> {
    let mut key_counts = HashMap::new();
    for line in stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let Some((db_part, fields)) = line
            .strip_prefix("db")
            .and_then(|rest| rest.split_once(':'))
        else {
            continue;
        };
        let Ok(db) = db_part.parse::<u8>() else {
            continue;
        };
        let Some(keys) = fields
            .split(',')
            .find_map(|field| field.strip_prefix("keys="))
        else {
            continue;
        };
        let Ok(count) = keys.parse::<usize>() else {
            continue;
        };
        key_counts.insert(db, count);
    }
    key_counts
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

fn scan_keys_command_args(cursor: &str, pattern: &str) -> [String; 6] {
    [
        "SCAN".to_string(),
        cursor.to_string(),
        "MATCH".to_string(),
        pattern.to_string(),
        "COUNT".to_string(),
        "1000".to_string(),
    ]
}

const READ_ONLY_COMMANDS: &[&str] = &[
    "PING",
    "ECHO",
    "TYPE",
    "TTL",
    "PTTL",
    "EXPIRETIME",
    "PEXPIRETIME",
    "EXISTS",
    "DBSIZE",
    "RANDOMKEY",
    "KEYS",
    "SCAN",
    "OBJECT",
    "MEMORY",
    "DUMP",
    "TIME",
    "INFO",
    "COMMAND",
    "LOLWUT",
    "GET",
    "MGET",
    "STRLEN",
    "GETRANGE",
    "SUBSTR",
    "GETBIT",
    "BITCOUNT",
    "BITPOS",
    "LLEN",
    "LRANGE",
    "LINDEX",
    "LPOS",
    "SCARD",
    "SISMEMBER",
    "SMISMEMBER",
    "SMEMBERS",
    "SRANDMEMBER",
    "SSCAN",
    "SINTER",
    "SUNION",
    "SDIFF",
    "HGET",
    "HMGET",
    "HGETALL",
    "HKEYS",
    "HVALS",
    "HLEN",
    "HEXISTS",
    "HSTRLEN",
    "HSCAN",
    "HRANDFIELD",
    "ZCARD",
    "ZSCORE",
    "ZMSCORE",
    "ZRANK",
    "ZREVRANK",
    "ZCOUNT",
    "ZLEXCOUNT",
    "ZRANGE",
    "ZRANGEBYSCORE",
    "ZRANGEBYLEX",
    "ZREVRANGE",
    "ZREVRANGEBYSCORE",
    "ZREVRANGEBYLEX",
    "ZSCAN",
    "ZRANDMEMBER",
    "SORT_RO",
    "XLEN",
    "XRANGE",
    "XREVRANGE",
    "XINFO",
    "GEOPOS",
    "GEODIST",
    "GEOSEARCH",
    "GEOHASH",
    "PFCOUNT",
];

static REDIS_COMMANDS: &[&str] = &[
    "ACL",
    "APPEND",
    "AUTH",
    "BGREWRITEAOF",
    "BGSAVE",
    "BITCOUNT",
    "BITFIELD",
    "BITFIELD_RO",
    "BITOP",
    "BITPOS",
    "BLMOVE",
    "BLPOP",
    "BRPOP",
    "BRPOPLPUSH",
    "BZMPOP",
    "BZPOPMAX",
    "BZPOPMIN",
    "CLIENT",
    "CLUSTER",
    "COMMAND",
    "CONFIG",
    "COPY",
    "DBSIZE",
    "DEBUG",
    "DECR",
    "DECRBY",
    "DEL",
    "DISCARD",
    "DUMP",
    "ECHO",
    "EVAL",
    "EVALSHA",
    "EXEC",
    "EXISTS",
    "EXPIRE",
    "EXPIREAT",
    "EXPIRETIME",
    "FAILOVER",
    "FLUSHALL",
    "FLUSHDB",
    "GEOADD",
    "GEODIST",
    "GEOHASH",
    "GEOPOS",
    "GEORADIUS",
    "GEORADIUSBYMEMBER",
    "GEOSEARCH",
    "GEOSEARCHSTORE",
    "GET",
    "GETBIT",
    "GETDEL",
    "GETEX",
    "GETRANGE",
    "GETSET",
    "HDEL",
    "HELLO",
    "HEXISTS",
    "HEXPIRE",
    "HEXPIREAT",
    "HEXPIRETIME",
    "HGET",
    "HGETALL",
    "HINCRBY",
    "HINCRBYFLOAT",
    "HKEYS",
    "HLEN",
    "HMGET",
    "HMSET",
    "HPERSIST",
    "HPEXPIRE",
    "HPEXPIREAT",
    "HPEXPIRETIME",
    "HPTTL",
    "HRANDFIELD",
    "HSCAN",
    "HSET",
    "HSETNX",
    "HSTRLEN",
    "HTTL",
    "HVALS",
    "INCR",
    "INCRBY",
    "INCRBYFLOAT",
    "INFO",
    "KEYS",
    "LASTSAVE",
    "LINDEX",
    "LINSERT",
    "LLEN",
    "LMOVE",
    "LOLWUT",
    "LPOP",
    "LPOS",
    "LPUSH",
    "LPUSHX",
    "LRANGE",
    "LREM",
    "LSET",
    "LTRIM",
    "MEMORY",
    "MGET",
    "MIGRATE",
    "MONITOR",
    "MOVE",
    "MSET",
    "MSETNX",
    "MULTI",
    "OBJECT",
    "PERSIST",
    "PEXPIRE",
    "PEXPIREAT",
    "PEXPIRETIME",
    "PFADD",
    "PFCOUNT",
    "PFMERGE",
    "PING",
    "PTTL",
    "PUBLISH",
    "PUBSUB",
    "RANDOMKEY",
    "READONLY",
    "READWRITE",
    "RENAME",
    "RENAMENX",
    "REPLICAOF",
    "RESTORE",
    "ROLE",
    "RPOP",
    "RPOPLPUSH",
    "RPUSH",
    "RPUSHX",
    "SADD",
    "SAVE",
    "SCAN",
    "SCARD",
    "SDIFF",
    "SDIFFSTORE",
    "SELECT",
    "SET",
    "SETBIT",
    "SETEX",
    "SETNX",
    "SETRANGE",
    "SHUTDOWN",
    "SINTER",
    "SINTERCARD",
    "SINTERSTORE",
    "SISMEMBER",
    "SLAVEOF",
    "SMEMBERS",
    "SMISMEMBER",
    "SMOVE",
    "SORT",
    "SORT_RO",
    "SPOP",
    "SRANDMEMBER",
    "SREM",
    "SSCAN",
    "STRLEN",
    "SUBSCRIBE",
    "SUBSTR",
    "SUNION",
    "SUNIONSTORE",
    "SWAPDB",
    "TIME",
    "TOUCH",
    "TTL",
    "TYPE",
    "UNLINK",
    "UNSUBSCRIBE",
    "UNWATCH",
    "WAIT",
    "WATCH",
    "XACK",
    "XADD",
    "XAUTOCLAIM",
    "XCLAIM",
    "XDEL",
    "XGROUP",
    "XINFO",
    "XLEN",
    "XPENDING",
    "XRANGE",
    "XREAD",
    "XREADGROUP",
    "XREVRANGE",
    "XTRIM",
    "ZADD",
    "ZCARD",
    "ZCOUNT",
    "ZDIFF",
    "ZDIFFSTORE",
    "ZINCRBY",
    "ZINTER",
    "ZINTERCARD",
    "ZINTERSTORE",
    "ZLEXCOUNT",
    "ZMPOP",
    "ZMSCORE",
    "ZPOPMAX",
    "ZPOPMIN",
    "ZRANDMEMBER",
    "ZRANGE",
    "ZRANGEBYLEX",
    "ZRANGEBYSCORE",
    "ZRANGESTORE",
    "ZRANK",
    "ZREM",
    "ZREMRANGEBYLEX",
    "ZREMRANGEBYRANK",
    "ZREMRANGEBYSCORE",
    "ZREVRANGE",
    "ZREVRANGEBYLEX",
    "ZREVRANGEBYSCORE",
    "ZREVRANK",
    "ZSCAN",
    "ZSCORE",
    "ZUNION",
    "ZUNIONSTORE",
];

pub fn complete_command(prefix: &str) -> Vec<String> {
    if prefix.is_empty() {
        return Vec::new();
    }

    let prefix = prefix.to_ascii_uppercase();
    let mut candidates = REDIS_COMMANDS
        .iter()
        .copied()
        .filter(|command| command.starts_with(&prefix))
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    candidates.sort_unstable();
    candidates.dedup();
    candidates
}

pub fn ensure_command_allowed(command: &str, read_only: bool) -> Result<(), RedisCliError> {
    command_requires_confirmation(command, read_only).map(|_| ())
}

pub fn command_requires_confirmation(
    command: &str,
    read_only: bool,
) -> Result<bool, RedisCliError> {
    let Some(first_token) = command.split_whitespace().next() else {
        return Err(RedisCliError::CommandDenied(
            "Enter a Redis command.".to_string(),
        ));
    };

    let upper = first_token.to_ascii_uppercase();
    if read_only && !READ_ONLY_COMMANDS.contains(&upper.as_str()) {
        return Err(RedisCliError::CommandDenied(format!(
            "{upper} is blocked by read-only mode"
        )));
    }

    Ok(!READ_ONLY_COMMANDS.contains(&upper.as_str()))
}

/// Splits `:` command-modal input into redis-cli arguments with shell-style
/// quoting: double and single quotes preserve embedded whitespace, backslash
/// escapes (`\"`, `\\`, `\n`, `\t`, `\r`) apply inside double quotes, and
/// `\'` escapes a quote inside single quotes. An unterminated quote is an
/// explicit error.
fn command_args(command: &str) -> Result<Vec<String>, RedisCliError> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_token = false;
    let mut chars = command.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            c if c.is_whitespace() => {
                if in_token {
                    args.push(std::mem::take(&mut current));
                    in_token = false;
                }
            }
            '"' => {
                in_token = true;
                loop {
                    match chars.next() {
                        None => return Err(unclosed_quote_error('"')),
                        Some('"') => break,
                        Some('\\') => {
                            let Some(escaped) = chars.next() else {
                                return Err(unclosed_quote_error('"'));
                            };
                            match escaped {
                                'n' => current.push('\n'),
                                't' => current.push('\t'),
                                'r' => current.push('\r'),
                                '"' | '\\' => current.push(escaped),
                                other => {
                                    current.push('\\');
                                    current.push(other);
                                }
                            }
                        }
                        Some(c) => current.push(c),
                    }
                }
            }
            '\'' => {
                in_token = true;
                loop {
                    match chars.next() {
                        None => return Err(unclosed_quote_error('\'')),
                        Some('\'') => break,
                        Some('\\') if chars.peek() == Some(&'\'') => {
                            chars.next();
                            current.push('\'');
                        }
                        Some(c) => current.push(c),
                    }
                }
            }
            c => {
                in_token = true;
                current.push(c);
            }
        }
    }
    if in_token {
        args.push(current);
    }

    Ok(args)
}

fn unclosed_quote_error(quote: char) -> RedisCliError {
    RedisCliError::InvalidCommand(format!("Unclosed {quote} quote in command."))
}

fn command_output_result(stdout: &str) -> Result<String, RedisCliError> {
    let first_line = stdout.lines().find(|line| !line.trim().is_empty());
    if first_line.is_some_and(is_redis_error_line) {
        return Err(RedisCliError::CommandFailed(stdout.trim().to_string()));
    }
    Ok(stdout.to_string())
}

fn is_redis_error_line(line: &str) -> bool {
    const ERROR_PREFIXES: &[&str] = &[
        "(error)",
        "ERR ",
        "WRONGTYPE ",
        "NOAUTH ",
        "NOPERM ",
        "READONLY ",
        "BUSY ",
        "LOADING ",
        "OOM ",
        "MISCONF ",
    ];

    let line = line.trim_start();
    ERROR_PREFIXES.iter().any(|prefix| line.starts_with(prefix))
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

fn accumulate_scan_value_body<I>(mut body: Vec<String>, pages: I, cap: usize) -> (Vec<String>, bool)
where
    I: IntoIterator<Item = ScanPage>,
{
    let mut is_complete = false;
    for page in pages {
        is_complete = scan_is_complete(&page);
        let remaining = cap.saturating_sub(body.len());
        body.extend(page.keys.into_iter().take(remaining));

        if is_complete || body.len() >= cap {
            break;
        }
    }

    (body, is_complete)
}

#[derive(Debug, Clone)]
pub struct RedisCliSubprocess {
    dsn: RedisDsn,
    current_db: Arc<AtomicU8>,
    timeout: Duration,
    read_only: bool,
}

impl RedisCliSubprocess {
    const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

    pub fn new(dsn: &str) -> Result<Self, RedisCliError> {
        Self::with_timeout(dsn, Self::DEFAULT_TIMEOUT)
    }

    pub fn with_read_only(dsn: &str, read_only: bool) -> Result<Self, RedisCliError> {
        Self::with_timeout_and_read_only(dsn, Self::DEFAULT_TIMEOUT, read_only)
    }

    pub fn with_timeout(dsn: &str, timeout: Duration) -> Result<Self, RedisCliError> {
        Self::with_timeout_and_read_only(dsn, timeout, false)
    }

    pub fn with_timeout_and_read_only(
        dsn: &str,
        timeout: Duration,
        read_only: bool,
    ) -> Result<Self, RedisCliError> {
        let dsn = RedisDsn::parse(dsn)?;
        Ok(Self {
            current_db: Arc::new(AtomicU8::new(dsn.db)),
            dsn,
            timeout,
            read_only,
        })
    }

    pub fn read_only(&self) -> bool {
        self.read_only
    }

    async fn scan_value_lines(
        &self,
        command: &str,
        key: &str,
        body_cap: usize,
    ) -> Result<Vec<String>, RedisCliError> {
        let mut cursor = "0".to_string();
        let mut body = Vec::new();

        loop {
            let stdout = self
                .run_command(&[
                    command.to_string(),
                    key.to_string(),
                    cursor,
                    "COUNT".to_string(),
                    REDIS_VALUE_PREVIEW_LIMIT.to_string(),
                ])
                .await?;
            let page = parse_scan_page(&stdout)?;
            let next_cursor = page.next_cursor.clone();
            let (next_body, is_complete) = accumulate_scan_value_body(body, [page], body_cap);
            body = next_body;

            if is_complete || body.len() >= body_cap {
                break;
            }
            cursor = next_cursor;
        }

        Ok(body)
    }

    async fn run_command(&self, args: &[String]) -> Result<String, RedisCliError> {
        self.run_command_in_db(self.current_db.load(Ordering::Relaxed), args)
            .await
    }

    fn command_in_db(&self, db: u8, args: &[String]) -> Command {
        let mut cmd = Command::new("redis-cli");
        cmd.kill_on_drop(true);
        if self.dsn.tls {
            cmd.arg("--tls");
        }
        if let Some(username) = &self.dsn.username {
            cmd.arg("--user").arg(username);
        }
        if let Some(password) = &self.dsn.password {
            cmd.env("REDISCLI_AUTH", password);
        }
        cmd.arg("-h")
            .arg(&self.dsn.host)
            .arg("-p")
            .arg(self.dsn.port.to_string())
            .arg("-n")
            .arg(db.to_string())
            .arg("--raw")
            .args(args);
        cmd
    }

    async fn run_command_in_db(&self, db: u8, args: &[String]) -> Result<String, RedisCliError> {
        let mut cmd = self.command_in_db(db, args);

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

    fn select_db(&self, db: u8) {
        self.current_db.store(db, Ordering::Relaxed);
    }

    async fn db_overview(&self) -> Result<DbOverview, RedisCliError> {
        let database_count = match self
            .run_command(&[
                "CONFIG".to_string(),
                "GET".to_string(),
                "databases".to_string(),
            ])
            .await
        {
            Ok(stdout) => parse_config_databases_or_denied(&stdout)?,
            // redis-cli reports server-side rejections (e.g. an ACL denying
            // CONFIG) as a failed invocation; the overview then shows the
            // database count as unknown instead of inventing a default.
            Err(RedisCliError::CommandFailed(_)) => None,
            Err(e) => return Err(e),
        };
        let key_counts = parse_info_keyspace(
            &self
                .run_command(&["INFO".to_string(), "keyspace".to_string()])
                .await?,
        );

        Ok(build_db_overview(
            database_count,
            &key_counts,
            self.current_db.load(Ordering::Relaxed),
        ))
    }

    async fn scan_keys(&self, pattern: &str) -> Result<Vec<RedisKey>, RedisCliError> {
        let mut cursor = "0".to_string();
        let mut keys = Vec::new();

        loop {
            let args = scan_keys_command_args(&cursor, pattern);
            let stdout = self.run_command(&args).await?;
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
                let body = self.scan_value_lines("SSCAN", key, cap).await?;
                parse_set_value(&body.join("\n"), cap)
            }
            RedisKind::Hash => {
                let body = self
                    .scan_value_lines("HSCAN", key, cap.saturating_mul(2))
                    .await?;
                parse_hash_value(&body.join("\n"), cap)
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
        ensure_command_allowed(command, self.read_only)?;
        let stdout = self.run_command(&command_args(command)?).await?;
        command_output_result(&stdout)
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
                tls: false,
                username: None,
                password: None,
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
                tls: false,
                username: None,
                password: None,
            }
        );
    }

    #[test]
    fn parses_rediss_elasticache_url() {
        let dsn = RedisDsn::parse("rediss://cache.example.amazonaws.com:6379/0").unwrap();

        assert_eq!(dsn.host, "cache.example.amazonaws.com");
        assert_eq!(dsn.port, 6379);
        assert_eq!(dsn.db, 0);
        assert!(dsn.tls);
        assert_eq!(dsn.username, None);
        assert_eq!(dsn.password, None);
    }

    #[test]
    fn percent_decodes_redis_credentials() {
        let dsn = RedisDsn::parse("rediss://app%40prod:p%40ss%2Fword@cache.example.com/2").unwrap();

        assert_eq!(dsn.username.as_deref(), Some("app@prod"));
        assert_eq!(dsn.password.as_deref(), Some("p@ss/word"));
    }

    #[test]
    fn parses_password_only_redis_credentials() {
        let dsn = RedisDsn::parse("redis://p%40ssword@localhost").unwrap();

        assert_eq!(dsn.username, None);
        assert_eq!(dsn.password.as_deref(), Some("p@ssword"));
    }

    #[test]
    fn rejects_malformed_or_unsupported_redis_urls() {
        for input in [
            "http://localhost",
            "redis:///0",
            "redis://localhost:not-a-port",
            "redis://localhost:65536",
            "redis://localhost/not-a-db",
            "redis://localhost/1/2",
            "redis://localhost/256",
            "redis://localhost/0?foo=bar",
            "redis://localhost/0#fragment",
            "redis://user:%ZZ@localhost",
            "redis://user:%@localhost",
            "redis://user:%A@localhost",
            "redis://%ZZ:password@localhost",
            "redis://%:password@localhost",
            "redis://%A:password@localhost",
        ] {
            let error = RedisDsn::parse(input).unwrap_err();
            assert!(
                matches!(error, RedisCliError::Parse(_)),
                "input {input:?} returned {error:?}"
            );
        }
    }

    #[test]
    fn builds_tls_authenticated_redis_cli_command_without_password_argv() {
        let client = RedisCliSubprocess::new(
            "rediss://app%40prod:p%40ss%2Fword@cache.example.amazonaws.com:6380/2",
        )
        .unwrap();
        let command = client.command_in_db(3, &["PING".to_string()]);
        let std_command = command.as_std();
        let args = std_command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert_eq!(
            args,
            [
                "--tls",
                "--user",
                "app@prod",
                "-h",
                "cache.example.amazonaws.com",
                "-p",
                "6380",
                "-n",
                "3",
                "--raw",
                "PING",
            ]
        );
        assert!(!args.iter().any(|arg| arg.contains("p@ss/word")));
        let password = std_command
            .get_envs()
            .find(|(name, _)| *name == "REDISCLI_AUTH")
            .and_then(|(_, value)| value)
            .map(|value| value.to_string_lossy().into_owned());
        assert_eq!(password.as_deref(), Some("p@ss/word"));
    }

    #[test]
    fn builds_password_only_command_without_user_argument() {
        let client = RedisCliSubprocess::new("redis://:secret@localhost").unwrap();
        let command = client.command_in_db(0, &["PING".to_string()]);
        let std_command = command.as_std();
        let args = std_command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert!(!args.iter().any(|arg| arg == "--user"));
        assert!(!args.iter().any(|arg| arg.contains("secret")));
        let password = std_command
            .get_envs()
            .find(|(name, _)| *name == "REDISCLI_AUTH")
            .and_then(|(_, value)| value)
            .map(|value| value.to_string_lossy().into_owned());
        assert_eq!(password.as_deref(), Some("secret"));
    }

    #[test]
    fn builds_legacy_redis_cli_command_without_tls_or_auth() {
        let client = RedisCliSubprocess::new("redis://localhost").unwrap();
        let command = client.command_in_db(0, &["PING".to_string()]);
        let std_command = command.as_std();
        let args = std_command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert_eq!(
            args,
            ["-h", "localhost", "-p", "6379", "-n", "0", "--raw", "PING"]
        );
        assert!(
            std_command
                .get_envs()
                .all(|(name, _)| name != "REDISCLI_AUTH")
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
    fn parses_config_databases_reply() {
        assert_eq!(parse_config_databases_reply("databases\n32\n"), Ok(32));
        assert_eq!(parse_config_databases_reply("DATABASES\r\n16\r\n"), Ok(16));
    }

    #[test]
    fn config_databases_denied_reply_maps_to_unknown_count() {
        assert_eq!(
            parse_config_databases_or_denied(
                "NOPERM this user has no permissions to run the 'config' command\n"
            ),
            Ok(None)
        );
        assert_eq!(
            parse_config_databases_or_denied("ERR unknown command 'CONFIG'\n"),
            Ok(None)
        );
    }

    #[test]
    fn config_databases_valid_reply_maps_to_known_count() {
        assert_eq!(
            parse_config_databases_or_denied("databases\n32\n"),
            Ok(Some(32))
        );
    }

    #[test]
    fn config_databases_garbage_reply_is_a_parse_error() {
        for garbage in [
            "unexpected\n",
            "databases\nnot-a-number\n",
            "databases\n0\n",
        ] {
            let err = parse_config_databases_or_denied(garbage).unwrap_err();
            assert!(matches!(err, RedisCliError::Parse(_)), "input: {garbage:?}");
        }
    }

    #[test]
    fn db_overview_with_known_count_lists_every_database() {
        let key_counts = HashMap::from([(0, 3), (2, 7)]);

        assert_eq!(
            build_db_overview(Some(4), &key_counts, 0),
            DbOverview {
                entries: vec![(0, 3), (1, 0), (2, 7), (3, 0)],
                database_count_known: true,
            }
        );
    }

    #[test]
    fn db_overview_with_unknown_count_lists_keyspace_and_current_db() {
        let key_counts = HashMap::from([(3, 5), (1, 2)]);

        assert_eq!(
            build_db_overview(None, &key_counts, 6),
            DbOverview {
                entries: vec![(1, 2), (3, 5), (6, 0)],
                database_count_known: false,
            }
        );
    }

    #[test]
    fn db_overview_with_unknown_count_does_not_duplicate_current_db() {
        let key_counts = HashMap::from([(0, 1)]);

        assert_eq!(
            build_db_overview(None, &key_counts, 0),
            DbOverview {
                entries: vec![(0, 1)],
                database_count_known: false,
            }
        );
    }

    #[test]
    fn parses_info_keyspace_with_multiple_dbs_and_crlf() {
        let key_counts = parse_info_keyspace(
            "# Keyspace\r\n\
             db0:keys=1234,expires=10,avg_ttl=0\r\n\
             db1:keys=56,expires=0,avg_ttl=0\r\n",
        );

        assert_eq!(key_counts, HashMap::from([(0, 1234), (1, 56)]));
    }

    #[test]
    fn parse_info_keyspace_skips_headers_empty_lines_and_invalid_rows() {
        let key_counts = parse_info_keyspace(
            "# Keyspace\n\
             \n\
             db0:keys=7,expires=0,avg_ttl=0\n\
             ignored\n\
             dbx:keys=9,expires=0,avg_ttl=0\n\
             db1:expires=0,avg_ttl=0\n\
             db2:keys=not-a-number,expires=0,avg_ttl=0\n\
             db3:keys=4\n",
        );

        assert_eq!(key_counts, HashMap::from([(0, 7), (3, 4)]));
    }

    #[test]
    fn parse_info_keyspace_returns_empty_map_for_empty_input() {
        assert!(parse_info_keyspace("").is_empty());
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
    fn scan_keys_args_pass_match_pattern_as_single_argv_element() {
        assert_eq!(
            scan_keys_command_args("42", "user:* [ab]?"),
            [
                "SCAN".to_string(),
                "42".to_string(),
                "MATCH".to_string(),
                "user:* [ab]?".to_string(),
                "COUNT".to_string(),
                "1000".to_string(),
            ]
        );
    }

    #[test]
    fn accumulates_scan_value_body_across_pages_until_terminal_cursor() {
        let (body, is_complete) = accumulate_scan_value_body(
            Vec::new(),
            [
                ScanPage {
                    next_cursor: "5".to_string(),
                    keys: vec!["a".to_string(), "b".to_string()],
                },
                ScanPage {
                    next_cursor: "0".to_string(),
                    keys: vec!["c".to_string(), "d".to_string()],
                },
            ],
            REDIS_VALUE_PREVIEW_LIMIT,
        );

        assert!(is_complete);
        assert_eq!(body, vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn accumulates_scan_value_body_stops_at_cap() {
        let first_page = (0..300).map(|i| format!("v{i}")).collect();
        let second_page = (300..700).map(|i| format!("v{i}")).collect();

        let (body, is_complete) = accumulate_scan_value_body(
            Vec::new(),
            [
                ScanPage {
                    next_cursor: "5".to_string(),
                    keys: first_page,
                },
                ScanPage {
                    next_cursor: "9".to_string(),
                    keys: second_page,
                },
                ScanPage {
                    next_cursor: "0".to_string(),
                    keys: vec!["should-not-be-read".to_string()],
                },
            ],
            REDIS_VALUE_PREVIEW_LIMIT,
        );

        assert!(!is_complete);
        assert_eq!(body.len(), REDIS_VALUE_PREVIEW_LIMIT);
        assert_eq!(body.first().map(String::as_str), Some("v0"));
        assert_eq!(body.last().map(String::as_str), Some("v499"));
    }

    #[test]
    fn accumulates_scan_value_body_from_single_terminal_page() {
        let (body, is_complete) = accumulate_scan_value_body(
            Vec::new(),
            [ScanPage {
                next_cursor: "0".to_string(),
                keys: vec!["only".to_string()],
            }],
            REDIS_VALUE_PREVIEW_LIMIT,
        );

        assert!(is_complete);
        assert_eq!(body, vec!["only"]);
    }

    #[test]
    fn accumulated_hash_scan_body_preserves_hash_entry_cap() {
        let first_page = (0..400)
            .flat_map(|i| [format!("field{i}"), format!("value{i}")])
            .collect();
        let second_page = (400..700)
            .flat_map(|i| [format!("field{i}"), format!("value{i}")])
            .collect();

        let (body, is_complete) = accumulate_scan_value_body(
            Vec::new(),
            [
                ScanPage {
                    next_cursor: "5".to_string(),
                    keys: first_page,
                },
                ScanPage {
                    next_cursor: "9".to_string(),
                    keys: second_page,
                },
            ],
            REDIS_VALUE_PREVIEW_LIMIT.saturating_mul(2),
        );
        let value = parse_hash_value(&body.join("\n"), REDIS_VALUE_PREVIEW_LIMIT).unwrap();

        assert!(!is_complete);
        assert_eq!(body.len(), REDIS_VALUE_PREVIEW_LIMIT.saturating_mul(2));
        assert_eq!(
            value,
            RedisValue::Hash(
                (0..REDIS_VALUE_PREVIEW_LIMIT)
                    .map(|i| (format!("field{i}"), format!("value{i}")))
                    .collect()
            )
        );
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
    fn command_completion_matches_prefix() {
        assert_eq!(complete_command("GETR"), vec!["GETRANGE".to_string()]);
    }

    #[test]
    fn command_completion_is_case_insensitive() {
        assert_eq!(
            complete_command("hget"),
            vec!["HGET".to_string(), "HGETALL".to_string()]
        );
    }

    #[test]
    fn command_completion_returns_sorted_output() {
        let candidates = complete_command("Z");

        assert!(
            candidates
                .windows(2)
                .all(|pair| pair[0].as_str() <= pair[1].as_str())
        );
    }

    #[test]
    fn command_completion_returns_no_candidates_for_empty_prefix() {
        assert!(complete_command("").is_empty());
    }

    #[test]
    fn redis_commands_include_every_read_only_command() {
        for command in READ_ONLY_COMMANDS {
            assert!(
                REDIS_COMMANDS.contains(command),
                "missing read-only command from completion list: {command}"
            );
        }
    }

    #[test]
    fn command_guard_allows_write_commands_in_read_write_mode() {
        for command in ["flushall", "  FLUSHDB  ", "Del key1", "\tDeL\tkey1\n"] {
            assert_eq!(
                ensure_command_allowed(command, false),
                Ok(()),
                "expected command to be allowed after UI confirmation: {command:?}"
            );
        }
    }

    #[test]
    fn command_confirmation_detects_writes_in_read_write_mode() {
        for command in ["SET k v", "UNLINK key1", "FLUSHDB", "EVAL script 0"] {
            assert_eq!(
                command_requires_confirmation(command, false),
                Ok(true),
                "expected command to require confirmation: {command:?}"
            );
        }
    }

    #[test]
    fn command_confirmation_skips_read_commands() {
        for command in ["GET foo", " scan 0 ", "\tping\n"] {
            assert_eq!(
                command_requires_confirmation(command, false),
                Ok(false),
                "expected command to run without confirmation: {command:?}"
            );
        }
    }

    #[test]
    fn command_guard_rejects_empty_or_whitespace_only_input() {
        for command in ["", "   ", "\n\t"] {
            assert!(
                matches!(
                    ensure_command_allowed(command, false),
                    Err(RedisCliError::CommandDenied(_))
                ),
                "expected command to be denied: {command:?}"
            );
        }
    }

    #[test]
    fn command_guard_allows_non_destructive_reads_and_writes() {
        for command in [
            "GET foo",
            "set k v",
            "  LPUSH list value  ",
            "EVAL script 0",
        ] {
            assert_eq!(ensure_command_allowed(command, false), Ok(()));
        }
    }

    #[test]
    fn command_guard_allows_read_only_allow_list_in_read_only_mode() {
        for command in [
            "GET foo",
            " scan 0 ",
            "LRANGE list 0 -1",
            "SMEMBERS set",
            "HGETALL hash",
            "ZRANGE zset 0 -1",
            "XRANGE stream - +",
            "SORT_RO list",
            "\tping\n",
        ] {
            assert_eq!(
                ensure_command_allowed(command, true),
                Ok(()),
                "expected read-only command to be allowed: {command:?}"
            );
        }
    }

    #[test]
    fn command_guard_rejects_non_allow_list_commands_in_read_only_mode() {
        for command in [
            "SET k v",
            "LPUSH list value",
            "EVAL script 0",
            "GETDEL k",
            "GETEX k",
            "SORT list",
            "CONFIG GET *",
        ] {
            let err = ensure_command_allowed(command, true).unwrap_err();

            assert!(
                matches!(err, RedisCliError::CommandDenied(ref message) if message.contains("read-only mode")),
                "expected read-only denial for {command:?}, got {err:?}"
            );
        }
    }

    #[test]
    fn command_args_splits_on_whitespace() {
        assert_eq!(
            command_args("  SET   mykey\tvalue \n"),
            Ok(vec![
                "SET".to_string(),
                "mykey".to_string(),
                "value".to_string(),
            ])
        );
    }

    #[test]
    fn command_args_preserves_whitespace_inside_double_quotes() {
        assert_eq!(
            command_args(r#"SET mykey "hello world""#),
            Ok(vec![
                "SET".to_string(),
                "mykey".to_string(),
                "hello world".to_string(),
            ])
        );
    }

    #[test]
    fn command_args_preserves_whitespace_inside_single_quotes() {
        assert_eq!(
            command_args("SET mykey 'hello  world'"),
            Ok(vec![
                "SET".to_string(),
                "mykey".to_string(),
                "hello  world".to_string(),
            ])
        );
    }

    #[test]
    fn command_args_applies_backslash_escapes_inside_double_quotes() {
        assert_eq!(
            command_args(r#"SET k "a\"b\\c\nd\te\rf\zg""#),
            Ok(vec![
                "SET".to_string(),
                "k".to_string(),
                "a\"b\\c\nd\te\rf\\zg".to_string(),
            ])
        );
    }

    #[test]
    fn command_args_escapes_quote_inside_single_quotes() {
        assert_eq!(
            command_args(r"SET k 'it\'s a \path'"),
            Ok(vec![
                "SET".to_string(),
                "k".to_string(),
                "it's a \\path".to_string(),
            ])
        );
    }

    #[test]
    fn command_args_concatenates_adjacent_quoted_and_bare_segments() {
        assert_eq!(
            command_args(r#"SET k pre"mid dle"post"#),
            Ok(vec![
                "SET".to_string(),
                "k".to_string(),
                "premid dlepost".to_string(),
            ])
        );
    }

    #[test]
    fn command_args_keeps_empty_quoted_token() {
        assert_eq!(
            command_args(r#"SET k """#),
            Ok(vec!["SET".to_string(), "k".to_string(), String::new()])
        );
    }

    #[test]
    fn command_args_rejects_unclosed_double_quote() {
        for command in [r#"SET k "unterminated"#, r#"SET k "trailing\"#] {
            let err = command_args(command).unwrap_err();

            assert_eq!(
                err,
                RedisCliError::InvalidCommand("Unclosed \" quote in command.".to_string()),
                "input: {command:?}"
            );
        }
    }

    #[test]
    fn command_args_rejects_unclosed_single_quote() {
        let err = command_args("SET k 'unterminated").unwrap_err();

        assert_eq!(
            err,
            RedisCliError::InvalidCommand("Unclosed ' quote in command.".to_string())
        );
    }

    #[test]
    fn command_output_result_rejects_redis_error_replies() {
        for output in [
            "(error) ERR syntax error\n",
            "ERR unknown command 'NOPE'\n",
            "WRONGTYPE Operation against a key holding the wrong kind of value\n",
            "NOAUTH Authentication required.\n",
        ] {
            assert_eq!(
                command_output_result(output),
                Err(RedisCliError::CommandFailed(output.trim().to_string()))
            );
        }
    }

    #[test]
    fn command_output_result_accepts_regular_output() {
        assert_eq!(command_output_result("OK\n"), Ok("OK\n".to_string()));
        assert_eq!(
            command_output_result("value before an error-like line\nERR data\n"),
            Ok("value before an error-like line\nERR data\n".to_string())
        );
    }

    #[test]
    fn subprocess_defaults_to_read_write_and_can_be_read_only() {
        let read_write = RedisCliSubprocess::new("redis://localhost").unwrap();
        let read_only = RedisCliSubprocess::with_read_only("redis://localhost", true).unwrap();

        assert!(!read_write.read_only);
        assert!(read_only.read_only);
    }

    #[tokio::test]
    #[ignore = "requires Redis and redis-cli; DSN from SABIQL_REDIS_TEST_DSN"]
    async fn subprocess_smoke_test_connects_and_scans() {
        let dsn = std::env::var("SABIQL_REDIS_TEST_DSN")
            .unwrap_or_else(|_| "redis://127.0.0.1:6379/0".to_string());
        let cli = RedisCliSubprocess::new(&dsn).unwrap();

        cli.ping().await.unwrap();
        let _dbsize = cli.dbsize().await.unwrap();
        let _keys = cli.scan_keys("*").await.unwrap();
    }
}
