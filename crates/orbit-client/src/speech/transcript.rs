//! Ordered voice spans; a late sentence correction cannot overwrite newer live text.
use anyhow::{Result, ensure};

enum Sentence {
    Preview(String),
    Corrected(String),
}

#[derive(Default)]
pub(super) struct Transcript {
    sentences: Vec<Sentence>,
    live: String,
}

impl Transcript {
    pub fn preview(&mut self, text: String) {
        self.live = text;
    }

    pub fn finish_sentence(&mut self) -> Option<usize> {
        if self.live.trim().is_empty() {
            self.live.clear();
            return None;
        }
        let index = self.sentences.len();
        self.sentences
            .push(Sentence::Preview(std::mem::take(&mut self.live)));
        Some(index)
    }

    pub fn correct(&mut self, index: usize, text: String) -> Result<()> {
        ensure!(
            !text.trim().is_empty(),
            "句尾校正未返回文字，已保留实时预览"
        );
        let Some(sentence @ Sentence::Preview(_)) = self.sentences.get_mut(index) else {
            anyhow::bail!("句尾校正对应的语音片段已失效");
        };
        *sentence = Sentence::Corrected(text);
        Ok(())
    }

    pub fn pending(&self) -> bool {
        self.sentences
            .iter()
            .any(|sentence| matches!(sentence, Sentence::Preview(_)))
    }

    pub fn text(&self) -> String {
        let mut text = String::new();
        for sentence in &self.sentences {
            let (Sentence::Preview(part) | Sentence::Corrected(part)) = sentence;
            append(&mut text, part);
        }
        append(&mut text, &self.live);
        text
    }
}

fn append(text: &mut String, part: &str) {
    let part = part.trim();
    if text.ends_with(|c: char| c.is_ascii_alphanumeric())
        && part.starts_with(|c: char| c.is_ascii_alphanumeric())
    {
        text.push(' ');
    }
    text.push_str(part);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delayed_corrections_preserve_newer_preview_and_other_sentences() {
        let mut transcript = Transcript::default();
        transcript.preview("检查阿尔法".into());
        let first = transcript.finish_sentence().unwrap();
        transcript.preview("再检查贝塔".into());
        let second = transcript.finish_sentence().unwrap();
        transcript.preview("最后运行测试".into());
        transcript
            .correct(second, "再检查 Beta。".into())
            .unwrap();
        transcript
            .correct(first, "检查 Alpha。".into())
            .unwrap();
        assert_eq!(transcript.text(), "检查 Alpha。再检查 Beta。最后运行测试");
        assert!(!transcript.pending());
        assert!(
            transcript
                .correct(first, "过期结果".into())
                .is_err()
        );
        assert_eq!(transcript.text(), "检查 Alpha。再检查 Beta。最后运行测试");
    }

    #[test]
    fn empty_corrections_do_not_erase_preview_and_english_boundaries_survive() {
        let mut transcript = Transcript::default();
        transcript.preview("build".into());
        let first = transcript.finish_sentence().unwrap();
        transcript.preview("module".into());
        assert!(
            transcript
                .correct(first, "  ".into())
                .is_err()
        );
        assert!(transcript.pending());
        assert_eq!(transcript.text(), "build module");
        transcript
            .correct(first, "Build".into())
            .unwrap();
        assert_eq!(transcript.text(), "Build module");
        transcript.preview(" ".into());
        assert!(transcript.finish_sentence().is_none());
        assert_eq!(transcript.text(), "Build");
    }
}
