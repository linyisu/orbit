use std::io::{self, Stdout, Write};

use crossterm::{
    ExecutableCommand, QueueableCommand,
    cursor::{Hide, Show},
    event::{DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture},
    execute,
    terminal::{
        BeginSynchronizedUpdate, EndSynchronizedUpdate, EnterAlternateScreen, LeaveAlternateScreen,
        disable_raw_mode, enable_raw_mode, size as terminal_size,
    },
};
use ratatui::{
    Frame, Terminal, TerminalOptions, Viewport,
    backend::{Backend, CrosstermBackend},
    layout::Rect,
};

pub type AppTerminal = Terminal<CrosstermBackend<Stdout>>;

fn fixed_terminal(stdout: Stdout, area: Rect) -> io::Result<AppTerminal> {
    Terminal::with_options(
        CrosstermBackend::new(stdout),
        TerminalOptions {
            viewport: Viewport::Fixed(area),
        },
    )
}

fn prime_full_redraw<B: Backend>(terminal: &mut Terminal<B>) {
    // Make every cell differ from the fresh drawing buffer. This clears stale
    // cells with ordinary cell updates instead of a visible Clear(All).
    for cell in &mut terminal.current_buffer_mut().content {
        cell.set_bg(ratatui::style::Color::Rgb(1, 2, 3));
    }
    terminal.swap_buffers();
}

fn draw_buffer<B: Backend>(
    terminal: &mut Terminal<B>,
    render: impl FnOnce(&mut Frame<'_>),
) -> Result<(), B::Error> {
    {
        let mut frame = terminal.get_frame();
        render(&mut frame);
    }
    terminal.flush()?;
    terminal.swap_buffers();
    Backend::flush(terminal.backend_mut())
}

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
        let (width, height) = terminal_size()?;
        let terminal = fixed_terminal(stdout, Rect::new(0, 0, width, height))?;
        Ok((guard, terminal))
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore();
    }
}

/// Keep clear-and-repaint frames hidden until they are complete. Terminals
/// without synchronized-update support safely ignore these control sequences.
pub fn draw_synchronized(
    terminal: &mut AppTerminal,
    render: impl FnOnce(&mut Frame<'_>),
) -> io::Result<()> {
    terminal.backend_mut().queue(BeginSynchronizedUpdate)?;
    let draw = (|| {
        let actual: Rect = terminal.size()?.into();
        let previous = terminal.get_frame().area();
        if actual != previous {
            // Fullscreen autoresize calls Clear(All). Recreate a fixed viewport
            // at the new size so dragging remains immediate without blank frames.
            let mut replacement = fixed_terminal(io::stdout(), actual)?;
            prime_full_redraw(&mut replacement);
            let old = std::mem::replace(terminal, replacement);
            drop(old);
        }
        draw_buffer(terminal, render)
    })();
    let end = terminal
        .backend_mut()
        .execute(EndSynchronizedUpdate)
        .map(drop);
    draw?;
    end
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

#[cfg(test)]
mod tests {
    use ratatui::{
        Terminal,
        backend::{Backend, TestBackend},
        buffer::Cell,
    };

    use super::{draw_buffer, prime_full_redraw};

    #[test]
    fn full_redraw_overwrites_stale_cells_without_a_terminal_clear() {
        let mut terminal = Terminal::new(TestBackend::new(3, 2)).unwrap();
        let mut stale = Cell::default();
        stale.set_symbol("x");
        terminal
            .backend_mut()
            .draw(std::iter::once((0, 0, &stale)))
            .unwrap();

        prime_full_redraw(&mut terminal);
        draw_buffer(&mut terminal, |_| {}).unwrap();

        assert_eq!(terminal.backend().buffer()[(0, 0)].symbol(), " ");
    }
}
