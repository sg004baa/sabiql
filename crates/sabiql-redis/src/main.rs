use std::sync::Arc;

use clap::Parser;
use color_eyre::eyre::Result;
use sabiql_tui_kit::input::{InputEvent, Key, KeyCombo};
use sabiql_tui_kit::tui::TuiRunner;
use tokio::sync::mpsc;

pub mod app;
pub mod domain;
pub mod infra;
pub mod ui;

use app::{Action, AppState, EffectRunner};
use infra::RedisCliSubprocess;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(default_value = "redis://127.0.0.1:6379/0")]
    dsn: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    let args = Args::parse();

    let cli = Arc::new(RedisCliSubprocess::new(&args.dsn)?);
    let (action_tx, mut action_rx) = mpsc::channel::<Action>(64);
    let effect_runner = EffectRunner::new(cli, action_tx);
    let mut state = AppState::new(args.dsn);

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

async fn process_action(
    action: Action,
    state: &mut AppState,
    tui: &mut TuiRunner,
    effect_runner: &EffectRunner,
) -> Result<()> {
    let effects = app::reduce(state, action);
    tui.terminal().draw(|frame| ui::render(frame, state))?;
    effect_runner.run(effects).await;
    Ok(())
}

fn handle_event(event: InputEvent, state: &AppState) -> Option<Action> {
    match event {
        InputEvent::Key(combo) if state.command_modal.is_open => handle_modal_key(combo),
        InputEvent::Key(combo) if state.filter_active => handle_filter_key(combo),
        InputEvent::Resize(width, height) => Some(Action::Resize(width, height)),
        InputEvent::Key(combo) if combo == KeyCombo::plain(Key::Char(':')) => {
            Some(Action::OpenCommandModal)
        }
        InputEvent::Key(combo) if combo == KeyCombo::plain(Key::Char('/')) => {
            Some(Action::OpenFilter)
        }
        InputEvent::Key(combo) if combo == KeyCombo::plain(Key::Char('e')) => {
            Some(Action::RequestExportCsv)
        }
        InputEvent::Key(combo) if combo == KeyCombo::plain(Key::Char('q')) => Some(Action::Quit),
        InputEvent::Key(combo) if combo == KeyCombo::ctrl(Key::Char('c')) => Some(Action::Quit),
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
