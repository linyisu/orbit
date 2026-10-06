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
            "spawn_agent",
            "send_agent_message",
            "wait_agents",
            "read_agent",
            "cancel_agents",
        ] {
            tools.remove(name);
        }
    }

    if capability_is_disabled(disabled, Capability::History) {
        tools.remove("read_history");
    }

    if capability_is_disabled(disabled, Capability::Web) {
        tools.remove("web_search");
        tools.remove("web_fetch");
    }

    if capability_is_disabled(disabled, Capability::Mcp) {
        tools.remove_prefix("mcp__");
    }

    if capability_is_disabled(disabled, Capability::Skills) {
        tools.remove("load_skill");
    }

    if capability_is_disabled(disabled, Capability::Image) {
        tools.remove("read_image");
    }

    if capability_is_disabled(disabled, Capability::Document) {
        tools.remove("read_document");
    }

    if capability_is_disabled(disabled, Capability::Files) {
        for name in [
            "read_file",
            "list_files",
            "search_text",
            "project_overview",
            "verification_status",
        ] {
            tools.remove(name);
        }
    }

    if capability_is_disabled(disabled, Capability::Git) {
        tools.remove("git_inspect");
        tools.remove("git_export_patch");
        tools.remove("git_push");
    }

    if capability_is_disabled(disabled, Capability::Write) {
        tools.remove("write_file");
        tools.remove("edit_file");
    }

    if capability_is_disabled(disabled, Capability::Capability) {
        tools.remove("request_capability");
    }

    if capability_is_disabled(disabled, Capability::Command) {
        tools.remove("run_command");
        tools.remove("read_command_output");
        tools.remove("get_command_status");
        tools.remove("wait_commands");
        tools.remove("cancel_commands");
        tools.remove("export_artifact");
        tools.remove("verification_status");
    }

    if capability_is_disabled(disabled, Capability::ProjectState) {
        for name in [
            "task_list",
            "task_update",
            "memory_list",
            "memory_search",
            "memory_save",
            "memory_forget",
        ] {
            tools.remove(name);
        }
    }
}
