//! Durable project ordering shared by desktop and mobile views.
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Projects {
    pinned: BTreeSet<String>,
    order: Vec<String>,
}

impl Projects {
    pub fn is_empty(&self) -> bool {
        self.pinned.is_empty() && self.order.is_empty()
    }

    pub fn is_pinned(&self, path: &str) -> bool {
        self.pinned.contains(path)
    }

    pub fn position(&self, path: &str) -> usize {
        self.order
            .iter()
            .position(|item| item == path)
            .unwrap_or(usize::MAX)
    }

    pub fn order(&self) -> &[String] {
        &self.order
    }

    pub fn set_pin(&mut self, path: &str, pinned: bool) {
        if pinned {
            self.pinned.insert(path.to_owned());
        } else {
            self.pinned.remove(path);
        }
    }

    /// Moves an existing project and adopts the destination's pin state.
    pub fn reorder(&mut self, source: &str, target: &str, paths: &[String]) -> anyhow::Result<()> {
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
        self.set_pin(source, self.is_pinned(target));
        let mut order = paths.to_vec();
        let source = order.remove(from);
        order.insert(to, source);
        order.extend(
            self.order
                .iter()
                .filter(|path| !paths.contains(path))
                .cloned(),
        );
        self.order = order;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn project_order_and_pins_round_trip_and_reject_missing_targets() {
        let mut projects = Projects::default();
        let paths = ["/first", "/second", "/third"].map(str::to_owned);
        projects.set_pin("/first", true);
        projects
            .reorder("/third", "/first", &paths)
            .unwrap();
        assert!(projects.is_pinned("/third"));
        let restored: Projects =
            serde_json::from_str(&serde_json::to_string(&projects).unwrap()).unwrap();
        assert_eq!(restored, projects);
        assert_eq!(restored.order(), ["/third", "/first", "/second"]);
        assert!(
            projects
                .reorder("/missing", "/first", &paths)
                .is_err()
        );
        assert_eq!(restored, projects);
    }
}
