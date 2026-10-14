//! Bounded anchor search and diagnostics. Candidates never authorize a write.
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};

use super::MAX_EXCERPT_CHARS;

const MAX_WINDOWS: usize = 64;
const COMPARISON_BYTES: usize = 4 * 1_024 * 1_024;

fn key(line: &str) -> String {
    line.chars()
        .filter(|c| !c.is_whitespace())
        .collect()
}

fn near_line(left: &str, right: &str) -> bool {
    let length = left.len().max(right.len());
    if left.len().min(right.len()) < 4 {
        return false;
    }
    let prefix = left
        .bytes()
        .zip(right.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    let suffix = left.as_bytes()[prefix..]
        .iter()
        .rev()
        .zip(right.as_bytes()[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    (prefix + suffix) * 100 >= length * 85
}

pub(super) fn find(before: &str, old: &str) -> (Vec<Value>, bool) {
    let pattern: Vec<_> = old.split_terminator('\n').collect();
    let lines: Vec<_> = before.split_inclusive('\n').collect();
    if pattern.is_empty() || pattern.len() > lines.len() {
        return (Vec::new(), false);
    }
    let expected: Vec<_> = pattern.iter().map(|s| key(s)).collect();
    let current: Vec<_> = lines.iter().map(|s| key(s)).collect();
    let mut locations = HashMap::<&str, Vec<usize>>::new();
    for (index, text) in current.iter().enumerate() {
        if !text.is_empty() {
            locations
                .entry(text)
                .or_default()
                .push(index);
        }
    }
    let meaningful: Vec<_> = expected
        .iter()
        .enumerate()
        .filter(|(_, s)| s.len() >= 4)
        .map(|(i, _)| i)
        .collect();
    if meaningful.is_empty() {
        return (Vec::new(), false);
    }
    let mut anchors = Vec::new();
    for n in 0..5 {
        anchors.push(meaningful[n * (meaningful.len() - 1) / 4]);
    }
    anchors.dedup();
    let mut votes = BTreeMap::<usize, usize>::new();
    for anchor in anchors {
        if let Some(indices) = locations.get(expected[anchor].as_str()) {
            for &index in indices {
                if let Some(start) = index
                    .checked_sub(anchor)
                    .filter(|start| start + pattern.len() <= lines.len())
                {
                    *votes.entry(start).or_default() += 1;
                }
            }
        }
    }
    let mut budget = COMPARISON_BYTES;
    let mut exhausted = false;
    // A single changed line has no unchanged anchor. Only consider a close
    // prefix/suffix match; do not run edit distance over every file window.
    if votes.is_empty() && pattern.len() == 1 {
        for (index, text) in current.iter().enumerate() {
            let cost = text.len() + expected[0].len();
            if cost > budget {
                exhausted = true;
                break;
            }
            budget -= cost;
            if near_line(text, &expected[0]) {
                votes.insert(index, 1);
            }
        }
    }
    let mut windows: Vec<_> = votes.into_iter().collect();
    windows.sort_by_key(|(line, votes)| (std::cmp::Reverse(*votes), *line));
    exhausted |= windows.len() > MAX_WINDOWS;
    let mut candidates = Vec::new();
    for (start, _) in windows.into_iter().take(MAX_WINDOWS) {
        let mut matched = 0;
        for (a, b) in current[start..start + pattern.len()]
            .iter()
            .zip(&expected)
        {
            let cost = (a.len() + b.len()).max(1);
            if cost > budget {
                return (candidates, true);
            }
            budget -= cost;
            if a == b || near_line(a, b) {
                matched += 1;
            }
        }
        if matched + 1 < pattern.len() && matched * 100 < pattern.len() * 90 {
            continue;
        }
        let window = &lines[start..start + pattern.len()];
        // The single-line near-match search already checks similarity.
        if pattern.len() == 1
            && current[start] != expected[0]
            && !near_line(&current[start], &expected[0])
        {
            continue;
        }
        if candidates.len() == super::MAX_CANDIDATES {
            return (candidates, true);
        }
        let excerpt: String = window
            .iter()
            .flat_map(|s| s.chars())
            .take(MAX_EXCERPT_CHARS)
            .collect();
        let difference = window
            .iter()
            .zip(&pattern)
            .enumerate()
            .find_map(|(offset, (a, b))| {
                let actual = a.strip_suffix('\n').unwrap_or(a);
                if actual == *b {
                    return None;
                }
                let column = actual
                    .chars()
                    .zip(b.chars())
                    .take_while(|(a, b)| a == b)
                    .count()
                    + 1;
                Some(json!({
                    "line": start + offset + 1,
                    "column": column,
                    "current_line": actual
                        .chars()
                        .take(200)
                        .collect::<String>(),
                    "expected_line": b.chars().take(200).collect::<String>(),
                    "line_text_truncated": actual.chars().count() > 200 || b.chars().count() > 200,
                }))
            });
        candidates.push(json!({
            "line": start+1,
            "end_line": start+pattern.len(),
            "basis": "nearby lines found by bounded anchors; verify current text, whitespace and \
                line endings",
            "current_text": excerpt,
            "text_truncated": window
                .iter()
                .map(|s|s
                .chars()
                .count())
                .sum::<usize>()>MAX_EXCERPT_CHARS,
            "first_difference": difference,
        }));
    }
    (candidates, exhausted)
}
