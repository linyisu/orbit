//! Match ordering shared by completion providers and interactive pickers.

pub fn rank(text: &str, query: &str) -> Option<u8> {
    let text = text.to_lowercase();
    let query = query.to_lowercase();
    if text.starts_with(&query) {
        return Some(0);
    }
    if text.contains(&query) {
        return Some(1);
    }

    let mut remaining = query.chars();
    let mut next = remaining.next();
    for character in text.chars() {
        if Some(character) == next {
            next = remaining.next();
            if next.is_none() {
                return Some(2);
            }
        }
    }

    None
}

pub fn ranked<T>(items: impl IntoIterator<Item = T>, score: impl Fn(&T) -> Option<u8>) -> Vec<T> {
    let mut matches = items
        .into_iter()
        .filter_map(|item| score(&item).map(|rank| (rank, item)))
        .collect::<Vec<_>>();
    matches.sort_by_key(|(rank, _)| *rank);
    matches.into_iter().map(|(_, item)| item).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_then_substring_then_subsequence_preserves_ties() {
        let values = ["aXbYc", "zabc", "ABC-first", "abc-second", "acb"];
        assert_eq!(
            ranked(values, |value| rank(value, "abc")),
            ["ABC-first", "abc-second", "zabc", "aXbYc"]
        );
        assert_eq!(rank("Desktop", "dsk"), Some(2));
        assert_eq!(rank("中文会话标题", "中话题"), Some(2));
        assert_eq!(rank("中文会话标题", "题中"), None);
        assert_eq!(ranked(values, |value| rank(value, "")), values);
    }
}
