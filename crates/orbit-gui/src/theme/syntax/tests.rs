use super::*;
use gpui_kit::component::ThemeSet;

fn palettes() -> Vec<syntect::highlighting::Theme> {
    let themes: ThemeSet =
        serde_json::from_str(include_str!("../../../appearance/theme.json")).unwrap();
    themes
        .themes
        .into_iter()
        .map(|config| {
            let source = HighlightTheme {
                name: config.name.to_string(),
                appearance: config.mode,
                style: config.highlight.unwrap(),
            };
            syntax_theme(&source, source.style.editor_foreground.unwrap())
        })
        .collect()
}

fn color_at(styles: &[(Range<usize>, HighlightStyle)], offset: usize) -> Hsla {
    styles
        .iter()
        .find(|(range, _)| range.contains(&offset))
        .and_then(|(_, style)| style.color)
        .expect("token must receive a color")
}

#[test]
fn fenced_languages_distinguish_code_from_comments_in_both_themes() {
    for theme in palettes() {
        for (language, source, token) in [
            (
                "verilog",
                "module top; wire [99:0] value; // 中文注释\nendmodule",
                "module",
            ),
            (
                "systemverilog",
                "module top; logic [99:0] value; // 中文注释\nendmodule",
                "logic",
            ),
            ("bash", "cargo run -- --check # 中文注释", "cargo"),
            ("sh", "cargo run -- --check # 中文注释", "cargo"),
            ("rust", "fn main() {} // 中文注释", "fn"),
            ("python", "return 42 # 中文注释", "return"),
            ("sql", "SELECT 42; -- 中文注释", "SELECT"),
            ("cpp", "int main() {} // 中文注释", "int"),
            ("java", "public class Main {} // 中文注释", "public"),
            ("kotlin", "fun main() {} // 中文注释", "fun"),
            ("lua", "local value = 42 -- 中文注释", "local"),
            ("yaml", "enabled: true # 中文注释", "true"),
        ] {
            let block = CodeBlock::from_code(source, Some(language));
            let styles = highlight(&block, &theme);
            assert_ne!(
                color_at(&styles, source.find(token).unwrap()),
                color_at(&styles, source.find("中文").unwrap()),
                "{language} in {:?}",
                theme.name
            );
            assert_eq!(
                block.code().as_ref(),
                source,
                "highlighting cannot alter copied source"
            );
        }
    }
}

#[test]
fn hdl_keywords_numbers_strings_and_comments_have_distinct_roles() {
    let source = "module top;\n  initial $display(\"你好\", 99); // 中文\nendmodule";
    for theme in palettes() {
        for language in [
            "verilog",
            "systemverilog",
            "SystemVerilog",
            "system-verilog",
            "top.sv",
            "{.verilog}",
        ] {
            let styles = highlight(&CodeBlock::from_code(source, Some(language)), &theme);
            let colors: Vec<_> = ["module", "99", "你好", "中文"]
                .map(|token| color_at(&styles, source.find(token).unwrap()))
                .into();
            for (index, color) in colors.iter().enumerate() {
                assert!(
                    !colors[index + 1..].contains(color),
                    "distinct HDL roles for {language} in {:?}",
                    theme.name
                );
            }
        }
    }
}

#[test]
fn multiline_parser_preserves_utf8_and_crlf_offsets() {
    for theme in palettes() {
        for (language, source, first, continuation, following) in [
            (
                "verilog",
                "/* 中文\r\n跨行注释 */\r\nmodule top;\r\nendmodule",
                "中文",
                "跨行",
                "module",
            ),
            (
                "python",
                "text = \"\"\"你好\r\n多行字符串\"\"\"\r\nreturn 42",
                "你好",
                "多行",
                "return",
            ),
        ] {
            let styles = highlight(&CodeBlock::from_code(source, Some(language)), &theme);
            assert_eq!(
                color_at(&styles, source.find(first).unwrap()),
                color_at(&styles, source.find(continuation).unwrap())
            );
            assert_ne!(
                color_at(&styles, source.find(continuation).unwrap()),
                color_at(&styles, source.find(following).unwrap())
            );
            let mut end = 0;
            for (range, _) in styles {
                assert!(end <= range.start && range.start < range.end && range.end <= source.len());
                assert!(source.is_char_boundary(range.start) && source.is_char_boundary(range.end));
                assert!(!source[range.clone()].contains(['\r', '\n']));
                end = range.end;
            }
        }
    }
}

#[test]
fn unknown_and_unsupported_labels_keep_plain_text() {
    for theme in palettes() {
        for language in [
            None,
            Some("unknown-orbit-language"),
            Some("text"),
            Some("v"),
        ] {
            let block = CodeBlock::from_code("module top; // 中文\r\n", language);
            let styles = highlight(&block, &theme);
            // A recognized explicit plain-text grammar may supply one foreground.
            assert!(
                styles
                    .iter()
                    .all(|(_, style)| style.color
                        == styles.first().and_then(|(_, style)| style.color))
            );
            if language != Some("text") {
                assert!(styles.is_empty());
            }
        }
    }
}

#[test]
fn changing_theme_recolors_existing_code_and_incomplete_stream_is_safe() {
    let palettes = palettes();
    let source = "module top;\n/* 未完成";
    let block = CodeBlock::from_code(source, Some("verilog"));
    assert_ne!(
        highlight(&block, &palettes[0]),
        highlight(&block, &palettes[1])
    );
    for theme in palettes {
        for boundary in source
            .char_indices()
            .map(|(index, _)| index)
            .chain([source.len()])
        {
            let prefix = &source[..boundary];
            let styles = highlight(
                &CodeBlock::from_code(prefix.to_owned(), Some("verilog")),
                &theme,
            );
            assert!(styles.iter().all(|(range, _)| range.end <= prefix.len()));
        }
    }
}

#[test]
fn shipped_code_colors_remain_legible_on_code_surfaces() {
    let themes: serde_json::Value =
        serde_json::from_str(include_str!("../../../appearance/theme.json")).unwrap();
    let luminance = |hex: &str| {
        [1, 3, 5]
            .into_iter()
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap() as f64 / 255.)
            .map(|value| {
                if value <= 0.04045 {
                    value / 12.92
                } else {
                    ((value + 0.055) / 1.055).powf(2.4)
                }
            })
            .zip([0.2126, 0.7152, 0.0722])
            .map(|(channel, weight)| channel * weight)
            .sum::<f64>()
    };
    for theme in themes["themes"].as_array().unwrap() {
        let background = luminance(theme["colors"]["muted.background"].as_str().unwrap());
        for (role, style) in theme["highlight"]["syntax"].as_object().unwrap() {
            let foreground = luminance(style["color"].as_str().unwrap());
            let contrast =
                (foreground.max(background) + 0.05) / (foreground.min(background) + 0.05);
            assert!(contrast >= 4.5, "{} {role}: {contrast:.2}", theme["name"]);
        }
    }
}
