use std::io::{self, Stdout, Write};

use crossterm::{
    ExecutableCommand, QueueableCommand,
    cursor::{Hide, SetCursorStyle, Show},
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
    buffer::{Buffer, CellDiffOption, CellWidth},
    layout::Rect,
    style::Color,
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

/// Make every cell differ from the fresh drawing buffer. This clears stale cells with ordinary cell
/// updates instead of a visible Clear(All).
fn prime_full_redraw<B: Backend>(terminal: &mut Terminal<B>) {
    for cell in &mut terminal.current_buffer_mut().content {
        cell.set_bg(crate::tui::theme::THEME.repaint_sentinel);
    }

    terminal.swap_buffers();
}

/// Replacing an old wide glyph can spread the new background to its other half. Repaint the
/// boundary even when the next blank matches the cache.
fn mark_background_edges(buffer: &mut Buffer) {
    let area = buffer.area;
    for y in area.top()..area.bottom() {
        let mut x = area.left();
        while x < area.right() {
            let cell = &buffer[(x, y)];
            let next = x.saturating_add(cell.cell_width().max(1));
            if next < area.right()
                && cell.bg != Color::Reset
                && cell.bg != buffer[(next, y)].bg
                && buffer[(next, y)].diff_option == CellDiffOption::None
            {
                buffer[(next, y)].set_diff_option(CellDiffOption::AlwaysUpdate);
            }

            x = next;
        }
    }
}

fn draw_buffer<B: Backend>(
    terminal: &mut Terminal<B>,
    render: impl FnOnce(&mut Frame<'_>),
) -> Result<(), B::Error> {
    terminal
        .draw(|frame| {
            super::media::begin_frame();
            render(frame);
            super::media::paint(frame.buffer_mut());
            mark_background_edges(frame.buffer_mut());
        })
        .map(|_| ())
}

pub struct TerminalGuard;

fn restore() {
    super::media::clear_layers();
    let _ = super::media::delete_stale(&mut io::stdout());
    let _ = disable_raw_mode();
    let _ = execute!(
        io::stdout(),
        DisableMouseCapture,
        DisableBracketedPaste,
        SetCursorStyle::DefaultUserShape,
        Show,
        LeaveAlternateScreen
    );
    let _ = io::stdout().write_all(b"\x1b[?1007r");
    let _ = io::stdout().flush();
}

impl TerminalGuard {
    pub fn enter() -> anyhow::Result<(Self, AppTerminal)> {
        static PANIC_HOOK: std::sync::Once = std::sync::Once::new();
        PANIC_HOOK.call_once(|| {
            let previous = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                restore();
                previous(info);
            }));
        });
        enable_raw_mode()?;
        let guard = Self;
        let mut stdout = io::stdout();
        execute!(
            stdout,
            EnterAlternateScreen,
            EnableBracketedPaste,
            EnableMouseCapture,
            SetCursorStyle::SteadyBlock,
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

pub fn suspend() {
    restore();
}

pub fn resume(terminal: &mut AppTerminal) -> io::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableBracketedPaste,
        EnableMouseCapture,
        SetCursorStyle::SteadyBlock,
        Hide
    )?;
    stdout.write_all(b"\x1b[?1002l\x1b[?1003h\x1b[?1007s\x1b[?1007l")?;
    stdout.flush()?;
    let (width, height) = terminal_size()?;
    *terminal = fixed_terminal(io::stdout(), Rect::new(0, 0, width, height))?;
    prime_full_redraw(terminal);

    Ok(())
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
    terminal
        .backend_mut()
        .queue(BeginSynchronizedUpdate)?;
    super::media::delete_stale(&mut io::stdout())?;
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

    #[test]
    fn overlay_repaint_clears_terminal_only_highlight_artifacts() {
        use ratatui::{
            layout::Rect,
            style::{Color, Style},
            text::Line,
            widgets::Paragraph,
        };

        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
        let draw = |frame: &mut ratatui::Frame<'_>| {
            super::super::overlay::clear(frame, Rect::new(2, 2, 36, 8));
            frame.render_widget(
                Paragraph::new(Line::styled(
                    "模型 (视觉)",
                    Style::default().bg(crate::tui::theme::THEME.accent),
                )),
                Rect::new(3, 3, 30, 1),
            );
        };

        draw_buffer(&mut terminal, draw).unwrap();
        let mut artifact = Cell::default();
        artifact.set_bg(crate::tui::theme::THEME.accent);
        terminal
            .backend_mut()
            .draw([(24, 5, &artifact), (28, 7, &artifact)].into_iter())
            .unwrap();

        draw_buffer(&mut terminal, draw).unwrap();

        for position in [(24, 5), (28, 7)] {
            let cell = &terminal.backend().buffer()[position];
            assert_eq!(cell.symbol(), " ");
            assert_eq!(cell.bg, Color::Reset, "stale highlight at {position:?}");
        }

        assert_eq!(
            terminal.backend().buffer()[(3, 3)].bg,
            crate::tui::theme::THEME.accent
        );
    }

    #[test]
    fn scrolling_a_code_badge_over_cjk_repaints_its_trailing_blank() {
        use ratatui::style::Color;

        let mut layout = super::super::transcript::TranscriptLayout::default();
        layout.update(
            &[super::super::app::TranscriptItem::Assistant(
                "Orbit: 中文标签\n```python\nprint('Orbit')\n```\n后文".into(),
            )],
            None,
            30,
        );
        let mut terminal = Terminal::new(TestBackend::new(30, 2)).unwrap();

        for _ in 0..3 {
            draw_buffer(&mut terminal, |frame| layout.draw(frame, frame.area(), 0)).unwrap();
            let previous = terminal.backend().buffer().clone();
            assert_eq!(previous[(11, 0)].symbol(), "文");
            assert_eq!(previous[(11, 0)].bg, Color::Reset);

            draw_buffer(&mut terminal, |frame| layout.draw(frame, frame.area(), 1)).unwrap();
            let current = terminal.backend().buffer();
            assert_eq!(current[(11, 0)].symbol(), " ");
            assert_ne!(current[(11, 0)].bg, Color::Reset);
            assert_eq!(current[(12, 0)].symbol(), " ");
            assert_eq!(current[(12, 0)].bg, Color::Reset);
            assert!(
                previous
                    .diff(current)
                    .iter()
                    .any(|(x, y, cell)| {
                        (*x, *y) == (12, 0) && cell.symbol() == " " && cell.bg == Color::Reset
                    }),
                "the terminal must receive a background reset after the badge's final padding cell"
            );

            draw_buffer(&mut terminal, |frame| layout.draw(frame, frame.area(), 3)).unwrap();
        }
    }
}
