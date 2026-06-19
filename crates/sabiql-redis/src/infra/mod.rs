use std::time::Duration;

use async_trait::async_trait;
use tokio::process::Command;
use tokio::time::timeout;

use crate::domain::RedisKey;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RedisCliError {
    #[error("redis-cli not found: {0}")]
    CommandNotFound(String),
    #[error("redis-cli failed: {0}")]
    CommandFailed(String),
    #[error("redis-cli timed out: {0}")]
    Timeout(String),
    #[error("failed to parse redis-cli output: {0}")]
    Parse(String),
}

#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait RedisCli: Send + Sync {
    async fn ping(&self) -> Result<(), RedisCliError>;
    async fn dbsize(&self) -> Result<usize, RedisCliError>;
    async fn scan_keys(&self) -> Result<Vec<RedisKey>, RedisCliError>;
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
        cmd.arg("-h")
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
