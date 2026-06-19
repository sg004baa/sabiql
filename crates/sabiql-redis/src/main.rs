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
                    if let Some(action) = handle_event(event) {
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

fn handle_event(event: InputEvent) -> Option<Action> {
    match event {
        InputEvent::Resize(width, height) => Some(Action::Resize(width, height)),
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
