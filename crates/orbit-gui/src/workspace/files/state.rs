//! A local edit can match its old baseline while the disk outcome is unknown.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) enum DraftState {
    #[default]
    Clean,
    Edited,
    Unconfirmed {
        edited: bool,
    },
}

impl DraftState {
    pub(super) fn has_changes(&self) -> bool {
        !matches!(self, Self::Clean)
    }

    pub(super) fn is_edited(&self) -> bool {
        matches!(self, Self::Edited | Self::Unconfirmed { edited: true })
    }

    pub(super) fn is_unconfirmed(&self) -> bool {
        matches!(self, Self::Unconfirmed { .. })
    }

    pub(super) fn edit(&mut self, original: Option<&str>, edited: &str) {
        let changed = original.is_some_and(|original| original != edited);
        *self = if self.is_unconfirmed() {
            Self::Unconfirmed { edited: changed }
        } else if changed {
            Self::Edited
        } else {
            Self::Clean
        };
    }

    pub(super) fn unconfirm(&mut self) {
        *self = Self::Unconfirmed {
            edited: self.is_edited(),
        };
    }

    pub(super) fn confirm(&mut self, disk: &str, edited: &str) {
        *self = if disk == edited {
            Self::Clean
        } else {
            Self::Edited
        };
    }

    pub(super) fn discard(&mut self) {
        *self = Self::Clean;
    }
}

#[cfg(test)]
mod tests {
    use super::DraftState;

    #[test]
    fn lost_save_receipt_keeps_a_local_revert_until_disk_is_confirmed() {
        let mut draft = DraftState::default();
        draft.edit(Some("A"), "B");
        assert!(draft.is_edited());
        // The write may have committed B even though the client lost its reply.
        draft.unconfirm();
        draft.edit(Some("A"), "A");
        assert!(!draft.is_edited());
        assert!(
            draft.has_changes(),
            "opening another file must still ask before replacing A"
        );
        assert!(draft.is_unconfirmed());
        // A comparison establishes B as the real baseline. A is now a real edit.
        draft.confirm("B", "A");
        assert!(draft.has_changes());
        assert!(draft.is_edited());
        assert!(!draft.is_unconfirmed());
        // An acknowledged write of A reconciles the retained local revert.
        draft.confirm("A", "A");
        assert!(!draft.has_changes());
    }

    #[test]
    fn explicit_discard_and_matching_disk_comparison_resolve_unknown_results() {
        let mut draft = DraftState::default();
        draft.edit(Some("before"), "after");
        draft.unconfirm();
        draft.confirm("after", "after");
        assert!(!draft.has_changes());
        draft.edit(Some("after"), "another edit");
        draft.unconfirm();
        draft.edit(Some("after"), "after");
        assert!(draft.has_changes());
        draft.discard();
        assert!(!draft.has_changes());
        assert!(!draft.is_unconfirmed());
    }
}
