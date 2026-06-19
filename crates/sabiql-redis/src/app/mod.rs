use std::sync::Arc;

use tokio::sync::mpsc;

use crate::domain::{RedisKey, RedisKind, RedisValue};
use crate::infra::RedisCli;

const DEFAULT_TABLE_VISIBLE_ROWS: usize = 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionStatus {
    Disconnected,
    Connecting,
    Connected,
    Error(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValueState {
    Empty,
    Loading {
        key: String,
    },
    Loaded {
        key: String,
        kind: RedisKind,
        ttl: Option<u64>,
        value: RedisValue,
    },
    Failed {
        key: String,
        message: String,
    },
}

#[derive(Debug, Clone)]
pub struct AppState {
    pub dsn: String,
    pub connection_status: ConnectionStatus,
    pub keys: Vec<RedisKey>,
    pub selected_index: usize,
    pub scroll_offset: usize,
    pub table_visible_rows: usize,
    pub dbsize: Option<usize>,
    pub value_state: ValueState,
    pub should_quit: bool,
}

impl AppState {
    pub fn new(dsn: impl Into<String>) -> Self {
        Self {
            dsn: dsn.into(),
            connection_status: ConnectionStatus::Disconnected,
            keys: Vec::new(),
            selected_index: 0,
            scroll_offset: 0,
            table_visible_rows: DEFAULT_TABLE_VISIBLE_ROWS,
            dbsize: None,
            value_state: ValueState::Empty,
            should_quit: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    StartConnect,
    Connected {
        keys: Vec<RedisKey>,
        dbsize: Option<usize>,
    },
    ConnectFailed(String),
    SelectNext,
    SelectPrev,
    Quit,
    Resize(u16, u16),
    ValueLoaded {
        key: String,
        kind: RedisKind,
        ttl: Option<u64>,
        value: RedisValue,
    },
    ValueFetchFailed {
        key: String,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Connect { dsn: String },
    FetchValue { key: String },
}

pub fn reduce(state: &mut AppState, action: Action) -> Vec<Effect> {
    match action {
        Action::StartConnect => {
            state.connection_status = ConnectionStatus::Connecting;
            state.keys.clear();
            state.dbsize = None;
            state.selected_index = 0;
            state.scroll_offset = 0;
            state.value_state = ValueState::Empty;
            vec![Effect::Connect {
                dsn: state.dsn.clone(),
            }]
        }
        Action::Connected { keys, dbsize } => {
            state.connection_status = ConnectionStatus::Connected;
            state.keys = keys;
            state.dbsize = dbsize;
            clamp_selection_and_scroll(state);
            fetch_selected_key(state)
        }
        Action::ConnectFailed(message) => {
            state.connection_status = ConnectionStatus::Error(message);
            state.keys.clear();
            state.dbsize = None;
            state.selected_index = 0;
            state.scroll_offset = 0;
            state.value_state = ValueState::Empty;
            Vec::new()
        }
        Action::SelectNext => {
            let previous_index = state.selected_index;
            if !state.keys.is_empty() {
                state.selected_index = (state.selected_index + 1).min(state.keys.len() - 1);
                keep_selection_visible(state);
            }
            if state.selected_index == previous_index {
                Vec::new()
            } else {
                fetch_selected_key(state)
            }
        }
        Action::SelectPrev => {
            let previous_index = state.selected_index;
            state.selected_index = state.selected_index.saturating_sub(1);
            keep_selection_visible(state);
            if state.selected_index == previous_index {
                Vec::new()
            } else {
                fetch_selected_key(state)
            }
        }
        Action::Quit => {
            state.should_quit = true;
            Vec::new()
        }
        Action::Resize(_, height) => {
            state.table_visible_rows = table_visible_rows_for_height(height);
            clamp_selection_and_scroll(state);
            Vec::new()
        }
        Action::ValueLoaded {
            key,
            kind,
            ttl,
            value,
        } => {
            if !is_current_key(state, &key) {
                return Vec::new();
            }
            if let Some(redis_key) = state.keys.get_mut(state.selected_index) {
                redis_key.kind = kind;
                redis_key.ttl = ttl;
            }
            state.value_state = ValueState::Loaded {
                key,
                kind,
                ttl,
                value,
            };
            Vec::new()
        }
        Action::ValueFetchFailed { key, message } => {
            if !is_current_key(state, &key) {
                return Vec::new();
            }
            state.value_state = ValueState::Failed { key, message };
            Vec::new()
        }
    }
}

fn selected_key(state: &AppState) -> Option<String> {
    state
        .keys
        .get(state.selected_index)
        .map(|redis_key| redis_key.key.clone())
}

fn is_current_key(state: &AppState, key: &str) -> bool {
    selected_key(state).as_deref() == Some(key)
}

fn fetch_selected_key(state: &mut AppState) -> Vec<Effect> {
    let Some(key) = selected_key(state) else {
        state.value_state = ValueState::Empty;
        return Vec::new();
    };
    state.value_state = ValueState::Loading { key: key.clone() };
    vec![Effect::FetchValue { key }]
}

fn table_visible_rows_for_height(height: u16) -> usize {
    // status line + footer + table header + table scroll indicator
    usize::from(height.saturating_sub(4)).max(1)
}

fn clamp_selection_and_scroll(state: &mut AppState) {
    if state.keys.is_empty() {
        state.selected_index = 0;
        state.scroll_offset = 0;
        return;
    }

    state.selected_index = state.selected_index.min(state.keys.len() - 1);
    let max_scroll = state.keys.len().saturating_sub(state.table_visible_rows);
    state.scroll_offset = state.scroll_offset.min(max_scroll);
    keep_selection_visible(state);
}

fn keep_selection_visible(state: &mut AppState) {
    if state.keys.is_empty() {
        state.scroll_offset = 0;
        return;
    }

    let visible_rows = state.table_visible_rows.max(1);
    if state.selected_index < state.scroll_offset {
        state.scroll_offset = state.selected_index;
    } else if state.selected_index >= state.scroll_offset + visible_rows {
        state.scroll_offset = state.selected_index + 1 - visible_rows;
    }
}

pub struct EffectRunner {
    cli: Arc<dyn RedisCli>,
    action_tx: mpsc::Sender<Action>,
}

impl EffectRunner {
    pub fn new(cli: Arc<dyn RedisCli>, action_tx: mpsc::Sender<Action>) -> Self {
        Self { cli, action_tx }
    }

    pub async fn run(&self, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::Connect { dsn: _ } => {
                    let action = match self.connect().await {
                        Ok((keys, dbsize)) => Action::Connected { keys, dbsize },
                        Err(e) => Action::ConnectFailed(e.to_string()),
                    };
                    let _ = self.action_tx.send(action).await;
                }
                Effect::FetchValue { key } => {
                    let action = match self.fetch_value(&key).await {
                        Ok((kind, ttl, value)) => Action::ValueLoaded {
                            key,
                            kind,
                            ttl,
                            value,
                        },
                        Err(e) => Action::ValueFetchFailed {
                            key,
                            message: e.to_string(),
                        },
                    };
                    let _ = self.action_tx.send(action).await;
                }
            }
        }
    }

    async fn connect(&self) -> Result<(Vec<RedisKey>, Option<usize>), crate::infra::RedisCliError> {
        self.cli.ping().await?;
        let dbsize = self.cli.dbsize().await?;
        let keys = self.cli.scan_keys().await?;
        Ok((keys, Some(dbsize)))
    }

    async fn fetch_value(
        &self,
        key: &str,
    ) -> Result<(RedisKind, Option<u64>, RedisValue), crate::infra::RedisCliError> {
        let (kind, ttl) = self.cli.key_type_and_ttl(key).await?;
        let value = self.cli.fetch_value(key, kind).await?;
        Ok((kind, ttl, value))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::infra::MockRedisCli;

    fn key(name: &str) -> RedisKey {
        RedisKey::unknown(name)
    }

    mod reducer {
        use super::*;

        #[test]
        fn start_connect_sets_connecting_and_emits_connect_effect() {
            let mut state = AppState::new("redis://localhost");

            let effects = reduce(&mut state, Action::StartConnect);

            assert_eq!(state.connection_status, ConnectionStatus::Connecting);
            assert_eq!(
                effects,
                vec![Effect::Connect {
                    dsn: "redis://localhost".to_string()
                }]
            );
        }

        #[test]
        fn connected_populates_keys_and_clamps_selection() {
            let mut state = AppState::new("redis://localhost");
            state.keys = vec![key("a"), key("b"), key("c")];
            state.selected_index = 2;
            state.scroll_offset = 2;

            let effects = reduce(
                &mut state,
                Action::Connected {
                    keys: vec![key("z")],
                    dbsize: Some(10),
                },
            );

            assert_eq!(
                effects,
                vec![Effect::FetchValue {
                    key: "z".to_string()
                }]
            );
            assert_eq!(state.connection_status, ConnectionStatus::Connected);
            assert_eq!(state.keys, vec![key("z")]);
            assert_eq!(state.selected_index, 0);
            assert_eq!(state.scroll_offset, 0);
            assert_eq!(state.dbsize, Some(10));
            assert_eq!(
                state.value_state,
                ValueState::Loading {
                    key: "z".to_string()
                }
            );
        }

        #[test]
        fn select_next_and_prev_clamp_at_bounds() {
            let mut state = AppState::new("redis://localhost");
            state.keys = vec![key("a"), key("b")];

            assert_eq!(
                reduce(&mut state, Action::SelectNext),
                vec![Effect::FetchValue {
                    key: "b".to_string()
                }]
            );
            assert_eq!(state.selected_index, 1);
            assert_eq!(
                state.value_state,
                ValueState::Loading {
                    key: "b".to_string()
                }
            );
            assert!(reduce(&mut state, Action::SelectNext).is_empty());
            assert_eq!(state.selected_index, 1);

            assert_eq!(
                reduce(&mut state, Action::SelectPrev),
                vec![Effect::FetchValue {
                    key: "a".to_string()
                }]
            );
            assert_eq!(state.selected_index, 0);
            assert!(reduce(&mut state, Action::SelectPrev).is_empty());
            assert_eq!(state.selected_index, 0);
        }

        #[test]
        fn connect_failed_sets_error_state() {
            let mut state = AppState::new("redis://localhost");

            let effects = reduce(
                &mut state,
                Action::ConnectFailed("connection refused".to_string()),
            );

            assert!(effects.is_empty());
            assert_eq!(
                state.connection_status,
                ConnectionStatus::Error("connection refused".to_string())
            );
        }

        #[test]
        fn connected_with_empty_keys_clears_value_state_without_fetch() {
            let mut state = AppState::new("redis://localhost");
            state.value_state = ValueState::Loading {
                key: "old".to_string(),
            };

            let effects = reduce(
                &mut state,
                Action::Connected {
                    keys: Vec::new(),
                    dbsize: Some(0),
                },
            );

            assert!(effects.is_empty());
            assert_eq!(state.value_state, ValueState::Empty);
        }

        #[test]
        fn value_loaded_populates_value_state_and_backfills_selected_key_metadata() {
            let mut state = AppState::new("redis://localhost");
            state.keys = vec![key("a"), key("b")];
            state.selected_index = 1;
            state.value_state = ValueState::Loading {
                key: "b".to_string(),
            };

            let effects = reduce(
                &mut state,
                Action::ValueLoaded {
                    key: "b".to_string(),
                    kind: RedisKind::Hash,
                    ttl: Some(60),
                    value: RedisValue::Hash(vec![("field".to_string(), "value".to_string())]),
                },
            );

            assert!(effects.is_empty());
            assert_eq!(state.keys[1].kind, RedisKind::Hash);
            assert_eq!(state.keys[1].ttl, Some(60));
            assert_eq!(
                state.value_state,
                ValueState::Loaded {
                    key: "b".to_string(),
                    kind: RedisKind::Hash,
                    ttl: Some(60),
                    value: RedisValue::Hash(vec![("field".to_string(), "value".to_string())]),
                }
            );
        }

        #[test]
        fn stale_value_loaded_for_previous_selection_is_ignored() {
            let mut state = AppState::new("redis://localhost");
            state.keys = vec![key("a"), key("b")];
            state.selected_index = 1;
            state.value_state = ValueState::Loading {
                key: "b".to_string(),
            };

            let effects = reduce(
                &mut state,
                Action::ValueLoaded {
                    key: "a".to_string(),
                    kind: RedisKind::String,
                    ttl: None,
                    value: RedisValue::String("stale".to_string()),
                },
            );

            assert!(effects.is_empty());
            assert_eq!(state.keys[0].kind, RedisKind::Unknown);
            assert_eq!(
                state.value_state,
                ValueState::Loading {
                    key: "b".to_string()
                }
            );
        }

        #[test]
        fn value_fetch_failed_sets_failure_for_current_selection_only() {
            let mut state = AppState::new("redis://localhost");
            state.keys = vec![key("a"), key("b")];
            state.selected_index = 1;
            state.value_state = ValueState::Loading {
                key: "b".to_string(),
            };

            assert!(
                reduce(
                    &mut state,
                    Action::ValueFetchFailed {
                        key: "a".to_string(),
                        message: "old error".to_string(),
                    },
                )
                .is_empty()
            );
            assert_eq!(
                state.value_state,
                ValueState::Loading {
                    key: "b".to_string()
                }
            );

            assert!(
                reduce(
                    &mut state,
                    Action::ValueFetchFailed {
                        key: "b".to_string(),
                        message: "wrong type".to_string(),
                    },
                )
                .is_empty()
            );
            assert_eq!(
                state.value_state,
                ValueState::Failed {
                    key: "b".to_string(),
                    message: "wrong type".to_string(),
                }
            );
        }
    }

    mod effect_runner {
        use super::*;

        #[tokio::test]
        async fn connect_effect_pings_counts_and_scans_then_dispatches_connected() {
            let mut cli = MockRedisCli::new();
            cli.expect_ping().once().returning(|| Ok(()));
            cli.expect_dbsize().once().returning(|| Ok(2));
            cli.expect_scan_keys()
                .once()
                .returning(|| Ok(vec![key("a"), key("b")]));

            let (tx, mut rx) = mpsc::channel(4);
            let runner = EffectRunner::new(Arc::new(cli), tx);

            runner
                .run(vec![Effect::Connect {
                    dsn: "redis://localhost".to_string(),
                }])
                .await;

            let action = tokio::time::timeout(Duration::from_millis(500), rx.recv())
                .await
                .expect("action timeout")
                .expect("channel closed");
            assert_eq!(
                action,
                Action::Connected {
                    keys: vec![key("a"), key("b")],
                    dbsize: Some(2),
                }
            );
        }

        #[tokio::test]
        async fn connect_effect_dispatches_failure_when_ping_fails() {
            let mut cli = MockRedisCli::new();
            cli.expect_ping().once().returning(|| {
                Err(crate::infra::RedisCliError::CommandFailed(
                    "connection refused".to_string(),
                ))
            });
            cli.expect_dbsize().never();
            cli.expect_scan_keys().never();

            let (tx, mut rx) = mpsc::channel(4);
            let runner = EffectRunner::new(Arc::new(cli), tx);

            runner
                .run(vec![Effect::Connect {
                    dsn: "redis://localhost".to_string(),
                }])
                .await;

            let action = tokio::time::timeout(Duration::from_millis(500), rx.recv())
                .await
                .expect("action timeout")
                .expect("channel closed");
            assert_eq!(
                action,
                Action::ConnectFailed("redis-cli failed: connection refused".to_string())
            );
        }

        #[tokio::test]
        async fn fetch_value_effect_loads_type_ttl_and_value() {
            let mut cli = MockRedisCli::new();
            cli.expect_key_type_and_ttl()
                .once()
                .withf(|key| key == "user:1")
                .returning(|_| Ok((RedisKind::String, Some(30))));
            cli.expect_fetch_value()
                .once()
                .withf(|key, kind| key == "user:1" && *kind == RedisKind::String)
                .returning(|_, _| Ok(RedisValue::String("alice".to_string())));

            let (tx, mut rx) = mpsc::channel(4);
            let runner = EffectRunner::new(Arc::new(cli), tx);

            runner
                .run(vec![Effect::FetchValue {
                    key: "user:1".to_string(),
                }])
                .await;

            let action = tokio::time::timeout(Duration::from_millis(500), rx.recv())
                .await
                .expect("action timeout")
                .expect("channel closed");
            assert_eq!(
                action,
                Action::ValueLoaded {
                    key: "user:1".to_string(),
                    kind: RedisKind::String,
                    ttl: Some(30),
                    value: RedisValue::String("alice".to_string()),
                }
            );
        }

        #[tokio::test]
        async fn fetch_value_effect_dispatches_failure() {
            let mut cli = MockRedisCli::new();
            cli.expect_key_type_and_ttl().once().returning(|_| {
                Err(crate::infra::RedisCliError::CommandFailed(
                    "missing".to_string(),
                ))
            });
            cli.expect_fetch_value().never();

            let (tx, mut rx) = mpsc::channel(4);
            let runner = EffectRunner::new(Arc::new(cli), tx);

            runner
                .run(vec![Effect::FetchValue {
                    key: "gone".to_string(),
                }])
                .await;

            let action = tokio::time::timeout(Duration::from_millis(500), rx.recv())
                .await
                .expect("action timeout")
                .expect("channel closed");
            assert_eq!(
                action,
                Action::ValueFetchFailed {
                    key: "gone".to_string(),
                    message: "redis-cli failed: missing".to_string(),
                }
            );
        }
    }
}
