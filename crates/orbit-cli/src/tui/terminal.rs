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
        // Keep button/drag tracking, but do not flood the event loop with
        // ordinary pointer motion (1003) while the user is not selecting.
        stdout.write_all(b"\x1b[?1003l\x1b[?1007s\x1b[?1007l")?;
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
