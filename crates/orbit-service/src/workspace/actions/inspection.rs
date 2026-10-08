use super::report;
use crate::PreparedWorkspace;
use anyhow::Result;
use orbit_model::types::ChatMessage;
use orbit_protocol::{Inspection, Report};

pub(super) async fn inspect(setup: &PreparedWorkspace, section: Inspection) -> Result<Report> {
    let session = &setup.session;
    let resources = &setup.resources;
    let budget = &resources.budget;
    Ok(match section {
        Inspection::ExportPatch => report(
            "Git patch",
            orbit_tools::git_inspect::working_tree_patch(std::path::Path::new(
                &session.snapshot.session.workspace,
            ))?,
        ),
        Inspection::Skills => report("Skills", budget.skills.render()),
        Inspection::Mcp => report(
            "MCP 状态",
            if budget.mcp_statuses.is_empty() {
                "未配置 MCP server。".into()
            } else {
                budget
                    .mcp_statuses
                    .iter()
                    .map(|status| {
                        format!(
                            "{} · {} · {} 个工具",
                            status.name, status.detail, status.tool_count
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            },
        ),
        Inspection::Diagnostics => report(
            "诊断记录",
            orbit_store::diagnostics::text(&session.store.diagnostics(session.id()).await?),
        ),
        Inspection::Changes => {
            let listing =
                super::super::changes::list(&session.store.workspace_changes(session.id()).await?);
            report(
                "文件改动",
                format!(
                    "{listing}{}",
                    crate::verification::changes_text(session).await
                ),
            )
        }
        Inspection::Diff { path } => {
            let changes = session.store.workspace_changes(session.id()).await?;
            let text = match path {
                Some(path) => super::super::changes::diff(&changes, &path),
                None => super::super::changes::list(&changes),
            };
            report("文件差异", text)
        }
        Inspection::Status => report("会话状态", status(setup).await?),
        Inspection::Context => {
            let mut history = session.store.side_question_context(session.id()).await?;
            crate::instructions::inject(&mut history, &budget.instructions);
            budget.skills.inject(&mut history);
            if !resources
                .policy
                .disabled(orbit_protocol::Capability::ProjectState)
            {
                crate::project_state::inject(
                    &mut history,
                    &session.store,
                    &session.snapshot.session.project_id,
                    session.id(),
                )
                .await?;
            }
            let estimated =
                orbit_agent::context::estimate_tokens(&history, &resources.tools.definitions());
            let calibration = session.calibration(&resources.model_name);
            let system = estimate_part(&history, |message| {
                matches!(message,ChatMessage::System{content} if !content.starts_with("ORBIT_PROJECT_STATE_V1\n"))
                    && !crate::instructions::is_orbit_instruction(message)
            });
            let rules = estimate_part(&history, crate::instructions::is_orbit_instruction);
            let conversation = estimate_part(&history, |message| {
                !matches!(message, ChatMessage::System { .. })
            });
            let definitions =
                orbit_agent::context::estimate_tokens(&[], &resources.tools.definitions());
            let overview = tool_context_tokens(&history, "project_overview");
            let project_state = estimate_part(
                &history,
                |message| matches!(message,ChatMessage::System{content} if content.starts_with("ORBIT_PROJECT_STATE_V1\n")),
            );
            report(
                "上下文预算",
                format!(
                    "原始估算：{estimated} tokens\n校准估算：{} tokens\n输入窗口：{} · 输出上限：{} · 请求预算：{}\n自动整理：{}\n项目指令：{}\nSkills：{}\n\n系统提示：约 {system} tokens\n项目指令内容：约 {rules} tokens\n历史消息：约 {conversation} tokens\n工具定义：约 {definitions} tokens\n代码概览工具结果：约 {overview} tokens\n任务与会话记忆：约 {project_state} tokens\n校准：{} 个服务端样本 · 系数 {:.3}\n分类估算包含各自协议开销，不能相加为总计。",
                    calibration.estimate(estimated),
                    budget.input_context_tokens,
                    budget.max_output_tokens,
                    budget.context_tokens,
                    if budget.auto_compact {
                        "开启"
                    } else {
                        "关闭"
                    },
                    budget.instructions.source_summary(),
                    budget.skills.len(),
                    calibration.samples(),
                    calibration.factor(),
                ),
            )
        }
    })
}

fn estimate_part(history: &[ChatMessage], include: impl Fn(&ChatMessage) -> bool) -> usize {
    let messages = history
        .iter()
        .filter(|message| include(message))
        .cloned()
        .collect::<Vec<_>>();
    orbit_agent::context::estimate_tokens(&messages, &[])
}

fn tool_context_tokens(history: &[ChatMessage], name: &str) -> usize {
    let ids: std::collections::HashSet<_> = history
        .iter()
        .flat_map(|message| match message {
            ChatMessage::Assistant { tool_calls, .. } => tool_calls.as_slice(),
            _ => &[],
        })
        .filter(|call| call.function.name == name)
        .map(|call| call.id.as_str())
        .collect();
    let messages = history
        .iter()
        .filter(|message| {
            matches!(message, ChatMessage::Tool { tool_call_id, .. } if ids.contains(tool_call_id.as_str()))
        })
        .cloned()
        .collect::<Vec<_>>();
    if messages.is_empty() {
        0
    } else {
        orbit_agent::context::estimate_tokens(&messages, &[])
    }
}

async fn status(setup: &PreparedWorkspace) -> Result<String> {
    let session = &setup.session;
    let resources = &setup.resources;
    let budget = &resources.budget;
    let scratch = session
        .scratch
        .read()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| "无法读取".into());
    let output = session
        .command_output
        .read()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| "无法读取".into());
    let baseline = session.git_baseline.as_ref().map_or_else(
        || "非 Git 工作区或无法读取".into(),
        |value| {
            format!(
                "{} · HEAD {} · 初始改动 {} 项{}",
                value["branch"]["head"].as_str().unwrap_or("detached"),
                value["branch"]["oid"].as_str().unwrap_or("unknown"),
                value["entries"].as_array().map_or(0, Vec::len),
                if value["has_more"].as_bool() == Some(true) {
                    "（达到扫描上限）"
                } else {
                    ""
                }
            )
        },
    );
    let records = session.store.verification_records(session.id()).await?;
    let passed = records
        .iter()
        .filter(|record| record.exit_code == Some(0) && !record.timed_out && !record.cancelled)
        .count();
    let recent = records.last().map_or_else(
        || "暂无记录".into(),
        |record| {
            format!(
                "exit {} · {} ms",
                record
                    .exit_code
                    .map_or_else(|| "无".into(), |code| code.to_string()),
                record.elapsed_ms
            )
        },
    );
    Ok(format!(
        "会话：{}\n工作目录：{}\n数据库：{}\n模型：{}\n执行：{}\nScratch：{scratch}\n命令输出目录：{output}\nGit 基线：{baseline}\n上下文：{} 条持久化消息\n项目指令：{}\nSkills：{} 个\nMCP：{} 个 server\n超时：{}s（模型流空闲） · 命令默认 {}s / 最大 {}s\n审批：{:?}\n工具：{}\n命令执行：{} 次 · 成功退出 {} · 异常结束 {} · 最近 {recent}\n{}",
        session.id(),
        session.snapshot.session.workspace,
        session.store.path().display(),
        resources.model_name,
        if resources.command_limits.sandbox {
            "沙箱"
        } else {
            "普通执行"
        },
        session.snapshot.history.len(),
        budget.instructions.source_summary(),
        budget.skills.len(),
        budget.mcp_statuses.len(),
        resources.model.config().timeout.as_secs(),
        resources.command_limits.default_timeout_seconds,
        resources.command_limits.max_timeout_seconds,
        resources.mode,
        resources
            .tools
            .definitions()
            .iter()
            .map(|tool| tool.function.name.clone())
            .collect::<Vec<_>>()
            .join(" / "),
        records.len(),
        passed,
        records.len() - passed,
        crate::verification::summary(session).await
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn overview_budget_counts_only_matching_tool_results_in_effective_history() {
        use orbit_model::types::ToolCall;

        let call = ToolCall::function("map", "project_overview", serde_json::json!({}));
        let output = ChatMessage::tool("map", "repository map");
        let messages = vec![
            ChatMessage::user("project_overview"),
            ChatMessage::assistant_tool_calls(vec![call]),
            output.clone(),
            ChatMessage::tool("other", "unrelated output"),
        ];

        assert_eq!(
            tool_context_tokens(&messages, "project_overview"),
            orbit_agent::context::estimate_tokens(&[output], &[])
        );
        assert_eq!(tool_context_tokens(&[], "project_overview"), 0);
        assert_eq!(tool_context_tokens(&messages, "missing"), 0);
    }
}
