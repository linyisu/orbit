use crate::{config::FileConfig, models};
use anyhow::Result;
pub use orbit_protocol::Capability;
use std::collections::BTreeSet;

fn capability_is_disabled(disabled: &BTreeSet<Capability>, capability: Capability) -> bool {
    disabled.contains(&Capability::All) || disabled.contains(&capability)
}

pub(crate) fn select_model(
    config: &FileConfig,
    requested: Option<&str>,
) -> Result<models::ModelOption> {
    config.select_model(requested.ok_or_else(|| anyhow::anyhow!("请选择模型"))?)
}

pub(crate) fn filter_tools(
    tools: &mut orbit_agent::tool::ToolRegistry,
    disabled: &BTreeSet<Capability>,
) {
    if disabled.contains(&Capability::All) {
        tools.remove_prefix("");
        return;
    }
    if capability_is_disabled(disabled, Capability::Subagents) {
        for name in [
            "SpawnAgent",
            "SendAgentMessage",
            "WaitAgents",
            "ReadAgent",
            "StopAgents",
        ] {
            tools.remove(name);
        }
    }

    if capability_is_disabled(disabled, Capability::History) {
        tools.remove("ReadHistory");
    }

    if capability_is_disabled(disabled, Capability::Web) {
        tools.remove("WebSearch");
        tools.remove("WebFetch");
    }

    if capability_is_disabled(disabled, Capability::Mcp) {
        tools.remove_origin(orbit_agent::tool::ToolOrigin::Mcp);
    }

    if capability_is_disabled(disabled, Capability::Skills) {
        tools.remove("LoadSkill");
    }

    if capability_is_disabled(disabled, Capability::Image) {
        tools.remove("ReadImage");
    }

    if capability_is_disabled(disabled, Capability::Document) {
        tools.remove("ReadDocument");
    }

    if capability_is_disabled(disabled, Capability::Files) {
        for name in [
            "ReadFile",
            "ListFiles",
            "Grep",
            "ProjectOverview",
            "CodeSymbols",
            "CheckEvidence",
        ] {
            tools.remove(name);
        }
    }

    if capability_is_disabled(disabled, Capability::Write) {
        tools.remove("WriteFile");
        tools.remove("EditFile");
    }

    if capability_is_disabled(disabled, Capability::Capability) {
        tools.remove("RequestAccess");
    }

    if capability_is_disabled(disabled, Capability::Command) {
        tools.remove("Exec");
        tools.remove("CommandOutput");
        tools.remove("CommandStatus");
        tools.remove("WaitCommands");
        tools.remove("StopCommands");
        tools.remove("CheckEvidence");
    }

    if capability_is_disabled(disabled, Capability::ProjectState) {
        for name in [
            "ListTasks",
            "UpdateTask",
            "ListMemories",
            "SearchMemory",
            "SaveMemory",
            "ForgetMemory",
        ] {
            tools.remove(name);
        }
    }
}
