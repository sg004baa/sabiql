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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RedisValue {
    String(String),
    List(Vec<String>),
    Set(Vec<String>),
    Hash(Vec<(String, String)>),
    ZSet(Vec<(String, String)>),
    Stream(Vec<(String, String)>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedisValueTable {
    pub headers: Vec<&'static str>,
    pub rows: Vec<Vec<String>>,
}

pub fn redis_value_table(value: &RedisValue) -> RedisValueTable {
    match value {
        RedisValue::String(v) => RedisValueTable {
            headers: vec!["value"],
            rows: vec![vec![v.clone()]],
        },
        RedisValue::List(values) => RedisValueTable {
            headers: vec!["index", "value"],
            rows: values
                .iter()
                .enumerate()
                .map(|(index, value)| vec![index.to_string(), value.clone()])
                .collect(),
        },
        RedisValue::Set(values) => RedisValueTable {
            headers: vec!["value"],
            rows: values.iter().map(|value| vec![value.clone()]).collect(),
        },
        RedisValue::Hash(entries) => RedisValueTable {
            headers: vec!["field", "value"],
            rows: entries
                .iter()
                .map(|(field, value)| vec![field.clone(), value.clone()])
                .collect(),
        },
        RedisValue::ZSet(entries) => RedisValueTable {
            headers: vec!["member", "score"],
            rows: entries
                .iter()
                .map(|(member, score)| vec![member.clone(), score.clone()])
                .collect(),
        },
        RedisValue::Stream(entries) => RedisValueTable {
            headers: vec!["id", "fields"],
            rows: entries
                .iter()
                .map(|(id, fields)| vec![id.clone(), fields.clone()])
                .collect(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_value_maps_to_single_value_column() {
        let table = redis_value_table(&RedisValue::String("hello".to_string()));

        assert_eq!(table.headers, vec!["value"]);
        assert_eq!(table.rows, vec![vec!["hello".to_string()]]);
    }

    #[test]
    fn list_value_maps_to_index_and_value_columns() {
        let table = redis_value_table(&RedisValue::List(vec!["a".to_string(), "b".to_string()]));

        assert_eq!(table.headers, vec!["index", "value"]);
        assert_eq!(
            table.rows,
            vec![
                vec!["0".to_string(), "a".to_string()],
                vec!["1".to_string(), "b".to_string()],
            ]
        );
    }

    #[test]
    fn set_value_maps_to_single_value_column() {
        let table = redis_value_table(&RedisValue::Set(vec!["member".to_string()]));

        assert_eq!(table.headers, vec!["value"]);
        assert_eq!(table.rows, vec![vec!["member".to_string()]]);
    }

    #[test]
    fn hash_value_maps_to_field_and_value_columns() {
        let table = redis_value_table(&RedisValue::Hash(vec![(
            "field".to_string(),
            "value".to_string(),
        )]));

        assert_eq!(table.headers, vec!["field", "value"]);
        assert_eq!(
            table.rows,
            vec![vec!["field".to_string(), "value".to_string()]]
        );
    }

    #[test]
    fn zset_value_maps_to_member_and_score_columns() {
        let table = redis_value_table(&RedisValue::ZSet(vec![(
            "member".to_string(),
            "1.5".to_string(),
        )]));

        assert_eq!(table.headers, vec!["member", "score"]);
        assert_eq!(
            table.rows,
            vec![vec!["member".to_string(), "1.5".to_string()]]
        );
    }

    #[test]
    fn stream_value_maps_to_id_and_fields_columns() {
        let table = redis_value_table(&RedisValue::Stream(vec![(
            "1-0".to_string(),
            "name=alice, role=admin".to_string(),
        )]));

        assert_eq!(table.headers, vec!["id", "fields"]);
        assert_eq!(
            table.rows,
            vec![vec![
                "1-0".to_string(),
                "name=alice, role=admin".to_string()
            ]]
        );
    }
}
