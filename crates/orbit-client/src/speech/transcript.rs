//! Committed utterances followed by one replaceable Qwen hypothesis.
#[derive(Default)]
pub(super) struct Transcript {
    completed: String,
    live: String,
}

impl Transcript {
    pub fn preview(&mut self, text: String) {
        self.live = text;
    }

    pub fn commit(&mut self) {
        append(&mut self.completed, &self.live);
        self.live.clear();
    }

    pub fn text(&self) -> String {
        let mut text = self.completed.clone();
        append(&mut text, &self.live);
        text
    }
}

fn append(text: &mut String, part: &str) {
    let part = part.trim();
    if part.is_empty() {
        return;
    }
    if text.ends_with(|c: char| c.is_ascii_alphanumeric())
        && part.starts_with(|c: char| c.is_ascii_alphanumeric())
    {
        text.push(' ');
    }
    text.push_str(part);
}

#[cfg(test)]
mod tests {
    use super::Transcript;

    #[test]
    fn revisions_replace_only_the_live_utterance_and_commit_once() {
        let mut transcript = Transcript::default();
        transcript.preview("检查 Alpha。".into());
        transcript.commit();
        transcript.preview("再检查贝塔".into());
        transcript.preview("再检查 Beta。".into());
        assert_eq!(transcript.text(), "检查 Alpha。再检查 Beta。");
        transcript.commit();
        transcript.commit();
        assert_eq!(transcript.text(), "检查 Alpha。再检查 Beta。");
    }

    #[test]
    fn empty_revisions_preserve_committed_text_and_ascii_boundaries() {
        let mut transcript = Transcript::default();
        transcript.preview("build".into());
        transcript.commit();
        transcript.preview("module".into());
        assert_eq!(transcript.text(), "build module");
        transcript.preview(String::new());
        assert_eq!(transcript.text(), "build");
        transcript.commit();
        assert_eq!(transcript.text(), "build");
    }
}
