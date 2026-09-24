//! Human-readable tool cards. Canonical tool messages retain the original JSON.

use std::{collections::BTreeMap, fmt::Write};

use serde_json::Value;

pub fn display(name: &str, content: &str) -> (String, String) {
    let Ok(value) = serde_json::from_str::<Value>(content) else {
        return (content.to_owned(), first_line(content));
    };

    match name {
        "read_file" => read_file(&value),
        "search_text" => search_text(&value),
        "memory_save" | "memory_forget" => memory(&value),
        _ => return generic(&value, content),
    }
    .unwrap_or_else(|| generic(&value, content))
}

fn read_file(value: &Value) -> Option<(String, String)> {
    let content = value.get("content")?.as_str()?;
    let start = value.get("start_line")?.as_u64()?;
    let end = value.get("end_line")?.as_u64()?;
    let width = end.to_string().len().max(1);
    let mut output = String::new();
    for source in content.lines() {
        if let Some((number, line)) = source.split_once(": ")
            && number.bytes().all(|byte| byte.is_ascii_digit())
        {
            let _ = writeln!(output, "{number:>width$} │ {line}");
        } else {
            let _ = writeln!(output, "{} │ {source}", " ".repeat(width));
        }
    }
    if output.is_empty() {
        output.push_str("(empty file)");
    }
    if value.get("truncated").and_then(Value::as_bool) == Some(true) {
        output.push_str("More content is available with another read_file range.\n");
    }

    let summary = format!("lines {start}–{end}");
    let preview = format!("{summary}\n{}", preview_lines(&output, 4));
    Some((output.trim_end().to_owned(), preview.trim_end().to_owned()))
}

fn search_text(value: &Value) -> Option<(String, String)> {
    let matches = value.get("matches")?.as_array()?;
    let mut files = BTreeMap::<&str, Vec<&Value>>::new();
    for entry in matches {
        files
            .entry(entry.get("path")?.as_str()?)
            .or_default()
            .push(entry);
    }

    let count = matches.len();
    let file_count = files.len();
    let heading = format!(
        "{count} {} in {file_count} {}",
        if count == 1 { "match" } else { "matches" },
        if file_count == 1 { "file" } else { "files" }
    );
    let mut output = format!("{heading}\n");
    for (path, entries) in files {
        let _ = writeln!(output, "\n{path}");
        let mut lines = BTreeMap::<u64, (&str, bool)>::new();
        for entry in entries {
            if let Some(context) = entry.get("context").and_then(Value::as_array) {
                for surrounding in context {
                    let line = surrounding.get("line")?.as_u64()?;
                    let text = surrounding.get("text")?.as_str()?;
                    lines.entry(line).or_insert((text, false));
                }
            }
            let line = entry.get("line")?.as_u64()?;
            let text = entry.get("text")?.as_str()?;
            lines.insert(line, (text, true));
        }
        let width = lines
            .keys()
            .next_back()
            .copied()
            .unwrap_or(1)
            .to_string()
            .len();
        for (line, (text, matched)) in lines {
            let marker = if matched { '>' } else { ' ' };
            let _ = writeln!(output, "{marker} {line:>width$} │ {text}");
        }
    }
    if value.get("has_more").and_then(Value::as_bool) == Some(true) {
        output.push_str("\nMore matches are available with the next offset.\n");
    }
    if value.get("scan_truncated").and_then(Value::as_bool) == Some(true) {
        output.push_str("\nThe search scan was truncated; narrow the path or query.\n");
    }

    Some((output.trim_end().to_owned(), preview_lines(&output, 5)))
}

fn memory(value: &Value) -> Option<(String, String)> {
    let memory = value.get("memory")?;
    let id = memory.get("id")?.as_i64()?;
    let title = memory.get("title")?.as_str()?;
    let category = memory.get("category")?.as_str()?;
    let content = memory.get("content")?.as_str()?;
    let status = if memory.get("active").and_then(Value::as_bool) == Some(false) {
        "Inactive"
    } else {
        "Saved"
    };
    let heading = format!("{status} · {category} · current session · #{id}");
    let output = format!("{heading}\n{title}\n\n{content}");
    Some((output, format!("{title}\n{heading}")))
}

fn generic(value: &Value, content: &str) -> (String, String) {
    let full = serde_json::to_string_pretty(value).unwrap_or_else(|_| content.to_owned());
    let summary = if let Some(entries) = value.get("entries").and_then(Value::as_array) {
        format!("{} entries", entries.len())
    } else {
        ["content", "stdout", "message", "summary"]
            .into_iter()
            .find_map(|key| value.get(key).and_then(Value::as_str))
            .map(first_line)
            .unwrap_or_else(|| first_line(content))
    };
    (full, summary)
}

fn first_line(text: &str) -> String {
    text.lines()
        .find(|line| !line.is_empty())
        .unwrap_or("")
        .chars()
        .take(120)
        .collect()
}

fn preview_lines(text: &str, limit: usize) -> String {
    text.lines().take(limit).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn read_file_formats_numbered_lines() {
        let raw = json!({
            "path": "src/lib.rs",
            "start_line": 9,
            "end_line": 10,
            "total_lines": 20,
            "truncated": true,
            "content": "9: let x = 1;\n10: println!(\"{x}\");\n"
        })
        .to_string();

        let (full, preview) = display("read_file", &raw);

        assert!(full.contains(" 9 │ let x = 1;"), "{full}");
        assert!(full.contains("10 │ println!"), "{full}");
        assert!(preview.starts_with("lines 9–10"), "{preview}");
    }

    #[test]
    fn search_groups_matches_and_memory_shows_its_title() {
        let search = json!({
            "matches": [
                { "path": "a.rs", "line": 2, "text": "first", "column": 1, "context": [{ "line": 1, "text": "before" }] },
                { "path": "b.rs", "line": 12, "text": "second", "column": 1, "context": [] }
            ],
            "has_more": false,
            "scan_truncated": false
        });
        let (full, preview) = display("search_text", &search.to_string());
        assert!(full.contains("a.rs\n  1 │ before\n> 2 │ first"), "{full}");
        assert!(full.contains("b.rs\n> 12 │ second"), "{full}");
        assert!(preview.starts_with("2 matches in 2 files"));

        let saved = json!({
            "memory": {
                "id": 3,
                "category": "fact",
                "title": "Preferred checks",
                "content": "Run cargo test.",
                "active": true
            }
        });
        let (full, preview) = display("memory_save", &saved.to_string());
        assert!(full.contains("Run cargo test."));
        assert!(preview.contains("Preferred checks"));
    }
}
