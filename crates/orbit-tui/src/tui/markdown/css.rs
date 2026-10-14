//! Parse a deliberately small, portable CSS presentation subset. Pixel lengths
//! become cell spacing; layout, selectors and external resources are not evaluated.
use cssparser::{Delimiter, Parser, ParserInput, Token};
use ratatui::{
    layout::Alignment,
    style::{Color, Modifier, Style},
};

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Presentation {
    pub style: Style,
    pub alignment: Option<Alignment>,
    pub rail: Option<Color>,
    pub padding: bool,
    pub margin: bool,
}

pub(super) fn parse(source: &str) -> Presentation {
    let mut input = ParserInput::new(source);
    let mut parser = Parser::new(&mut input);
    let mut result = Presentation::default();
    while !parser.is_exhausted() {
        let _ = parser.parse_until_after(Delimiter::Semicolon, |value| {
            let property = value
                .expect_ident_cloned()?
                .to_ascii_lowercase();
            value.expect_colon()?;
            let mut tokens = Vec::new();
            while let Ok(token) = value.next() {
                tokens.push(token.clone());
            }
            apply(&mut result, &property, &tokens);
            Ok::<_, cssparser::ParseError<'_, ()>>(())
        });
    }
    // Bright callout backgrounds need a readable default foreground.
    if result.style.fg.is_none()
        && let Some(Color::Rgb(r, g, b)) = result.style.bg
        && u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114 > 160_000
    {
        result.style = result.style.fg(Color::Rgb(35, 40, 45));
    }

    result
}

fn color(token: &Token<'_>) -> Option<Color> {
    let (r, g, b) = match token {
        Token::Hash(value) | Token::IDHash(value) => {
            let (r, g, b, alpha) = cssparser::color::parse_hash_color(value.as_bytes()).ok()?;
            if alpha < 1.0 {
                return None;
            }
            (r, g, b)
        }

        Token::Ident(value) => cssparser::color::parse_named_color(value).ok()?,
        _ => return None,
    };
    Some(Color::Rgb(r, g, b))
}

fn apply(out: &mut Presentation, property: &str, tokens: &[Token<'_>]) {
    let first = tokens.first();
    let ident = match first {
        Some(Token::Ident(value)) => value.to_ascii_lowercase(),
        _ => String::new(),
    };

    match property {
        "color" => {
            if let Some(color) = first.and_then(color) {
                out.style = out.style.fg(color);
            }
        }

        "background" | "background-color" => {
            if let Some(color) = first.and_then(color) {
                out.style = out.style.bg(color);
            }
        }

        "font-weight" => {
            if matches!(ident.as_str(), "bold" | "bolder")
                || matches!(
                    first,
                    Some(Token::Number { value, .. }) if *value >= 600.0
                )
            {
                out.style = out.style.add_modifier(Modifier::BOLD);
            } else if ident == "normal"
                || matches!(
                    first,
                    Some(Token::Number { value, .. }) if *value < 600.0
                )
            {
                out.style = out
                    .style
                    .remove_modifier(Modifier::BOLD);
            }
        }

        "font-style" if ident == "italic" || ident == "oblique" => {
            out.style = out.style.add_modifier(Modifier::ITALIC);
        }
        "text-decoration" | "text-decoration-line" => {
            for token in tokens {
                if let Token::Ident(value) = token {
                    match value.to_ascii_lowercase().as_str() {
                        "underline" => {
                            out.style = out
                                .style
                                .add_modifier(Modifier::UNDERLINED)
                        }
                        "line-through" => {
                            out.style = out
                                .style
                                .add_modifier(Modifier::CROSSED_OUT)
                        }
                        _ => {}
                    }
                }
            }
        }

        "text-align" => out.alignment = alignment(&ident),
        "border-left" | "border-left-color" => out.rail = tokens.iter().find_map(color),
        "padding" | "margin" => {
            let nonzero = tokens.iter().any(|token| {
                matches!(
                    token,
                    Token::Dimension { value, .. } | Token::Number { value, .. }
                        if *value > 0.0
                )
            });
            if property == "padding" {
                out.padding = nonzero;
            } else {
                out.margin = nonzero;
            }
        }

        _ => {}
    }
}

pub(super) fn alignment(value: &str) -> Option<Alignment> {
    match value.to_ascii_lowercase().as_str() {
        "center" => Some(Alignment::Center),
        "right" => Some(Alignment::Right),
        "left" => Some(Alignment::Left),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn css_parser_skips_invalid_and_unsupported_declarations_and_obeys_later_values() {
        let value = parse(
            "broken; color:red; color: #12ab34; /* separator */ font-weight:bold; \
                font-style:italic; text-decoration:underline line-through; \
                background-image:url('https://orbit.example/no-fetch'); text-align:center; \
                font-size:200px;",
        );
        assert_eq!(value.style.fg, Some(Color::Rgb(18, 171, 52)));
        assert!(value.style.add_modifier.contains(
            Modifier::BOLD | Modifier::ITALIC | Modifier::UNDERLINED | Modifier::CROSSED_OUT
        ));
        assert_eq!(value.alignment, Some(Alignment::Center));
        assert_eq!(value.style.bg, None);
        assert_eq!(
            parse("color: rgba(0,0,0,0.3); color:#1234")
                .style
                .fg,
            None
        );
    }
}
