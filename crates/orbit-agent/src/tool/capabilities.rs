/// Effects on user resources, separate from filesystem/network isolation.
/// Internal caches and audit writes do not turn an observation into a project edit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolEffect {
    Observation,
    Interaction,
    Planning,
    AgentControl,
    ProjectState,
    WorkspaceWrite,
    HostExecution,
    Permission,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ToolCapabilities {
    pub effect: ToolEffect,
    pub parallel: bool,
    pub child: bool,
}

impl ToolCapabilities {
    pub const UNKNOWN: Self = Self::exclusive(ToolEffect::Unknown, false);
    pub const READ: Self = Self {
        effect: ToolEffect::Observation,
        parallel: true,
        child: true,
    };
    pub const INTERACTION: Self = Self::exclusive(ToolEffect::Interaction, true);
    pub const PLANNING: Self = Self::exclusive(ToolEffect::Planning, false);
    pub const AGENT_CONTROL: Self = Self::exclusive(ToolEffect::AgentControl, false);
    pub const PROJECT_STATE: Self = Self::exclusive(ToolEffect::ProjectState, true);
    pub const WRITE: Self = Self::exclusive(ToolEffect::WorkspaceWrite, true);
    pub const COMMAND: Self = Self::exclusive(ToolEffect::HostExecution, true);
    pub const PERMISSION: Self = Self::exclusive(ToolEffect::Permission, false);

    pub const fn exclusive(effect: ToolEffect, child: bool) -> Self {
        Self {
            effect,
            parallel: false,
            child,
        }
    }

    pub const fn in_plan(self) -> bool {
        matches!(
            self.effect,
            ToolEffect::Observation
                | ToolEffect::Interaction
                | ToolEffect::Planning
                | ToolEffect::AgentControl
        )
    }

    pub const fn in_child(self, develop: bool) -> bool {
        self.child
            && (develop
                || matches!(
                    self.effect,
                    ToolEffect::Observation | ToolEffect::Interaction
                ))
    }

    pub const fn parallel_read(self) -> bool {
        self.parallel && matches!(self.effect, ToolEffect::Observation)
    }
}
