//! Follow active tool batches unless the user explicitly chooses a disclosure state.
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

    pub(super) fn sync(&mut self, batch_finished: bool) {
        if !self.manual {
            self.expanded = !batch_finished;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_disclosure_follows_each_batch_until_it_finishes() {
        let mut disclosure = ProcessDisclosure::default();
        assert!(disclosure.expanded());
        for finished in [false, true, false, true] {
            disclosure.sync(finished);
            assert_eq!(disclosure.expanded(), !finished);
        }
    }

    #[test]
    fn manual_choice_survives_batch_updates_and_completion() {
        for expanded in [false, true] {
            let mut disclosure = ProcessDisclosure::default();
            disclosure.sync(expanded);
            disclosure.toggle();
            assert_eq!(disclosure.expanded(), expanded);
            for finished in [false, true, false, true] {
                disclosure.sync(finished);
                assert_eq!(disclosure.expanded(), expanded);
            }
        }
    }
}
