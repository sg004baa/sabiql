use std::sync::Arc;

use color_eyre::eyre::Result;
use sabiql_tui_kit::input::{InputEvent, Key, KeyCombo};
use sabiql_tui_kit::tui::TuiRunner;
use tokio::sync::mpsc;

pub mod app;
pub mod domain;
pub mod error;
pub mod infra;
pub mod ui;

use app::{Action, AppState, EffectRunner};
use infra::{RedisCliSubprocess, RedisCliSubprocessFactory};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    Quit,
    SwitchConnection,
}

pub async fn run(dsn: String, read_only: bool) -> Result<RunOutcome> {
    error::install_hooks()?;

    let cli = Arc::new(RedisCliSubprocess::with_read_only(&dsn, read_only)?);
    let factory = Arc::new(RedisCliSubprocessFactory);
    let (action_tx, mut action_rx) = mpsc::channel::<Action>(64);
    let effect_runner = Arc::new(EffectRunner::new(cli, factory, action_tx));
    let mut state = AppState::with_read_only(dsn, read_only);

    let mut tui = TuiRunner::new()?;
    tui.enter()?;

    let run_result = async {
        let size = tui.terminal().size()?;
        process_action(
            Action::Resize(size.width, size.height),
            &mut state,
            &mut tui,
            &effect_runner,
        )?;
        process_action(Action::StartConnect, &mut state, &mut tui, &effect_runner)?;

        loop {
            tokio::select! {
                Some(event) = tui.next_event() => {
                    if let Some(action) = handle_event(event, &state) {
                        process_action(action, &mut state, &mut tui, &effect_runner)?;
                    }
                }
                Some(action) = action_rx.recv() => {
                    process_action(action, &mut state, &mut tui, &effect_runner)?;
                }
                else => break,
            }

            if state.should_quit {
                break;
            }
        }

        Ok::<RunOutcome, color_eyre::Report>(if state.should_switch_connection {
            RunOutcome::SwitchConnection
        } else {
            RunOutcome::Quit
        })
    }
    .await;

    let exit_result = tui.exit();
    let outcome = run_result?;
    exit_result?;
    Ok(outcome)
}

fn process_action(
    action: Action,
    state: &mut AppState,
    tui: &mut TuiRunner,
    effect_runner: &Arc<EffectRunner>,
) -> Result<()> {
    let effects = app::reduce(state, action);
    tui.terminal().draw(|frame| ui::render(frame, state))?;
    if !effects.is_empty() {
        let runner = Arc::clone(effect_runner);
        tokio::spawn(async move {
            runner.run(effects).await;
        });
    }
    Ok(())
}

fn handle_event(event: InputEvent, state: &AppState) -> Option<Action> {
    match event {
        InputEvent::Key(combo) if state.confirm_state.is_some() => handle_confirm_key(combo),
        InputEvent::Key(combo) if state.db_overlay.is_some() => handle_db_overlay_key(combo),
        InputEvent::Paste(text) if state.command_modal.is_open => Some(Action::CommandPaste(text)),
        InputEvent::Key(combo) if state.command_modal.is_open => handle_modal_key(combo),
        InputEvent::Key(combo) if state.filter_active => handle_filter_key(combo),
        InputEvent::Resize(width, height) => Some(Action::Resize(width, height)),
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
            Some(Action::RequestConnectionSwitch)
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
    fn c_key_requests_shell_connection_switch() {
        let state = AppState::new("redis://localhost");

        let action = handle_event(InputEvent::Key(KeyCombo::plain(Key::Char('c'))), &state);

        assert_eq!(action, Some(Action::RequestConnectionSwitch));
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
}
