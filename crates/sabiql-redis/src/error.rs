use std::io::stdout;
use std::panic;

use color_eyre::eyre::Result;
use ratatui::crossterm::{
    cursor::SetCursorStyle,
    event::{DisableBracketedPaste, DisableMouseCapture},
    execute,
    terminal::{LeaveAlternateScreen, disable_raw_mode},
};

#[allow(
    clippy::print_stderr,
    reason = "panic hook must write to stderr after terminal restore"
)]
pub fn install_hooks() -> Result<()> {
    let hook_builder = color_eyre::config::HookBuilder::default().display_env_section(false);
    let (panic_hook, eyre_hook) = hook_builder.into_hooks();
    // The report hook is process-global and may already be installed by the
    // RDB TUI. The active terminal panic hook is still replaced below.
    let _ = eyre_hook.install();

    panic::set_hook(Box::new(move |panic_info| {
        let _ = restore_terminal();
        let report = panic_hook.panic_report(panic_info);
        eprintln!("{report}");
    }));

    Ok(())
}

fn restore_terminal() -> Result<()> {
    if ratatui::crossterm::terminal::is_raw_mode_enabled()? {
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
