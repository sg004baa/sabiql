use std::path::PathBuf;
use std::sync::Arc;

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};
use tokio::sync::mpsc;

use crate::domain::{RedisKey, RedisKind, RedisValue, redis_value_table};
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandStatus {
    Idle,
    Running,
    Success(String),
    Error(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandModalState {
    pub is_open: bool,
    pub input: String,
    pub status: CommandStatus,
}

impl CommandModalState {
    fn new() -> Self {
        Self {
            is_open: false,
            input: String::new(),
            status: CommandStatus::Idle,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusMessage {
    Info(String),
    Success(String),
    Error(String),
}

#[derive(Debug, Clone)]
pub struct AppState {
    pub dsn: String,
    pub connection_status: ConnectionStatus,
    pub keys: Vec<RedisKey>,
    pub filtered_indices: Vec<usize>,
    pub filter_query: String,
    pub filter_active: bool,
    pub selected_index: usize,
    pub scroll_offset: usize,
    pub table_visible_rows: usize,
    pub dbsize: Option<usize>,
    pub value_state: ValueState,
    pub command_modal: CommandModalState,
    pub status_message: Option<StatusMessage>,
    pub should_quit: bool,
}

impl AppState {
    pub fn new(dsn: impl Into<String>) -> Self {
        Self {
            dsn: dsn.into(),
            connection_status: ConnectionStatus::Disconnected,
            keys: Vec::new(),
            filtered_indices: Vec::new(),
            filter_query: String::new(),
            filter_active: false,
            selected_index: 0,
            scroll_offset: 0,
            table_visible_rows: DEFAULT_TABLE_VISIBLE_ROWS,
            dbsize: None,
            value_state: ValueState::Empty,
            command_modal: CommandModalState::new(),
            status_message: None,
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
    OpenFilter,
    FilterInput(char),
    FilterBackspace,
    ClearFilter,
    CommitFilter,
    OpenCommandModal,
    CloseCommandModal,
    CommandInput(char),
    CommandBackspace,
    SubmitCommand,
    CommandSucceeded {
        output: String,
    },
    CommandFailed {
        message: String,
    },
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
    RequestExportCsv,
    ExportSucceeded {
        path: PathBuf,
    },
    ExportFailed {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Connect {
        dsn: String,
    },
    FetchValue {
        key: String,
    },
    ExecuteCommand {
        command: String,
    },
    ExportCsv {
        stem: String,
        headers: Vec<String>,
        rows: Vec<Vec<String>>,
    },
}

pub fn reduce(state: &mut AppState, action: Action) -> Vec<Effect> {
    match action {
        Action::StartConnect => {
            state.connection_status = ConnectionStatus::Connecting;
            state.keys.clear();
            recompute_filtered_indices(state);
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
            recompute_filtered_indices(state);
            clamp_selection_and_scroll(state);
            fetch_selected_key(state)
        }
        Action::ConnectFailed(message) => {
            state.connection_status = ConnectionStatus::Error(message);
            state.keys.clear();
            recompute_filtered_indices(state);
            state.dbsize = None;
            state.selected_index = 0;
            state.scroll_offset = 0;
            state.value_state = ValueState::Empty;
            Vec::new()
        }
        Action::SelectNext => {
            let previous_index = state.selected_index;
            let count = filtered_count(state);
            if count > 0 {
                state.selected_index = (state.selected_index + 1).min(count - 1);
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
        Action::OpenFilter => {
            state.filter_active = true;
            Vec::new()
        }
        Action::FilterInput(ch) => {
            if !state.filter_active {
                return Vec::new();
            }
            state.filter_query.push(ch);
            apply_filter_change(state)
        }
        Action::FilterBackspace => {
            if !state.filter_active {
                return Vec::new();
            }
            if state.filter_query.pop().is_none() {
                return Vec::new();
            }
            apply_filter_change(state)
        }
        Action::ClearFilter => {
            state.filter_active = false;
            if state.filter_query.is_empty() {
                return Vec::new();
            }
            state.filter_query.clear();
            apply_filter_change(state)
        }
        Action::CommitFilter => {
            state.filter_active = false;
            Vec::new()
        }
        Action::OpenCommandModal => {
            state.command_modal.is_open = true;
            state.command_modal.input.clear();
            state.command_modal.status = CommandStatus::Idle;
            Vec::new()
        }
        Action::CloseCommandModal => {
            state.command_modal.is_open = false;
            Vec::new()
        }
        Action::CommandInput(ch) => {
            if state.command_modal.is_open
                && !matches!(state.command_modal.status, CommandStatus::Running)
            {
                state.command_modal.input.push(ch);
            }
            Vec::new()
        }
        Action::CommandBackspace => {
            if state.command_modal.is_open
                && !matches!(state.command_modal.status, CommandStatus::Running)
            {
                state.command_modal.input.pop();
            }
            Vec::new()
        }
        Action::SubmitCommand => {
            if !state.command_modal.is_open {
                return Vec::new();
            }
            let command = state.command_modal.input.trim().to_string();
            if command.is_empty() {
                state.command_modal.status =
                    CommandStatus::Error("Enter a Redis command.".to_string());
                return Vec::new();
            }
            state.command_modal.status = CommandStatus::Running;
            vec![Effect::ExecuteCommand { command }]
        }
        Action::CommandSucceeded { output } => {
            state.command_modal.input.clear();
            state.command_modal.status = CommandStatus::Success(output);
            vec![Effect::Connect {
                dsn: state.dsn.clone(),
            }]
        }
        Action::CommandFailed { message } => {
            state.command_modal.status = CommandStatus::Error(message);
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
            if let Some(redis_key) =
                selected_full_index(state).and_then(|index| state.keys.get_mut(index))
            {
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
        Action::RequestExportCsv => request_export_csv(state),
        Action::ExportSucceeded { path } => {
            state.status_message = Some(StatusMessage::Success(format!(
                "Exported CSV to {}",
                path.display()
            )));
            Vec::new()
        }
        Action::ExportFailed { message } => {
            state.status_message = Some(StatusMessage::Error(format!(
                "CSV export failed: {message}"
            )));
            Vec::new()
        }
    }
}

pub fn filtered_count(state: &AppState) -> usize {
    state.filtered_indices.len()
}

fn selected_full_index(state: &AppState) -> Option<usize> {
    state.filtered_indices.get(state.selected_index).copied()
}

fn selected_key(state: &AppState) -> Option<String> {
    selected_full_index(state)
        .and_then(|index| state.keys.get(index))
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

fn apply_filter_change(state: &mut AppState) -> Vec<Effect> {
    recompute_filtered_indices(state);
    clamp_selection_and_scroll(state);
    fetch_selected_key(state)
}

fn recompute_filtered_indices(state: &mut AppState) {
    if state.filter_query.is_empty() {
        state.filtered_indices = (0..state.keys.len()).collect();
        return;
    }

    // Mirrors the RDB nucleo filtering locally; extracting this into tui-kit is
    // a separate boundary task.
    let mut matcher = Matcher::new(Config::DEFAULT);
    let pattern = Pattern::parse(
        &state.filter_query,
        CaseMatching::Ignore,
        Normalization::Smart,
    );

    state.filtered_indices = state
        .keys
        .iter()
        .enumerate()
        .filter_map(|(index, redis_key)| {
            let mut indices = Vec::new();
            let mut buf = Vec::new();
            let haystack = nucleo_matcher::Utf32Str::new(&redis_key.key, &mut buf);
            pattern
                .indices(haystack, &mut matcher, &mut indices)
                .map(|_| index)
        })
        .collect();
}

fn request_export_csv(state: &mut AppState) -> Vec<Effect> {
    let ValueState::Loaded { key, value, .. } = &state.value_state else {
        state.status_message = Some(StatusMessage::Info(
            "Load a key before exporting CSV.".to_string(),
        ));
        return Vec::new();
    };

    let table = redis_value_table(value);
    let headers = table.headers.into_iter().map(ToString::to_string).collect();
    let stem = sanitize_export_file_stem(key);
    state.status_message = Some(StatusMessage::Info(format!("Exporting CSV for {stem}.csv")));

    vec![Effect::ExportCsv {
        stem,
        headers,
        rows: table.rows,
    }]
}

fn sanitize_export_file_stem(key: &str) -> String {
    let sanitized = key
        .chars()
        .map(|ch| match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            ch if ch.is_control() => '_',
            ch => ch,
        })
        .collect::<String>();
    let trimmed = sanitized.trim_matches([' ', '.']);
    if trimmed.is_empty() {
        "redis_value".to_string()
    } else {
        trimmed.to_string()
    }
}

fn table_visible_rows_for_height(height: u16) -> usize {
    // status line + footer + table header + table scroll indicator
    usize::from(height.saturating_sub(4)).max(1)
}

fn clamp_selection_and_scroll(state: &mut AppState) {
    let count = filtered_count(state);
    if count == 0 {
        state.selected_index = 0;
        state.scroll_offset = 0;
        return;
    }

    state.selected_index = state.selected_index.min(count - 1);
    let max_scroll = count.saturating_sub(state.table_visible_rows);
    state.scroll_offset = state.scroll_offset.min(max_scroll);
    keep_selection_visible(state);
}

fn keep_selection_visible(state: &mut AppState) {
    if filtered_count(state) == 0 {
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
                Effect::ExecuteCommand { command } => {
                    let action = match self.cli.execute_command(&command).await {
                        Ok(output) => Action::CommandSucceeded { output },
                        Err(e) => Action::CommandFailed {
                            message: e.to_string(),
                        },
                    };
                    let _ = self.action_tx.send(action).await;
                }
                Effect::ExportCsv {
                    stem,
                    headers,
                    rows,
                } => {
                    let action = match crate::infra::write_csv_file(&stem, &headers, &rows) {
                        Ok(path) => Action::ExportSucceeded { path },
                        Err(e) => Action::ExportFailed {
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

        fn sync_filter(state: &mut AppState) {
            state.filtered_indices = (0..state.keys.len()).collect();
        }

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
            sync_filter(&mut state);

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
            sync_filter(&mut state);
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
            sync_filter(&mut state);
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
            sync_filter(&mut state);
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

        #[test]
        fn command_modal_open_input_backspace_and_close_updates_state_only() {
            let mut state = AppState::new("redis://localhost");

            assert!(reduce(&mut state, Action::OpenCommandModal).is_empty());
            assert!(state.command_modal.is_open);
            assert_eq!(state.command_modal.input, "");
            assert_eq!(state.command_modal.status, CommandStatus::Idle);

            assert!(reduce(&mut state, Action::CommandInput('s')).is_empty());
            assert!(reduce(&mut state, Action::CommandInput('e')).is_empty());
            assert!(reduce(&mut state, Action::CommandInput('t')).is_empty());
            assert_eq!(state.command_modal.input, "set");

            assert!(reduce(&mut state, Action::CommandBackspace).is_empty());
            assert_eq!(state.command_modal.input, "se");

            assert!(reduce(&mut state, Action::CloseCommandModal).is_empty());
            assert!(!state.command_modal.is_open);
        }

        #[test]
        fn submit_command_emits_execute_command_and_sets_running() {
            let mut state = AppState::new("redis://localhost");
            state.command_modal.is_open = true;
            state.command_modal.input = " set k v ".to_string();

            let effects = reduce(&mut state, Action::SubmitCommand);

            assert_eq!(
                effects,
                vec![Effect::ExecuteCommand {
                    command: "set k v".to_string(),
                }]
            );
            assert_eq!(state.command_modal.status, CommandStatus::Running);
        }

        #[test]
        fn submit_empty_command_stores_error_without_effect() {
            let mut state = AppState::new("redis://localhost");
            state.command_modal.is_open = true;
            state.command_modal.input = "   ".to_string();

            let effects = reduce(&mut state, Action::SubmitCommand);

            assert!(effects.is_empty());
            assert_eq!(
                state.command_modal.status,
                CommandStatus::Error("Enter a Redis command.".to_string())
            );
        }

        #[test]
        fn successful_command_stores_output_and_refreshes_keys() {
            let mut state = AppState::new("redis://localhost");
            state.command_modal.is_open = true;
            state.command_modal.input = "set k v".to_string();
            state.command_modal.status = CommandStatus::Running;

            let effects = reduce(
                &mut state,
                Action::CommandSucceeded {
                    output: "OK\n".to_string(),
                },
            );

            assert_eq!(state.command_modal.input, "");
            assert_eq!(
                state.command_modal.status,
                CommandStatus::Success("OK\n".to_string())
            );
            assert_eq!(
                effects,
                vec![Effect::Connect {
                    dsn: "redis://localhost".to_string(),
                }]
            );
        }

        #[test]
        fn failed_command_stores_error_without_refresh() {
            let mut state = AppState::new("redis://localhost");
            state.command_modal.status = CommandStatus::Running;

            let effects = reduce(
                &mut state,
                Action::CommandFailed {
                    message: "ERR unknown command".to_string(),
                },
            );

            assert!(effects.is_empty());
            assert_eq!(
                state.command_modal.status,
                CommandStatus::Error("ERR unknown command".to_string())
            );
        }

        #[test]
        fn filter_query_narrows_key_list_in_original_order() {
            let mut state = AppState::new("redis://localhost");
            state.keys = vec![
                key("user:1"),
                key("session:1"),
                key("user:settings"),
                key("cache:1"),
            ];
            sync_filter(&mut state);

            assert!(reduce(&mut state, Action::OpenFilter).is_empty());
            let effects = reduce(&mut state, Action::FilterInput('u'));

            assert_eq!(state.filter_query, "u");
            assert_eq!(state.filtered_indices, vec![0, 2]);
            assert_eq!(state.selected_index, 0);
            assert_eq!(
                effects,
                vec![Effect::FetchValue {
                    key: "user:1".to_string()
                }]
            );
        }

        #[test]
        fn filter_change_clamps_selection_and_fetches_filtered_selected_key() {
            let mut state = AppState::new("redis://localhost");
            state.keys = vec![key("alpha"), key("beta"), key("gamma"), key("alpine")];
            sync_filter(&mut state);
            state.selected_index = 3;

            assert!(reduce(&mut state, Action::OpenFilter).is_empty());
            let effects = reduce(&mut state, Action::FilterInput('p'));

            assert_eq!(state.filtered_indices, vec![0, 3]);
            assert_eq!(state.selected_index, 1);
            assert_eq!(
                effects,
                vec![Effect::FetchValue {
                    key: "alpine".to_string()
                }]
            );
        }

        #[test]
        fn moving_selection_while_filtered_fetches_filtered_key() {
            let mut state = AppState::new("redis://localhost");
            state.keys = vec![key("alpha"), key("beta"), key("gamma"), key("alpine")];
            state.filter_query = "al".to_string();
            state.filtered_indices = vec![0, 3];

            let effects = reduce(&mut state, Action::SelectNext);

            assert_eq!(state.selected_index, 1);
            assert_eq!(
                effects,
                vec![Effect::FetchValue {
                    key: "alpine".to_string()
                }]
            );
        }

        #[test]
        fn value_loaded_backfills_full_key_index_while_filtered() {
            let mut state = AppState::new("redis://localhost");
            state.keys = vec![key("alpha"), key("beta"), key("gamma"), key("alpine")];
            state.filter_query = "al".to_string();
            state.filtered_indices = vec![0, 3];
            state.selected_index = 1;
            state.value_state = ValueState::Loading {
                key: "alpine".to_string(),
            };

            let effects = reduce(
                &mut state,
                Action::ValueLoaded {
                    key: "alpine".to_string(),
                    kind: RedisKind::List,
                    ttl: Some(90),
                    value: RedisValue::List(vec!["a".to_string()]),
                },
            );

            assert!(effects.is_empty());
            assert_eq!(state.keys[1].kind, RedisKind::Unknown);
            assert_eq!(state.keys[3].kind, RedisKind::List);
            assert_eq!(state.keys[3].ttl, Some(90));
        }

        #[test]
        fn clearing_filter_restores_full_view_and_refetches_selected_full_key() {
            let mut state = AppState::new("redis://localhost");
            state.keys = vec![key("alpha"), key("beta"), key("gamma"), key("alpine")];
            state.filter_query = "alp".to_string();
            state.filtered_indices = vec![0, 3];
            state.selected_index = 1;
            state.filter_active = true;

            let effects = reduce(&mut state, Action::ClearFilter);

            assert_eq!(state.filter_query, "");
            assert!(!state.filter_active);
            assert_eq!(state.filtered_indices, vec![0, 1, 2, 3]);
            assert_eq!(state.selected_index, 1);
            assert_eq!(
                effects,
                vec![Effect::FetchValue {
                    key: "beta".to_string()
                }]
            );
        }

        #[test]
        fn no_match_filter_yields_empty_view_and_value_state() {
            let mut state = AppState::new("redis://localhost");
            state.keys = vec![key("alpha"), key("beta")];
            sync_filter(&mut state);
            state.selected_index = 1;
            state.value_state = ValueState::Loaded {
                key: "beta".to_string(),
                kind: RedisKind::String,
                ttl: None,
                value: RedisValue::String("b".to_string()),
            };

            assert!(reduce(&mut state, Action::OpenFilter).is_empty());
            let effects = reduce(&mut state, Action::FilterInput('z'));

            assert!(effects.is_empty());
            assert_eq!(state.filtered_indices, Vec::<usize>::new());
            assert_eq!(state.selected_index, 0);
            assert_eq!(state.scroll_offset, 0);
            assert_eq!(state.value_state, ValueState::Empty);
        }

        #[test]
        fn enter_commits_filter_without_clearing_query() {
            let mut state = AppState::new("redis://localhost");
            state.filter_active = true;
            state.filter_query = "user".to_string();

            let effects = reduce(&mut state, Action::CommitFilter);

            assert!(effects.is_empty());
            assert!(!state.filter_active);
            assert_eq!(state.filter_query, "user");
        }

        #[test]
        fn export_loaded_value_builds_csv_effect_for_each_kind() {
            let cases = vec![
                (
                    RedisKind::String,
                    RedisValue::String("hello".to_string()),
                    vec!["value".to_string()],
                    vec![vec!["hello".to_string()]],
                ),
                (
                    RedisKind::List,
                    RedisValue::List(vec!["a".to_string(), "b".to_string()]),
                    vec!["index".to_string(), "value".to_string()],
                    vec![
                        vec!["0".to_string(), "a".to_string()],
                        vec!["1".to_string(), "b".to_string()],
                    ],
                ),
                (
                    RedisKind::Set,
                    RedisValue::Set(vec!["member".to_string()]),
                    vec!["value".to_string()],
                    vec![vec!["member".to_string()]],
                ),
                (
                    RedisKind::Hash,
                    RedisValue::Hash(vec![("field".to_string(), "value".to_string())]),
                    vec!["field".to_string(), "value".to_string()],
                    vec![vec!["field".to_string(), "value".to_string()]],
                ),
                (
                    RedisKind::ZSet,
                    RedisValue::ZSet(vec![("member".to_string(), "1.5".to_string())]),
                    vec!["member".to_string(), "score".to_string()],
                    vec![vec!["member".to_string(), "1.5".to_string()]],
                ),
                (
                    RedisKind::Stream,
                    RedisValue::Stream(vec![("1-0".to_string(), "name=alice".to_string())]),
                    vec!["id".to_string(), "fields".to_string()],
                    vec![vec!["1-0".to_string(), "name=alice".to_string()]],
                ),
            ];

            for (kind, value, headers, rows) in cases {
                let mut state = AppState::new("redis://localhost");
                state.keys = vec![key("user:1/profile")];
                sync_filter(&mut state);
                state.value_state = ValueState::Loaded {
                    key: "user:1/profile".to_string(),
                    kind,
                    ttl: None,
                    value,
                };

                let effects = reduce(&mut state, Action::RequestExportCsv);

                assert_eq!(
                    effects,
                    vec![Effect::ExportCsv {
                        stem: "user_1_profile".to_string(),
                        headers,
                        rows,
                    }]
                );
            }
        }

        #[test]
        fn export_without_loaded_value_is_noop_with_status() {
            let mut state = AppState::new("redis://localhost");

            let effects = reduce(&mut state, Action::RequestExportCsv);

            assert!(effects.is_empty());
            assert_eq!(
                state.status_message,
                Some(StatusMessage::Info(
                    "Load a key before exporting CSV.".to_string()
                ))
            );
        }

        #[test]
        fn export_success_and_failure_update_status_message() {
            let mut state = AppState::new("redis://localhost");

            assert!(
                reduce(
                    &mut state,
                    Action::ExportSucceeded {
                        path: std::path::PathBuf::from("out.csv"),
                    },
                )
                .is_empty()
            );
            assert_eq!(
                state.status_message,
                Some(StatusMessage::Success(
                    "Exported CSV to out.csv".to_string()
                ))
            );

            assert!(
                reduce(
                    &mut state,
                    Action::ExportFailed {
                        message: "permission denied".to_string(),
                    },
                )
                .is_empty()
            );
            assert_eq!(
                state.status_message,
                Some(StatusMessage::Error(
                    "CSV export failed: permission denied".to_string()
                ))
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

        #[tokio::test]
        async fn execute_command_effect_dispatches_success() {
            let mut cli = MockRedisCli::new();
            cli.expect_execute_command()
                .once()
                .withf(|command| command == "set k v")
                .returning(|_| Ok("OK\n".to_string()));

            let (tx, mut rx) = mpsc::channel(4);
            let runner = EffectRunner::new(Arc::new(cli), tx);

            runner
                .run(vec![Effect::ExecuteCommand {
                    command: "set k v".to_string(),
                }])
                .await;

            let action = tokio::time::timeout(Duration::from_millis(500), rx.recv())
                .await
                .expect("action timeout")
                .expect("channel closed");
            assert_eq!(
                action,
                Action::CommandSucceeded {
                    output: "OK\n".to_string(),
                }
            );
        }

        #[tokio::test]
        async fn execute_command_effect_dispatches_failure() {
            let mut cli = MockRedisCli::new();
            cli.expect_execute_command().once().returning(|_| {
                Err(crate::infra::RedisCliError::CommandDenied(
                    "DEL is blocked".to_string(),
                ))
            });

            let (tx, mut rx) = mpsc::channel(4);
            let runner = EffectRunner::new(Arc::new(cli), tx);

            runner
                .run(vec![Effect::ExecuteCommand {
                    command: "DEL key".to_string(),
                }])
                .await;

            let action = tokio::time::timeout(Duration::from_millis(500), rx.recv())
                .await
                .expect("action timeout")
                .expect("channel closed");
            assert_eq!(
                action,
                Action::CommandFailed {
                    message: "DEL is blocked".to_string(),
                }
            );
        }

        #[tokio::test]
        async fn export_csv_effect_writes_file_and_dispatches_success() {
            let cli = MockRedisCli::new();
            let stem = format!("sabiql_redis_export_{}_success", std::process::id());
            let path = std::env::current_dir().unwrap().join(format!("{stem}.csv"));
            let _ = std::fs::remove_file(&path);

            let (tx, mut rx) = mpsc::channel(4);
            let runner = EffectRunner::new(Arc::new(cli), tx);

            runner
                .run(vec![Effect::ExportCsv {
                    stem,
                    headers: vec!["name".to_string(), "note".to_string()],
                    rows: vec![vec!["alice".to_string(), "hello, world".to_string()]],
                }])
                .await;

            let action = tokio::time::timeout(Duration::from_millis(500), rx.recv())
                .await
                .expect("action timeout")
                .expect("channel closed");
            assert_eq!(action, Action::ExportSucceeded { path: path.clone() });
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                "name,note\nalice,\"hello, world\"\n"
            );

            let _ = std::fs::remove_file(path);
        }
    }
}
