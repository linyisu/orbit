//! Personal project and session organization is durable desktop state. Session IDs are
//! meaningful only within their service/data source, including remote stores.
use crate::backend::Options;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
mod projects;
pub(crate) use projects::Projects;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Placement {
    pinned: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    category: Option<String>,
}

impl Placement {
    pub(crate) fn is_pinned(&self) -> bool {
        self.pinned
    }

    pub(crate) fn category(&self) -> Option<&str> {
        self.category.as_deref()
    }
}

#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct Organization {
    hosts: Vec<Host>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Host {
    options: Options,
    sessions: BTreeMap<String, Placement>,
    #[serde(default, skip_serializing_if = "Projects::is_empty")]
    projects: Projects,
}

impl Organization {
    pub(super) fn is_empty(&self) -> bool {
        self.hosts.is_empty()
    }

    pub(super) fn placement(&self, options: &Options, id: &str) -> Placement {
        self.hosts
            .iter()
            .find(|host| host.options.same_host(options))
            .and_then(|host| host.sessions.get(id))
            .cloned()
            .unwrap_or_default()
    }

    pub(super) fn categories(&self, options: &Options) -> Vec<String> {
        self.hosts
            .iter()
            .filter(|host| host.options.same_host(options))
            .flat_map(|host| host.sessions.values())
            .filter_map(|placement| placement.category.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    #[cfg(test)]
    pub(super) fn set_pin(&mut self, options: &Options, id: &str, pinned: bool) {
        let mut placement = self.placement(options, id);
        placement.pinned = pinned;
        self.store(options, id, placement);
    }

    pub(super) fn set_category(
        &mut self,
        options: &Options,
        id: &str,
        category: Option<String>,
    ) -> anyhow::Result<()> {
        let category = category
            .map(|category| -> anyhow::Result<String> {
                let category = category.trim();
                anyhow::ensure!(
                    (1..=32).contains(&category.chars().count()),
                    "分类名称需为 1–32 个字符"
                );
                anyhow::ensure!(
                    !category.chars().any(char::is_control),
                    "分类名称不能包含换行或控制字符"
                );
                Ok(category.to_owned())
            })
            .transpose()?;
        let mut placement = self.placement(options, id);
        placement.category = category;
        self.store(options, id, placement);
        Ok(())
    }

    pub(super) fn forget(&mut self, options: &Options, id: &str) {
        for host in &mut self.hosts {
            if host.options.same_host(options) {
                host.sessions.remove(id);
            }
        }
        self.hosts
            .retain(|host| !host.sessions.is_empty() || !host.projects.is_empty());
    }

    fn store(&mut self, options: &Options, id: &str, placement: Placement) {
        if placement == Placement::default() {
            self.forget(options, id);
            return;
        }
        if let Some(host) = self
            .hosts
            .iter_mut()
            .find(|host| host.options.same_host(options))
        {
            host.sessions.insert(id.to_owned(), placement);
        } else {
            self.hosts.push(Host {
                options: host_options(options),
                sessions: BTreeMap::from([(id.to_owned(), placement)]),
                projects: Projects::default(),
            });
        }
    }
}

fn host_options(options: &Options) -> Options {
    let mut host = options.clone();
    host.resume = None;
    host.recover_draft = false;
    host.relink_on_resume = false;
    host.model = None;
    host.appearance = None;
    host.preferences = None;
    let workspace = host.workspace.take();
    // Most databases belong to the host. A relative remote database belongs to
    // its workspace, so use the transport's identity rule instead of a second
    // independently maintained definition of that exception.
    if !options.same_host(&host) {
        host.workspace = workspace;
    }
    host
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn local() -> Options {
        Options::parse_from(["orbit-gui", "--config", "/app/config.toml"])
    }

    #[test]
    fn pinning_preserves_category_and_unpinning_restores_its_placement() {
        let options = local();
        let mut organization = Organization::default();
        organization
            .set_category(&options, "one", Some("学习".into()))
            .unwrap();
        organization.set_pin(&options, "one", true);
        let placement = organization.placement(&options, "one");
        assert!(placement.is_pinned());
        assert_eq!(placement.category(), Some("学习"));
        organization.set_pin(&options, "one", false);
        let placement = organization.placement(&options, "one");
        assert!(!placement.is_pinned());
        assert_eq!(placement.category(), Some("学习"));
        organization.set_category(&options, "one", None).unwrap();
        assert!(organization.is_empty());
    }

    #[test]
    fn categories_are_trimmed_deduplicated_and_limited_by_unicode_characters() {
        let options = local();
        let mut organization = Organization::default();
        organization
            .set_category(&options, "one", Some("  学习  ".into()))
            .unwrap();
        organization
            .set_category(&options, "two", Some("学习".into()))
            .unwrap();
        assert_eq!(organization.categories(&options), ["学习"]);
        let maximum = "语".repeat(32);
        organization
            .set_category(&options, "three", Some(maximum.clone()))
            .unwrap();
        assert_eq!(
            organization.placement(&options, "three").category(),
            Some(maximum.as_str())
        );
        for category in [
            " ".into(),
            "语".repeat(33),
            "学习\n任务".into(),
            "x\0y".into(),
        ] {
            assert!(
                organization
                    .set_category(&options, "one", Some(category))
                    .is_err()
            );
            assert_eq!(
                organization.placement(&options, "one").category(),
                Some("学习")
            );
        }
    }

    #[test]
    fn session_organization_roundtrips_without_transient_tab_options() {
        let mut options = local();
        options.workspace = Some("/project/one".into());
        options.resume = Some("resumed-tab".into());
        options.recover_draft = true;
        options.model = Some("provider::model".into());
        options.appearance = Some("/app/appearance".into());
        options.preferences = Some("/app/gui.json".into());
        let mut organization = Organization::default();
        organization.set_pin(&options, "one", true);
        organization
            .set_category(&options, "one", Some("学习".into()))
            .unwrap();
        let bytes = serde_json::to_vec(&organization).unwrap();
        let restored: Organization = serde_json::from_slice(&bytes).unwrap();
        assert!(restored == organization);
        let stored = &restored.hosts[0].options;
        assert!(stored.resume.is_none());
        assert!(!stored.recover_draft);
        assert!(stored.model.is_none());
        assert!(stored.appearance.is_none());
        assert!(stored.preferences.is_none());
        assert!(stored.workspace.is_none());
        options.workspace = Some("/project/two".into());
        options.resume = Some("other-tab".into());
        options.model = None;
        assert!(restored.placement(&options, "one").is_pinned());
        assert_eq!(restored.hosts.len(), 1);
    }

    #[test]
    fn identical_session_ids_are_isolated_by_host_and_database() {
        let local = local();
        let mut ssh = local.clone();
        ssh.ssh = Some("dev".into());
        let mut ssh_other = ssh.clone();
        ssh_other.ssh = Some("other".into());
        let mut database = local.clone();
        database.database = Some("/other/orbit.db".into());
        let mut user = ssh.clone();
        user.ssh_user = Some("another-user".into());
        let mut config = local.clone();
        config.config = "/other/config.toml".into();
        let hosts = [local, ssh, ssh_other, database, user, config];
        let mut organization = Organization::default();
        for (ix, options) in hosts.iter().enumerate() {
            organization
                .set_category(options, "same-id", Some(format!("host-{ix}")))
                .unwrap();
        }
        for (ix, options) in hosts.iter().enumerate() {
            assert_eq!(organization.categories(options), [format!("host-{ix}")]);
            assert_eq!(
                organization.placement(options, "same-id").category(),
                Some(format!("host-{ix}").as_str())
            );
        }
        assert_eq!(organization.hosts.len(), hosts.len());
    }

    #[test]
    fn relative_remote_databases_keep_their_workspace_namespace() {
        for args in [["--ssh", "dev"], ["--wsl", "Ubuntu"]] {
            let mut first = Options::parse_from(["orbit-gui", args[0], args[1]]);
            first.workspace = Some("/work/one".into());
            first.database = Some("state/orbit.db".into());
            let mut second = first.clone();
            second.workspace = Some("/work/two".into());
            let mut organization = Organization::default();
            organization.set_pin(&first, "one", true);
            assert!(organization.placement(&first, "one").is_pinned());
            assert!(!organization.placement(&second, "one").is_pinned());
            assert_eq!(organization.hosts[0].options.workspace, first.workspace);
            first.database = Some("~/.local/share/orbit.db".into());
            second.database = first.database.clone();
            organization.set_pin(&first, "home-store", true);
            assert!(organization.placement(&second, "home-store").is_pinned());
            assert!(organization.hosts[1].options.workspace.is_none());
        }
    }

    #[test]
    fn forgetting_removes_only_that_session_and_cleans_empty_hosts_and_categories() {
        let local = local();
        let mut ssh = local.clone();
        ssh.ssh = Some("dev".into());
        let mut organization = Organization::default();
        organization
            .set_category(&local, "one", Some("学习".into()))
            .unwrap();
        organization
            .set_category(&local, "two", Some("学习".into()))
            .unwrap();
        organization.set_pin(&ssh, "one", true);
        organization.forget(&local, "one");
        assert_eq!(organization.placement(&local, "one"), Placement::default());
        assert_eq!(organization.categories(&local), ["学习"]);
        assert!(organization.placement(&ssh, "one").is_pinned());
        organization.forget(&local, "two");
        assert!(organization.categories(&local).is_empty());
        assert_eq!(organization.hosts.len(), 1);
        organization.forget(&ssh, "one");
        assert!(organization.is_empty());
        organization.set_pin(&local, "unknown", false);
        organization.set_category(&local, "unknown", None).unwrap();
        assert!(organization.is_empty());
    }
}
