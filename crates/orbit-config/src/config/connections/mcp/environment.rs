//! Text editing syntax for MCP environment maps; TOML and process values stay literal.
use anyhow::{Context, Result, bail, ensure};
use std::collections::BTreeMap;

/// Parse one assignment per line. Bare values trim surrounding whitespace;
/// single quotes preserve literal contents and double quotes use JSON escapes.
/// No variable expansion, command substitution or shell execution occurs.
pub fn parse(source: &str) -> Result<BTreeMap<String, String>> {
    let mut values = BTreeMap::new();
    for (index, line) in source.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let value = (|| -> Result<(String, String)> {
            let (key, value) = line.split_once('=').context("环境变量格式应为 KEY=value")?;
            let key = key.trim();
            ensure!(!key.is_empty(), "环境变量名称不能为空");
            let value = value.trim();
            let value = if value.starts_with('"') {
                serde_json::from_str::<String>(value)
                    .context("双引号值应为完整 JSON 字符串；反斜杠需转义")?
            } else if let Some(inner) = value.strip_prefix('\'') {
                inner
                    .strip_suffix('\'')
                    .context("环境变量的单引号未闭合")?
                    .to_owned()
            } else {
                value.to_owned()
            };
            ensure!(
                !key.contains('\0') && !value.contains('\0'),
                "环境变量不能包含 NUL 字符"
            );
            Ok((key.to_owned(), value))
        })()
        .with_context(|| format!("环境变量第 {} 行", index + 1))?;
        if values.insert(value.0, value.1).is_some() {
            bail!("环境变量第 {} 行：名称重复", index + 1);
        }
    }
    Ok(values)
}

/// Preserve significant whitespace, quotes and multiline values across editing.
pub fn format(values: &BTreeMap<String, String>) -> String {
    values
        .iter()
        .map(|(key, value)| {
            let value = if value.trim() != value
                || value
                    .chars()
                    .any(|ch| ch.is_control() || matches!(ch, '\'' | '"'))
            {
                serde_json::Value::String(value.clone()).to_string()
            } else {
                value.clone()
            };
            format!("{key}={value}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoted_paths_and_assignment_spacing_are_not_part_of_process_values() {
        for input in [
            "GHIDRA_INSTALL_DIR=/opt/ghidra",
            " GHIDRA_INSTALL_DIR = '/opt/ghidra' ",
            "GHIDRA_INSTALL_DIR= \"/opt/ghidra\"",
        ] {
            assert_eq!(parse(input).unwrap()["GHIDRA_INSTALL_DIR"], "/opt/ghidra");
        }
        let values = parse("TOKEN=a=b=c\nEMPTY=\nLITERAL=$HOME/$(command)\nSPACES=' a b '\nWINDOWS=C:\\Tools\\Ghidra").unwrap();
        assert_eq!(values["TOKEN"], "a=b=c");
        assert_eq!(values["EMPTY"], "");
        assert_eq!(values["LITERAL"], "$HOME/$(command)");
        assert_eq!(values["SPACES"], " a b ");
        assert_eq!(values["WINDOWS"], "C:\\Tools\\Ghidra");
    }

    #[test]
    fn display_round_trips_literal_quotes_spaces_and_multiline_values() {
        let values: BTreeMap<_, _> = [
            ("EMPTY", ""),
            ("PATH_VALUE", "/opt/ghidra"),
            ("SPACES", " spaced "),
            ("QUOTED", "'literal'"),
            ("DOUBLE", "\"literal\""),
            ("LINES", "first\nsecond\r\n"),
            ("TABS", "a\tb"),
            ("BACKSLASH", "C:\\Tools\\Ghidra"),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect();
        let displayed = format(&values);
        assert_eq!(displayed.lines().count(), values.len());
        assert_eq!(parse(&displayed).unwrap(), values);
        assert!(displayed.contains("PATH_VALUE=/opt/ghidra"));
    }

    #[test]
    fn invalid_assignments_are_rejected_without_echoing_secret_values() {
        for input in [
            "=secret",
            "missing-equals",
            "TOKEN='secret",
            "TOKEN=\"secret",
            "TOKEN=a\n TOKEN=b",
            "TOKEN=\"\\u0000\"",
        ] {
            let error = format!("{:#}", parse(input).unwrap_err());
            assert!(!error.contains("secret"));
        }
    }
}
