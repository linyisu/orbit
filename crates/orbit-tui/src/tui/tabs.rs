pub(super) mod live {
    use super::super::app::SessionTab;
    use std::collections::HashMap;
    #[derive(Default)]
    pub(crate) struct Tabs {
        pub remote: HashMap<String, super::super::workspace_client::Tab>,
        pub active_remote: Option<String>,
        order: Vec<String>,
        discovery_revision: u64,
    }

    impl Tabs {
        pub fn add_remote(&mut self, tab: super::super::workspace_client::Tab) -> String {
            let id = format!("workspace:{}", crate::workspace::request_id());
            self.order.push(id.clone());
            self.remote.insert(id.clone(), tab);
            self.active_remote = Some(id.clone());
            id
        }

        pub fn restore_tabs(
            &mut self,
            source: &str,
            saved: &[&crate::workspace::windows::SavedTab],
        ) {
            let media = self.remote[source].state.media.clone();
            let mut replace = self.remote[source].can_replace_with_resumed();
            for saved in saved {
                let existing = self
                    .remote
                    .iter()
                    .find_map(|(id, tab)| {
                        (tab.id.as_ref() == Some(&saved.id)
                            && tab.target_identity().as_ref() == Some(&saved.target))
                        .then(|| id.clone())
                    });
                let restored = if let Some(id) = existing {
                    if replace && id != source {
                        self.remove_remote(source);
                    }
                    id
                } else {
                    let next = super::super::workspace_client::Tab::connecting(
                        super::super::connection_spec::Spec::restored(saved),
                        media.clone(),
                    );
                    if replace {
                        self.remote.insert(source.into(), next);
                        source.into()
                    } else {
                        self.add_remote(next)
                    }
                };
                self.active_remote = Some(restored);
                replace = false;
            }
        }

        pub fn open_session(&mut self, source: &str, resume: Option<String>) {
            let current = &self.remote[source];
            let replace = resume.is_some() && current.can_replace_with_resumed();
            let existing = resume.as_ref().and_then(|session| {
                self.remote
                    .iter()
                    .find(|(_, tab)| {
                        tab.id.as_ref() == Some(session)
                            && tab
                                .connection
                                .as_ref()
                                .map(|c| &c.target)
                                == current
                                    .connection
                                    .as_ref()
                                    .map(|c| &c.target)
                    })
                    .map(|(id, _)| id.clone())
            });
            if let Some(existing) = existing {
                if replace && existing != source {
                    self.remove_remote(source);
                }
                self.active_remote = Some(existing);
            } else {
                let next = current.sibling(resume);
                if replace {
                    self.remote.insert(source.into(), next);
                    self.active_remote = Some(source.into());
                } else {
                    self.add_remote(next);
                }
            }
        }

        pub fn remove_remote(&mut self, id: &str) -> bool {
            if self.remote.remove(id).is_none() {
                return false;
            }
            self.order.retain(|item| item != id);
            if self.active_remote.as_deref() == Some(id) {
                self.active_remote = None;
            }
            true
        }

        pub async fn poll_remote(&mut self) -> bool {
            let revision = crate::workspace::discovery::revision();
            let mut changed = self.discovery_revision != revision;
            self.discovery_revision = revision;
            for tab in self.remote.values_mut() {
                changed |= tab.poll().await;
            }
            changed
        }

        fn remote_descriptor(
            &self,
            id: &str,
            tab: &super::super::workspace_client::Tab,
            show_target: bool,
        ) -> SessionTab {
            SessionTab {
                id: id.into(),
                title: tab.display_title(show_target),
                status: tab.state.status.clone(),
                elapsed: tab.state.active_elapsed(),
                waiting_approval: tab.state.approval.is_some(),
                active: self.active_remote.as_deref() == Some(id),
            }
        }

        pub fn sync_remote_ui(&mut self) {
            let targets: std::collections::HashSet<_> = self
                .remote
                .values()
                .filter_map(|tab| tab.target_identity())
                .collect();
            let show_target = targets.len() > 1;
            let entries: Vec<_> = self
                .order
                .iter()
                .filter_map(|id| {
                    self.remote
                        .get(id)
                        .map(|tab| self.remote_descriptor(id, tab, show_target))
                })
                .collect();
            for tab in self.remote.values_mut() {
                tab.state.tabs = entries.clone();
            }
        }

        pub fn target(&self, index: usize) -> Option<&str> {
            self.order
                .get(index)
                .map(String::as_str)
        }

        pub fn close_target(&self, id: &str) -> Option<&str> {
            let index = self
                .order
                .iter()
                .position(|item| item == id)?;
            // Closing the leftmost tab selects the first remaining tab, rather
            // than wrapping to the other end as keyboard navigation does.
            self.target(if index == 0 { 1 } else { index - 1 })
        }
    }
}
