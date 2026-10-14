//! Present each pending decision once; navigation and other overlays defer it.
use super::*;
use std::collections::HashSet;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum Decision {
    Approval(u64),
    Plan {
        generation: u64,
        id: String,
        revision: u64,
    },
}

#[derive(Default)]
pub(super) struct Attention {
    shown: HashSet<Decision>,
    active: Option<Decision>,
}
impl Attention {
    fn sync(&mut self, pending: &[Decision]) -> bool {
        self.shown.retain(|key| pending.contains(key));
        if self
            .active
            .as_ref()
            .is_some_and(|key| !pending.contains(key))
        {
            self.active = None;
            true
        } else {
            false
        }
    }
    fn next(&self, pending: &[Decision], available: bool) -> Option<Decision> {
        available
            .then(|| {
                pending
                    .iter()
                    .find(|key| !self.shown.contains(key))
                    .cloned()
            })
            .flatten()
    }
    pub(super) fn present(&mut self, key: Decision) {
        self.shown.insert(key.clone());
        self.active = Some(key);
    }
    pub(super) fn dismiss(&mut self, key: &Decision) {
        if self.active.as_ref() == Some(key) {
            self.active = None;
        }
    }
    pub(super) fn clear(&mut self) -> bool {
        self.shown.clear();
        self.active.take().is_some()
    }
    pub(super) fn suspend(&mut self) -> bool {
        if let Some(key) = self.active.take() {
            self.shown.remove(&key);
            true
        } else {
            false
        }
    }
}

impl Workspace {
    pub(super) fn plan_decision(&self) -> Option<Decision> {
        self.planning
            .plan
            .as_ref()
            .filter(|plan| {
                self.planning.mode == orbit_protocol::plan::WorkMode::Plan
                    && plan.status == orbit_protocol::plan::Status::Review
            })
            .map(|plan| Decision::Plan {
                generation: self.planning.generation,
                id: plan.id.clone(),
                revision: plan.revision,
            })
    }

    pub(super) fn show_pending_decision(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.visible || !self.connected || self.opening || self.info.is_none() {
            return;
        }
        let mut pending: Vec<_> = self
            .approvals
            .iter()
            .map(|approval| Decision::Approval(approval.id))
            .collect();
        pending.extend(self.plan_decision());
        if self.attention.sync(&pending) {
            // Only an owned decision can reach this path. Its close callback
            // and actions clear ownership before presenting another surface.
            window.close_all_dialogs(cx);
        }
        let available = !window.has_active_dialog(cx)
            && !window.has_active_sheet(cx)
            && self.connection_challenges.is_empty();
        match self.attention.next(&pending, available) {
            Some(Decision::Approval(id)) => {
                if let Some(approval) = self
                    .approvals
                    .iter()
                    .find(|approval| approval.id == id)
                    .cloned()
                {
                    self.approval(&approval, window, cx);
                }
            }
            Some(Decision::Plan { .. }) => self.show_plan(window, cx),
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Attention, Decision};

    #[test]
    fn decisions_wait_for_visibility_and_overlays_and_dismissal_survives_polling() {
        let mut attention = Attention::default();
        let pending = [Decision::Approval(7), Decision::Approval(8)];
        assert_eq!(attention.next(&pending, false), None);
        assert_eq!(attention.next(&pending, true), Some(pending[0].clone()));
        attention.present(pending[0].clone());
        attention.dismiss(&pending[0]);
        assert!(!attention.sync(&pending));
        assert_eq!(attention.next(&pending, true), Some(pending[1].clone()));
        attention.present(pending[1].clone());
        attention.suspend();
        assert_eq!(attention.next(&pending, true), Some(pending[1].clone()));
    }

    #[test]
    fn revised_plans_and_resolved_requests_replace_the_old_decision() {
        let mut attention = Attention::default();
        let plan = |revision| Decision::Plan {
            generation: 7,
            id: "plan".into(),
            revision,
        };
        attention.present(plan(1));
        assert_eq!(attention.next(&[plan(1)], true), None);
        assert!(attention.sync(&[plan(2)]));
        assert_eq!(attention.next(&[plan(2)], true), Some(plan(2)));
        attention.present(plan(2));
        assert!(attention.sync(&[]));
        assert_eq!(attention.next(&[], true), None);
        attention.present(Decision::Approval(7));
        assert!(attention.clear());
        assert_eq!(
            attention.next(&[Decision::Approval(7)], true),
            Some(Decision::Approval(7))
        );
    }
}
