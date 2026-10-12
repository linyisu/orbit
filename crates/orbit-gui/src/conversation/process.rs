//! Execution stays visible until an answer begins. Manual inspection persists
//! across updates within that phase; a new execution phase opens it again.
pub(super) struct ProcessDisclosure {
    expanded: bool,
    answering: bool,
}

impl Default for ProcessDisclosure {
    fn default() -> Self {
        Self {
            expanded: true,
            answering: false,
        }
    }
}

impl ProcessDisclosure {
    pub(super) fn expanded(&self) -> bool {
        self.expanded
    }

    pub(super) fn toggle(&mut self) {
        self.expanded = !self.expanded;
    }

    pub(super) fn sync(&mut self, answering: bool) {
        if self.answering != answering {
            self.expanded = !answering;
            self.answering = answering;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::model::Transcript;
    use serde_json::{Value, json};

    fn apply(transcript: &mut Transcript, disclosure: &mut ProcessDisclosure, event: Value) {
        if transcript
            .apply(serde_json::from_value(event).unwrap())
            .is_some()
        {
            disclosure.sync(transcript.turns.last().unwrap().conclusion().is_some());
        }
    }

    #[test]
    fn execution_is_visible_until_answer_and_manual_inspection_survives_streaming() {
        let mut transcript = Transcript::default();
        let mut disclosure = ProcessDisclosure::default();
        let call = json!({"id":"read","function":{"name":"read_file","arguments":"{}"}});
        for event in [
            json!({"Started":{"prompt":"检查项目"}}),
            json!({"Agent":{"ToolStarted":{"call":call}}}),
            json!({"Agent":{"ToolFinished":{"call":call,"content":"文件内容","changes":[]}}}),
            json!({"Agent":{"Model":{"ReasoningDelta":"分析结果"}}}),
            json!({"Agent":{"Model":{"TextDelta":"  \n"}}}),
        ] {
            apply(&mut transcript, &mut disclosure, event);
            assert!(disclosure.expanded());
        }
        apply(
            &mut transcript,
            &mut disclosure,
            json!({"Agent":{"Model":{"TextDelta":"检查完成。"}}}),
        );
        assert!(!disclosure.expanded());
        apply(
            &mut transcript,
            &mut disclosure,
            json!({"Agent":{"Model":{"ReasoningDelta":"补充思考"}}}),
        );
        assert!(!disclosure.expanded());
        disclosure.toggle();
        for event in [
            json!({"Agent":{"Model":{"TextDelta":"这是结果。"}}}),
            json!({"Finished":{"status":"completed"}}),
        ] {
            apply(&mut transcript, &mut disclosure, event);
            assert!(disclosure.expanded());
        }
    }

    #[test]
    fn new_execution_phase_reopens_and_empty_deltas_do_not_override_manual_choice() {
        let mut transcript = Transcript::default();
        let mut disclosure = ProcessDisclosure::default();
        apply(
            &mut transcript,
            &mut disclosure,
            json!({"Agent":{"Model":{"TextDelta":"先检查源码。"}}}),
        );
        assert!(!disclosure.expanded());
        let call = json!({"id":"read","function":{"name":"read_file","arguments":"{}"}});
        apply(
            &mut transcript,
            &mut disclosure,
            json!({"Agent":{"ToolStarted":{"call":call}}}),
        );
        assert!(disclosure.expanded());
        disclosure.toggle();
        for event in [
            json!({"Agent":{"Model":{"ReasoningDelta":"检查中"}}}),
            json!({"Agent":{"Model":{"TextDelta":""}}}),
            json!({"Agent":{"ToolFinished":{"call":call,"content":"文件内容","changes":[]}}}),
        ] {
            apply(&mut transcript, &mut disclosure, event);
            assert!(!disclosure.expanded());
        }
        disclosure.toggle();
        apply(
            &mut transcript,
            &mut disclosure,
            json!({"Agent":{"Model":{"TextDelta":"检查完成。"}}}),
        );
        assert!(!disclosure.expanded());
    }

    #[test]
    fn failed_execution_without_an_answer_keeps_its_details_available() {
        let mut transcript = Transcript::default();
        let mut disclosure = ProcessDisclosure::default();
        for event in [
            json!({"Started":{"prompt":"检查项目"}}),
            json!({"Report":{"title":"连接失败","text":"连接中断，请重试。"}}),
            json!({"Finished":{"status":"failed"}}),
        ] {
            apply(&mut transcript, &mut disclosure, event);
        }
        assert!(disclosure.expanded());
    }
}
