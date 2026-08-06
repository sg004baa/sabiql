use std::io::{Stdout, stdout};

use color_eyre::eyre::Result;
use crossterm::cursor::SetCursorStyle;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event as CrosstermEvent, EventStream, KeyEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures::{FutureExt, StreamExt};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::input::InputEvent;

pub type Tui = Terminal<CrosstermBackend<Stdout>>;

pub struct TuiRunner {
    terminal: Tui,
    event_rx: UnboundedReceiver<InputEvent>,
    event_tx: UnboundedSender<InputEvent>,
    task: Option<JoinHandle<()>>,
    cancellation_token: CancellationToken,
}

impl TuiRunner {
    pub fn new() -> Result<Self> {
        let terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let cancellation_token = CancellationToken::new();

        Ok(Self {
            terminal,
            event_rx,
            event_tx,
            task: None,
            cancellation_token,
        })
    }

    pub fn enter(&mut self) -> Result<()> {
        acquire_terminal()?;
        self.start_event_loop();
        Ok(())
    }

    pub fn exit(&mut self) -> Result<()> {
        self.stop_event_loop();
        release_terminal()?;
        Ok(())
    }

    /// Release the terminal so an external interactive program (e.g. `$EDITOR`) owns the TTY.
    pub fn suspend(&mut self) -> std::io::Result<()> {
        self.stop_event_loop();
        release_terminal()
    }

    /// Re-acquire the terminal released by [`Self::suspend`] and force a full repaint.
    pub fn resume(&mut self) -> std::io::Result<()> {
        // `stop_event_loop` cancels the token permanently, so the reader task needs a fresh one.
        self.cancellation_token = CancellationToken::new();
        acquire_terminal()?;
        // `Terminal::clear` snapshots the cursor with a DSR query, which needs a reader on
        // stdin and blocks until it times out. `resize` to the current size clears the screen
        // and resets the back buffer — a full repaint — without the round-trip.
        let size = self.terminal.size()?;
        self.terminal
            .resize(Rect::new(0, 0, size.width, size.height))?;
        self.start_event_loop();
        Ok(())
    }

    fn start_event_loop(&mut self) {
        let event_tx = self.event_tx.clone();
        let cancellation_token = self.cancellation_token.clone();

        self.task = Some(tokio::spawn(async move {
            let mut event_stream = EventStream::new();

            let _ = event_tx.send(InputEvent::Init);

            loop {
                let event = tokio::select! {
                    () = cancellation_token.cancelled() => break,
                    crossterm_event = event_stream.next().fuse() => {
                        match crossterm_event {
                            Some(Ok(evt)) => match evt {
                                CrosstermEvent::Key(key) if key.kind == KeyEventKind::Press || key.kind == KeyEventKind::Repeat => {
                                    InputEvent::Key(crate::event::key_translator::translate(key))
                                }
                                CrosstermEvent::Resize(x, y) => InputEvent::Resize(x, y),
                                CrosstermEvent::Paste(text) => InputEvent::Paste(text),
                                _ => continue,
                            },
                            Some(Err(_)) | None => break,
                        }
                    }
                };

                if event_tx.send(event).is_err() {
                    break;
                }
            }
        }));
    }

    fn stop_event_loop(&mut self) {
        self.cancellation_token.cancel();
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }

    pub async fn next_event(&mut self) -> Option<InputEvent> {
        self.event_rx.recv().await
    }

    pub fn try_next_event(&mut self) -> Option<InputEvent> {
        self.event_rx.try_recv().ok()
    }

    pub fn terminal(&mut self) -> &mut Tui {
        &mut self.terminal
    }
}

fn acquire_terminal() -> std::io::Result<()> {
    enable_raw_mode()?;
    execute!(
        stdout(),
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )
}

fn release_terminal() -> std::io::Result<()> {
    if crossterm::terminal::is_raw_mode_enabled()? {
        let _ = execute!(stdout(), SetCursorStyle::DefaultUserShape);
        execute!(
            stdout(),
            LeaveAlternateScreen,
            DisableMouseCapture,
            DisableBracketedPaste
        )?;
        disable_raw_mode()?;
    }
    Ok(())
}
