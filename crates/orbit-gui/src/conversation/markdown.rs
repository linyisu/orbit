//! Repair model-written CJK emphasis only where CommonMark left literal text.
//! Code, escaped markers, links and existing emphasis keep their original source.
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use std::{borrow::Cow, ops::Range};

pub(super) fn prose(source: &str) -> Cow<'_, str> {
    if !source.contains("**") {
        return Cow::Borrowed(source);
    }
    let mut text = Vec::<Range<usize>>::new();
    let mut edits = Vec::new();
    let mut code = false;
    for (event, range) in Parser::new_ext(source, Options::ENABLE_MATH).into_offset_iter() {
        match event {
            Event::Start(Tag::CodeBlock(_)) => code = true,
            Event::End(TagEnd::CodeBlock) => code = false,
            Event::Text(_) if !code => {
                if let Some(previous) = text.last_mut()
                    && previous.end == range.start
                {
                    previous.end = range.end;
                } else {
                    text.push(range);
                }
            }
            Event::End(TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::TableCell)
            | Event::SoftBreak
            | Event::HardBreak => {
                repair(source, &text, &mut edits);
                text.clear();
            }
            _ => {}
        }
    }
    repair(source, &text, &mut edits);
    if edits.is_empty() {
        return Cow::Borrowed(source);
    }
    let mut rendered = String::with_capacity(source.len() + edits.len() * 7);
    let mut cursor = 0;
    for (offset, replacement) in edits {
        rendered.push_str(&source[cursor..offset]);
        rendered.push_str(replacement);
        cursor = offset + 2;
    }
    rendered.push_str(&source[cursor..]);
    Cow::Owned(rendered)
}

fn repair(source: &str, text: &[Range<usize>], edits: &mut Vec<(usize, &'static str)>) {
    let mut opening = None;
    for range in text {
        for (index, _) in source[range.clone()].match_indices("**") {
            let offset = range.start + index;
            let before = &source[..offset];
            let after = &source[offset + 2..];
            if before.ends_with('*')
                || after.starts_with('*')
                || before
                    .bytes()
                    .rev()
                    .take_while(|ch| *ch == b'\\')
                    .count()
                    % 2
                    != 0
            {
                continue;
            }
            if let Some(start) = opening {
                let content = &source[start + 2..offset];
                if !content.is_empty()
                    && !content.ends_with(char::is_whitespace)
                    && content.chars().any(is_cjk)
                {
                    edits.push((start, "<strong>"));
                    edits.push((offset, "</strong>"));
                    opening = None;
                    continue;
                }
            }
            opening =
                (!after.is_empty() && !after.starts_with(char::is_whitespace)).then_some(offset);
        }
    }
}

fn is_cjk(ch: char) -> bool {
    matches!(
        ch,
        '\u{2e80}'..='\u{9fff}' | '\u{f900}'..='\u{faff}' | '\u{20000}'..='\u{323af}'
    )
}

#[cfg(test)]
mod tests {
    use super::prose;

    #[test]
    fn quoted_cjk_emphasis_is_recovered_without_changing_literal_examples() {
        assert_eq!(
            prose("合成的**“紫色方片”和“青色圆片”**。"),
            "合成的<strong>“紫色方片”和“青色圆片”</strong>。"
        );
        for source in [
            "**正常加粗**",
            "`这里**“代码”**`",
            "```\n这里**“代码”**\n```",
            r"这里\*\*“字面量”\*\*",
            "这里** 未完成 **",
            "这里**“未闭合",
            "***粗斜体***",
            "[文档](https://example.test/**中文**)",
            "$a**中文**b$",
        ] {
            assert_eq!(prose(source), source, "{source}");
        }
    }

    #[test]
    fn streamed_closing_markers_repair_the_whole_emphasis_and_keep_nested_content() {
        let source = "这是**“样本”与 $R_{set}=4$**的解释。";
        assert_eq!(
            prose(source),
            "这是<strong>“样本”与 $R_{set}=4$</strong>的解释。"
        );
        let opening = "这是**“样本”";
        assert_eq!(prose(opening), opening);
        assert_eq!(
            prose(&format!("{opening}**。")),
            "这是<strong>“样本”</strong>。"
        );
        let separate = "第一段**“开始”\n\n第二段“结束”**。";
        assert_eq!(prose(separate), separate);
    }
}
