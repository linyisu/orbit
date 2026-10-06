//! HTML tokenization only. Formatting and layout belong to the shared inline renderer.
use std::cell::RefCell;

use html5ever::{
    tendril::StrTendril,
    tokenizer::{BufferQueue, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer},
};
use ratatui::style::Style;

use super::{css, inline::Formatting};

pub(super) enum Event {
    Start(String, Formatting),
    End(String),
    Text(String),
    Break,
    Image(String, String),
}

#[derive(Default)]
struct Sink(RefCell<Vec<Token>>);

impl TokenSink for Sink {
    type Handle = ();

    fn process_token(&self, token: Token, _: u64) -> TokenSinkResult<()> {
        self.0.borrow_mut().push(token);
        TokenSinkResult::Continue
    }
}

/// Unsupported constructs remain visible as their original source. The tokenizer
/// handles quoting, entities, case folding and malformed input before this semantic map.
pub(super) fn events(source: &str) -> Option<Vec<Event>> {
    let input = BufferQueue::default();
    input.push_back(StrTendril::from(source));
    let tokenizer = Tokenizer::new(Sink::default(), Default::default());
    let _ = tokenizer.feed(&input);
    tokenizer.end();
    let mut events = Vec::new();
    for token in tokenizer.sink.0.into_inner() {
        match token {
            Token::TagToken(tag) => {
                let name = tag.name.to_string();
                let attr = |key: &str| {
                    tag.attrs
                        .iter()
                        .find(|attr| attr.name.local.as_ref() == key)
                        .map(|attr| attr.value.to_string())
                        .unwrap_or_default()
                };
                let formatting = match name.as_str() {
                    "b" | "strong" => Formatting::Bold,
                    "i" | "em" => Formatting::Italic,
                    "s" | "del" => Formatting::Strike,
                    "u" => Formatting::Underline,
                    "mark" => Formatting::Mark,
                    "code" => Formatting::Code,
                    "span" => Formatting::Custom(Style::default()),
                    "img" if tag.kind == TagKind::StartTag => {
                        let label = attr("alt");
                        events.push(Event::Image(
                            if label.is_empty() {
                                "图片".into()
                            } else {
                                label
                            },
                            attr("src"),
                        ));
                        continue;
                    }

                    "a" if attr("href").starts_with('#') => Formatting::Custom(Style::default()),
                    "a" => Formatting::Link(
                        tag.attrs
                            .iter()
                            .find(|attr| attr.name.local.as_ref() == "href")
                            .map(|attr| attr.value.to_string())
                            .unwrap_or_default(),
                    ),

                    "br" if tag.kind == TagKind::StartTag => {
                        events.push(Event::Break);
                        continue;
                    }

                    _ => return None,
                };

                if tag.kind == TagKind::EndTag {
                    events.push(Event::End(name));
                } else if !tag.self_closing {
                    let presentation = css::parse(&attr("style"));
                    let formatting = if presentation.style == Style::default() {
                        formatting
                    } else {
                        Formatting::Group(vec![formatting, Formatting::Custom(presentation.style)])
                    };

                    events.push(Event::Start(name, formatting));
                }
            }

            Token::CharacterTokens(text) => events.push(Event::Text(text.to_string())),
            Token::EOFToken => {}
            Token::ParseError(_) => return None,
            _ => return None,
        }
    }

    Some(events)
}
