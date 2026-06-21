use std::sync::Arc;

use clap::Parser;
use color_eyre::eyre::Result;
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
        InputEvent::Key(combo) if state.expire_input.is_some() => handle_expire_input_key(combo),
        InputEvent::Key(combo) if state.connection_form.is_some() => {
            handle_connection_form_key(combo)
        }
        InputEvent::Key(combo) if state.db_overlay.is_some() => handle_db_overlay_key(combo),
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
            Some(Action::OpenConnectionForm)
        }
        InputEvent::Key(combo) if combo == KeyCombo::plain(Key::Char('e')) => {
            Some(Action::RequestExportCsv)
        }
        InputEvent::Key(combo) if combo == KeyCombo::plain(Key::Char('x')) => {
            Some(Action::RequestDelete)
        }
        InputEvent::Key(combo) if combo == KeyCombo::plain(Key::Char('X')) => {
            Some(Action::RequestUnlink)
        }
        InputEvent::Key(combo) if combo == KeyCombo::plain(Key::Char('t')) => {
            Some(Action::OpenExpireInput)
        }
        InputEvent::Key(combo) if combo == KeyCombo::plain(Key::Char('p')) => {
            Some(Action::RequestPersist)
        }
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

fn handle_expire_input_key(combo: KeyCombo) -> Option<Action> {
    match combo {
        combo if combo == KeyCombo::plain(Key::Enter) => Some(Action::SubmitExpire),
        combo if combo == KeyCombo::plain(Key::Esc) => Some(Action::CancelExpireInput),
        combo if combo == KeyCombo::plain(Key::Backspace) => Some(Action::ExpireInputBackspace),
        KeyCombo {
            key: Key::Char(c), ..
        } if c.is_ascii_digit() => Some(Action::ExpireInputDigit(c)),
        _ => None,
    }
}

fn handle_connection_form_key(combo: KeyCombo) -> Option<Action> {
    match combo {
        combo if combo == KeyCombo::plain(Key::Enter) => Some(Action::SubmitConnectionForm),
        combo if combo == KeyCombo::plain(Key::Esc) => Some(Action::CancelConnectionForm),
        combo if combo == KeyCombo::plain(Key::Backspace) => Some(Action::ConnectionFormBackspace),
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
    fn d_key_opens_db_overlay() {
        let state = AppState::new("redis://localhost");

        let action = handle_event(InputEvent::Key(KeyCombo::plain(Key::Char('d'))), &state);

        assert_eq!(action, Some(Action::OpenDbOverlay));
    }

    #[test]
    fn write_keys_request_delete_unlink_expire_and_persist() {
        let state = AppState::new("redis://localhost");

        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::plain(Key::Char('x'))), &state),
            Some(Action::RequestDelete)
        );
        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::plain(Key::Char('X'))), &state),
            Some(Action::RequestUnlink)
        );
        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::plain(Key::Char('t'))), &state),
            Some(Action::OpenExpireInput)
        );
        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::plain(Key::Char('p'))), &state),
            Some(Action::RequestPersist)
        );
    }

    #[test]
    fn confirm_overlay_routes_yes_no_and_swallows_other_keys() {
        let mut state = AppState::new("redis://localhost");
        state.confirm_state = Some(app::ConfirmState {
            op: app::PendingWrite::Delete {
                key: "a".to_string(),
                unlink: false,
            },
            prompt: "Delete key \"a\" with DEL?".to_string(),
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
    fn expire_input_routes_digits_submit_backspace_and_escape() {
        let mut state = AppState::new("redis://localhost");
        state.expire_input = Some(app::ExpireInputState {
            key: "a".to_string(),
            input: String::new(),
        });

        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::plain(Key::Char('6'))), &state),
            Some(Action::ExpireInputDigit('6'))
        );
        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::plain(Key::Backspace)), &state),
            Some(Action::ExpireInputBackspace)
        );
        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::plain(Key::Enter)), &state),
            Some(Action::SubmitExpire)
        );
        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::plain(Key::Esc)), &state),
            Some(Action::CancelExpireInput)
        );
        assert_eq!(
            handle_event(InputEvent::Key(KeyCombo::plain(Key::Char('x'))), &state),
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
    fn connection_form_suppresses_regular_keymaps() {
        let mut state = AppState::new("redis://localhost");
        state.connection_form = Some(app::ConnectionFormState {
            dsn: "redis://localhost".to_string(),
            read_only: false,
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
    fn modal_routes_enter_backspace_and_escape() {
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
            handle_event(InputEvent::Key(KeyCombo::plain(Key::Esc)), &state),
            Some(Action::CloseCommandModal)
        );
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
