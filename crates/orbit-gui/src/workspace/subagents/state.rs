//! Transport generations own requests, while rows continue to own their drafts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Operation {
    Refresh,
    Send { id: String, message: String },
    Cancel { id: String, run: i64 },
}

pub(super) struct Requests {
    pub pending: Option<Operation>,
    pub connected: bool,
    pub needs_refresh: bool,
    serial: u64,
}

impl Requests {
    pub fn new() -> Self {
        Self {
            pending: None,
            connected: true,
            needs_refresh: true,
            serial: 0,
        }
    }

    pub fn begin(&mut self, operation: Operation) -> Option<u64> {
        if !self.connected
            || self.pending.is_some()
            || (self.needs_refresh && !matches!(operation, Operation::Refresh))
        {
            return None;
        }
        self.serial += 1;
        self.pending = Some(operation);
        Some(self.serial)
    }

    pub fn accepts(&self, serial: u64) -> bool {
        self.connected && self.serial == serial && self.pending.is_some()
    }

    pub fn finish(&mut self, serial: u64) -> bool {
        if !self.accepts(serial) {
            return false;
        }
        self.pending = None;
        true
    }

    pub fn refreshed(&mut self, serial: u64) -> bool {
        if !matches!(self.pending, Some(Operation::Refresh)) || !self.finish(serial) {
            return false;
        }
        self.needs_refresh = false;
        true
    }

    pub fn disconnected(&mut self) -> Option<Operation> {
        self.serial += 1;
        self.connected = false;
        self.needs_refresh = true;
        self.pending.take()
    }

    pub fn reconnected(&mut self) {
        self.connected = true;
        self.needs_refresh = true;
    }

    pub fn can_mutate(&self) -> bool {
        self.connected && !self.needs_refresh && self.pending.is_none()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Uncertain {
    Send,
    Cancel { run: i64 },
}

impl Uncertain {
    pub fn message(self) -> &'static str {
        match self {
            Self::Send => {
                "上次消息是否送达尚未确认，草稿已保留。请刷新并查看对话，再决定是否重发。"
            }
            Self::Cancel { .. } => {
                "上次停止请求的结果尚未确认。请刷新真实状态，再决定是否再次停止。"
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready() -> Requests {
        let mut requests = Requests::new();
        let first = requests
            .begin(Operation::Refresh)
            .unwrap();
        assert!(requests.refreshed(first));
        requests
    }

    #[test]
    fn interrupted_send_keeps_exact_draft_and_reconnect_never_replays_it() {
        let mut requests = ready();
        let send = Operation::Send {
            id: "owned-child".into(),
            message: "未提交的原始消息\n第二行".into(),
        };
        let old = requests.begin(send.clone()).unwrap();
        assert_eq!(requests.disconnected(), Some(send.clone()));
        assert!(requests.pending.is_none());
        requests.reconnected();
        assert!(
            requests.pending.is_none(),
            "reconnect must not resend a mutation"
        );
        assert!(!requests.can_mutate());
        assert!(requests.begin(send.clone()).is_none());
        let refresh = requests
            .begin(Operation::Refresh)
            .unwrap();
        assert!(
            !requests.finish(old),
            "an old receipt cannot clear the new refresh"
        );
        assert_eq!(requests.pending, Some(Operation::Refresh));
        assert!(requests.refreshed(refresh));
        assert!(requests.can_mutate());
        assert!(
            requests.begin(send).is_some(),
            "only a new explicit user action may retry"
        );
    }

    #[test]
    fn interrupted_cancel_keeps_target_run_without_claiming_a_terminal_result() {
        let mut requests = ready();
        let cancel = Operation::Cancel {
            id: "child-a".into(),
            run: 42,
        };
        let old = requests.begin(cancel.clone()).unwrap();
        assert_eq!(requests.disconnected(), Some(cancel));
        requests.reconnected();
        assert!(!requests.finish(old));
        assert!(requests.pending.is_none());
        assert!(requests.needs_refresh);
        assert!(
            Uncertain::Cancel { run: 42 }
                .message()
                .contains("尚未确认")
        );
    }

    #[test]
    fn interrupted_or_failed_refresh_can_retry_and_old_pages_cannot_mark_it_ready() {
        let mut requests = Requests::new();
        let old = requests
            .begin(Operation::Refresh)
            .unwrap();
        assert_eq!(requests.disconnected(), Some(Operation::Refresh));
        assert!(
            requests
                .begin(Operation::Refresh)
                .is_none()
        );
        requests.reconnected();
        let retry = requests
            .begin(Operation::Refresh)
            .unwrap();
        assert!(!requests.refreshed(old));
        assert_eq!(requests.pending, Some(Operation::Refresh));
        assert!(requests.finish(retry));
        assert!(requests.needs_refresh);
        let final_retry = requests
            .begin(Operation::Refresh)
            .unwrap();
        assert!(requests.refreshed(final_retry));
        assert!(requests.can_mutate());
    }

    #[test]
    fn new_requests_reject_duplicate_receipts_from_the_same_connection() {
        let mut requests = ready();
        let first = requests
            .begin(Operation::Cancel {
                id: "child".into(),
                run: 1,
            })
            .unwrap();
        assert!(requests.finish(first));
        let second = requests
            .begin(Operation::Refresh)
            .unwrap();
        assert!(!requests.finish(first));
        assert!(requests.accepts(second));
        assert!(requests.refreshed(second));
    }
}
