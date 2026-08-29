use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};
use tokio::sync::{RwLock, mpsc};

use crate::domain::{
    RedisKey, RedisKind, RedisValue, redis_string_display_value, redis_value_table,
};
use crate::infra::{
    DbOverview, RedisCli, RedisCliFactory, RedisDsn, command_requires_confirmation,
    complete_command,
};

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
    pub cursor: usize,
    pub status: CommandStatus,
    pub history: Vec<String>,
    pub history_cursor: Option<usize>,
    pub history_draft: String,
    pub completion: CommandCompletionState,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandCompletionState {
    pub candidates: Vec<String>,
    pub selected: usize,
    pub visible: bool,
}

impl CommandModalState {
    fn new() -> Self {
        Self {
            is_open: false,
            input: String::new(),
            cursor: 0,
            status: CommandStatus::Idle,
            history: Vec::new(),
            history_cursor: None,
            history_draft: String::new(),
            completion: CommandCompletionState::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValueSelection {
    pub row: usize,
    pub column: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ValueEditTarget {
    String { key: String },
    List { key: String, index: usize },
    Hash { key: String, field: String },
    ZSet { key: String, member: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValueEditState {
    pub input: String,
    pub cursor: usize,
    target: ValueEditTarget,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusMessage {
    Info(String),
    Success(String),
    Error(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbOverlayState {
    pub entries: Vec<(u8, Option<usize>)>,
    pub selected: usize,
    pub loading: bool,
    /// `false` when the server refused to reveal its database count (e.g. an
    /// ACL denying CONFIG); the overlay then only lists observed databases.
    pub database_count_known: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingWrite {
    Command { command: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmState {
    pub op: PendingWrite,
    pub prompt: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionFormState {
    pub dsn: String,
    pub read_only: bool,
    pub cursor: usize,
}

#[derive(Debug, Clone)]
pub struct AppState {
    pub dsn: String,
    pub read_only: bool,
    pub current_db: u8,
    pub connection_status: ConnectionStatus,
    pub keys: Vec<RedisKey>,
    pub filtered_indices: Vec<usize>,
    pub search_pattern: String,
    pub filter_active: bool,
    pub selected_index: usize,
    pub scroll_offset: usize,
    pub value_scroll_offset: usize,
    pub table_visible_rows: usize,
    pub dbsize: Option<usize>,
    pub value_state: ValueState,
    pub value_selection: Option<ValueSelection>,
    pub value_edit: Option<ValueEditState>,
    value_write_pending: Option<()>,
    pub command_modal: CommandModalState,
    pub db_overlay: Option<DbOverlayState>,
    pub confirm_state: Option<ConfirmState>,
    pub connection_form: Option<ConnectionFormState>,
    pub status_message: Option<StatusMessage>,
    pub should_quit: bool,
}

impl AppState {
    pub fn new(dsn: impl Into<String>) -> Self {
        Self::with_read_only(dsn, false)
    }

    pub fn with_read_only(dsn: impl Into<String>, read_only: bool) -> Self {
        let dsn = dsn.into();
        let current_db = RedisDsn::parse(&dsn).map(|parsed| parsed.db).unwrap_or(0);
        Self {
            dsn,
            read_only,
            current_db,
            connection_status: ConnectionStatus::Disconnected,
            keys: Vec::new(),
            filtered_indices: Vec::new(),
            search_pattern: "*".to_string(),
            filter_active: false,
            selected_index: 0,
            scroll_offset: 0,
            value_scroll_offset: 0,
            table_visible_rows: DEFAULT_TABLE_VISIBLE_ROWS,
            dbsize: None,
            value_state: ValueState::Empty,
            value_selection: None,
            value_edit: None,
            value_write_pending: None,
            command_modal: CommandModalState::new(),
            db_overlay: None,
            confirm_state: None,
            connection_form: None,
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
    KeysScanned {
        keys: Vec<RedisKey>,
    },
    KeysScanFailed {
        message: String,
    },
    SelectNext,
    SelectPrev,
    ValueScrollDown,
    ValueScrollUp,
    ActivateValue,
    DeactivateValue,
    ValueSelectNext,
    ValueSelectPrev,
    ValueSelectLeft,
    ValueSelectRight,
    YankSelected,
    ClipboardSucceeded,
    ClipboardFailed {
        message: String,
    },
    OpenValueEditor,
    RequestExternalValueEditor,
    ExternalValueEditSucceeded {
        content: String,
    },
    ExternalValueEditFailed {
        message: String,
    },
    ValueEditInput(char),
    ValueEditPaste(String),
    ValueEditBackspace,
    ValueEditCursorLeft,
    ValueEditCursorRight,
    SubmitValueEdit,
    CancelValueEdit,
    Quit,
    Resize(u16, u16),
    OpenFilter,
    FilterInput(char),
    FilterBackspace,
    ClearFilter,
    CommitFilter,
    Reload,
    OpenCommandModal,
    CloseCommandModal,
    OpenConnectionForm,
    ConnectionFormInput(char),
    ConnectionFormPaste(String),
    ConnectionFormBackspace,
    ConnectionFormCursorLeft,
    ConnectionFormCursorRight,
    ToggleConnectionFormReadOnly,
    SubmitConnectionForm,
    CancelConnectionForm,
    OpenDbOverlay,
    CloseDbOverlay,
    DbOverlaySelectNext,
    DbOverlaySelectPrev,
    SubmitDbSelection,
    DbOverviewLoaded {
        overview: DbOverview,
    },
    DbOverviewFailed {
        message: String,
    },
    CommandInput(char),
    CommandBackspace,
    CommandCursorLeft,
    CommandCursorRight,
    CommandPaste(String),
    CommandHistoryPrev,
    CommandHistoryNext,
    CommandCompleteNext,
    CommandCompletePrev,
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
    ConfirmWrite,
    CancelWrite,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Connect {
        dsn: String,
        read_only: bool,
    },
    FetchValue {
        key: String,
    },
    SearchKeys {
        pattern: String,
    },
    ExecuteCommand {
        command: String,
    },
    LoadDbOverview,
    SelectDb {
        db: u8,
    },
    ExportCsv {
        stem: String,
        headers: Vec<String>,
        rows: Vec<Vec<String>>,
    },
    CopyToClipboard {
        text: String,
    },
    OpenExternalValueEditor {
        content: String,
    },
}

pub fn reduce(state: &mut AppState, action: Action) -> Vec<Effect> {
    match action {
        Action::StartConnect => {
            reset_for_connect(state);
            vec![Effect::Connect {
                dsn: state.dsn.clone(),
                read_only: state.read_only,
            }]
        }
        Action::Connected { keys, dbsize } => {
            state.connection_status = ConnectionStatus::Connected;
            state.keys = keys;
            state.dbsize = dbsize;
            state.search_pattern = "*".to_string();
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
            state.value_scroll_offset = 0;
            state.value_state = ValueState::Empty;
            state.value_selection = None;
            state.value_edit = None;
            Vec::new()
        }
        Action::KeysScanned { keys } => {
            state.keys = keys;
            state.selected_index = 0;
            state.scroll_offset = 0;
            recompute_filtered_indices(state);
            clamp_selection_and_scroll(state);
            fetch_selected_key(state)
        }
        Action::KeysScanFailed { message } => {
            state.status_message = Some(StatusMessage::Error(format!(
                "Redis key search failed: {message}"
            )));
            Vec::new()
        }
        Action::SelectNext => {
            if key_navigation_blocked(state) {
                return Vec::new();
            }
            let previous_index = state.selected_index;
            let count = key_count(state);
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
            if key_navigation_blocked(state) {
                return Vec::new();
            }
            let previous_index = state.selected_index;
            state.selected_index = state.selected_index.saturating_sub(1);
            keep_selection_visible(state);
            if state.selected_index == previous_index {
                Vec::new()
            } else {
                fetch_selected_key(state)
            }
        }
        Action::ValueScrollDown => {
            let ValueState::Loaded { value, .. } = &state.value_state else {
                return Vec::new();
            };
            let max_scroll = value_row_count(value).saturating_sub(1);
            state.value_scroll_offset = state.value_scroll_offset.saturating_add(1).min(max_scroll);
            Vec::new()
        }
        Action::ValueScrollUp => {
            if matches!(state.value_state, ValueState::Loaded { .. }) {
                state.value_scroll_offset = state.value_scroll_offset.saturating_sub(1);
            }
            Vec::new()
        }
        Action::ActivateValue => {
            let ValueState::Loaded { value, .. } = &state.value_state else {
                return Vec::new();
            };
            let (rows, columns) = value_target_dimensions(value);
            if rows == 0 || columns == 0 {
                state.status_message = Some(StatusMessage::Info(
                    "This value has no selectable cells.".to_string(),
                ));
                return Vec::new();
            }
            state.value_selection = Some(ValueSelection { row: 0, column: 0 });
            state.value_scroll_offset = 0;
            Vec::new()
        }
        Action::DeactivateValue => {
            state.value_selection = None;
            Vec::new()
        }
        Action::ValueSelectNext => {
            move_value_selection(state, 1, 0);
            Vec::new()
        }
        Action::ValueSelectPrev => {
            move_value_selection(state, -1, 0);
            Vec::new()
        }
        Action::ValueSelectLeft => {
            move_value_selection(state, 0, -1);
            Vec::new()
        }
        Action::ValueSelectRight => {
            move_value_selection(state, 0, 1);
            Vec::new()
        }
        Action::YankSelected => {
            let text = if state.value_selection.is_some() {
                selected_value_cell(state)
            } else {
                selected_key(state)
            };
            let Some(text) = text else {
                state.status_message = Some(StatusMessage::Info(
                    "Nothing is selected to copy.".to_string(),
                ));
                return Vec::new();
            };
            vec![Effect::CopyToClipboard { text }]
        }
        Action::ClipboardSucceeded => {
            state.status_message = Some(StatusMessage::Success("Copied to clipboard.".to_string()));
            Vec::new()
        }
        Action::ClipboardFailed { message } => {
            state.status_message = Some(StatusMessage::Error(format!(
                "Clipboard copy failed: {message}"
            )));
            Vec::new()
        }
        Action::OpenValueEditor => open_value_editor(state),
        Action::RequestExternalValueEditor => request_external_value_editor(state),
        Action::ExternalValueEditSucceeded { content } => {
            if let Some(editor) = &mut state.value_edit {
                editor.cursor = content.chars().count();
                editor.input = content;
                state.status_message = None;
            }
            Vec::new()
        }
        Action::ExternalValueEditFailed { message } => {
            state.status_message = Some(StatusMessage::Error(format!(
                "External editor failed: {message}"
            )));
            Vec::new()
        }
        Action::ValueEditInput(ch) => {
            if let Some(editor) = &mut state.value_edit {
                insert_value_edit_char(editor, ch);
            }
            Vec::new()
        }
        Action::ValueEditPaste(text) => {
            if let Some(editor) = &mut state.value_edit {
                insert_value_edit_text(editor, &text);
            }
            Vec::new()
        }
        Action::ValueEditBackspace => {
            if let Some(editor) = &mut state.value_edit {
                backspace_value_edit(editor);
            }
            Vec::new()
        }
        Action::ValueEditCursorLeft => {
            if let Some(editor) = &mut state.value_edit {
                let char_count = editor.input.chars().count();
                editor.cursor = editor.cursor.min(char_count).saturating_sub(1);
            }
            Vec::new()
        }
        Action::ValueEditCursorRight => {
            if let Some(editor) = &mut state.value_edit {
                let char_count = editor.input.chars().count();
                editor.cursor = editor
                    .cursor
                    .min(char_count)
                    .saturating_add(1)
                    .min(char_count);
            }
            Vec::new()
        }
        Action::SubmitValueEdit => submit_value_edit(state),
        Action::CancelValueEdit => {
            state.value_edit = None;
            Vec::new()
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
            if overlay_or_modal_open(state) {
                return Vec::new();
            }
            if state.search_pattern == "*" {
                state.search_pattern.clear();
            }
            state.filter_active = true;
            Vec::new()
        }
        Action::FilterInput(ch) => {
            if !state.filter_active {
                return Vec::new();
            }
            state.search_pattern.push(ch);
            apply_filter_change(state)
        }
        Action::FilterBackspace => {
            if !state.filter_active {
                return Vec::new();
            }
            if state.search_pattern.pop().is_none() {
                return Vec::new();
            }
            apply_filter_change(state)
        }
        Action::ClearFilter => {
            state.filter_active = false;
            state.search_pattern = "*".to_string();
            recompute_filtered_indices(state);
            state.selected_index = 0;
            state.scroll_offset = 0;
            vec![Effect::SearchKeys {
                pattern: state.search_pattern.clone(),
            }]
        }
        Action::CommitFilter => {
            state.filter_active = false;
            state.search_pattern = normalized_search_pattern(&state.search_pattern);
            recompute_filtered_indices(state);
            clamp_selection_and_scroll(state);
            vec![Effect::SearchKeys {
                pattern: state.search_pattern.clone(),
            }]
        }
        Action::Reload => {
            if overlay_or_modal_open(state) || state.filter_active {
                return Vec::new();
            }
            vec![Effect::SearchKeys {
                pattern: state.search_pattern.clone(),
            }]
        }
        Action::OpenCommandModal => {
            if state.db_overlay.is_some()
                || state.connection_form.is_some()
                || state.confirm_state.is_some()
                || state.filter_active
                || state.value_selection.is_some()
                || state.value_edit.is_some()
            {
                return Vec::new();
            }
            state.command_modal.is_open = true;
            state.command_modal.input.clear();
            state.command_modal.cursor = 0;
            state.command_modal.status = CommandStatus::Idle;
            reset_command_history_navigation(&mut state.command_modal);
            hide_command_completion(&mut state.command_modal);
            Vec::new()
        }
        Action::CloseCommandModal => {
            if state.command_modal.is_open && state.command_modal.completion.visible {
                hide_command_completion(&mut state.command_modal);
                return Vec::new();
            }
            state.command_modal.is_open = false;
            reset_command_history_navigation(&mut state.command_modal);
            hide_command_completion(&mut state.command_modal);
            Vec::new()
        }
        Action::OpenConnectionForm => {
            if overlay_or_modal_open(state) || state.filter_active {
                return Vec::new();
            }
            state.connection_form = Some(ConnectionFormState {
                dsn: state.dsn.clone(),
                read_only: state.read_only,
                cursor: state.dsn.chars().count(),
            });
            Vec::new()
        }
        Action::ConnectionFormInput(ch) => {
            if let Some(form) = &mut state.connection_form {
                insert_connection_form_char(form, ch);
            }
            Vec::new()
        }
        Action::ConnectionFormPaste(text) => {
            if let Some(form) = &mut state.connection_form {
                insert_connection_form_text(form, &text);
            }
            Vec::new()
        }
        Action::ConnectionFormBackspace => {
            if let Some(form) = &mut state.connection_form {
                backspace_connection_form_char(form);
            }
            Vec::new()
        }
        Action::ConnectionFormCursorLeft => {
            if let Some(form) = &mut state.connection_form {
                let char_count = form.dsn.chars().count();
                form.cursor = form.cursor.min(char_count).saturating_sub(1);
            }
            Vec::new()
        }
        Action::ConnectionFormCursorRight => {
            if let Some(form) = &mut state.connection_form {
                let char_count = form.dsn.chars().count();
                form.cursor = form
                    .cursor
                    .min(char_count)
                    .saturating_add(1)
                    .min(char_count);
            }
            Vec::new()
        }
        Action::ToggleConnectionFormReadOnly => {
            if let Some(form) = &mut state.connection_form {
                form.read_only = !form.read_only;
            }
            Vec::new()
        }
        Action::SubmitConnectionForm => submit_connection_form(state),
        Action::CancelConnectionForm => {
            state.connection_form = None;
            Vec::new()
        }
        Action::OpenDbOverlay => {
            if !matches!(state.connection_status, ConnectionStatus::Connected)
                || state.filter_active
                || state.command_modal.is_open
                || state.db_overlay.is_some()
                || state.confirm_state.is_some()
                || state.connection_form.is_some()
                || state.value_selection.is_some()
                || state.value_edit.is_some()
            {
                return Vec::new();
            }

            state.db_overlay = Some(DbOverlayState {
                entries: Vec::new(),
                selected: 0,
                loading: true,
                database_count_known: true,
            });
            vec![Effect::LoadDbOverview]
        }
        Action::CloseDbOverlay => {
            state.db_overlay = None;
            Vec::new()
        }
        Action::DbOverlaySelectNext => {
            if let Some(overlay) = &mut state.db_overlay {
                let count = overlay.entries.len();
                if count > 0 {
                    overlay.selected = (overlay.selected + 1).min(count - 1);
                }
            }
            Vec::new()
        }
        Action::DbOverlaySelectPrev => {
            if let Some(overlay) = &mut state.db_overlay {
                overlay.selected = overlay.selected.saturating_sub(1);
            }
            Vec::new()
        }
        Action::SubmitDbSelection => submit_db_selection(state),
        Action::DbOverviewLoaded { overview } => {
            if let Some(overlay) = &mut state.db_overlay {
                overlay.selected = overview
                    .entries
                    .iter()
                    .position(|(db, _)| *db == state.current_db)
                    .unwrap_or(0);
                overlay.entries = overview
                    .entries
                    .into_iter()
                    .map(|(db, count)| (db, Some(count)))
                    .collect();
                overlay.database_count_known = overview.database_count_known;
                overlay.loading = false;
            }
            Vec::new()
        }
        Action::DbOverviewFailed { message } => {
            state.db_overlay = None;
            state.status_message = Some(StatusMessage::Error(format!(
                "Failed to load Redis databases: {message}"
            )));
            Vec::new()
        }
        Action::CommandInput(ch) => {
            if state.command_modal.is_open
                && !matches!(state.command_modal.status, CommandStatus::Running)
            {
                hide_command_completion(&mut state.command_modal);
                reset_command_history_navigation(&mut state.command_modal);
                insert_command_modal_char(&mut state.command_modal, ch);
            }
            Vec::new()
        }
        Action::CommandBackspace => {
            if state.command_modal.is_open
                && !matches!(state.command_modal.status, CommandStatus::Running)
            {
                hide_command_completion(&mut state.command_modal);
                reset_command_history_navigation(&mut state.command_modal);
                backspace_command_modal_char(&mut state.command_modal);
            }
            Vec::new()
        }
        Action::CommandCursorLeft => {
            if state.command_modal.is_open
                && !matches!(state.command_modal.status, CommandStatus::Running)
            {
                hide_command_completion(&mut state.command_modal);
                let char_count = state.command_modal.input.chars().count();
                state.command_modal.cursor =
                    state.command_modal.cursor.min(char_count).saturating_sub(1);
            }
            Vec::new()
        }
        Action::CommandCursorRight => {
            if state.command_modal.is_open
                && !matches!(state.command_modal.status, CommandStatus::Running)
            {
                hide_command_completion(&mut state.command_modal);
                let char_count = state.command_modal.input.chars().count();
                state.command_modal.cursor = state
                    .command_modal
                    .cursor
                    .min(char_count)
                    .saturating_add(1)
                    .min(char_count);
            }
            Vec::new()
        }
        Action::CommandPaste(text) => {
            if state.command_modal.is_open
                && !matches!(state.command_modal.status, CommandStatus::Running)
            {
                hide_command_completion(&mut state.command_modal);
                reset_command_history_navigation(&mut state.command_modal);
                insert_command_modal_text(&mut state.command_modal, &text);
            }
            Vec::new()
        }
        Action::CommandHistoryPrev => {
            if state.command_modal.is_open
                && !matches!(state.command_modal.status, CommandStatus::Running)
            {
                hide_command_completion(&mut state.command_modal);
                select_previous_command_history(&mut state.command_modal);
            }
            Vec::new()
        }
        Action::CommandHistoryNext => {
            if state.command_modal.is_open
                && !matches!(state.command_modal.status, CommandStatus::Running)
            {
                hide_command_completion(&mut state.command_modal);
                select_next_command_history(&mut state.command_modal);
            }
            Vec::new()
        }
        Action::CommandCompleteNext => {
            if !state.command_modal.is_open
                || matches!(state.command_modal.status, CommandStatus::Running)
            {
                return Vec::new();
            }

            if state.command_modal.completion.visible {
                let count = state.command_modal.completion.candidates.len();
                if count == 0 {
                    hide_command_completion(&mut state.command_modal);
                } else {
                    state.command_modal.completion.selected =
                        (state.command_modal.completion.selected + 1) % count;
                }
                return Vec::new();
            }

            let prefix = first_command_token(&state.command_modal.input);
            let candidates = complete_command(prefix);
            match candidates.len() {
                0 => {}
                1 => {
                    state.command_modal.completion.candidates = candidates;
                    state.command_modal.completion.selected = 0;
                    state.command_modal.completion.visible = true;
                    accept_command_completion(&mut state.command_modal);
                }
                _ => {
                    state.command_modal.completion.candidates = candidates;
                    state.command_modal.completion.selected = 0;
                    state.command_modal.completion.visible = true;
                }
            }
            Vec::new()
        }
        Action::CommandCompletePrev => {
            if state.command_modal.is_open
                && !matches!(state.command_modal.status, CommandStatus::Running)
                && state.command_modal.completion.visible
            {
                let count = state.command_modal.completion.candidates.len();
                if count == 0 {
                    hide_command_completion(&mut state.command_modal);
                } else {
                    state.command_modal.completion.selected =
                        (state.command_modal.completion.selected + count - 1) % count;
                }
            }
            Vec::new()
        }
        Action::SubmitCommand => {
            if !state.command_modal.is_open {
                return Vec::new();
            }
            if state.command_modal.completion.visible {
                accept_command_completion(&mut state.command_modal);
                return Vec::new();
            }
            let command = state.command_modal.input.trim().to_string();
            if command.is_empty() {
                state.command_modal.status =
                    CommandStatus::Error("Enter a Redis command.".to_string());
                return Vec::new();
            }
            let requires_confirmation =
                match command_requires_confirmation(&command, state.read_only) {
                    Ok(requires_confirmation) => requires_confirmation,
                    Err(e) => {
                        state.command_modal.status = CommandStatus::Error(e.to_string());
                        return Vec::new();
                    }
                };
            if requires_confirmation {
                state.command_modal.status = CommandStatus::Idle;
                state.confirm_state = Some(ConfirmState {
                    op: PendingWrite::Command {
                        command: command.clone(),
                    },
                    prompt: format!("Run this command? {command}"),
                });
                return Vec::new();
            }
            push_command_history(&mut state.command_modal, &command);
            state.command_modal.status = CommandStatus::Running;
            vec![Effect::ExecuteCommand { command }]
        }
        Action::CommandSucceeded { output } => {
            state.command_modal.input.clear();
            state.command_modal.cursor = 0;
            state.command_modal.status = CommandStatus::Success(output);
            if state.value_write_pending.take().is_some() {
                state.status_message =
                    Some(StatusMessage::Success("Redis value updated.".to_string()));
            }
            vec![Effect::Connect {
                dsn: state.dsn.clone(),
                read_only: state.read_only,
            }]
        }
        Action::CommandFailed { message } => {
            if state.value_write_pending.take().is_some() {
                state.status_message = Some(StatusMessage::Error(format!(
                    "Redis value update failed: {message}"
                )));
            }
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
            state.value_scroll_offset = 0;
            state.value_selection = None;
            state.value_edit = None;
            Vec::new()
        }
        Action::ValueFetchFailed { key, message } => {
            if !is_current_key(state, &key) {
                return Vec::new();
            }
            state.value_scroll_offset = 0;
            state.value_state = ValueState::Failed { key, message };
            state.value_selection = None;
            state.value_edit = None;
            Vec::new()
        }
        Action::RequestExportCsv => {
            if overlay_or_modal_open(state) {
                Vec::new()
            } else {
                request_export_csv(state)
            }
        }
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
        Action::ConfirmWrite => confirm_write(state),
        Action::CancelWrite => {
            state.confirm_state = None;
            state.value_write_pending = None;
            state.command_modal.status = CommandStatus::Idle;
            Vec::new()
        }
    }
}

fn reset_for_connect(state: &mut AppState) {
    state.connection_status = ConnectionStatus::Connecting;
    state.keys.clear();
    state.filtered_indices.clear();
    state.search_pattern = "*".to_string();
    state.dbsize = None;
    state.selected_index = 0;
    state.scroll_offset = 0;
    state.value_scroll_offset = 0;
    state.value_state = ValueState::Empty;
    state.value_selection = None;
    state.value_edit = None;
    state.value_write_pending = None;
}

fn key_navigation_blocked(state: &AppState) -> bool {
    state.db_overlay.is_some()
        || state.connection_form.is_some()
        || state.confirm_state.is_some()
        || state.value_selection.is_some()
        || state.value_edit.is_some()
}

fn overlay_or_modal_open(state: &AppState) -> bool {
    state.db_overlay.is_some()
        || state.connection_form.is_some()
        || state.command_modal.is_open
        || state.confirm_state.is_some()
        || state.value_selection.is_some()
        || state.value_edit.is_some()
}

fn push_command_history(modal: &mut CommandModalState, command: &str) {
    if modal.history.last().is_none_or(|last| last != command) {
        modal.history.push(command.to_string());
    }
    reset_command_history_navigation(modal);
}

fn select_previous_command_history(modal: &mut CommandModalState) {
    if modal.history.is_empty() {
        return;
    }

    let index = if let Some(index) = modal.history_cursor {
        index.saturating_sub(1)
    } else {
        modal.history_draft.clone_from(&modal.input);
        modal.history.len() - 1
    };
    modal.history_cursor = Some(index);
    modal.input.clone_from(&modal.history[index]);
    modal.cursor = modal.input.chars().count();
}

fn select_next_command_history(modal: &mut CommandModalState) {
    let Some(index) = modal.history_cursor else {
        return;
    };

    if index + 1 < modal.history.len() {
        let next_index = index + 1;
        modal.history_cursor = Some(next_index);
        modal.input.clone_from(&modal.history[next_index]);
        modal.cursor = modal.input.chars().count();
    } else {
        modal.history_cursor = None;
        modal.input.clone_from(&modal.history_draft);
        modal.cursor = modal.input.chars().count();
        modal.history_draft.clear();
    }
}

fn reset_command_history_navigation(modal: &mut CommandModalState) {
    modal.history_cursor = None;
    modal.history_draft.clear();
}

fn hide_command_completion(modal: &mut CommandModalState) {
    modal.completion.candidates.clear();
    modal.completion.selected = 0;
    modal.completion.visible = false;
}

fn accept_command_completion(modal: &mut CommandModalState) {
    let Some(candidate) = modal
        .completion
        .candidates
        .get(modal.completion.selected)
        .cloned()
    else {
        hide_command_completion(modal);
        return;
    };
    let remainder = command_completion_remainder(&modal.input);
    modal.input = if remainder.is_empty() {
        format!("{candidate} ")
    } else {
        format!("{candidate} {remainder}")
    };
    modal.cursor = candidate.chars().count() + 1;
    hide_command_completion(modal);
    reset_command_history_navigation(modal);
}

fn first_command_token(input: &str) -> &str {
    input.split_whitespace().next().unwrap_or("")
}

fn command_completion_remainder(input: &str) -> &str {
    let Some((token_start, _)) = input.char_indices().find(|(_, ch)| !ch.is_whitespace()) else {
        return "";
    };
    let token_end = input[token_start..]
        .char_indices()
        .find(|(_, ch)| ch.is_whitespace())
        .map_or(input.len(), |(offset, _)| token_start + offset);
    input[token_end..].trim_start()
}

fn insert_command_modal_char(modal: &mut CommandModalState, ch: char) {
    let cursor = modal.cursor.min(modal.input.chars().count());
    let byte_index = byte_index_at_char(&modal.input, cursor);
    modal.input.insert(byte_index, ch);
    modal.cursor = cursor + 1;
}

fn backspace_command_modal_char(modal: &mut CommandModalState) {
    let cursor = modal.cursor.min(modal.input.chars().count());
    if cursor == 0 {
        modal.cursor = 0;
        return;
    }

    let start = byte_index_at_char(&modal.input, cursor - 1);
    let end = byte_index_at_char(&modal.input, cursor);
    modal.input.replace_range(start..end, "");
    modal.cursor = cursor - 1;
}

fn insert_command_modal_text(modal: &mut CommandModalState, text: &str) {
    let cursor = modal.cursor.min(modal.input.chars().count());
    let byte_index = byte_index_at_char(&modal.input, cursor);
    modal.input.insert_str(byte_index, text);
    modal.cursor = cursor + text.chars().count();
}

fn insert_connection_form_char(form: &mut ConnectionFormState, ch: char) {
    let cursor = form.cursor.min(form.dsn.chars().count());
    let byte_index = byte_index_at_char(&form.dsn, cursor);
    form.dsn.insert(byte_index, ch);
    form.cursor = cursor + 1;
}

fn insert_connection_form_text(form: &mut ConnectionFormState, text: &str) {
    let cursor = form.cursor.min(form.dsn.chars().count());
    let byte_index = byte_index_at_char(&form.dsn, cursor);
    form.dsn.insert_str(byte_index, text);
    form.cursor = cursor + text.chars().count();
}

fn backspace_connection_form_char(form: &mut ConnectionFormState) {
    let cursor = form.cursor.min(form.dsn.chars().count());
    if cursor == 0 {
        form.cursor = 0;
        return;
    }

    let start = byte_index_at_char(&form.dsn, cursor - 1);
    let end = byte_index_at_char(&form.dsn, cursor);
    form.dsn.replace_range(start..end, "");
    form.cursor = cursor - 1;
}

fn byte_index_at_char(input: &str, char_index: usize) -> usize {
    input
        .char_indices()
        .nth(char_index)
        .map_or(input.len(), |(index, _)| index)
}

fn insert_value_edit_char(editor: &mut ValueEditState, ch: char) {
    let cursor = editor.cursor.min(editor.input.chars().count());
    let byte_index = byte_index_at_char(&editor.input, cursor);
    editor.input.insert(byte_index, ch);
    editor.cursor = cursor + 1;
}

fn insert_value_edit_text(editor: &mut ValueEditState, text: &str) {
    let cursor = editor.cursor.min(editor.input.chars().count());
    let byte_index = byte_index_at_char(&editor.input, cursor);
    editor.input.insert_str(byte_index, text);
    editor.cursor = cursor + text.chars().count();
}

fn backspace_value_edit(editor: &mut ValueEditState) {
    let cursor = editor.cursor.min(editor.input.chars().count());
    if cursor == 0 {
        editor.cursor = 0;
        return;
    }
    let start = byte_index_at_char(&editor.input, cursor - 1);
    let end = byte_index_at_char(&editor.input, cursor);
    editor.input.replace_range(start..end, "");
    editor.cursor = cursor - 1;
}

fn submit_connection_form(state: &mut AppState) -> Vec<Effect> {
    let Some(form) = state.connection_form.take() else {
        return Vec::new();
    };

    let dsn = form.dsn;
    let read_only = form.read_only;
    state.dsn.clone_from(&dsn);
    state.read_only = read_only;
    state.current_db = RedisDsn::parse(&dsn).map(|parsed| parsed.db).unwrap_or(0);
    reset_for_connect(state);
    vec![Effect::Connect { dsn, read_only }]
}

fn submit_db_selection(state: &mut AppState) -> Vec<Effect> {
    let Some(db) = state
        .db_overlay
        .as_ref()
        .and_then(|overlay| overlay.entries.get(overlay.selected))
        .map(|(db, _)| *db)
    else {
        return Vec::new();
    };

    state.db_overlay = None;
    if db == state.current_db {
        return Vec::new();
    }

    state.current_db = db;
    state.dsn = dsn_with_db(&state.dsn, db);
    reset_for_connect(state);
    vec![
        Effect::SelectDb { db },
        Effect::Connect {
            dsn: state.dsn.clone(),
            read_only: state.read_only,
        },
    ]
}

fn dsn_with_db(dsn: &str, db: u8) -> String {
    if RedisDsn::parse(dsn).is_err() {
        return dsn.to_string();
    }

    let Ok(mut url) = url::Url::parse(dsn.trim()) else {
        return dsn.to_string();
    };
    url.set_path(&format!("/{db}"));
    url.to_string()
}

pub fn key_count(state: &AppState) -> usize {
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

fn value_target_dimensions(value: &RedisValue) -> (usize, usize) {
    match value {
        RedisValue::String(_) => (1, 1),
        RedisValue::List(rows) => (rows.len(), 2),
        RedisValue::Set(rows) => (rows.len(), 1),
        RedisValue::Hash(rows) | RedisValue::ZSet(rows) | RedisValue::Stream(rows) => {
            (rows.len(), 2)
        }
    }
}

fn move_value_selection(state: &mut AppState, row_delta: isize, column_delta: isize) {
    let (Some(mut selection), ValueState::Loaded { value, .. }) =
        (state.value_selection, &state.value_state)
    else {
        return;
    };
    let (rows, columns) = value_target_dimensions(value);
    if rows == 0 || columns == 0 {
        state.value_selection = None;
        return;
    }

    selection.row = if row_delta.is_negative() {
        selection.row.saturating_sub(row_delta.unsigned_abs())
    } else {
        selection
            .row
            .saturating_add(row_delta as usize)
            .min(rows - 1)
    };
    selection.column = if column_delta.is_negative() {
        selection.column.saturating_sub(column_delta.unsigned_abs())
    } else {
        selection
            .column
            .saturating_add(column_delta as usize)
            .min(columns - 1)
    };
    selection.row = selection.row.min(rows - 1);
    selection.column = selection.column.min(columns - 1);
    state.value_selection = Some(selection);

    let visible_rows = state.table_visible_rows.max(1);
    if selection.row < state.value_scroll_offset {
        state.value_scroll_offset = selection.row;
    } else if selection.row >= state.value_scroll_offset.saturating_add(visible_rows) {
        state.value_scroll_offset = selection.row.saturating_sub(visible_rows - 1);
    }
}

fn selected_value_cell(state: &AppState) -> Option<String> {
    let selection = state.value_selection?;
    let ValueState::Loaded { value, .. } = &state.value_state else {
        return None;
    };
    match value {
        RedisValue::String(value) if selection.row == 0 && selection.column == 0 => {
            Some(value.clone())
        }
        RedisValue::List(values) => match selection.column {
            0 => values.get(selection.row).map(|_| selection.row.to_string()),
            1 => values.get(selection.row).cloned(),
            _ => None,
        },
        RedisValue::Set(values) if selection.column == 0 => values.get(selection.row).cloned(),
        RedisValue::String(_) | RedisValue::Set(_) => None,
        RedisValue::Hash(entries) | RedisValue::ZSet(entries) | RedisValue::Stream(entries) => {
            entries
                .get(selection.row)
                .and_then(|(left, right)| match selection.column {
                    0 => Some(left.clone()),
                    1 => Some(right.clone()),
                    _ => None,
                })
        }
    }
}

fn open_value_editor(state: &mut AppState) -> Vec<Effect> {
    let Some(selection) = state.value_selection else {
        return Vec::new();
    };
    let ValueState::Loaded { key, value, .. } = &state.value_state else {
        return Vec::new();
    };

    let editable = match editable_value_target(key, value, selection) {
        Ok(editable) => editable,
        Err(message) => {
            state.status_message = Some(StatusMessage::Info(message.to_string()));
            return Vec::new();
        }
    };
    let (target, input) = editable;
    let cursor = input.chars().count();
    state.value_edit = Some(ValueEditState {
        input,
        cursor,
        target,
    });
    state.status_message = None;
    Vec::new()
}

fn request_external_value_editor(state: &AppState) -> Vec<Effect> {
    let Some(editor) = &state.value_edit else {
        return Vec::new();
    };

    vec![Effect::OpenExternalValueEditor {
        content: editor.input.clone(),
    }]
}

fn editable_value_target(
    key: &str,
    value: &RedisValue,
    selection: ValueSelection,
) -> Result<(ValueEditTarget, String), &'static str> {
    match value {
        RedisValue::String(value) if selection.row == 0 && selection.column == 0 => Ok((
            ValueEditTarget::String {
                key: key.to_string(),
            },
            value.clone(),
        )),
        RedisValue::String(_) => Err("The selected string cell is unavailable."),
        RedisValue::List(values) if selection.column == 1 => values
            .get(selection.row)
            .cloned()
            .map(|value| {
                (
                    ValueEditTarget::List {
                        key: key.to_string(),
                        index: selection.row,
                    },
                    value,
                )
            })
            .ok_or("The selected list item is unavailable."),
        RedisValue::List(_) => Err("List indexes cannot be edited."),
        RedisValue::Set(_) => {
            Err("Set members cannot be edited safely because replacement is non-atomic.")
        }
        RedisValue::Hash(entries) if selection.column == 1 => entries
            .get(selection.row)
            .map(|(field, value)| {
                (
                    ValueEditTarget::Hash {
                        key: key.to_string(),
                        field: field.clone(),
                    },
                    value.clone(),
                )
            })
            .ok_or("The selected hash value is unavailable."),
        RedisValue::Hash(_) => Err("Hash fields cannot be edited."),
        RedisValue::ZSet(entries) if selection.column == 1 => entries
            .get(selection.row)
            .map(|(member, score)| {
                (
                    ValueEditTarget::ZSet {
                        key: key.to_string(),
                        member: member.clone(),
                    },
                    score.clone(),
                )
            })
            .ok_or("The selected sorted-set score is unavailable."),
        RedisValue::ZSet(_) => Err("Sorted-set members cannot be edited."),
        RedisValue::Stream(_) => Err("Stream entries are immutable and cannot be edited."),
    }
}

fn submit_value_edit(state: &mut AppState) -> Vec<Effect> {
    let Some(editor) = &state.value_edit else {
        return Vec::new();
    };
    let command = value_edit_command(&editor.target, &editor.input);
    let requires_confirmation = match command_requires_confirmation(&command, state.read_only) {
        Ok(requires_confirmation) => requires_confirmation,
        Err(error) => {
            state.status_message = Some(StatusMessage::Error(error.to_string()));
            return Vec::new();
        }
    };

    state.value_edit = None;
    state.value_write_pending = Some(());
    if requires_confirmation {
        state.confirm_state = Some(ConfirmState {
            op: PendingWrite::Command { command },
            prompt: "Update the selected Redis value?".to_string(),
        });
        Vec::new()
    } else {
        state.command_modal.status = CommandStatus::Running;
        vec![Effect::ExecuteCommand { command }]
    }
}

fn value_edit_command(target: &ValueEditTarget, value: &str) -> String {
    let value = quote_redis_argument(value);
    match target {
        ValueEditTarget::String { key } => {
            format!("SET {} {value}", quote_redis_argument(key))
        }
        ValueEditTarget::List { key, index } => {
            format!("LSET {} {index} {value}", quote_redis_argument(key))
        }
        ValueEditTarget::Hash { key, field } => format!(
            "HSET {} {} {value}",
            quote_redis_argument(key),
            quote_redis_argument(field)
        ),
        ValueEditTarget::ZSet { key, member } => format!(
            "ZADD {} {value} {}",
            quote_redis_argument(key),
            quote_redis_argument(member)
        ),
    }
}

fn quote_redis_argument(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('"');
    for ch in value.chars() {
        match ch {
            '\\' => quoted.push_str("\\\\"),
            '"' => quoted.push_str("\\\""),
            '\n' => quoted.push_str("\\n"),
            '\t' => quoted.push_str("\\t"),
            '\r' => quoted.push_str("\\r"),
            _ => quoted.push(ch),
        }
    }
    quoted.push('"');
    quoted
}

fn confirm_write(state: &mut AppState) -> Vec<Effect> {
    let Some(confirm_state) = state.confirm_state.take() else {
        return Vec::new();
    };
    match confirm_state.op {
        PendingWrite::Command { command } => {
            if let Err(e) = command_requires_confirmation(&command, state.read_only) {
                if state.value_write_pending.take().is_some() {
                    state.status_message = Some(StatusMessage::Error(e.to_string()));
                }
                state.command_modal.status = CommandStatus::Error(e.to_string());
                return Vec::new();
            }
            if state.value_write_pending.is_some() {
                state.value_selection = None;
            }
            push_command_history(&mut state.command_modal, &command);
            state.command_modal.status = CommandStatus::Running;
            vec![Effect::ExecuteCommand { command }]
        }
    }
}

fn is_current_key(state: &AppState, key: &str) -> bool {
    selected_key(state).as_deref() == Some(key)
}

fn value_row_count(value: &RedisValue) -> usize {
    match value {
        RedisValue::String(value) => line_count(&redis_string_display_value(value)),
        RedisValue::List(rows) | RedisValue::Set(rows) => rows.len(),
        RedisValue::Hash(rows) | RedisValue::ZSet(rows) | RedisValue::Stream(rows) => rows.len(),
    }
}

fn line_count(value: &str) -> usize {
    value.split('\n').count()
}

fn fetch_selected_key(state: &mut AppState) -> Vec<Effect> {
    state.value_scroll_offset = 0;
    state.value_selection = None;
    state.value_edit = None;
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
    Vec::new()
}

fn recompute_filtered_indices(state: &mut AppState) {
    let query = fuzzy_filter_query(&state.search_pattern);
    if query.is_empty() {
        state.filtered_indices = (0..state.keys.len()).collect();
        return;
    }

    let mut matcher = Matcher::new(Config::DEFAULT);
    let pattern = Pattern::parse(&query, CaseMatching::Ignore, Normalization::Smart);

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

fn fuzzy_filter_query(pattern: &str) -> String {
    let query = pattern.trim();
    if query.is_empty() || query == "*" {
        return String::new();
    }

    let mut output = String::new();
    let mut in_bracket_expression = false;
    for ch in query.chars() {
        match ch {
            '[' => in_bracket_expression = true,
            ']' => in_bracket_expression = false,
            '*' | '?' if !in_bracket_expression => {}
            _ if in_bracket_expression => {}
            _ => output.push(ch),
        }
    }
    output
}

fn normalized_search_pattern(pattern: &str) -> String {
    if pattern.is_empty() {
        "*".to_string()
    } else {
        pattern.to_string()
    }
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
    let count = key_count(state);
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
    if key_count(state) == 0 {
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
    current_cli: RwLock<Arc<dyn RedisCli>>,
    factory: Arc<dyn RedisCliFactory>,
    action_tx: mpsc::Sender<Action>,
    clipboard: Arc<Mutex<Option<arboard::Clipboard>>>,
}

impl EffectRunner {
    #[must_use]
    pub fn new(
        initial_cli: Arc<dyn RedisCli>,
        factory: Arc<dyn RedisCliFactory>,
        action_tx: mpsc::Sender<Action>,
    ) -> Self {
        Self {
            current_cli: RwLock::new(initial_cli),
            factory,
            action_tx,
            clipboard: Arc::new(Mutex::new(None)),
        }
    }

    pub async fn run(&self, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::Connect { dsn, read_only } => {
                    let action = match self.connect(&dsn, read_only).await {
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
                Effect::SearchKeys { pattern } => {
                    let cli = self.current_cli().await;
                    let action = match cli.scan_keys(&pattern).await {
                        Ok(keys) => Action::KeysScanned { keys },
                        Err(e) => Action::KeysScanFailed {
                            message: e.to_string(),
                        },
                    };
                    let _ = self.action_tx.send(action).await;
                }
                Effect::ExecuteCommand { command } => {
                    let cli = self.current_cli().await;
                    let action = match cli.execute_command(&command).await {
                        Ok(output) => Action::CommandSucceeded { output },
                        Err(e) => Action::CommandFailed {
                            message: command_error_message(e),
                        },
                    };
                    let _ = self.action_tx.send(action).await;
                }
                Effect::LoadDbOverview => {
                    let cli = self.current_cli().await;
                    let action = match cli.db_overview().await {
                        Ok(overview) => Action::DbOverviewLoaded { overview },
                        Err(e) => Action::DbOverviewFailed {
                            message: e.to_string(),
                        },
                    };
                    let _ = self.action_tx.send(action).await;
                }
                Effect::SelectDb { db } => {
                    let cli = self.current_cli().await;
                    cli.select_db(db);
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
                Effect::CopyToClipboard { text } => {
                    let clipboard = Arc::clone(&self.clipboard);
                    let result = tokio::task::spawn_blocking(move || {
                        let mut owner = clipboard
                            .lock()
                            .map_err(|error| format!("clipboard lock poisoned: {error}"))?;
                        if owner.is_none() {
                            *owner =
                                Some(arboard::Clipboard::new().map_err(|error| error.to_string())?);
                        }
                        owner
                            .as_mut()
                            .ok_or_else(|| {
                                "clipboard owner unavailable after initialization".to_string()
                            })?
                            .set_text(text)
                            .map_err(|error| error.to_string())
                    })
                    .await;
                    let action = match result {
                        Ok(Ok(())) => Action::ClipboardSucceeded,
                        Ok(Err(message)) => Action::ClipboardFailed { message },
                        Err(error) => Action::ClipboardFailed {
                            message: error.to_string(),
                        },
                    };
                    let _ = self.action_tx.send(action).await;
                }
                Effect::OpenExternalValueEditor { .. } => {
                    unreachable!("external editor effects must be handled by the TUI owner")
                }
            }
        }
    }

    async fn current_cli(&self) -> Arc<dyn RedisCli> {
        self.current_cli.read().await.clone()
    }

    async fn connect(
        &self,
        dsn: &str,
        read_only: bool,
    ) -> Result<(Vec<RedisKey>, Option<usize>), crate::infra::RedisCliError> {
        let cli = self.factory.create(dsn, read_only)?;
        *self.current_cli.write().await = cli.clone();
        Self::load_keys(&cli).await
    }

    async fn load_keys(
        cli: &Arc<dyn RedisCli>,
    ) -> Result<(Vec<RedisKey>, Option<usize>), crate::infra::RedisCliError> {
        cli.ping().await?;
        let dbsize = cli.dbsize().await?;
        let keys = cli.scan_keys("*").await?;
        Ok((keys, Some(dbsize)))
    }

    async fn fetch_value(
        &self,
        key: &str,
    ) -> Result<(RedisKind, Option<u64>, RedisValue), crate::infra::RedisCliError> {
        let cli = self.current_cli().await;
        let (kind, ttl) = cli.key_type_and_ttl(key).await?;
        let value = cli.fetch_value(key, kind).await?;
        Ok((kind, ttl, value))
    }
}

fn command_error_message(error: crate::infra::RedisCliError) -> String {
    match error {
        crate::infra::RedisCliError::CommandFailed(message) => message,
        error => error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::infra::{MockRedisCli, MockRedisCliFactory};

    fn key(name: &str) -> RedisKey {
        RedisKey::unknown(name)
    }

    fn set_keys(state: &mut AppState, keys: Vec<RedisKey>) {
        state.keys = keys;
        state.filtered_indices = (0..state.keys.len()).collect();
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
                    dsn: "redis://localhost".to_string(),
                    read_only: false,
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
            assert_eq!(state.filtered_indices, vec![0]);
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
            set_keys(&mut state, vec![key("a"), key("b")]);

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
            set_keys(&mut state, vec![key("a"), key("b")]);
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
        fn value_loaded_backfills_filtered_selection_full_key_metadata() {
            let mut state = AppState::new("redis://localhost");
            set_keys(&mut state, vec![key("a"), key("b")]);
            state.filtered_indices = vec![1];
            state.selected_index = 0;
            state.value_state = ValueState::Loading {
                key: "b".to_string(),
            };

            let effects = reduce(
                &mut state,
                Action::ValueLoaded {
                    key: "b".to_string(),
                    kind: RedisKind::List,
                    ttl: Some(12),
                    value: RedisValue::List(vec!["item".to_string()]),
                },
            );

            assert!(effects.is_empty());
            assert_eq!(state.keys[0].kind, RedisKind::Unknown);
            assert_eq!(state.keys[1].kind, RedisKind::List);
            assert_eq!(state.keys[1].ttl, Some(12));
        }

        #[test]
        fn stale_value_loaded_for_previous_selection_is_ignored() {
            let mut state = AppState::new("redis://localhost");
            set_keys(&mut state, vec![key("a"), key("b")]);
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
            set_keys(&mut state, vec![key("a"), key("b")]);
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
        fn value_scroll_down_clamps_at_last_row() {
            let mut state = AppState::new("redis://localhost");
            state.value_state = ValueState::Loaded {
                key: "items".to_string(),
                kind: RedisKind::List,
                ttl: None,
                value: RedisValue::List(vec!["a".to_string(), "b".to_string(), "c".to_string()]),
            };

            assert!(reduce(&mut state, Action::ValueScrollDown).is_empty());
            assert_eq!(state.value_scroll_offset, 1);
            assert!(reduce(&mut state, Action::ValueScrollDown).is_empty());
            assert_eq!(state.value_scroll_offset, 2);
            assert!(reduce(&mut state, Action::ValueScrollDown).is_empty());
            assert_eq!(state.value_scroll_offset, 2);
        }

        #[test]
        fn value_scroll_down_uses_pretty_json_string_row_count() {
            let mut state = AppState::new("redis://localhost");
            let value = RedisValue::String(r#"{"items":[1,2]}"#.to_string());
            assert_eq!(value_row_count(&value), 6);
            state.value_state = ValueState::Loaded {
                key: "json".to_string(),
                kind: RedisKind::String,
                ttl: None,
                value,
            };

            for _ in 0..6 {
                assert!(reduce(&mut state, Action::ValueScrollDown).is_empty());
            }

            assert_eq!(state.value_scroll_offset, 5);
        }

        #[test]
        fn value_scroll_up_saturates_at_zero() {
            let mut state = AppState::new("redis://localhost");
            state.value_state = ValueState::Loaded {
                key: "items".to_string(),
                kind: RedisKind::List,
                ttl: None,
                value: RedisValue::List(vec!["a".to_string()]),
            };
            state.value_scroll_offset = 1;

            assert!(reduce(&mut state, Action::ValueScrollUp).is_empty());
            assert_eq!(state.value_scroll_offset, 0);
            assert!(reduce(&mut state, Action::ValueScrollUp).is_empty());
            assert_eq!(state.value_scroll_offset, 0);
        }

        #[test]
        fn value_scroll_offset_resets_when_value_loaded_and_selection_changes() {
            let mut state = AppState::new("redis://localhost");
            set_keys(&mut state, vec![key("a"), key("b")]);
            state.value_scroll_offset = 3;
            state.value_state = ValueState::Loading {
                key: "a".to_string(),
            };

            assert!(
                reduce(
                    &mut state,
                    Action::ValueLoaded {
                        key: "a".to_string(),
                        kind: RedisKind::List,
                        ttl: None,
                        value: RedisValue::List(vec!["a".to_string(), "b".to_string()]),
                    },
                )
                .is_empty()
            );
            assert_eq!(state.value_scroll_offset, 0);

            state.value_scroll_offset = 1;
            let effects = reduce(&mut state, Action::SelectNext);

            assert_eq!(
                effects,
                vec![Effect::FetchValue {
                    key: "b".to_string()
                }]
            );
            assert_eq!(state.value_scroll_offset, 0);
            assert_eq!(
                state.value_state,
                ValueState::Loading {
                    key: "b".to_string()
                }
            );
        }

        #[test]
        fn value_scroll_actions_are_noops_when_value_not_loaded() {
            for value_state in [
                ValueState::Empty,
                ValueState::Loading {
                    key: "a".to_string(),
                },
                ValueState::Failed {
                    key: "a".to_string(),
                    message: "missing".to_string(),
                },
            ] {
                let mut state = AppState::new("redis://localhost");
                state.value_state = value_state;
                state.value_scroll_offset = 3;

                assert!(reduce(&mut state, Action::ValueScrollDown).is_empty());
                assert_eq!(state.value_scroll_offset, 3);
                assert!(reduce(&mut state, Action::ValueScrollUp).is_empty());
                assert_eq!(state.value_scroll_offset, 3);
            }
        }

        #[test]
        fn command_modal_open_input_backspace_and_close_updates_state_only() {
            let mut state = AppState::new("redis://localhost");

            assert!(reduce(&mut state, Action::OpenCommandModal).is_empty());
            assert!(state.command_modal.is_open);
            assert_eq!(state.command_modal.input, "");
            assert_eq!(state.command_modal.cursor, 0);
            assert_eq!(state.command_modal.status, CommandStatus::Idle);

            assert!(reduce(&mut state, Action::CommandInput('s')).is_empty());
            assert!(reduce(&mut state, Action::CommandInput('e')).is_empty());
            assert!(reduce(&mut state, Action::CommandInput('t')).is_empty());
            assert_eq!(state.command_modal.input, "set");
            assert_eq!(state.command_modal.cursor, 3);

            assert!(reduce(&mut state, Action::CommandBackspace).is_empty());
            assert_eq!(state.command_modal.input, "se");
            assert_eq!(state.command_modal.cursor, 2);

            assert!(reduce(&mut state, Action::CloseCommandModal).is_empty());
            assert!(!state.command_modal.is_open);
        }

        #[test]
        fn command_modal_edits_at_cursor_position() {
            let mut state = AppState::new("redis://localhost");
            state.command_modal.is_open = true;
            state.command_modal.input = "abcd".to_string();
            state.command_modal.cursor = 2;

            assert!(reduce(&mut state, Action::CommandInput('X')).is_empty());
            assert_eq!(state.command_modal.input, "abXcd");
            assert_eq!(state.command_modal.cursor, 3);

            assert!(reduce(&mut state, Action::CommandBackspace).is_empty());
            assert_eq!(state.command_modal.input, "abcd");
            assert_eq!(state.command_modal.cursor, 2);
        }

        #[test]
        fn command_modal_cursor_left_and_right_clamp_at_bounds() {
            let mut state = AppState::new("redis://localhost");
            state.command_modal.is_open = true;
            state.command_modal.input = "abc".to_string();
            state.command_modal.cursor = 0;

            assert!(reduce(&mut state, Action::CommandCursorLeft).is_empty());
            assert_eq!(state.command_modal.cursor, 0);

            assert!(reduce(&mut state, Action::CommandCursorRight).is_empty());
            assert_eq!(state.command_modal.cursor, 1);

            assert!(reduce(&mut state, Action::CommandCursorRight).is_empty());
            assert!(reduce(&mut state, Action::CommandCursorRight).is_empty());
            assert!(reduce(&mut state, Action::CommandCursorRight).is_empty());
            assert_eq!(state.command_modal.cursor, 3);
        }

        #[test]
        fn command_modal_paste_inserts_at_cursor_position() {
            let mut state = AppState::new("redis://localhost");
            state.command_modal.is_open = true;
            state.command_modal.input = "LR".to_string();
            state.command_modal.cursor = 1;
            state.command_modal.history_cursor = Some(0);
            state.command_modal.history_draft = "draft".to_string();

            assert!(reduce(&mut state, Action::CommandPaste("eft".to_string())).is_empty());

            assert_eq!(state.command_modal.input, "LeftR");
            assert_eq!(state.command_modal.cursor, 4);
            assert_eq!(state.command_modal.history_cursor, None);
            assert_eq!(state.command_modal.history_draft, "");
        }

        #[test]
        fn command_completion_tab_with_multiple_matches_opens_popup() {
            let mut state = AppState::new("redis://localhost");
            state.command_modal.is_open = true;
            state.command_modal.input = "GET".to_string();
            state.command_modal.cursor = 3;

            assert!(reduce(&mut state, Action::CommandCompleteNext).is_empty());

            assert!(state.command_modal.completion.visible);
            assert_eq!(state.command_modal.completion.selected, 0);
            assert!(state.command_modal.completion.candidates.len() > 1);
            assert_eq!(state.command_modal.input, "GET");
        }

        #[test]
        fn command_completion_tab_with_single_match_accepts_candidate() {
            let mut state = AppState::new("redis://localhost");
            state.command_modal.is_open = true;
            state.command_modal.input = "PIN".to_string();
            state.command_modal.cursor = 3;
            state.command_modal.history_cursor = Some(0);
            state.command_modal.history_draft = "draft".to_string();

            assert!(reduce(&mut state, Action::CommandCompleteNext).is_empty());

            assert_eq!(state.command_modal.input, "PING ");
            assert_eq!(state.command_modal.cursor, "PING ".chars().count());
            assert!(!state.command_modal.completion.visible);
            assert!(state.command_modal.completion.candidates.is_empty());
            assert_eq!(state.command_modal.history_cursor, None);
            assert_eq!(state.command_modal.history_draft, "");
        }

        #[test]
        fn command_completion_tab_cycles_selection_when_visible() {
            let mut state = AppState::new("redis://localhost");
            state.command_modal.is_open = true;
            state.command_modal.input = "GET".to_string();
            state.command_modal.cursor = 3;
            assert!(reduce(&mut state, Action::CommandCompleteNext).is_empty());
            let count = state.command_modal.completion.candidates.len();

            assert!(reduce(&mut state, Action::CommandCompleteNext).is_empty());

            assert_eq!(state.command_modal.completion.selected, 1 % count);
        }

        #[test]
        fn command_completion_backtab_cycles_selection_back() {
            let mut state = AppState::new("redis://localhost");
            state.command_modal.is_open = true;
            state.command_modal.input = "GET".to_string();
            state.command_modal.cursor = 3;
            assert!(reduce(&mut state, Action::CommandCompleteNext).is_empty());
            let count = state.command_modal.completion.candidates.len();

            assert!(reduce(&mut state, Action::CommandCompletePrev).is_empty());

            assert_eq!(state.command_modal.completion.selected, count - 1);
        }

        #[test]
        fn command_completion_submit_accepts_visible_candidate_without_submitting() {
            let mut state = AppState::new("redis://localhost");
            state.command_modal.is_open = true;
            state.command_modal.input = "GET".to_string();
            state.command_modal.cursor = 3;
            assert!(reduce(&mut state, Action::CommandCompleteNext).is_empty());

            let effects = reduce(&mut state, Action::SubmitCommand);

            assert!(effects.is_empty());
            assert_eq!(state.command_modal.input, "GET ");
            assert_eq!(state.command_modal.cursor, "GET ".chars().count());
            assert!(!state.command_modal.completion.visible);
            assert_eq!(state.command_modal.status, CommandStatus::Idle);
        }

        #[test]
        fn command_completion_escape_hides_popup_and_keeps_modal_open() {
            let mut state = AppState::new("redis://localhost");
            state.command_modal.is_open = true;
            state.command_modal.input = "GET".to_string();
            state.command_modal.cursor = 3;
            assert!(reduce(&mut state, Action::CommandCompleteNext).is_empty());

            assert!(reduce(&mut state, Action::CloseCommandModal).is_empty());

            assert!(state.command_modal.is_open);
            assert!(!state.command_modal.completion.visible);
            assert!(state.command_modal.completion.candidates.is_empty());
        }

        #[test]
        fn command_completion_input_hides_popup() {
            let mut state = AppState::new("redis://localhost");
            state.command_modal.is_open = true;
            state.command_modal.input = "GET".to_string();
            state.command_modal.cursor = 3;
            assert!(reduce(&mut state, Action::CommandCompleteNext).is_empty());

            assert!(reduce(&mut state, Action::CommandInput('X')).is_empty());

            assert_eq!(state.command_modal.input, "GETX");
            assert!(!state.command_modal.completion.visible);
            assert!(state.command_modal.completion.candidates.is_empty());
        }

        #[test]
        fn open_connection_form_prefills_current_connection() {
            let mut state = AppState::with_read_only("redis://localhost:6380/2", true);

            let effects = reduce(&mut state, Action::OpenConnectionForm);

            assert!(effects.is_empty());
            assert_eq!(
                state.connection_form,
                Some(ConnectionFormState {
                    dsn: "redis://localhost:6380/2".to_string(),
                    read_only: true,
                    cursor: "redis://localhost:6380/2".chars().count(),
                })
            );
        }

        #[test]
        fn connection_form_input_backspace_toggle_and_cancel_update_state_only() {
            let mut state = AppState::with_read_only("redis://localhost", true);
            state.connection_form = Some(ConnectionFormState {
                dsn: "redis://localhost".to_string(),
                read_only: true,
                cursor: "redis://localhost".chars().count(),
            });

            assert!(reduce(&mut state, Action::ConnectionFormInput('/')).is_empty());
            assert!(reduce(&mut state, Action::ConnectionFormInput('1')).is_empty());
            assert_eq!(
                state.connection_form.as_ref().map(|form| form.dsn.as_str()),
                Some("redis://localhost/1")
            );

            assert!(reduce(&mut state, Action::ConnectionFormBackspace).is_empty());
            assert_eq!(
                state.connection_form.as_ref().map(|form| form.dsn.as_str()),
                Some("redis://localhost/")
            );

            assert!(reduce(&mut state, Action::ToggleConnectionFormReadOnly).is_empty());
            assert_eq!(
                state.connection_form.as_ref().map(|form| form.read_only),
                Some(false)
            );

            assert!(reduce(&mut state, Action::CancelConnectionForm).is_empty());
            assert_eq!(state.connection_form, None);
            assert_eq!(state.dsn, "redis://localhost");
            assert!(state.read_only);
        }

        #[test]
        fn connection_form_edits_at_cursor_position() {
            let mut state = AppState::new("redis://localhost");
            state.connection_form = Some(ConnectionFormState {
                dsn: "abcd".to_string(),
                read_only: false,
                cursor: 2,
            });

            assert!(reduce(&mut state, Action::ConnectionFormInput('X')).is_empty());
            assert_eq!(
                state.connection_form.as_ref().map(|form| form.dsn.as_str()),
                Some("abXcd")
            );
            assert_eq!(
                state.connection_form.as_ref().map(|form| form.cursor),
                Some(3)
            );

            assert!(reduce(&mut state, Action::ConnectionFormBackspace).is_empty());
            assert_eq!(
                state.connection_form.as_ref().map(|form| form.dsn.as_str()),
                Some("abcd")
            );
            assert_eq!(
                state.connection_form.as_ref().map(|form| form.cursor),
                Some(2)
            );
        }

        #[test]
        fn connection_form_paste_inserts_unicode_at_character_cursor_only() {
            let mut state = AppState::new("redis://localhost");
            state.connection_form = Some(ConnectionFormState {
                dsn: "redis://ホスト/0".to_string(),
                read_only: true,
                cursor: "redis://ホ".chars().count(),
            });
            state.command_modal.is_open = true;
            state.command_modal.input = "PING".to_string();
            state.command_modal.cursor = 2;

            assert!(reduce(&mut state, Action::ConnectionFormPaste("名🔐".to_string())).is_empty());

            let form = state.connection_form.as_ref().unwrap();
            assert_eq!(form.dsn, "redis://ホ名🔐スト/0");
            assert_eq!(form.cursor, "redis://ホ名🔐".chars().count());
            assert!(form.read_only);
            assert_eq!(state.dsn, "redis://localhost");
            assert!(!state.read_only);
            assert_eq!(state.command_modal.input, "PING");
            assert_eq!(state.command_modal.cursor, 2);
        }

        #[test]
        fn connection_form_cursor_left_and_right_clamp_at_bounds() {
            let mut state = AppState::new("redis://localhost");
            state.connection_form = Some(ConnectionFormState {
                dsn: "abc".to_string(),
                read_only: false,
                cursor: 0,
            });

            assert!(reduce(&mut state, Action::ConnectionFormCursorLeft).is_empty());
            assert_eq!(
                state.connection_form.as_ref().map(|form| form.cursor),
                Some(0)
            );

            assert!(reduce(&mut state, Action::ConnectionFormCursorRight).is_empty());
            assert_eq!(
                state.connection_form.as_ref().map(|form| form.cursor),
                Some(1)
            );

            assert!(reduce(&mut state, Action::ConnectionFormCursorRight).is_empty());
            assert!(reduce(&mut state, Action::ConnectionFormCursorRight).is_empty());
            assert!(reduce(&mut state, Action::ConnectionFormCursorRight).is_empty());
            assert_eq!(
                state.connection_form.as_ref().map(|form| form.cursor),
                Some(3)
            );
        }

        #[test]
        fn submit_connection_form_updates_state_and_emits_connect() {
            let mut state = AppState::new("redis://localhost:6379/0");
            state.keys = vec![key("a"), key("b")];
            state.selected_index = 1;
            state.scroll_offset = 1;
            state.dbsize = Some(2);
            state.value_state = ValueState::Loaded {
                key: "b".to_string(),
                kind: RedisKind::String,
                ttl: None,
                value: RedisValue::String("value".to_string()),
            };
            state.connection_form = Some(ConnectionFormState {
                dsn: "redis://cache.example.com:6380/4".to_string(),
                read_only: true,
                cursor: "redis://cache.example.com:6380/4".chars().count(),
            });

            let effects = reduce(&mut state, Action::SubmitConnectionForm);

            assert_eq!(state.connection_form, None);
            assert_eq!(state.dsn, "redis://cache.example.com:6380/4");
            assert!(state.read_only);
            assert_eq!(state.current_db, 4);
            assert_eq!(state.connection_status, ConnectionStatus::Connecting);
            assert!(state.keys.is_empty());
            assert_eq!(state.selected_index, 0);
            assert_eq!(state.scroll_offset, 0);
            assert_eq!(state.dbsize, None);
            assert_eq!(state.value_state, ValueState::Empty);
            assert_eq!(
                effects,
                vec![Effect::Connect {
                    dsn: "redis://cache.example.com:6380/4".to_string(),
                    read_only: true,
                }]
            );
        }

        #[test]
        fn submit_read_command_emits_execute_command_and_sets_running() {
            let mut state = AppState::new("redis://localhost");
            state.command_modal.is_open = true;
            state.command_modal.input = " get k ".to_string();

            let effects = reduce(&mut state, Action::SubmitCommand);

            assert_eq!(
                effects,
                vec![Effect::ExecuteCommand {
                    command: "get k".to_string(),
                }]
            );
            assert_eq!(state.command_modal.status, CommandStatus::Running);
        }

        #[test]
        fn submit_command_appends_history_and_skips_consecutive_duplicates() {
            let mut state = AppState::new("redis://localhost");
            state.command_modal.is_open = true;
            state.command_modal.input = " get k ".to_string();

            assert_eq!(
                reduce(&mut state, Action::SubmitCommand),
                vec![Effect::ExecuteCommand {
                    command: "get k".to_string(),
                }]
            );
            assert_eq!(state.command_modal.history, vec!["get k".to_string()]);
            assert_eq!(state.command_modal.history_cursor, None);

            state.command_modal.status = CommandStatus::Idle;
            state.command_modal.input = "get k".to_string();
            assert_eq!(
                reduce(&mut state, Action::SubmitCommand),
                vec![Effect::ExecuteCommand {
                    command: "get k".to_string(),
                }]
            );
            assert_eq!(state.command_modal.history, vec!["get k".to_string()]);

            state.command_modal.status = CommandStatus::Idle;
            state.command_modal.input = "get other".to_string();
            assert_eq!(
                reduce(&mut state, Action::SubmitCommand),
                vec![Effect::ExecuteCommand {
                    command: "get other".to_string(),
                }]
            );
            assert_eq!(
                state.command_modal.history,
                vec!["get k".to_string(), "get other".to_string()]
            );
        }

        #[test]
        fn command_history_prev_next_restore_draft_and_clamp_at_bounds() {
            let mut state = AppState::new("redis://localhost");
            state.command_modal.is_open = true;
            state.command_modal.input = "draft".to_string();
            state.command_modal.history = vec!["get a".to_string(), "get b".to_string()];

            assert!(reduce(&mut state, Action::CommandHistoryPrev).is_empty());
            assert_eq!(state.command_modal.input, "get b");
            assert_eq!(state.command_modal.cursor, "get b".chars().count());
            assert_eq!(state.command_modal.history_cursor, Some(1));
            assert_eq!(state.command_modal.history_draft, "draft");

            assert!(reduce(&mut state, Action::CommandHistoryPrev).is_empty());
            assert_eq!(state.command_modal.input, "get a");
            assert_eq!(state.command_modal.cursor, "get a".chars().count());
            assert_eq!(state.command_modal.history_cursor, Some(0));

            assert!(reduce(&mut state, Action::CommandHistoryPrev).is_empty());
            assert_eq!(state.command_modal.input, "get a");
            assert_eq!(state.command_modal.cursor, "get a".chars().count());
            assert_eq!(state.command_modal.history_cursor, Some(0));

            assert!(reduce(&mut state, Action::CommandHistoryNext).is_empty());
            assert_eq!(state.command_modal.input, "get b");
            assert_eq!(state.command_modal.cursor, "get b".chars().count());
            assert_eq!(state.command_modal.history_cursor, Some(1));

            assert!(reduce(&mut state, Action::CommandHistoryNext).is_empty());
            assert_eq!(state.command_modal.input, "draft");
            assert_eq!(state.command_modal.cursor, "draft".chars().count());
            assert_eq!(state.command_modal.history_cursor, None);

            assert!(reduce(&mut state, Action::CommandHistoryNext).is_empty());
            assert_eq!(state.command_modal.input, "draft");
            assert_eq!(state.command_modal.cursor, "draft".chars().count());
            assert_eq!(state.command_modal.history_cursor, None);
        }

        #[test]
        fn command_history_navigation_is_noop_without_history() {
            let mut state = AppState::new("redis://localhost");
            state.command_modal.is_open = true;
            state.command_modal.input = "draft".to_string();

            assert!(reduce(&mut state, Action::CommandHistoryPrev).is_empty());
            assert_eq!(state.command_modal.input, "draft");
            assert_eq!(state.command_modal.history_cursor, None);

            assert!(reduce(&mut state, Action::CommandHistoryNext).is_empty());
            assert_eq!(state.command_modal.input, "draft");
            assert_eq!(state.command_modal.history_cursor, None);
        }

        #[test]
        fn submit_write_command_opens_confirmation_without_effect() {
            let mut state = AppState::new("redis://localhost");
            state.command_modal.is_open = true;
            state.command_modal.input = " DEL key ".to_string();

            let effects = reduce(&mut state, Action::SubmitCommand);

            assert!(effects.is_empty());
            assert_eq!(state.command_modal.status, CommandStatus::Idle);
            assert_eq!(
                state.confirm_state,
                Some(ConfirmState {
                    op: PendingWrite::Command {
                        command: "DEL key".to_string(),
                    },
                    prompt: "Run this command? DEL key".to_string(),
                })
            );
        }

        #[test]
        fn confirming_write_command_executes_pending_command() {
            let mut state = AppState::new("redis://localhost");
            state.command_modal.is_open = true;
            state.command_modal.input = "DEL key".to_string();
            state.confirm_state = Some(ConfirmState {
                op: PendingWrite::Command {
                    command: "DEL key".to_string(),
                },
                prompt: "Run this command? DEL key".to_string(),
            });

            let effects = reduce(&mut state, Action::ConfirmWrite);

            assert_eq!(
                effects,
                vec![Effect::ExecuteCommand {
                    command: "DEL key".to_string(),
                }]
            );
            assert_eq!(state.confirm_state, None);
            assert_eq!(state.command_modal.status, CommandStatus::Running);
        }

        #[test]
        fn canceling_write_confirmation_returns_to_command_modal() {
            let mut state = AppState::new("redis://localhost");
            state.command_modal.is_open = true;
            state.command_modal.input = "SET key value".to_string();
            state.confirm_state = Some(ConfirmState {
                op: PendingWrite::Command {
                    command: "SET key value".to_string(),
                },
                prompt: "Run this command? SET key value".to_string(),
            });

            let effects = reduce(&mut state, Action::CancelWrite);

            assert!(effects.is_empty());
            assert_eq!(state.confirm_state, None);
            assert!(state.command_modal.is_open);
            assert_eq!(state.command_modal.input, "SET key value");
            assert_eq!(state.command_modal.status, CommandStatus::Idle);
        }

        #[test]
        fn read_only_submit_write_command_blocks_without_confirmation() {
            let mut state = AppState::with_read_only("redis://localhost", true);
            state.command_modal.is_open = true;
            state.command_modal.input = "SET key value".to_string();

            let effects = reduce(&mut state, Action::SubmitCommand);

            assert!(effects.is_empty());
            assert_eq!(state.confirm_state, None);
            assert_eq!(
                state.command_modal.status,
                CommandStatus::Error("SET is blocked by read-only mode".to_string())
            );
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
            state.command_modal.cursor = "set k v".chars().count();
            state.command_modal.status = CommandStatus::Running;

            let effects = reduce(
                &mut state,
                Action::CommandSucceeded {
                    output: "OK\n".to_string(),
                },
            );

            assert_eq!(state.command_modal.input, "");
            assert_eq!(state.command_modal.cursor, 0);
            assert_eq!(
                state.command_modal.status,
                CommandStatus::Success("OK\n".to_string())
            );
            assert_eq!(
                effects,
                vec![Effect::Connect {
                    dsn: "redis://localhost".to_string(),
                    read_only: false,
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
        fn open_db_overlay_on_connected_state_loads_overview() {
            let mut state = AppState::new("redis://localhost");
            state.connection_status = ConnectionStatus::Connected;

            let effects = reduce(&mut state, Action::OpenDbOverlay);

            assert_eq!(effects, vec![Effect::LoadDbOverview]);
            assert_eq!(
                state.db_overlay,
                Some(DbOverlayState {
                    entries: Vec::new(),
                    selected: 0,
                    loading: true,
                    database_count_known: true,
                })
            );
        }

        #[test]
        fn db_overview_loaded_populates_entries_and_selects_current_db() {
            let mut state = AppState::new("redis://localhost:6379/2");
            state.db_overlay = Some(DbOverlayState {
                entries: Vec::new(),
                selected: 0,
                loading: true,
                database_count_known: true,
            });

            let effects = reduce(
                &mut state,
                Action::DbOverviewLoaded {
                    overview: DbOverview {
                        entries: vec![(0, 1), (2, 7), (3, 0)],
                        database_count_known: true,
                    },
                },
            );

            assert!(effects.is_empty());
            assert_eq!(
                state.db_overlay,
                Some(DbOverlayState {
                    entries: vec![(0, Some(1)), (2, Some(7)), (3, Some(0))],
                    selected: 1,
                    loading: false,
                    database_count_known: true,
                })
            );
        }

        #[test]
        fn db_overview_loaded_with_unknown_count_marks_overlay_unknown() {
            let mut state = AppState::new("redis://localhost:6379/2");
            state.db_overlay = Some(DbOverlayState {
                entries: Vec::new(),
                selected: 0,
                loading: true,
                database_count_known: true,
            });

            let effects = reduce(
                &mut state,
                Action::DbOverviewLoaded {
                    overview: DbOverview {
                        entries: vec![(1, 4), (2, 0)],
                        database_count_known: false,
                    },
                },
            );

            assert!(effects.is_empty());
            assert_eq!(
                state.db_overlay,
                Some(DbOverlayState {
                    entries: vec![(1, Some(4)), (2, Some(0))],
                    selected: 1,
                    loading: false,
                    database_count_known: false,
                })
            );
        }

        #[test]
        fn submit_db_selection_updates_current_db_resets_view_and_reconnects() {
            let mut state = AppState::new("redis://localhost:6379/0");
            state.connection_status = ConnectionStatus::Connected;
            state.keys = vec![key("a"), key("b")];
            state.search_pattern = "user:*".to_string();
            state.selected_index = 1;
            state.scroll_offset = 1;
            state.dbsize = Some(2);
            state.value_state = ValueState::Loaded {
                key: "b".to_string(),
                kind: RedisKind::String,
                ttl: None,
                value: RedisValue::String("value".to_string()),
            };
            state.db_overlay = Some(DbOverlayState {
                entries: vec![(0, Some(2)), (3, Some(5))],
                selected: 1,
                loading: false,
                database_count_known: true,
            });

            let effects = reduce(&mut state, Action::SubmitDbSelection);

            assert_eq!(state.current_db, 3);
            assert_eq!(state.dsn, "redis://localhost:6379/3");
            assert_eq!(state.connection_status, ConnectionStatus::Connecting);
            assert!(state.keys.is_empty());
            assert_eq!(state.search_pattern, "*");
            assert_eq!(state.selected_index, 0);
            assert_eq!(state.scroll_offset, 0);
            assert_eq!(state.dbsize, None);
            assert_eq!(state.value_state, ValueState::Empty);
            assert_eq!(state.db_overlay, None);
            assert_eq!(
                effects,
                vec![
                    Effect::SelectDb { db: 3 },
                    Effect::Connect {
                        dsn: "redis://localhost:6379/3".to_string(),
                        read_only: false,
                    },
                ]
            );
        }

        #[test]
        fn submit_db_selection_preserves_tls_and_encoded_credentials() {
            let original = "rediss://app%40prod:p%40ss%2Fword@cache.example.amazonaws.com:6380/0";
            let expected = "rediss://app%40prod:p%40ss%2Fword@cache.example.amazonaws.com:6380/3";
            let mut state = AppState::new(original);
            state.db_overlay = Some(DbOverlayState {
                entries: vec![(0, Some(2)), (3, Some(5))],
                selected: 1,
                loading: false,
                database_count_known: true,
            });

            let effects = reduce(&mut state, Action::SubmitDbSelection);

            assert_eq!(state.dsn, expected);
            assert_eq!(
                effects,
                vec![
                    Effect::SelectDb { db: 3 },
                    Effect::Connect {
                        dsn: expected.to_string(),
                        read_only: false,
                    },
                ]
            );
        }

        #[test]
        fn dsn_with_db_preserves_invalid_input() {
            assert_eq!(dsn_with_db("not a Redis DSN", 3), "not a Redis DSN");
        }

        #[test]
        fn submit_db_selection_same_db_closes_overlay_without_reconnect() {
            let mut state = AppState::new("redis://localhost:6379/1");
            state.connection_status = ConnectionStatus::Connected;
            state.keys = vec![key("a")];
            state.db_overlay = Some(DbOverlayState {
                entries: vec![(0, Some(0)), (1, Some(1))],
                selected: 1,
                loading: false,
                database_count_known: true,
            });

            let effects = reduce(&mut state, Action::SubmitDbSelection);

            assert!(effects.is_empty());
            assert_eq!(state.current_db, 1);
            assert_eq!(state.keys, vec![key("a")]);
            assert_eq!(state.db_overlay, None);
        }

        #[test]
        fn close_db_overlay_clears_overlay() {
            let mut state = AppState::new("redis://localhost");
            state.db_overlay = Some(DbOverlayState {
                entries: vec![(0, Some(1))],
                selected: 0,
                loading: false,
                database_count_known: true,
            });

            let effects = reduce(&mut state, Action::CloseDbOverlay);

            assert!(effects.is_empty());
            assert_eq!(state.db_overlay, None);
        }

        #[test]
        fn db_overlay_navigation_does_not_move_key_selection() {
            let mut state = AppState::new("redis://localhost");
            state.keys = vec![key("a"), key("b")];
            state.selected_index = 0;
            state.db_overlay = Some(DbOverlayState {
                entries: vec![(0, Some(2)), (1, Some(5))],
                selected: 0,
                loading: false,
                database_count_known: true,
            });

            let effects = reduce(&mut state, Action::DbOverlaySelectNext);

            assert!(effects.is_empty());
            assert_eq!(state.selected_index, 0);
            assert_eq!(
                state.db_overlay,
                Some(DbOverlayState {
                    entries: vec![(0, Some(2)), (1, Some(5))],
                    selected: 1,
                    loading: false,
                    database_count_known: true,
                })
            );
            assert!(reduce(&mut state, Action::SelectNext).is_empty());
            assert_eq!(state.selected_index, 0);
        }

        #[test]
        fn opening_filter_clears_implicit_wildcard_for_input() {
            let mut state = AppState::new("redis://localhost");

            let effects = reduce(&mut state, Action::OpenFilter);

            assert!(effects.is_empty());
            assert!(state.filter_active);
            assert!(state.search_pattern.is_empty());
        }

        #[test]
        fn filter_input_edits_pattern_without_scanning() {
            let mut state = AppState::new("redis://localhost");
            set_keys(
                &mut state,
                vec![
                    key("user:1"),
                    key("session:1"),
                    key("user:settings"),
                    key("cache:1"),
                ],
            );

            assert!(reduce(&mut state, Action::OpenFilter).is_empty());
            let effects = reduce(&mut state, Action::FilterInput('u'));

            assert_eq!(state.search_pattern, "u");
            assert_eq!(
                state.keys,
                vec![
                    key("user:1"),
                    key("session:1"),
                    key("user:settings"),
                    key("cache:1"),
                ]
            );
            assert_eq!(state.filtered_indices, vec![0, 2]);
            assert_eq!(state.selected_index, 0);
            assert!(effects.is_empty());
        }

        #[test]
        fn filtered_navigation_fetches_visible_selection_key() {
            let mut state = AppState::new("redis://localhost");
            set_keys(
                &mut state,
                vec![key("alpha"), key("user:one"), key("beta"), key("user:two")],
            );
            state.filter_active = true;
            state.search_pattern.clear();

            assert!(reduce(&mut state, Action::FilterInput('u')).is_empty());
            assert!(reduce(&mut state, Action::FilterInput('s')).is_empty());
            assert_eq!(state.filtered_indices, vec![1, 3]);

            let effects = reduce(&mut state, Action::SelectNext);

            assert_eq!(state.selected_index, 1);
            assert_eq!(
                effects,
                vec![Effect::FetchValue {
                    key: "user:two".to_string()
                }]
            );
        }

        #[test]
        fn commit_filter_emits_search_keys_with_typed_pattern() {
            let mut state = AppState::new("redis://localhost");
            state.filter_active = true;
            state.search_pattern = "user:*".to_string();

            let effects = reduce(&mut state, Action::CommitFilter);

            assert!(!state.filter_active);
            assert_eq!(state.search_pattern, "user:*");
            assert_eq!(
                effects,
                vec![Effect::SearchKeys {
                    pattern: "user:*".to_string()
                }]
            );
        }

        #[test]
        fn commit_filter_uses_wildcard_for_empty_pattern() {
            let mut state = AppState::new("redis://localhost");
            state.filter_active = true;
            state.search_pattern.clear();

            let effects = reduce(&mut state, Action::CommitFilter);

            assert_eq!(state.search_pattern, "*");
            assert_eq!(
                effects,
                vec![Effect::SearchKeys {
                    pattern: "*".to_string()
                }]
            );
        }

        #[test]
        fn keys_scanned_replaces_keys_resets_selection_and_fetches_first_key() {
            let mut state = AppState::new("redis://localhost");
            state.keys = vec![key("old:a"), key("old:b"), key("old:c")];
            state.selected_index = 1;
            state.scroll_offset = 1;
            state.value_state = ValueState::Loading {
                key: "old:b".to_string(),
            };

            let effects = reduce(
                &mut state,
                Action::KeysScanned {
                    keys: vec![key("user:1"), key("user:2")],
                },
            );

            assert_eq!(state.keys, vec![key("user:1"), key("user:2")]);
            assert_eq!(state.filtered_indices, vec![0, 1]);
            assert_eq!(state.selected_index, 0);
            assert_eq!(state.scroll_offset, 0);
            assert_eq!(
                state.value_state,
                ValueState::Loading {
                    key: "user:1".to_string()
                }
            );
            assert_eq!(
                effects,
                vec![Effect::FetchValue {
                    key: "user:1".to_string()
                }]
            );
        }

        #[test]
        fn keys_scanned_with_empty_results_clears_value_state() {
            let mut state = AppState::new("redis://localhost");
            state.keys = vec![key("old")];
            state.value_state = ValueState::Loaded {
                key: "old".to_string(),
                kind: RedisKind::String,
                ttl: None,
                value: RedisValue::String("value".to_string()),
            };

            let effects = reduce(&mut state, Action::KeysScanned { keys: Vec::new() });

            assert!(state.keys.is_empty());
            assert!(state.filtered_indices.is_empty());
            assert_eq!(state.selected_index, 0);
            assert_eq!(state.scroll_offset, 0);
            assert_eq!(state.value_state, ValueState::Empty);
            assert!(effects.is_empty());
        }

        #[test]
        fn clearing_filter_resets_pattern_and_searches_all_keys() {
            let mut state = AppState::new("redis://localhost");
            set_keys(&mut state, vec![key("user:1"), key("session:1")]);
            state.search_pattern = "user:*".to_string();
            state.filtered_indices = vec![0];
            state.filter_active = true;

            let effects = reduce(&mut state, Action::ClearFilter);

            assert_eq!(state.search_pattern, "*");
            assert_eq!(state.filtered_indices, vec![0, 1]);
            assert!(!state.filter_active);
            assert_eq!(
                effects,
                vec![Effect::SearchKeys {
                    pattern: "*".to_string()
                }]
            );
        }

        #[test]
        fn reload_searches_with_current_pattern() {
            let mut state = AppState::new("redis://localhost");
            state.search_pattern = "user:*".to_string();

            let effects = reduce(&mut state, Action::Reload);

            assert_eq!(
                effects,
                vec![Effect::SearchKeys {
                    pattern: "user:*".to_string()
                }]
            );
        }

        #[test]
        fn reload_is_blocked_by_overlays_modals_and_active_filter() {
            let mut state = AppState::new("redis://localhost");
            state.search_pattern = "user:*".to_string();
            state.filter_active = true;
            assert!(reduce(&mut state, Action::Reload).is_empty());

            let mut state = AppState::new("redis://localhost");
            state.connection_form = Some(ConnectionFormState {
                dsn: "redis://localhost".to_string(),
                read_only: false,
                cursor: "redis://localhost".chars().count(),
            });
            assert!(reduce(&mut state, Action::Reload).is_empty());

            let mut state = AppState::new("redis://localhost");
            state.command_modal.is_open = true;
            assert!(reduce(&mut state, Action::Reload).is_empty());

            let mut state = AppState::new("redis://localhost");
            state.db_overlay = Some(DbOverlayState {
                entries: Vec::new(),
                selected: 0,
                loading: true,
                database_count_known: true,
            });
            assert!(reduce(&mut state, Action::Reload).is_empty());

            let mut state = AppState::new("redis://localhost");
            state.confirm_state = Some(ConfirmState {
                op: PendingWrite::Command {
                    command: "DEL key".to_string(),
                },
                prompt: "Run this command? DEL key".to_string(),
            });
            assert!(reduce(&mut state, Action::Reload).is_empty());
        }

        #[test]
        fn keys_scanned_reapplies_active_fuzzy_filter_to_new_base_set() {
            let mut state = AppState::new("redis://localhost");
            state.search_pattern = "acct".to_string();
            state.filter_active = false;

            let effects = reduce(
                &mut state,
                Action::KeysScanned {
                    keys: vec![key("user:acct:1"), key("session:1"), key("user:profile:1")],
                },
            );

            assert_eq!(
                state.keys,
                vec![key("user:acct:1"), key("session:1"), key("user:profile:1"),]
            );
            assert_eq!(state.filtered_indices, vec![0]);
            assert_eq!(
                effects,
                vec![Effect::FetchValue {
                    key: "user:acct:1".to_string()
                }]
            );
        }

        #[test]
        fn keys_scanned_keeps_backend_glob_matches_visible() {
            let mut state = AppState::new("redis://localhost");
            state.search_pattern = "user:*:[ab]".to_string();

            let effects = reduce(
                &mut state,
                Action::KeysScanned {
                    keys: vec![key("user:1:a"), key("user:2:b")],
                },
            );

            assert_eq!(state.filtered_indices, vec![0, 1]);
            assert_eq!(
                effects,
                vec![Effect::FetchValue {
                    key: "user:1:a".to_string()
                }]
            );
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

        #[test]
        fn value_selection_activates_navigates_with_bounds_and_deactivates() {
            let mut state = AppState::new("redis://localhost");
            state.value_state = ValueState::Loaded {
                key: "items".to_string(),
                kind: RedisKind::Hash,
                ttl: None,
                value: RedisValue::Hash(vec![
                    ("a".to_string(), "one".to_string()),
                    ("b".to_string(), "two".to_string()),
                ]),
            };

            assert!(reduce(&mut state, Action::ActivateValue).is_empty());
            assert_eq!(
                state.value_selection,
                Some(ValueSelection { row: 0, column: 0 })
            );
            reduce(&mut state, Action::ValueSelectRight);
            reduce(&mut state, Action::ValueSelectRight);
            reduce(&mut state, Action::ValueSelectNext);
            reduce(&mut state, Action::ValueSelectNext);
            assert_eq!(
                state.value_selection,
                Some(ValueSelection { row: 1, column: 1 })
            );
            reduce(&mut state, Action::DeactivateValue);
            assert_eq!(state.value_selection, None);
        }

        #[test]
        fn yank_uses_selected_key_or_table_value_cell() {
            let mut state = AppState::new("redis://localhost");
            set_keys(&mut state, vec![key("profile key")]);

            assert_eq!(
                reduce(&mut state, Action::YankSelected),
                vec![Effect::CopyToClipboard {
                    text: "profile key".to_string()
                }]
            );

            state.value_state = ValueState::Loaded {
                key: "profile key".to_string(),
                kind: RedisKind::Hash,
                ttl: None,
                value: RedisValue::Hash(vec![("name".to_string(), "Ada Lovelace".to_string())]),
            };
            state.value_selection = Some(ValueSelection { row: 0, column: 1 });
            assert_eq!(
                reduce(&mut state, Action::YankSelected),
                vec![Effect::CopyToClipboard {
                    text: "Ada Lovelace".to_string()
                }]
            );

            reduce(&mut state, Action::ClipboardSucceeded);
            assert_eq!(
                state.status_message,
                Some(StatusMessage::Success("Copied to clipboard.".to_string()))
            );
            reduce(
                &mut state,
                Action::ClipboardFailed {
                    message: "unavailable".to_string(),
                },
            );
            assert_eq!(
                state.status_message,
                Some(StatusMessage::Error(
                    "Clipboard copy failed: unavailable".to_string()
                ))
            );
        }

        #[test]
        fn json_string_yank_uses_raw_value_while_display_remains_pretty() {
            let mut state = AppState::new("redis://localhost");
            let raw = r#"{"items":[1,2]}"#;
            state.value_state = ValueState::Loaded {
                key: "json".to_string(),
                kind: RedisKind::String,
                ttl: None,
                value: RedisValue::String(raw.to_string()),
            };
            state.value_selection = Some(ValueSelection { row: 0, column: 0 });

            assert_eq!(
                redis_string_display_value(raw),
                "{\n  \"items\": [\n    1,\n    2\n  ]\n}"
            );
            assert_eq!(
                reduce(&mut state, Action::YankSelected),
                vec![Effect::CopyToClipboard {
                    text: raw.to_string()
                }]
            );
        }

        #[test]
        fn unsupported_value_edit_targets_explain_why_without_writing() {
            let mut state = AppState::new("redis://localhost");
            state.value_state = ValueState::Loaded {
                key: "members".to_string(),
                kind: RedisKind::Set,
                ttl: None,
                value: RedisValue::Set(vec!["member".to_string()]),
            };
            state.value_selection = Some(ValueSelection { row: 0, column: 0 });

            assert!(reduce(&mut state, Action::OpenValueEditor).is_empty());
            assert!(state.value_edit.is_none());
            assert_eq!(
                state.status_message,
                Some(StatusMessage::Info(
                    "Set members cannot be edited safely because replacement is non-atomic."
                        .to_string()
                ))
            );
            assert!(state.confirm_state.is_none());
        }

        #[test]
        fn value_edit_commands_quote_every_argument_and_supported_target() {
            let unusual = "space \"quote\" \\\\ tab\tcr\rline\n";
            assert_eq!(
                value_edit_command(
                    &ValueEditTarget::String {
                        key: "string key".to_string()
                    },
                    unusual
                ),
                "SET \"string key\" \"space \\\"quote\\\" \\\\\\\\ tab\\tcr\\rline\\n\""
            );
            assert_eq!(
                value_edit_command(
                    &ValueEditTarget::List {
                        key: "list key".to_string(),
                        index: 2
                    },
                    "new value"
                ),
                "LSET \"list key\" 2 \"new value\""
            );
            assert_eq!(
                value_edit_command(
                    &ValueEditTarget::Hash {
                        key: "hash key".to_string(),
                        field: "field name".to_string()
                    },
                    "new value"
                ),
                "HSET \"hash key\" \"field name\" \"new value\""
            );
            assert_eq!(
                value_edit_command(
                    &ValueEditTarget::ZSet {
                        key: "rank key".to_string(),
                        member: "member name".to_string()
                    },
                    "4.25"
                ),
                "ZADD \"rank key\" \"4.25\" \"member name\""
            );
        }

        #[test]
        fn submitting_value_edit_uses_confirmation_pipeline() {
            let mut state = AppState::new("redis://localhost");
            state.value_edit = Some(ValueEditState {
                input: "new value".to_string(),
                cursor: 9,
                target: ValueEditTarget::List {
                    key: "items".to_string(),
                    index: 3,
                },
            });

            assert!(reduce(&mut state, Action::SubmitValueEdit).is_empty());
            assert!(state.value_edit.is_none());
            assert_eq!(
                state.confirm_state,
                Some(ConfirmState {
                    op: PendingWrite::Command {
                        command: "LSET \"items\" 3 \"new value\"".to_string()
                    },
                    prompt: "Update the selected Redis value?".to_string()
                })
            );
            assert_eq!(
                reduce(&mut state, Action::ConfirmWrite),
                vec![Effect::ExecuteCommand {
                    command: "LSET \"items\" 3 \"new value\"".to_string()
                }]
            );
        }

        #[test]
        fn read_only_value_edit_keeps_editor_open_and_never_executes() {
            let mut state = AppState::with_read_only("redis://localhost", true);
            state.value_edit = Some(ValueEditState {
                input: "new value".to_string(),
                cursor: 9,
                target: ValueEditTarget::String {
                    key: "item".to_string(),
                },
            });

            assert!(reduce(&mut state, Action::SubmitValueEdit).is_empty());
            assert!(state.value_edit.is_some());
            assert!(state.confirm_state.is_none());
            assert!(matches!(
                &state.status_message,
                Some(StatusMessage::Error(message)) if message.contains("read-only")
            ));
        }

        #[test]
        fn value_editor_supports_cursor_input_paste_backspace_and_cancel() {
            let mut state = AppState::new("redis://localhost");
            state.value_edit = Some(ValueEditState {
                input: "ac".to_string(),
                cursor: 1,
                target: ValueEditTarget::String {
                    key: "item".to_string(),
                },
            });

            reduce(&mut state, Action::ValueEditInput('b'));
            reduce(&mut state, Action::ValueEditPaste("日".to_string()));
            assert_eq!(state.value_edit.as_ref().unwrap().input, "ab日c");
            reduce(&mut state, Action::ValueEditCursorLeft);
            reduce(&mut state, Action::ValueEditBackspace);
            assert_eq!(state.value_edit.as_ref().unwrap().input, "a日c");
            reduce(&mut state, Action::CancelValueEdit);
            assert!(state.value_edit.is_none());
        }

        #[test]
        fn external_editor_request_without_inline_draft_has_no_effect() {
            let mut state = AppState::new("redis://localhost");
            state.value_state = ValueState::Loaded {
                key: "item".to_string(),
                kind: RedisKind::String,
                ttl: None,
                value: RedisValue::String("old value".to_string()),
            };
            state.value_selection = Some(ValueSelection { row: 0, column: 0 });

            assert!(reduce(&mut state, Action::RequestExternalValueEditor).is_empty());
            assert!(state.value_edit.is_none());
            assert!(state.status_message.is_none());
        }

        #[test]
        fn external_editor_request_uses_current_inline_draft() {
            let mut state = AppState::new("redis://localhost");
            state.value_edit = Some(ValueEditState {
                input: "draft 日本語".to_string(),
                cursor: 2,
                target: ValueEditTarget::String {
                    key: "item".to_string(),
                },
            });

            assert_eq!(
                reduce(&mut state, Action::RequestExternalValueEditor),
                vec![Effect::OpenExternalValueEditor {
                    content: "draft 日本語".to_string()
                }]
            );
            assert_eq!(state.value_edit.as_ref().unwrap().cursor, 2);
        }

        #[test]
        fn external_editor_success_replaces_draft_and_uses_character_cursor_end() {
            let mut state = AppState::new("redis://localhost");
            state.value_edit = Some(ValueEditState {
                input: "old".to_string(),
                cursor: 1,
                target: ValueEditTarget::String {
                    key: "item".to_string(),
                },
            });

            assert!(
                reduce(
                    &mut state,
                    Action::ExternalValueEditSucceeded {
                        content: "新しい🦀値".to_string()
                    }
                )
                .is_empty()
            );

            let editor = state.value_edit.as_ref().unwrap();
            assert_eq!(editor.input, "新しい🦀値");
            assert_eq!(editor.cursor, "新しい🦀値".chars().count());
            assert!(state.confirm_state.is_none());
        }

        #[test]
        fn external_editor_failure_preserves_inline_draft_and_reports_status() {
            let mut state = AppState::new("redis://localhost");
            state.value_edit = Some(ValueEditState {
                input: "keep this".to_string(),
                cursor: 4,
                target: ValueEditTarget::String {
                    key: "item".to_string(),
                },
            });

            assert!(
                reduce(
                    &mut state,
                    Action::ExternalValueEditFailed {
                        message: "editor exited with status 1".to_string()
                    }
                )
                .is_empty()
            );

            let editor = state.value_edit.as_ref().unwrap();
            assert_eq!(editor.input, "keep this");
            assert_eq!(editor.cursor, 4);
            assert_eq!(
                state.status_message,
                Some(StatusMessage::Error(
                    "External editor failed: editor exited with status 1".to_string()
                ))
            );
        }
    }

    mod effect_runner {
        use super::*;

        fn runner_with_cli(cli: MockRedisCli, action_tx: mpsc::Sender<Action>) -> EffectRunner {
            let mut factory = MockRedisCliFactory::new();
            factory.expect_create().never();
            EffectRunner::new(Arc::new(cli), Arc::new(factory), action_tx)
        }

        #[test]
        fn runner_is_send_sync_and_creates_clipboard_lazily() {
            fn assert_send_sync<T: Send + Sync>() {}

            assert_send_sync::<EffectRunner>();
            let (tx, _rx) = mpsc::channel(1);
            let runner = runner_with_cli(MockRedisCli::new(), tx);
            assert!(runner.clipboard.lock().unwrap().is_none());
        }

        #[tokio::test]
        async fn poisoned_clipboard_lock_dispatches_failure_instead_of_panicking() {
            let (tx, mut rx) = mpsc::channel(1);
            let runner = runner_with_cli(MockRedisCli::new(), tx);
            let clipboard = Arc::clone(&runner.clipboard);
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _guard = clipboard.lock().unwrap();
                panic!("poison clipboard lock");
            }));

            runner
                .run(vec![Effect::CopyToClipboard {
                    text: "value".to_string(),
                }])
                .await;

            let action = tokio::time::timeout(Duration::from_millis(500), rx.recv())
                .await
                .expect("action timeout")
                .expect("channel closed");
            let Action::ClipboardFailed { message } = action else {
                panic!("expected clipboard failure action");
            };
            assert!(message.contains("clipboard lock poisoned"));
        }

        #[tokio::test]
        async fn connect_effect_uses_factory_swaps_cli_and_dispatches_connected() {
            let initial_cli = MockRedisCli::new();
            let mut next_cli = MockRedisCli::new();
            next_cli.expect_ping().once().returning(|| Ok(()));
            next_cli.expect_dbsize().once().returning(|| Ok(2));
            next_cli
                .expect_scan_keys()
                .once()
                .withf(|pattern| pattern == "*")
                .returning(|_| Ok(vec![key("a"), key("b")]));
            next_cli
                .expect_execute_command()
                .once()
                .withf(|command| command == "PING")
                .returning(|_| Ok("PONG\n".to_string()));
            let next_cli: Arc<dyn RedisCli> = Arc::new(next_cli);

            let mut factory = MockRedisCliFactory::new();
            factory
                .expect_create()
                .once()
                .withf(|dsn, read_only| dsn == "redis://cache.example.com:6380/2" && *read_only)
                .return_once(move |_, _| Ok(next_cli));

            let (tx, mut rx) = mpsc::channel(4);
            let runner = EffectRunner::new(Arc::new(initial_cli), Arc::new(factory), tx);

            runner
                .run(vec![Effect::Connect {
                    dsn: "redis://cache.example.com:6380/2".to_string(),
                    read_only: true,
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

            runner
                .run(vec![Effect::ExecuteCommand {
                    command: "PING".to_string(),
                }])
                .await;
            let action = tokio::time::timeout(Duration::from_millis(500), rx.recv())
                .await
                .expect("action timeout")
                .expect("channel closed");
            assert_eq!(
                action,
                Action::CommandSucceeded {
                    output: "PONG\n".to_string(),
                }
            );
        }

        #[tokio::test]
        async fn connect_effect_dispatches_failure_when_factory_fails() {
            let cli = MockRedisCli::new();
            let mut factory = MockRedisCliFactory::new();
            factory
                .expect_create()
                .once()
                .withf(|dsn, read_only| dsn == "not-a-redis-dsn" && !*read_only)
                .returning(|_, _| {
                    Err(crate::infra::RedisCliError::Parse(
                        "DSN must start with redis://".to_string(),
                    ))
                });

            let (tx, mut rx) = mpsc::channel(4);
            let runner = EffectRunner::new(Arc::new(cli), Arc::new(factory), tx);

            runner
                .run(vec![Effect::Connect {
                    dsn: "not-a-redis-dsn".to_string(),
                    read_only: false,
                }])
                .await;

            let action = tokio::time::timeout(Duration::from_millis(500), rx.recv())
                .await
                .expect("action timeout")
                .expect("channel closed");
            assert_eq!(
                action,
                Action::ConnectFailed(
                    "failed to parse redis-cli output: DSN must start with redis://".to_string()
                )
            );
        }

        #[tokio::test]
        async fn search_keys_effect_scans_pattern_and_dispatches_keys_scanned() {
            let mut cli = MockRedisCli::new();
            cli.expect_scan_keys()
                .once()
                .withf(|pattern| pattern == "user:*:[ab]")
                .returning(|_| Ok(vec![key("user:1:a"), key("user:2:b")]));

            let (tx, mut rx) = mpsc::channel(4);
            let runner = EffectRunner::new(Arc::new(cli), Arc::new(MockRedisCliFactory::new()), tx);

            runner
                .run(vec![Effect::SearchKeys {
                    pattern: "user:*:[ab]".to_string(),
                }])
                .await;

            let action = tokio::time::timeout(Duration::from_millis(500), rx.recv())
                .await
                .expect("action timeout")
                .expect("channel closed");
            assert_eq!(
                action,
                Action::KeysScanned {
                    keys: vec![key("user:1:a"), key("user:2:b")],
                }
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
            let runner = runner_with_cli(cli, tx);

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
            let runner = runner_with_cli(cli, tx);

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
            let runner = runner_with_cli(cli, tx);

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
            let runner = runner_with_cli(cli, tx);

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
        async fn execute_command_effect_preserves_redis_error_reply_body() {
            let mut cli = MockRedisCli::new();
            cli.expect_execute_command().once().returning(|_| {
                Err(crate::infra::RedisCliError::CommandFailed(
                    "ERR unknown command 'NOPE'".to_string(),
                ))
            });

            let (tx, mut rx) = mpsc::channel(4);
            let runner = runner_with_cli(cli, tx);

            runner
                .run(vec![Effect::ExecuteCommand {
                    command: "NOPE".to_string(),
                }])
                .await;

            let action = tokio::time::timeout(Duration::from_millis(500), rx.recv())
                .await
                .expect("action timeout")
                .expect("channel closed");
            assert_eq!(
                action,
                Action::CommandFailed {
                    message: "ERR unknown command 'NOPE'".to_string(),
                }
            );
        }

        #[tokio::test]
        async fn load_db_overview_effect_dispatches_loaded_entries() {
            let mut cli = MockRedisCli::new();
            cli.expect_db_overview().once().returning(|| {
                Ok(DbOverview {
                    entries: vec![(0, 2), (1, 0), (2, 7)],
                    database_count_known: true,
                })
            });

            let (tx, mut rx) = mpsc::channel(4);
            let runner = runner_with_cli(cli, tx);

            runner.run(vec![Effect::LoadDbOverview]).await;

            let action = tokio::time::timeout(Duration::from_millis(500), rx.recv())
                .await
                .expect("action timeout")
                .expect("channel closed");
            assert_eq!(
                action,
                Action::DbOverviewLoaded {
                    overview: DbOverview {
                        entries: vec![(0, 2), (1, 0), (2, 7)],
                        database_count_known: true,
                    },
                }
            );
        }

        #[tokio::test]
        async fn select_db_effect_updates_cli_without_dispatching_action() {
            let mut cli = MockRedisCli::new();
            cli.expect_select_db()
                .once()
                .withf(|db| *db == 3)
                .returning(|_| ());

            let (tx, mut rx) = mpsc::channel(4);
            let runner = runner_with_cli(cli, tx);

            runner.run(vec![Effect::SelectDb { db: 3 }]).await;

            assert!(rx.try_recv().is_err());
        }

        #[tokio::test]
        async fn export_csv_effect_writes_file_and_dispatches_success() {
            let cli = MockRedisCli::new();
            let stem = format!("sabiql_redis_export_{}_success", std::process::id());
            let path = std::env::current_dir().unwrap().join(format!("{stem}.csv"));
            let _ = std::fs::remove_file(&path);

            let (tx, mut rx) = mpsc::channel(4);
            let runner = runner_with_cli(cli, tx);

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
