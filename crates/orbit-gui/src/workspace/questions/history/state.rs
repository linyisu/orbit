//! Pagination state is separate from the live answer form and its input entities.
use orbit_protocol::user_input::{History, Record};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PageRequest {
    pub before: Option<i64>,
    serial: u64,
}

#[derive(Default)]
pub(super) struct HistoryState {
    pub records: Vec<Record>,
    pub selected: Option<String>,
    pub next: Option<i64>,
    pub loaded: bool,
    pub pending: Option<PageRequest>,
    pub error: Option<String>,
    serial: u64,
}

impl HistoryState {
    pub fn begin(&mut self) -> Option<PageRequest> {
        if self.pending.is_some() || (self.loaded && self.next.is_none()) {
            return None;
        }
        self.serial += 1;
        let request = PageRequest {
            before: self.next,
            serial: self.serial,
        };
        self.pending = Some(request);
        self.error = None;
        Some(request)
    }

    pub fn receive(&mut self, request: PageRequest, result: Result<History, String>) -> bool {
        if self.pending != Some(request) {
            return false;
        }
        self.pending = None;
        let page = match result {
            Ok(page) => page,
            Err(error) => {
                self.error = Some(error);
                return false;
            }
        };
        // The cursor is a database row ID, independent of the request IDs in
        // records. Never derive it from the records or accept another page.
        if page.before != request.before
            || page.next.is_some_and(|next| {
                page.records.is_empty() || request.before.is_some_and(|before| next >= before)
            })
        {
            self.error = Some("历史分页响应不匹配，请重试。".into());
            return false;
        }
        for record in page.records {
            if let Some(existing) = self.records.iter_mut().find(|item| item.id == record.id) {
                *existing = record;
            } else {
                self.records.push(record);
            }
        }
        if self.selected.is_none() {
            self.selected = self.records.first().map(|record| record.id.clone());
        }
        self.next = page.next;
        self.loaded = true;
        self.error = None;
        true
    }

    pub fn invalidate(&mut self) {
        self.pending = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orbit_protocol::user_input::{Answer, Form, Status};

    fn record(id: &str) -> Record {
        Record {
            id: id.into(),
            call_id: format!("call-{id}"),
            source: None,
            form: Form { questions: vec![] },
            status: Status::Answered,
            answers: vec![Answer {
                question_id: "choice".into(),
                selected: vec!["retained-choice".into()],
                text: "原始答案".into(),
                ..Default::default()
            }],
        }
    }

    fn page(before: Option<i64>, ids: &[&str], next: Option<i64>) -> Result<History, String> {
        Ok(History {
            before,
            records: ids.iter().map(|id| record(id)).collect(),
            next,
        })
    }

    #[test]
    fn older_pages_keep_selection_answers_and_use_the_server_cursor() {
        let mut state = HistoryState::default();
        let first = state.begin().unwrap();
        assert!(state.begin().is_none());
        assert!(state.receive(first, page(None, &["request-z", "request-a"], Some(912))));
        state.selected = Some("request-a".into());
        let selected = state.records[1].clone();
        let older = state.begin().unwrap();
        assert_eq!(older.before, Some(912));
        assert!(state.receive(older, page(Some(912), &["request-q"], None)));
        assert_eq!(state.selected.as_deref(), Some("request-a"));
        assert_eq!(state.records[1], selected);
        assert_eq!(state.records.len(), 3);
        assert!(state.loaded && state.next.is_none());
        assert!(state.begin().is_none());
    }

    #[test]
    fn failed_page_retries_the_same_cursor_and_ignores_old_completion() {
        let mut state = HistoryState::default();
        let first = state.begin().unwrap();
        state.receive(first, page(None, &["current"], Some(120)));
        let failed = state.begin().unwrap();
        assert!(!state.receive(failed, Err("暂时不可用".into())));
        assert_eq!(state.records.len(), 1);
        assert_eq!(state.next, Some(120));
        let retry = state.begin().unwrap();
        assert_eq!(retry.before, failed.before);
        assert_ne!(retry, failed);
        assert!(!state.receive(failed, page(Some(120), &["stale"], None)));
        assert_eq!(state.pending, Some(retry));
        assert!(state.receive(retry, page(Some(120), &["older"], None)));
        assert_eq!(state.records[1].id, "older");
    }

    #[test]
    fn mismatched_or_nonadvancing_cursors_cannot_replace_visible_history() {
        let mut state = HistoryState::default();
        let first = state.begin().unwrap();
        state.receive(first, page(None, &["current"], Some(120)));
        for (before, next) in [(Some(121), Some(99)), (Some(120), Some(120))] {
            let request = state.begin().unwrap();
            assert!(!state.receive(request, page(before, &["wrong"], next)));
            assert_eq!(state.records.len(), 1);
            assert_eq!(state.next, Some(120));
            assert!(state.error.is_some());
        }
    }

    #[test]
    fn invalidated_generation_ignores_pending_results() {
        let mut state = HistoryState::default();
        let old = state.begin().unwrap();
        state.invalidate();
        let current = state.begin().unwrap();
        assert!(!state.receive(old, page(None, &["old-session"], None)));
        assert!(state.records.is_empty());
        assert_eq!(state.pending, Some(current));
        assert!(state.receive(current, page(None, &[], None)));
        assert!(state.loaded);
        assert!(state.begin().is_none());
    }

    #[test]
    fn overlapping_records_keep_stable_selection_without_duplicate_rows() {
        let mut state = HistoryState::default();
        let first = state.begin().unwrap();
        state.receive(first, page(None, &["current", "shared"], Some(900)));
        state.selected = Some("shared".into());
        let older = state.begin().unwrap();
        state.receive(older, page(Some(900), &["shared", "older"], None));
        assert_eq!(state.records.len(), 3);
        assert_eq!(state.selected.as_deref(), Some("shared"));
        assert_eq!(state.records[1].id, "shared");
    }
}
