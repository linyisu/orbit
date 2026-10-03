use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};
use syntect::{easy::HighlightLines, highlighting::HighlightState, parsing::ParseState};

use super::{languages, render::safe_output, theme::CODE_THEME};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Highlighter {
    state: (HighlightState, ParseState),
}

impl Highlighter {
    pub fn new(label: &str) -> Self {
        Self::with_syntax(languages::syntax_for_label(label))
    }

    pub fn for_path(path: &str) -> Self {
        Self::with_syntax(languages::syntax_for_path(path))
    }

    fn with_syntax(syntax: Option<&syntect::parsing::SyntaxReference>) -> Self {
        let syntax = syntax.unwrap_or_else(|| languages::SYNTAXES.find_syntax_plain_text());
        Self {
            state: HighlightLines::new(syntax, &CODE_THEME).state(),
        }
    }

    /// Keep parser state across contiguous source lines, independently for each file or diff side.
    pub fn line(&mut self, source: &str) -> Line<'static> {
        let text = safe_output(source);
        let mut highlighter =
            HighlightLines::from_state(&CODE_THEME, self.state.0.clone(), self.state.1.clone());
        let line = match highlighter.highlight_line(&text, &languages::SYNTAXES) {
            Ok(tokens) => Line::from(
                tokens
                    .into_iter()
                    .map(|(style, token)| {
                        let color = style.foreground;
                        Span::styled(
                            token.to_owned(),
                            Style::default().fg(Color::Rgb(color.r, color.g, color.b)),
                        )
                    })
                    .collect::<Vec<_>>(),
            ),
            Err(_) => Line::from(text),
        };
        self.state = highlighter.state();
        line
    }
}

pub(super) fn file_lines(path: &str, content: &str) -> Vec<Line<'static>> {
    let mut highlighter = Highlighter::for_path(path);
    content.lines().map(|line| highlighter.line(line)).collect()
}
