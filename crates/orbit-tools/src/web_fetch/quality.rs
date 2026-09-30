//! Detect known JavaScript-only HTML shells before returning page content.

pub(super) fn javascript_shell(text: &str) -> bool {
    let text = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    text.len() < 600
        && [
            "for full functionality of this site it is necessary to enable javascript",
            "you need to enable javascript to run this app",
            "please enable javascript to continue",
            "javascript is required to run this application",
            "请启用javascript",
            "请启用 javascript",
        ]
        .iter()
        .any(|message| text.contains(message))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identifies_js_shell_but_not_substantive_articles_or_json() {
        assert!(javascript_shell(
            "For full functionality of this site it is necessary to enable JavaScript."
        ));
        assert!(javascript_shell(
            "You need to enable JavaScript to run this app."
        ));
        assert!(!javascript_shell(&format!(
            "Please enable JavaScript to continue. {}",
            "Article content. ".repeat(60)
        )));
        assert!(!javascript_shell(
            "An introduction to JavaScript programming."
        ));
    }
}
