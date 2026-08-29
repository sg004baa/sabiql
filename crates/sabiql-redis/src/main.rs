use std::{process::Command, sync::Arc};

use clap::Parser;
use color_eyre::eyre::Result;
use ratatui::crossterm::{cursor::SetCursorStyle, execute};
use sabiql_tui_kit::input::{InputEvent, Key, KeyCombo, Modifiers};
use sabiql_tui_kit::tui::TuiRunner;
use tokio::sync::mpsc;

pub mod app;
pub mod domain;
pub mod error;
pub mod infra;
pub mod ui;

use app::{Action, AppState, EffectRunner};
use infra::{RedisCliSubprocess, RedisCliSubprocessFactory};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(long)]
    read_only: bool,
    #[arg(default_value = "redis://127.0.0.1:6379/0")]
    dsn: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    error::install_hooks()?;
    let args = Args::parse();

    let cli = Arc::new(redis_cli_from_args(&args)?);
    let factory = Arc::new(RedisCliSubprocessFactory);
    let (action_tx, mut action_rx) = mpsc::channel::<Action>(64);
    let effect_runner = Arc::new(EffectRunner::new(cli, factory, action_tx));
    let mut state = AppState::with_read_only(args.dsn, args.read_only);

    let mut tui = TuiRunner::new()?;
    tui.enter()?;

    let run_result = async {
        let size = tui.terminal().size()?;
        process_action(
            Action::Resize(size.width, size.height),
            &mut state,
            &mut tui,
            &effect_runner,
        )
        .await?;
        process_action(Action::StartConnect, &mut state, &mut tui, &effect_runner).await?;

        loop {
            tokio::select! {
                Some(event) = tui.next_event() => {
                    if let Some(action) = handle_event(event, &state) {
                        process_action(action, &mut state, &mut tui, &effect_runner).await?;
                    }
                }
                Some(action) = action_rx.recv() => {
                    process_action(action, &mut state, &mut tui, &effect_runner).await?;
                }
                else => break,
            }

            if state.should_quit {
                break;
            }
        }

        Ok::<(), color_eyre::Report>(())
    }
    .await;

    let exit_result = tui.exit();
    run_result?;
    exit_result?;
    Ok(())
}

fn redis_cli_from_args(
    args: &Args,
) -> std::result::Result<RedisCliSubprocess, infra::RedisCliError> {
    RedisCliSubprocess::with_read_only(&args.dsn, args.read_only)
}

async fn process_action(
    action: Action,
    state: &mut AppState,
    tui: &mut TuiRunner,
    effect_runner: &Arc<EffectRunner>,
) -> Result<()> {
    let effects = app::reduce(state, action);
    draw_ui(tui, state)?;

    let mut background_effects = Vec::with_capacity(effects.len());
    for effect in effects {
        match effect {
            app::Effect::OpenExternalValueEditor { content } => {
                let result_action = run_external_value_editor(tui, content).await?;
                let follow_up_effects = app::reduce(state, result_action);
                debug_assert!(follow_up_effects.is_empty());
                draw_ui(tui, state)?;
            }
            effect => background_effects.push(effect),
        }
    }

    if !background_effects.is_empty() {
        let runner = Arc::clone(effect_runner);
        tokio::spawn(async move {
            runner.run(background_effects).await;
        });
    }
    Ok(())
}

fn draw_ui(tui: &mut TuiRunner, state: &AppState) -> Result<()> {
    execute!(tui.terminal().backend_mut(), value_edit_cursor_style(state))?;
    tui.terminal().draw(|frame| ui::render(frame, state))?;
    Ok(())
}

fn value_edit_cursor_style(state: &AppState) -> SetCursorStyle {
    if state.value_edit.is_some() {
        SetCursorStyle::SteadyBar
    } else {
        SetCursorStyle::DefaultUserShape
    }
}

async fn run_external_value_editor(tui: &mut TuiRunner, content: String) -> Result<Action> {
    let editor_result = match tui.suspend() {
        Ok(()) => {
            match tokio::task::spawn_blocking(move || edit_value_in_external_editor(&content)).await
            {
                Ok(result) => result.map_err(|error| error.to_string()),
                Err(error) => Err(format!("editor task failed: {error}")),
            }
        }
        Err(error) => Err(format!("could not release terminal: {error}")),
    };

    // Re-acquire the terminal before the reducer exposes either success or failure to the UI.
    tui.resume()?;

    Ok(match editor_result {
        Ok(content) => Action::ExternalValueEditSucceeded { content },
        Err(message) => Action::ExternalValueEditFailed { message },
    })
}

fn edit_value_in_external_editor(content: &str) -> std::io::Result<String> {
    let editor = std::env::var_os("EDITOR")
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "$EDITOR is not set"))?;
    let editor = editor.to_string_lossy();
    let mut tokens = editor.split_whitespace();
    let program = tokens
        .next()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "$EDITOR is not set"))?;

    // Use a directory because editors may replace the file by rename while saving.
    let directory = tempfile::Builder::new().prefix("sabiql-redis-").tempdir()?;
    let path = directory.path().join("value.txt");
    // Seed and remove one line terminator symmetrically so unchanged text is an identity,
    // while a trailing blank line added by the user remains part of the draft.
    let mut file_content = String::with_capacity(content.len() + 1);
    file_content.push_str(content);
    file_content.push('\n');
    std::fs::write(&path, file_content)?;

    let status = Command::new(program).args(tokens).arg(&path).status()?;
    if !status.success() {
        return Err(std::io::Error::other(format!(
            "editor exited with {status}"
        )));
    }

    let mut edited = std::fs::read_to_string(path)?;
    if edited.ends_with('\n') {
        edited.truncate(edited.len() - 1);
        if edited.ends_with('\r') {
            edited.truncate(edited.len() - 1);
        }
    }
    Ok(edited)
}

fn handle_event(event: InputEvent, state: &AppState) -> Option<Action> {
    match event {
        InputEvent::Paste(text) if state.connection_form.is_some() => {
            Some(Action::ConnectionFormPaste(text))
        }
        InputEvent::Key(combo) if state.confirm_state.is_some() => handle_confirm_key(combo),
        InputEvent::Key(combo) if state.connection_form.is_some() => {
            handle_connection_form_key(combo)
        }
        InputEvent::Key(combo) if state.db_overlay.is_some() => handle_db_overlay_key(combo),
        InputEvent::Paste(text) if state.value_edit.is_some() => Some(Action::ValueEditPaste(text)),
        InputEvent::Paste(text) if state.command_modal.is_open => Some(Action::CommandPaste(text)),
        InputEvent::Key(combo) if state.command_modal.is_open => handle_modal_key(combo),
        InputEvent::Key(combo) if state.filter_active => handle_filter_key(combo),
        InputEvent::Key(combo) if state.value_edit.is_some() => handle_value_edit_key(combo),
        InputEvent::Key(combo) if state.value_selection.is_some() => {
            handle_value_selection_key(combo)
        }
        InputEvent::Resize(width, height) => Some(Action::Resize(width, height)),
        InputEvent::Key(combo) if combo == KeyCombo::plain(Key::Enter) => {
            Some(Action::ActivateValue)
        }
        InputEvent::Key(combo) if combo == KeyCombo::plain(Key::Char('y')) => {
            Some(Action::YankSelected)
        }
        InputEvent::Key(combo) if combo == KeyCombo::plain(Key::Char(':')) => {
            Some(Action::OpenCommandModal)
        }
        InputEvent::Key(combo) if combo == KeyCombo::plain(Key::Char('/')) => {
            Some(Action::OpenFilter)
        }
        InputEvent::Key(combo) if combo == KeyCombo::plain(Key::Char('d')) => {
            Some(Action::OpenDbOverlay)
        }
        InputEvent::Key(combo) if combo == KeyCombo::plain(Key::Char('c')) => {
            Some(Action::OpenConnectionForm)
        }
        InputEvent::Key(combo) if combo == KeyCombo::plain(Key::Char('e')) => {
            Some(Action::RequestExportCsv)
        }
        InputEvent::Key(combo) if combo == KeyCombo::plain(Key::Char('r')) => Some(Action::Reload),
        InputEvent::Key(combo) if combo == KeyCombo::plain(Key::Char('q')) => Some(Action::Quit),
        InputEvent::Key(combo) if combo == KeyCombo::ctrl(Key::Char('c')) => Some(Action::Quit),
        InputEvent::Key(combo) if combo == KeyCombo::plain(Key::Char('J')) => {
            Some(Action::ValueScrollDown)
        }
        InputEvent::Key(combo) if combo == KeyCombo::plain(Key::Char('K')) => {
            Some(Action::ValueScrollUp)
        }
        InputEvent::Key(combo)
            if combo == KeyCombo::plain(Key::Down) || combo == KeyCombo::plain(Key::Char('j')) =>
        {
            Some(Action::SelectNext)
        }
        InputEvent::Key(combo)
            if combo == KeyCombo::plain(Key::Up) || combo == KeyCombo::plain(Key::Char('k')) =>
        {
            Some(Action::SelectPrev)
        }
        InputEvent::Init | InputEvent::Paste(_) | InputEvent::Key(_) => None,
    }
}

fn handle_confirm_key(combo: KeyCombo) -> Option<Action> {
    match combo {
        combo
            if combo == KeyCombo::plain(Key::Enter)
                || combo == KeyCombo::plain(Key::Char('y'))
                || combo == KeyCombo::plain(Key::Char('Y')) =>
        {
            Some(Action::ConfirmWrite)
        }
        combo
            if combo == KeyCombo::plain(Key::Esc)
                || combo == KeyCombo::plain(Key::Char('n'))
                || combo == KeyCombo::plain(Key::Char('N')) =>
        {
            Some(Action::CancelWrite)
        }
        _ => None,
    }
}

fn handle_connection_form_key(combo: KeyCombo) -> Option<Action> {
    match combo {
        combo if combo == KeyCombo::plain(Key::Enter) => Some(Action::SubmitConnectionForm),
        combo if combo == KeyCombo::plain(Key::Esc) => Some(Action::CancelConnectionForm),
        combo if combo == KeyCombo::plain(Key::Backspace) => Some(Action::ConnectionFormBackspace),
        combo if combo == KeyCombo::plain(Key::Left) => Some(Action::ConnectionFormCursorLeft),
        combo if combo == KeyCombo::plain(Key::Right) => Some(Action::ConnectionFormCursorRight),
        combo if combo == KeyCombo::plain(Key::Tab) => Some(Action::ToggleConnectionFormReadOnly),
        KeyCombo {
            key: Key::Char(c),
            modifiers,
        } if is_printable_input(c, modifiers) => Some(Action::ConnectionFormInput(c)),
        _ => None,
    }
}

fn is_printable_input(ch: char, modifiers: Modifiers) -> bool {
    !ch.is_control() && !modifiers.intersects(Modifiers::CTRL | Modifiers::ALT)
}

fn handle_db_overlay_key(combo: KeyCombo) -> Option<Action> {
    match combo {
        combo if combo == KeyCombo::plain(Key::Enter) => Some(Action::SubmitDbSelection),
        combo if combo == KeyCombo::plain(Key::Esc) => Some(Action::CloseDbOverlay),
        combo
            if combo == KeyCombo::plain(Key::Down) || combo == KeyCombo::plain(Key::Char('j')) =>
        {
            Some(Action::DbOverlaySelectNext)
        }
        combo if combo == KeyCombo::plain(Key::Up) || combo == KeyCombo::plain(Key::Char('k')) => {
            Some(Action::DbOverlaySelectPrev)
        }
        _ => None,
    }
}

fn handle_value_selection_key(combo: KeyCombo) -> Option<Action> {
    match combo {
        combo if combo == KeyCombo::plain(Key::Esc) => Some(Action::DeactivateValue),
        combo if combo == KeyCombo::plain(Key::Char('y')) => Some(Action::YankSelected),
        combo if combo == KeyCombo::plain(Key::Char('e')) => Some(Action::OpenValueEditor),
        combo
            if combo == KeyCombo::plain(Key::Down) || combo == KeyCombo::plain(Key::Char('j')) =>
        {
            Some(Action::ValueSelectNext)
        }
        combo if combo == KeyCombo::plain(Key::Up) || combo == KeyCombo::plain(Key::Char('k')) => {
            Some(Action::ValueSelectPrev)
        }
        combo
            if combo == KeyCombo::plain(Key::Left) || combo == KeyCombo::plain(Key::Char('h')) =>
        {
            Some(Action::ValueSelectLeft)
        }
        combo
            if combo == KeyCombo::plain(Key::Right) || combo == KeyCombo::plain(Key::Char('l')) =>
        {
            Some(Action::ValueSelectRight)
        }
        _ => None,
    }
}

fn handle_value_edit_key(combo: KeyCombo) -> Option<Action> {
    match combo {
        combo if combo == KeyCombo::ctrl(Key::Char('e')) => {
            Some(Action::RequestExternalValueEditor)
        }
        combo if combo == KeyCombo::plain(Key::Enter) => Some(Action::SubmitValueEdit),
        combo if combo == KeyCombo::plain(Key::Esc) => Some(Action::CancelValueEdit),
        combo if combo == KeyCombo::plain(Key::Backspace) => Some(Action::ValueEditBackspace),
        combo if combo == KeyCombo::plain(Key::Left) => Some(Action::ValueEditCursorLeft),
        combo if combo == KeyCombo::plain(Key::Right) => Some(Action::ValueEditCursorRight),
        KeyCombo {
            key: Key::Char(c),
            modifiers,
        } if is_printable_input(c, modifiers) => Some(Action::ValueEditInput(c)),
        _ => None,
    }
}

fn handle_filter_key(combo: KeyCombo) -> Option<Action> {
    match combo {
        combo if combo == KeyCombo::plain(Key::Enter) => Some(Action::CommitFilter),
        combo if combo == KeyCombo::plain(Key::Esc) => Some(Action::ClearFilter),
        combo if combo == KeyCombo::plain(Key::Backspace) => Some(Action::FilterBackspace),
        KeyCombo {
            key: Key::Char(c), ..
        } => Some(Action::FilterInput(c)),
        _ => None,
    }
}

fn handle_modal_key(combo: KeyCombo) -> Option<Action> {
    match combo {
        combo if combo == KeyCombo::plain(Key::Enter) => Some(Action::SubmitCommand),
        combo if combo == KeyCombo::plain(Key::Esc) => Some(Action::CloseCommandModal),
        combo if combo == KeyCombo::plain(Key::Backspace) => Some(Action::CommandBackspace),
        combo if combo == KeyCombo::plain(Key::Left) => Some(Action::CommandCursorLeft),
        combo if combo == KeyCombo::plain(Key::Right) => Some(Action::CommandCursorRight),
        combo if combo == KeyCombo::plain(Key::Up) => Some(Action::CommandHistoryPrev),
        combo if combo == KeyCombo::plain(Key::Down) => Some(Action::CommandHistoryNext),
        combo if combo == KeyCombo::plain(Key::Tab) => Some(Action::CommandCompleteNext),
        // Shift+Tab arrives as BackTab carrying the SHIFT modifier, so match the
        // key regardless of modifiers rather than only the bare combo.
        combo if combo.key == Key::BackTab => Some(Action::CommandCompletePrev),
        KeyCombo {
            key: Key::Char(c), ..
        } => Some(Action::CommandInput(c)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sabiql_tui_kit::input::Modifiers;
    use std::{ffi::OsString, sync::Mutex};

    static EDITOR_ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EditorEnv(Option<OsString>);

    impl EditorEnv {
        fn set(value: Option<&str>) -> Self {
            let previous = std::env::var_os("EDITOR");
            write_editor_env(value.map(OsString::from));
            Self(previous)
        }
    }

    impl Drop for EditorEnv {
        fn drop(&mut self) {
            write_editor_env(self.0.take());
        }
    }

    fn write_editor_env(value: Option<OsString>) {
        // SAFETY: editor env mutation is test-only and serialized by EDITOR_ENV_LOCK.
        unsafe {
            match value {
                Some(value) => std::env::set_var("EDITOR", value),
                None => std::env::remove_var("EDITOR"),
            }
        }
    }

    #[cfg(unix)]
    fn editor_script(body: &str) -> (tempfile::TempDir, String) {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("editor.sh");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        (directory, path.to_str().unwrap().to_string())
    }

    #[test]
    fn external_editor_reports_missing_editor() {
        let _guard = EDITOR_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _env = EditorEnv::set(None);

        let error = edit_value_in_external_editor("unchanged").unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        assert!(error.to_string().contains("$EDITOR"));
    }

    #[cfg(unix)]
    #[test]
    fn external_editor_no_op_round_trip_is_identity() {
        let _guard = EDITOR_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (_directory, editor) = editor_script(":");
        let _env = EditorEnv::set(Some(&editor));
        let original = "first line\nsecond line\n";

        assert_eq!(edit_value_in_external_editor(original).unwrap(), original);
    }

    #[cfg(unix)]
    #[test]
    fn external_editor_preserves_one_real_trailing_blank_line() {
        let _guard = EDITOR_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (_directory, editor) = editor_script(r#"printf 'edited\n\n' > "$1""#);
        let _env = EditorEnv::set(Some(&editor));

        assert_eq!(
            edit_value_in_external_editor("original").unwrap(),
            "edited\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn external_editor_reports_nonzero_exit() {
        let _guard = EDITOR_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (_directory, editor) = editor_script("exit 7");
        let _env = EditorEnv::set(Some(&editor));

        let error = edit_value_in_external_editor("keep this draft").unwrap_err();

        assert!(error.to_string().contains("editor exited"));
    }

    #[test]
    fn read_only_flag_defaults_to_false() {
        let args = Args::parse_from(["sabiql-redis", "redis://localhost"]);

        assert!(!args.read_only);
        assert_eq!(args.dsn, "redis://localhost");
    }

    #[test]
    fn read_only_flag_parses_true() {
        let args = Args::parse_from(["sabiql-redis", "--read-only", "redis://localhost"]);

        assert!(args.read_only);
        assert_eq!(args.dsn, "redis://localhost");
    }

    #[test]
    fn read_only_flag_propagates_to_subprocess() {
        let args = Args::parse_from(["sabiql-redis", "--read-only", "redis://localhost"]);
        let cli = redis_cli_from_args(&args).unwrap();

        assert!(cli.read_only());
    }

    #[test]
    fn value_edit_cursor_style_is_bar_only_while_modal_is_open() {
        let mut state = AppState::new("redis://localhost");
        assert_eq!(
            value_edit_cursor_style(&state),
            SetCursorStyle::DefaultUserShape
        );

        state.value_state = app::ValueState::Loaded {
            key: "item".to_string(),
            kind: domain::RedisKind::String,
            ttl: None,
            value: domain::RedisValue::String("draft".to_string()),
        };
        state.value_selection = Some(app::ValueSelection { row: 0, column: 0 });
        app::reduce(&mut state, Action::OpenValueEditor);
        assert_eq!(value_edit_cursor_style(&state), SetCursorStyle::SteadyBar);

        app::reduce(&mut state, Action::CancelValueEdit);
        assert_eq!(
            value_edit_cursor_style(&state),
            SetCursorStyle::DefaultUserShape
        );
    }

    #[test]
    fn colon_opens_command_modal_when_closed() {
        let state = AppState::new("redis://localhost");

        let action = handle_event(InputEvent::Key(KeyCombo::plain(Key::Char(':'))), &state);

        assert_eq!(action, Some(Action::OpenCommandModal));
    }

    #[test]
    fn slash_opens_filter_when_closed() {
        let state = AppState::new("redis://localhost");

        let action = handle_event(InputEvent::Key(KeyCombo::plain(Key::Char('/'))), &state);

        assert_eq!(action, Some(Action::OpenFilter));
    }

    #[test]
    fn export_key_requests_csv_export() {
        let state = AppState::new("redis://localhost");

        let action = handle_event(InputEvent::Key(KeyCombo::plain(Key::Char('e'))), &state);

        assert_eq!(action, Some(Action::RequestExportCsv));
    }

    #[test]
    fn r_key_requests_reload() {
        let state = AppState::new("redis://localhost");

        let action = handle_event(InputEvent::Key(KeyCombo::plain(Key::Char('r'))), &state);

        assert_eq!(action, Some(Action::Reload));
    }

    #[test]
    fn d_key_opens_db_overlay() {
        let state = AppState::new("redis://localhost");

        let action = handle_event(InputEvent::Key(KeyCombo::plain(Key::Char('d'))), &state);

        assert_eq!(action, Some(Action::OpenDbOverlay));
    }

    #[test]
    fn removed_direct_write_keys_are_ignored() {
        let state = AppState::new("redis://localhost");

        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::plain(Key::Char('x'))), &state),
            None
        );
        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::plain(Key::Char('X'))), &state),
            None
        );
        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::plain(Key::Char('t'))), &state),
            None
        );
        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::plain(Key::Char('p'))), &state),
            None
        );
    }

    #[test]
    fn confirm_overlay_routes_yes_no_and_swallows_other_keys() {
        let mut state = AppState::new("redis://localhost");
        state.confirm_state = Some(app::ConfirmState {
            op: app::PendingWrite::Command {
                command: "DEL a".to_string(),
            },
            prompt: "Run this command? DEL a".to_string(),
        });

        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::plain(Key::Char('y'))), &state),
            Some(Action::ConfirmWrite)
        );
        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::plain(Key::Char('Y'))), &state),
            Some(Action::ConfirmWrite)
        );
        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::plain(Key::Enter)), &state),
            Some(Action::ConfirmWrite)
        );
        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::plain(Key::Char('n'))), &state),
            Some(Action::CancelWrite)
        );
        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::plain(Key::Esc)), &state),
            Some(Action::CancelWrite)
        );
        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::plain(Key::Char('j'))), &state),
            None
        );
    }

    #[test]
    fn c_key_opens_connection_form() {
        let state = AppState::new("redis://localhost");

        let action = handle_event(InputEvent::Key(KeyCombo::plain(Key::Char('c'))), &state);

        assert_eq!(action, Some(Action::OpenConnectionForm));
    }

    #[test]
    fn connection_form_routes_text_toggle_submit_and_escape() {
        let mut state = AppState::new("redis://localhost");
        state.connection_form = Some(app::ConnectionFormState {
            dsn: "redis://localhost".to_string(),
            read_only: false,
            cursor: "redis://localhost".chars().count(),
        });

        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::plain(Key::Char('r'))), &state),
            Some(Action::ConnectionFormInput('r'))
        );
        assert_eq!(
            handle_event(
                InputEvent::Key(KeyCombo {
                    key: Key::Char(':'),
                    modifiers: Modifiers::SHIFT,
                }),
                &state
            ),
            Some(Action::ConnectionFormInput(':'))
        );
        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::plain(Key::Backspace)), &state),
            Some(Action::ConnectionFormBackspace)
        );
        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::plain(Key::Left)), &state),
            Some(Action::ConnectionFormCursorLeft)
        );
        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::plain(Key::Right)), &state),
            Some(Action::ConnectionFormCursorRight)
        );
        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::plain(Key::Tab)), &state),
            Some(Action::ToggleConnectionFormReadOnly)
        );
        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::plain(Key::Enter)), &state),
            Some(Action::SubmitConnectionForm)
        );
        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::plain(Key::Esc)), &state),
            Some(Action::CancelConnectionForm)
        );
    }

    #[test]
    fn connection_form_routes_paste_before_command_modal() {
        let mut state = AppState::new("redis://localhost");
        state.connection_form = Some(app::ConnectionFormState {
            dsn: "redis://localhost".to_string(),
            read_only: false,
            cursor: "redis://localhost".chars().count(),
        });
        state.command_modal.is_open = true;

        let action = handle_event(InputEvent::Paste("秘密🔐".to_string()), &state);

        assert_eq!(
            action,
            Some(Action::ConnectionFormPaste("秘密🔐".to_string()))
        );
    }

    #[test]
    fn connection_form_suppresses_regular_keymaps() {
        let mut state = AppState::new("redis://localhost");
        state.connection_form = Some(app::ConnectionFormState {
            dsn: "redis://localhost".to_string(),
            read_only: false,
            cursor: "redis://localhost".chars().count(),
        });

        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::plain(Key::Char('j'))), &state),
            Some(Action::ConnectionFormInput('j'))
        );
        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::plain(Key::Char(':'))), &state),
            Some(Action::ConnectionFormInput(':'))
        );
    }

    #[test]
    fn uppercase_j_and_k_scroll_value_pane() {
        let state = AppState::new("redis://localhost");

        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::plain(Key::Char('J'))), &state),
            Some(Action::ValueScrollDown)
        );
        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::plain(Key::Char('K'))), &state),
            Some(Action::ValueScrollUp)
        );
    }

    #[test]
    fn db_overlay_routes_navigation_submit_and_escape() {
        let mut state = AppState::new("redis://localhost");
        state.db_overlay = Some(app::DbOverlayState {
            entries: vec![(0, Some(1)), (1, Some(2))],
            selected: 0,
            loading: false,
            database_count_known: true,
        });

        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::plain(Key::Char('j'))), &state),
            Some(Action::DbOverlaySelectNext)
        );
        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::plain(Key::Char('k'))), &state),
            Some(Action::DbOverlaySelectPrev)
        );
        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::plain(Key::Enter)), &state),
            Some(Action::SubmitDbSelection)
        );
        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::plain(Key::Esc)), &state),
            Some(Action::CloseDbOverlay)
        );
    }

    #[test]
    fn db_overlay_suppresses_regular_keymaps() {
        let mut state = AppState::new("redis://localhost");
        state.db_overlay = Some(app::DbOverlayState {
            entries: vec![(0, Some(1))],
            selected: 0,
            loading: false,
            database_count_known: true,
        });

        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::plain(Key::Char('d'))), &state),
            None
        );
        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::plain(Key::Char(':'))), &state),
            None
        );
    }

    #[test]
    fn modal_captures_q_as_command_input_instead_of_quit() {
        let mut state = AppState::new("redis://localhost");
        state.command_modal.is_open = true;

        let action = handle_event(InputEvent::Key(KeyCombo::plain(Key::Char('q'))), &state);

        assert_eq!(action, Some(Action::CommandInput('q')));
    }

    #[test]
    fn modal_routes_editing_navigation_submit_and_escape() {
        let mut state = AppState::new("redis://localhost");
        state.command_modal.is_open = true;

        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::plain(Key::Enter)), &state),
            Some(Action::SubmitCommand)
        );
        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::plain(Key::Backspace)), &state),
            Some(Action::CommandBackspace)
        );
        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::plain(Key::Left)), &state),
            Some(Action::CommandCursorLeft)
        );
        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::plain(Key::Right)), &state),
            Some(Action::CommandCursorRight)
        );
        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::plain(Key::Up)), &state),
            Some(Action::CommandHistoryPrev)
        );
        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::plain(Key::Down)), &state),
            Some(Action::CommandHistoryNext)
        );
        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::plain(Key::Tab)), &state),
            Some(Action::CommandCompleteNext)
        );
        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::plain(Key::BackTab)), &state),
            Some(Action::CommandCompletePrev)
        );
        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::shift(Key::BackTab)), &state),
            Some(Action::CommandCompletePrev)
        );
        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::plain(Key::Esc)), &state),
            Some(Action::CloseCommandModal)
        );
    }

    #[test]
    fn modal_routes_paste_to_command_paste() {
        let mut state = AppState::new("redis://localhost");
        state.command_modal.is_open = true;

        let action = handle_event(InputEvent::Paste("PING".to_string()), &state);

        assert_eq!(action, Some(Action::CommandPaste("PING".to_string())));
    }

    #[test]
    fn modal_captures_modified_char_input() {
        let mut state = AppState::new("redis://localhost");
        state.command_modal.is_open = true;

        let action = handle_event(
            InputEvent::Key(KeyCombo {
                key: Key::Char(' '),
                modifiers: Modifiers::SHIFT,
            }),
            &state,
        );

        assert_eq!(action, Some(Action::CommandInput(' ')));
    }

    #[test]
    fn filter_mode_routes_text_backspace_enter_and_escape() {
        let mut state = AppState::new("redis://localhost");
        state.filter_active = true;

        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::plain(Key::Char('u'))), &state),
            Some(Action::FilterInput('u'))
        );
        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::plain(Key::Backspace)), &state),
            Some(Action::FilterBackspace)
        );
        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::plain(Key::Enter)), &state),
            Some(Action::CommitFilter)
        );
        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::plain(Key::Esc)), &state),
            Some(Action::ClearFilter)
        );
    }

    #[test]
    fn normal_mode_routes_enter_and_yank() {
        let state = AppState::new("redis://localhost");

        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::plain(Key::Enter)), &state),
            Some(Action::ActivateValue)
        );
        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::plain(Key::Char('y'))), &state),
            Some(Action::YankSelected)
        );
    }

    #[test]
    fn active_value_mode_routes_navigation_inline_edit_yank_and_escape_but_ignores_ctrl_e() {
        let mut state = AppState::new("redis://localhost");
        state.value_selection = Some(app::ValueSelection { row: 0, column: 0 });

        for (key, action) in [
            (Key::Down, Action::ValueSelectNext),
            (Key::Char('k'), Action::ValueSelectPrev),
            (Key::Left, Action::ValueSelectLeft),
            (Key::Char('l'), Action::ValueSelectRight),
            (Key::Char('y'), Action::YankSelected),
            (Key::Char('e'), Action::OpenValueEditor),
            (Key::Esc, Action::DeactivateValue),
        ] {
            assert_eq!(
                handle_event(InputEvent::Key(KeyCombo::plain(key)), &state),
                Some(action)
            );
        }
        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::ctrl(Key::Char('e'))), &state),
            None
        );
        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::plain(Key::Char('q'))), &state),
            None
        );
    }

    #[test]
    fn value_edit_modal_routes_external_editor_text_paste_submit_and_escape() {
        let mut state = AppState::new("redis://localhost");
        state.value_state = app::ValueState::Loaded {
            key: "item".to_string(),
            kind: domain::RedisKind::String,
            ttl: None,
            value: domain::RedisValue::String("old".to_string()),
        };
        state.value_selection = Some(app::ValueSelection { row: 0, column: 0 });
        app::reduce(&mut state, Action::OpenValueEditor);

        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::plain(Key::Char('q'))), &state),
            Some(Action::ValueEditInput('q'))
        );
        assert_eq!(
            handle_event(InputEvent::Paste(" pasted".to_string()), &state),
            Some(Action::ValueEditPaste(" pasted".to_string()))
        );
        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::plain(Key::Enter)), &state),
            Some(Action::SubmitValueEdit)
        );
        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::ctrl(Key::Char('e'))), &state),
            Some(Action::RequestExternalValueEditor)
        );
        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::plain(Key::Esc)), &state),
            Some(Action::CancelValueEdit)
        );
    }
}
