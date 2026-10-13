//! Keep execution visible through intermediate prose and tool calls. Only a
//! completed answer collapses it automatically; explicit inspection wins.
pub(super) struct ProcessDisclosure {
    expanded: bool,
    manual: bool,
}

impl Default for ProcessDisclosure {
    fn default() -> Self {
        Self {
            expanded: true,
            manual: false,
        }
    }
}

impl ProcessDisclosure {
    pub(super) fn expanded(&self) -> bool {
        self.expanded
    }

    pub(super) fn toggle(&mut self) {
        self.expanded = !self.expanded;
        self.manual = true;
    }

    pub(super) fn sync(&mut self, completed_answer: bool) {
        if !self.manual {
            self.expanded = !completed_answer;
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
            let turn = transcript.turns.last().unwrap();
            disclosure.sync(turn.completed && turn.conclusion().is_some());
        }
    }

    fn commentary_and_tools() -> Vec<Value> {
        let call = json!({"id":"read","function":{"name":"read_file","arguments":"{}"}});
        vec![
            json!({"Started":{"prompt":"检查项目"}}),
            json!({"Agent":{"ToolStarted":{"call":call}}}),
            json!({"Agent":{"ToolFinished":{"call":call,"content":"文件内容","changes":[]}}}),
            json!({"Agent":{"Model":{"TextDelta":"先检查实现，再验证影响。"}}}),
            json!({"Agent":{"Model":{"ReasoningDelta":"补充思考"}}}),
            json!({"Agent":{"Model":{"TextDelta":"  \n"}}}),
            json!({"Agent":{"ToolStarted":{"call":call}}}),
            json!({"Agent":{"ToolFinished":{"call":call,"content":"检查结果","changes":[]}}}),
            json!({"Agent":{"Model":{"TextDelta":"检查完成。"}}}),
        ]
    }

    #[test]
    fn commentary_never_collapses_execution_before_the_run_finishes() {
        let mut transcript = Transcript::default();
        let mut disclosure = ProcessDisclosure::default();
        for event in commentary_and_tools() {
            apply(&mut transcript, &mut disclosure, event);
            assert!(disclosure.expanded());
        }
        apply(
            &mut transcript,
            &mut disclosure,
            json!({"Finished":{"status":"completed"}}),
        );
        assert!(!disclosure.expanded());
        // Opening a completed record must survive subsequent view syncs.
        disclosure.toggle();
        disclosure.sync(true);
        assert!(disclosure.expanded());
    }

    #[test]
    fn manual_choice_survives_prose_tools_and_completion() {
        for expanded in [false, true] {
            let mut transcript = Transcript::default();
            let mut disclosure = ProcessDisclosure::default();
            disclosure.toggle();
            if expanded {
                disclosure.toggle();
            }
            for event in commentary_and_tools()
                .into_iter()
                .chain([json!({"Finished":{"status":"completed"}})])
            {
                apply(&mut transcript, &mut disclosure, event);
                assert_eq!(disclosure.expanded(), expanded);
            }
        }
    }

    #[test]
    fn failed_or_cancelled_execution_keeps_partial_answers_and_details_visible() {
        for status in ["failed", "cancelled"] {
            let mut transcript = Transcript::default();
            let mut disclosure = ProcessDisclosure::default();
            for event in commentary_and_tools()
                .into_iter()
                .chain([json!({"Finished":{"status":status}})])
            {
                apply(&mut transcript, &mut disclosure, event);
            }
            assert!(disclosure.expanded());
        }
    }

    #[test]
    fn completed_execution_without_an_answer_keeps_its_details_visible() {
        let mut transcript = Transcript::default();
        let mut disclosure = ProcessDisclosure::default();
        for event in [
            json!({"Started":{"prompt":"检查项目"}}),
            json!({"Agent":{"Model":{"ReasoningDelta":"分析结果"}}}),
            json!({"Finished":{"status":"completed"}}),
        ] {
            apply(&mut transcript, &mut disclosure, event);
        }
        assert!(disclosure.expanded());
    }
}
