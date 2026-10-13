//! Project identity is its target-host path, scoped by the existing host rules.
use super::*;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Projects {
    pinned: BTreeSet<String>,
    order: Vec<String>,
}
impl Projects {
    pub(in crate::preferences) fn is_empty(&self) -> bool {
        self.pinned.is_empty() && self.order.is_empty()
    }
    pub(crate) fn is_pinned(&self, path: &str) -> bool {
        self.pinned.contains(path)
    }
    pub(crate) fn position(&self, path: &str) -> usize {
        self.order
            .iter()
            .position(|item| item == path)
            .unwrap_or(usize::MAX)
    }
}
impl Organization {
    pub(in crate::preferences) fn projects(&self, options: &Options) -> Projects {
        self.hosts
            .iter()
            .find(|host| host.options.same_host(options))
            .map(|host| host.projects.clone())
            .unwrap_or_default()
    }
    fn project_host(&mut self, options: &Options) -> &mut Host {
        let ix = self
            .hosts
            .iter()
            .position(|host| host.options.same_host(options))
            .unwrap_or_else(|| {
                self.hosts.push(Host {
                    options: host_options(options),
                    sessions: BTreeMap::new(),
                    projects: Projects::default(),
                });
                self.hosts.len() - 1
            });
        &mut self.hosts[ix]
    }
    pub(in crate::preferences) fn set_project_pin(
        &mut self,
        options: &Options,
        path: &str,
        pinned: bool,
    ) {
        if pinned {
            self.project_host(options)
                .projects
                .pinned
                .insert(path.to_owned());
        } else if let Some(host) = self
            .hosts
            .iter_mut()
            .find(|host| host.options.same_host(options))
        {
            host.projects.pinned.remove(path);
        }
        self.hosts
            .retain(|host| !host.sessions.is_empty() || !host.projects.is_empty());
    }
    pub(in crate::preferences) fn reorder_project(
        &mut self,
        options: &Options,
        source: &str,
        target: &str,
        paths: &[String],
    ) -> anyhow::Result<()> {
        let from = paths
            .iter()
            .position(|path| path == source)
            .ok_or_else(|| anyhow::anyhow!("拖动的项目已不在列表中"))?;
        let to = paths
            .iter()
            .position(|path| path == target)
            .ok_or_else(|| anyhow::anyhow!("目标项目已不在列表中"))?;
        if from == to {
            return Ok(());
        }
        let host = self.project_host(options);
        // A drop into the other section adopts that section's pin state.
        if host.projects.is_pinned(target) {
            host.projects.pinned.insert(source.to_owned());
        } else {
            host.projects.pinned.remove(source);
        }
        let mut order = paths.to_vec();
        let source = order.remove(from);
        order.insert(to, source);
        // A temporarily absent project retains its saved place after known rows.
        order.extend(
            host.projects
                .order
                .iter()
                .filter(|path| !paths.contains(path))
                .cloned(),
        );
        host.projects.order = order;
        Ok(())
    }
    pub(in crate::preferences) fn migrate_project_pins(
        &mut self,
        options: &Options,
        sessions: &[(String, String)],
    ) -> bool {
        let Some(host) = self
            .hosts
            .iter_mut()
            .find(|host| host.options.same_host(options))
        else {
            return false;
        };
        let mut changed = false;
        for (id, path) in sessions {
            if let Some(placement) = host.sessions.get_mut(id)
                && placement.is_pinned()
            {
                placement.pinned = false;
                host.projects.pinned.insert(path.clone());
                changed = true;
            }
        }
        host.sessions
            .retain(|_, placement| *placement != Placement::default());
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    #[test]
    fn projects_keep_order_pins_and_categories_across_refresh_and_restart() {
        let options = Options::parse_from(["orbit-gui"]);
        let mut organization = Organization::default();
        organization.set_pin(&options, "a", true);
        organization
            .set_category(&options, "a", Some("学习".into()))
            .unwrap();
        let catalog = [("a".into(), "/alpha".into()), ("b".into(), "/alpha".into())];
        assert!(organization.migrate_project_pins(&options, &catalog));
        assert!(!organization.migrate_project_pins(&options, &catalog));
        assert!(organization.projects(&options).is_pinned("/alpha"));
        assert!(!organization.placement(&options, "a").is_pinned());
        assert_eq!(
            organization.placement(&options, "a").category(),
            Some("学习")
        );
        let paths = ["/alpha", "/beta", "/gamma"].map(str::to_owned);
        organization
            .reorder_project(&options, "/gamma", "/beta", &paths)
            .unwrap();
        let mut restored: Organization =
            serde_json::from_slice(&serde_json::to_vec(&organization).unwrap()).unwrap();
        let projects = restored.projects(&options);
        assert_eq!(projects.order, ["/alpha", "/gamma", "/beta"]);
        assert!(projects.is_pinned("/alpha"));
        restored.set_project_pin(&options, "/alpha", false);
        assert!(!restored.projects(&options).is_pinned("/alpha"));
        restored.forget(&options, "a");
        assert_eq!(restored.projects(&options).order, projects.order);
        assert!(
            restored
                .reorder_project(&options, "/missing", "/beta", &paths)
                .is_err()
        );
    }
    #[test]
    fn project_paths_are_isolated_by_target_host_and_cross_section_drops_are_explicit() {
        let local = Options::parse_from(["orbit-gui"]);
        let mut ssh = local.clone();
        ssh.ssh = Some("dev".into());
        let mut organization = Organization::default();
        organization.set_project_pin(&ssh, "/same", true);
        assert!(!organization.projects(&local).is_pinned("/same"));
        let paths = ["/same", "/other"].map(str::to_owned);
        organization
            .reorder_project(&ssh, "/other", "/same", &paths)
            .unwrap();
        assert!(organization.projects(&ssh).is_pinned("/other"));
        organization.set_project_pin(&ssh, "/same", false);
        organization
            .reorder_project(&ssh, "/other", "/same", &paths)
            .unwrap();
        assert!(!organization.projects(&ssh).is_pinned("/other"));
    }
}
