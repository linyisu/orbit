use super::*;

pub(super) async fn run_command(
    prompt: &str,
    state: &mut UiState,
    model: &mut ModelClient,
    history: &mut Vec<ChatMessage>,
    session: &mut SessionState,
    context: CommandContext<'_>,
) -> Result<bool> {
    let CommandContext {
        tools,
        instructions,
        skills,
        mcp_statuses,
        model_options,
    } = context;
    let (command, argument) = prompt
        .split_once(char::is_whitespace)
        .unwrap_or((prompt, ""));
    let argument = argument.trim();

    match command {
        "/exit" | "/quit" if argument.is_empty() => return Ok(true),

        "/rename" => {
            if argument.is_empty() {
                state.report_local(TranscriptItem::CommandError(
                    "用法：/rename <会话标题>".into(),
                ));
            } else if argument.chars().count() > 120 {
                state.report_local(TranscriptItem::CommandError(
                    "会话标题不能超过 120 个字符。".into(),
                ));
            } else {
                session.rename(argument.into()).await?;
                state.report_local(TranscriptItem::CommandResult(format!(
                    "会话标题已改为：{argument}"
                )));
            }
        }

        "/mode" | "/permissions" => {
            if !argument.is_empty() {
                let Some(mode) = permissions::Mode::parse(argument) else {
                    state.report_local(TranscriptItem::CommandError(
                        "用法：/mode ask | auto | YOLO".into(),
                    ));
                    return Ok(false);
                };

                session
                    .store
                    .set_tui_mode(session.id(), mode.label())
                    .await?;
                state.permission_mode = mode;
            }

            let description = format!(
                "{}\nShift+Tab 切换 · /mode ask|auto|YOLO\n本会话各窗口同步；新会话默认为 auto。",
                state.permission_mode.description()
            );

            if argument.is_empty() {
                state.show_output("权限设置", description);
            } else {
                state.report_local(TranscriptItem::CommandResult(description));
            }
        }

        "/context" if argument.is_empty() => {
            let estimated = orbit_agent::context::estimate_tokens(history, &tools.definitions());
            let overview_tokens = tool_context_tokens(history, "project_overview");
            let system = history
                .iter()
                .filter(|message| matches!(message, ChatMessage::System { content } if !content.starts_with("ORBIT_PROJECT_STATE_V1\n")) && !crate::instructions::is_orbit_instruction(message))
                .cloned()
                .collect::<Vec<_>>();
            let project_rules = history
                .iter()
                .filter(|message| crate::instructions::is_orbit_instruction(message))
                .cloned()
                .collect::<Vec<_>>();
            let conversation = history
                .iter()
                .filter(|message| !matches!(message, ChatMessage::System { .. }))
                .cloned()
                .collect::<Vec<_>>();

            let system_tokens = orbit_agent::context::estimate_tokens(&system, &[]);
            let project_instruction_tokens =
                orbit_agent::context::estimate_tokens(&project_rules, &[]);
            let conversation_tokens = orbit_agent::context::estimate_tokens(&conversation, &[]);
            let tool_tokens = orbit_agent::context::estimate_tokens(&[], &tools.definitions());

            let sources = if instructions.sources.is_empty() {
                "未找到".to_owned()
            } else {
                format!("{} 条", instructions.sources.len())
            };

            let truncation = if instructions.truncated {
                "（达到 32 KiB 上限，后续文件未加载）"
            } else {
                ""
            };

            let project_state_tokens = history
                .iter()
                .filter_map(|message| match message {
                    ChatMessage::System { content }
                        if content.starts_with("ORBIT_PROJECT_STATE_V1\n") =>
                    {
                        Some(orbit_agent::context::estimate_tokens(
                            std::slice::from_ref(message),
                            &[],
                        ))
                    }

                    _ => None,
                })
                .sum::<usize>();
            let details = format!(
                "原始上下文估算：约 {estimated} tokens（含协议开销）\n\
                系统提示：约 {system_tokens} tokens\n\
                项目指令：{sources} · 约 {project_instruction_tokens} tokens {truncation}\n\
                {}\n\
                历史消息：约 {conversation_tokens} tokens\n\
                工具定义：约 {tool_tokens} tokens\n\
                代码概览工具结果：约 {overview_tokens} tokens\n\
                任务与会话记忆：约 {project_state_tokens} tokens\n\
                输入窗口：{} tokens · 输出上限：{} tokens · 请求预算：{} tokens\n\
                自动整理：{}（按校准后的输入窗口触发，目标按近期工作预算确定）\n\
                这是通用估算，不等于服务端实际用量；分类值含各自协议开销，不能相加为总计。\n\
                工具原文先归档再精简；需要摘要时调用模型，摘要失败保留原上下文。\n\
                /compact 按预算整理旧对话，保留完整工具配对和近期工作；归档内容可通过 read_history 回读。",
                instructions.source_summary(),
                state.input_context_tokens,
                state.max_output_tokens,
                state.context_tokens,
                if state.auto_compact {
                    "开启"
                } else {
                    "关闭"
                }
            );
            let calibration = state.calibration();
            state.show_output(
                "上下文预算",
                format!(
                    "{details}\n校准后总计：约 {} tokens · {} 个服务端样本 · 系数 {:.3}",
                    calibration.estimate(estimated),
                    calibration.samples(),
                    calibration.factor()
                ),
            );
        }

        "/skills" if argument.is_empty() => state.show_output("Skills", skills.render()),

        "/mcp" if argument.is_empty() => {
            state.show_output("MCP 状态", render_mcp_status(mcp_statuses))
        }

        "/plugins" if argument.is_empty() => {
            let project_id = session.snapshot.session.project_id.clone();
            let enabled = session.store.project_plugins(&project_id).await?;
            state.marketplace_dialog = Some(dialogs::MarketplaceDialog::new(
                crate::marketplaces::list_current()?,
                project_id,
                enabled,
            ));
        }

        "/skill" => {
            if argument.is_empty() {
                state.report_local(TranscriptItem::CommandError(
                    "用法：/skill <名称>。输入 /skills 查看已发现的 Skills。".into(),
                ));
            } else if let Some(skill) = skills.get(argument) {
                match std::fs::read_to_string(&skill.path) {
                    Ok(content) => {
                        history.push(ChatMessage::user(format!(
                            "请在后续工作中使用已载入的 Skill /{}：\n{}",
                            skill.command, content
                        )));
                        state.report_local(TranscriptItem::CommandResult(format!(
                            "已载入 /{}。",
                            skill.command
                        )));
                    }

                    Err(error) => state.report_local(TranscriptItem::CommandError(format!(
                        "无法读取 Skill /{}：{error}",
                        skill.command
                    ))),
                }
            } else {
                state.report_local(TranscriptItem::CommandError(format!(
                    "未找到 Skill {argument:?}。输入 /skills 查看已发现的 Skills。"
                )));
            }
        }

        "/tasks" => run_tasks_command(argument, state, history, session).await?,
        "/memory" => run_memory_command(argument, state, history, session).await?,

        "/changes" if argument.is_empty() => {
            let listing = changes::list(&session.store.workspace_changes(session.id()).await?);
            let comparison = crate::verification::changes_text(session).await;
            state.show_output("文件改动", format!("{listing}{comparison}"));
        }

        "/diff" if argument.trim().is_empty() => state.show_output(
            "文件改动",
            changes::list(&session.store.workspace_changes(session.id()).await?),
        ),

        "/diff" => state.show_output(
            "文件差异",
            changes::diff(
                &session.store.workspace_changes(session.id()).await?,
                argument.trim(),
            ),
        ),

        "/diagnostics" if argument.is_empty() => state.show_output(
            "诊断记录",
            orbit_store::diagnostics::text(&session.store.diagnostics(session.id()).await?),
        ),

        "/resume" | "/sessions" if argument.is_empty() => {
            state.picker = Some(dialogs::SessionPicker::new(session.resumable().await?));
            state.selection = Default::default();
        }

        "/resume" => {
            state.resume_request = Some(argument.to_owned());
        }

        "/status" if argument.is_empty() => {
            let verifications = session.store.verification_records(session.id()).await?;
            let passed = verifications
                .iter()
                .filter(|record| {
                    record.exit_code == Some(0) && !record.timed_out && !record.cancelled
                })
                .count();
            let failed = verifications.len().saturating_sub(passed);
            let verification = if let Some(last) = verifications.last() {
                format!(
                    "命令执行  {} 次 · 成功退出 {} · 异常结束 {} · 最近 exit {} · {} ms",
                    verifications.len(),
                    passed,
                    failed,
                    last.exit_code
                        .map_or_else(|| "无".to_owned(), |code| code.to_string()),
                    last.elapsed_ms,
                )
            } else {
                "命令执行  尚未执行命令".to_owned()
            };

            let verification = format!(
                "{}\n{}",
                verification,
                crate::verification::summary(session).await
            );
            let baseline = session.git_baseline.as_ref().map_or_else(
                || "Git 基线  非 Git 工作区或无法读取".to_owned(),
                |value| {
                    format!(
                        "Git 基线  {} · HEAD {} · 初始改动 {} 项{}",
                        value["branch"]["head"].as_str().unwrap_or("detached"),
                        value["branch"]["oid"].as_str().unwrap_or("unknown"),
                        value["entries"].as_array().map_or(0, Vec::len),
                        if value["has_more"].as_bool() == Some(true) {
                            "以上限截断"
                        } else {
                            ""
                        },
                    )
                },
            );
            let tool_names = tools
                .definitions()
                .iter()
                .map(|definition| definition.function.name.as_str())
                .collect::<Vec<_>>()
                .join(" / ");

            state.show_output("会话状态", format!(
                "会话  {}\n数据库  {}\n会话工作目录  {}\n命令输出目录  {}（按会话保留）\n{}\n{}\n模型  {}\n上下文  {} 条消息 · 已持久化\n项目指令  {} 条\nSkills  {} 个\nMCP  {} 个 server，输入 /mcp 查看状态\n超时  {}s（流空闲）\n工具  {} · 审批：{}",
                session.id(),
                session.store.path().display(),
                session.scratch.read().expect("scratch lock poisoned").display(),
                session.command_output.read().expect("command output lock poisoned").display(),
                baseline,
                verification,
                state.model,
                history.len(),
                instructions.sources.len(),
                skills.len(),
                mcp_statuses.len(),
                model.config().timeout.as_secs(),
                tool_names,
                state.permission_mode.label()
            ));
        }

        "/reasoning" => {
            let efforts = state.reasoning_efforts.clone();
            if efforts.is_empty() {
                state.show_output("思考强度", "当前模型未配置可调整的思考强度。".into());
            } else if argument.is_empty() {
                state.input.set("/reasoning ".into());
            } else if let Some(effort) = efforts.iter().find(|effort| effort.as_str() == argument) {
                let name = state.model.clone();
                select_model(&name, Some(*effort), state, model, model_options, session).await?;
            } else {
                state.report_local(TranscriptItem::CommandError(format!(
                    "当前模型支持的思考强度：{}",
                    efforts
                        .iter()
                        .map(|effort| effort.as_str())
                        .collect::<Vec<_>>()
                        .join(" / ")
                )));
            }
        }

        "/model" => {
            if argument.is_empty() {
                state.model_picker = Some(dialogs::ModelPicker::new(
                    model_options.to_vec(),
                    state.model.clone(),
                    model.config().reasoning_effort,
                ));
            } else {
                let current_provider = model_options
                    .iter()
                    .find(|choice| choice.qualified_name() == state.model)
                    .map(|choice| choice.provider.as_str());
                let choice = if let Some((provider, name)) = argument.split_once("::") {
                    model_options
                        .iter()
                        .find(|choice| choice.provider == provider && choice.model == name)
                } else {
                    model_options.iter().find(|choice| {
                        Some(choice.provider.as_str()) == current_provider
                            && choice.model == argument
                    })
                };

                if let Some(choice) = choice {
                    select_model(
                        &choice.qualified_name(),
                        None,
                        state,
                        model,
                        model_options,
                        session,
                    )
                    .await?;
                } else {
                    state.report_local(TranscriptItem::CommandError(format!(
                        "模型 {argument:?} 未找到。"
                    )));
                }
            }
        }

        _ => {
            let result = if let Some(suggestion) = commands::suggestion(command) {
                format!("未知命令：{command}。是否想输入 {}？", suggestion.name)
            } else {
                format!("未知命令：{command}。输入 / 查看可用命令。")
            };

            state.report_local(TranscriptItem::CommandError(result));
        }
    }

    state.follow_bottom = true;
    state.menu_dismissed = false;

    Ok(false)
}

pub(super) async fn run_tasks_command(
    argument: &str,
    state: &mut UiState,
    history: &mut Vec<ChatMessage>,
    session: &SessionState,
) -> Result<()> {
    use crate::project_state::actions::{TaskAction, TaskResult};
    let (action, rest) = argument
        .split_once(char::is_whitespace)
        .unwrap_or((argument, ""));
    let action = match action {
        "" | "list" | "all" => TaskAction::List {
            include_completed: action == "all",
        },
        "add" if !rest.trim().is_empty() => TaskAction::Add {
            title: rest.trim().into(),
        },
        "start" | "done" | "block" | "cancel" => {
            let Ok(id) = rest.trim().parse::<i64>() else {
                state.report_local(TranscriptItem::CommandError(
                    "用法：/tasks start|done|block|cancel <任务编号>".into(),
                ));
                return Ok(());
            };
            let status = match action {
                "start" => orbit_store::types::TaskStatus::InProgress,
                "done" => orbit_store::types::TaskStatus::Completed,
                "block" => orbit_store::types::TaskStatus::Blocked,
                _ => orbit_store::types::TaskStatus::Cancelled,
            };
            TaskAction::SetStatus { id, status }
        }
        _ => {
            state.report_local(TranscriptItem::CommandError("用法：/tasks [list|all] · /tasks add <标题> · /tasks start|done|block|cancel <编号>".into()));
            return Ok(());
        }
    };
    match crate::project_state::actions::task(session, history, action).await? {
        TaskResult::List(tasks) => state.show_output("项目任务", render_tasks(&tasks)),
        TaskResult::Added(task) => state.report_local(TranscriptItem::CommandResult(format!(
            "已添加任务 #{}：{}",
            task.id, task.title
        ))),
        TaskResult::Updated(task) => state.report_local(TranscriptItem::CommandResult(format!(
            "任务 #{} 已标记为{}。",
            task.id,
            task_status_label(task.status)
        ))),
    }
    Ok(())
}

pub(super) fn render_mcp_status(statuses: &[orbit_tools::mcp::McpServerStatus]) -> String {
    if statuses.is_empty() {
        return "没有配置 MCP server。可在 config.toml 中添加 [mcp.servers.<名称>]，指定 command、args 和 enabled = true。当前仅支持本地 stdio server。".into();
    }

    let mut output = String::from("MCP servers\n");

    for status in statuses {
        let marker = if status.connected {
            icons::CHECK
        } else {
            icons::CLOSE
        };

        output.push_str(&format!(
            "{marker} {}  {} · {} 个工具\n",
            status.name, status.detail, status.tool_count
        ));
    }

    output
}

pub(super) async fn run_memory_command(
    argument: &str,
    state: &mut UiState,
    history: &mut Vec<ChatMessage>,
    session: &SessionState,
) -> Result<()> {
    use crate::project_state::actions::{MemoryAction, MemoryResult};
    let (action, rest) = argument
        .split_once(char::is_whitespace)
        .unwrap_or((argument, ""));
    let action = match action {
        "" | "list" | "all" => MemoryAction::List {
            include_inactive: action == "all",
        },
        "add" => {
            let (category, content) = rest.split_once(char::is_whitespace).unwrap_or(("", ""));
            let Some(category) = orbit_store::types::MemoryCategory::parse(category) else {
                state.report_local(TranscriptItem::CommandError(
                    "用法：/memory add constraint|decision|fact|preference <内容>".into(),
                ));
                return Ok(());
            };
            MemoryAction::Add {
                category,
                content: content.into(),
            }
        }
        "search" => MemoryAction::Search { query: rest.into() },
        "forget" => {
            let Ok(id) = rest.trim().parse::<i64>() else {
                state.report_local(TranscriptItem::CommandError(
                    "用法：/memory forget <记忆编号>".into(),
                ));
                return Ok(());
            };
            MemoryAction::Forget { id }
        }
        _ => {
            state.report_local(TranscriptItem::CommandError("用法：/memory [list|all] · /memory search <关键词> · /memory add <类型> <内容> · /memory forget <编号>".into()));
            return Ok(());
        }
    };
    match crate::project_state::actions::memory(session, history, action).await? {
        MemoryResult::List(memories) => state.show_output("会话记忆", render_memories(&memories)),
        MemoryResult::Search { query, memories } => state.show_output(
            "记忆搜索",
            format!("记忆搜索：{query}\n{}", render_memories(&memories)),
        ),
        MemoryResult::Added(memory) => state.report_local(TranscriptItem::CommandResult(format!(
            "已保存会话记忆 #{}。",
            memory.id
        ))),
        MemoryResult::Forgotten(memory) => state.report_local(TranscriptItem::CommandResult(
            format!("会话记忆 #{} 已停用。", memory.id),
        )),
    }
    Ok(())
}

pub(super) fn render_tasks(tasks: &[orbit_store::types::ProjectTask]) -> String {
    if tasks.is_empty() {
        return "当前项目没有待跟踪的任务。\n\n/tasks add <标题> 可创建任务。".into();
    }

    let mut output = String::from("项目任务\n");

    for task in tasks {
        use std::fmt::Write;

        let _ = writeln!(
            output,
            "{}  #{}  {}{}",
            task_status_label(task.status),
            task.id,
            task.title,
            if !task.details.is_empty() {
                format!("\n        {}", task.details)
            } else {
                String::new()
            },
        );
    }

    output
}

pub(super) fn task_status_label(status: orbit_store::types::TaskStatus) -> String {
    let (icon, label) = match status {
        orbit_store::types::TaskStatus::Pending => (icons::CIRCLE_OUTLINE, "待办"),
        orbit_store::types::TaskStatus::InProgress => (icons::PLAY, "进行中"),
        orbit_store::types::TaskStatus::Blocked => (icons::BLOCK_HELPER, "已阻塞"),
        orbit_store::types::TaskStatus::Completed => (icons::CHECK, "完成"),
        orbit_store::types::TaskStatus::Cancelled => (icons::CANCEL, "已取消"),
    };
    format!("{icon} {label}")
}

pub(super) fn render_memories(memories: &[orbit_store::types::SessionMemory]) -> String {
    if memories.is_empty() {
        return "当前会话没有已保存的长期记忆。\n\n/memory add <类型> <内容> 可保存约束、决策、事实或偏好。".into();
    }

    let mut output = String::from("会话记忆\n");

    for memory in memories {
        use std::fmt::Write;

        let marker = if memory.active {
            icons::CIRCLE
        } else {
            icons::CIRCLE_OUTLINE
        };

        let _ = writeln!(
            output,
            "{marker} #{} [{}] {}\n  {}",
            memory.id,
            memory.category.as_str(),
            memory.title,
            memory.content,
        );
    }

    output
}
