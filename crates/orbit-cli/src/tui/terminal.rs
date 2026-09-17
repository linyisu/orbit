use std::io::{self, Stdout, Write};

use crossterm::{
    cursor::{Hide, Show},
    event::{DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};

pub type AppTerminal = Terminal<CrosstermBackend<Stdout>>;

pub struct TerminalGuard;

fn restore() {
    let _ = disable_raw_mode();
    let _ = execute!(
        io::stdout(),
        DisableMouseCapture,
        DisableBracketedPaste,
        Show,
        LeaveAlternateScreen
    );
    let _ = io::stdout().write_all(b"\x1b[?1007r");
    let _ = io::stdout().flush();
}

impl TerminalGuard {
    pub fn enter() -> anyhow::Result<(Self, AppTerminal)> {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore();
            previous(info);
        }));
        enable_raw_mode()?;
        let guard = Self;
        let mut stdout = io::stdout();
        execute!(
            stdout,
            EnterAlternateScreen,
            EnableBracketedPaste,
            EnableMouseCapture,
            Hide
        )?;
        // Save and disable alternate-scroll translation: otherwise some terminals
        // turn wheel input into Up/Down and accidentally navigate prompt history.
        // Hover navigation requires all-motion tracking. The event loop discards
        // motion unless the hovered target changes, avoiding redraw floods.
        stdout.write_all(b"\x1b[?1002l\x1b[?1003h\x1b[?1007s\x1b[?1007l")?;
        stdout.flush()?;
        let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
        Ok((guard, terminal))
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore();
    }
}

/// Keep the terminal IME anchor at the input caret without enabling cursor blink.
/// Rendering (including frozen selection frames) can move the hardware cursor.
pub fn anchor_cursor<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    position: Option<ratatui::layout::Position>,
) -> Result<(), B::Error> {
    if let Some(position) = position {
        terminal.set_cursor_position(position)?;
        terminal.backend_mut().flush()?;
    }
    Ok(())
}
