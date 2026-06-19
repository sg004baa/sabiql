use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedisKey {
    pub key: String,
    pub kind: RedisKind,
    pub ttl: Option<u64>,
}

impl RedisKey {
    pub fn unknown(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            kind: RedisKind::Unknown,
            ttl: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedisKind {
    String,
    List,
    Set,
    Hash,
    ZSet,
    Stream,
    Unknown,
}

impl fmt::Display for RedisKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::String => "string",
            Self::List => "list",
            Self::Set => "set",
            Self::Hash => "hash",
            Self::ZSet => "zset",
            Self::Stream => "stream",
            Self::Unknown => "unknown",
        };
        f.write_str(label)
    }
}
