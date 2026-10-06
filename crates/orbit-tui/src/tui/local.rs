use super::*;

pub async fn run(
    setup: crate::PreparedWorkspace,
    options: &crate::WorkspaceOptions,
) -> Result<Option<crate::PreparedWorkspace>> {
    let crate::PreparedWorkspace {
        mut session,
        resources,
    } = setup;
    let crate::WorkspaceResources {
        diagnostics,
        web_client,
        cursor_trail,
        mut model,
        model_name,
        mut tools,
        mut undo_writer,
        mut approvals,
        budget,
        moved,
        ..
    } = resources;
    let next_workspace: Option<crate::WorkspaceResources> = None;
    let (_guard, mut terminal) = TerminalGuard::enter()?;
    let mut state = UiState::new(model_name);
    state.media = media::Context::detect(web_client);
    state.cursor_motion.enabled = cursor_trail;
    let RequestBudget {
        context_tokens,
        input_context_tokens,
        mut auto_compact,
        max_output_tokens,
        mut max_turns,
        mut max_tool_calls,
        mut instructions,
        mut skills,
        plugin_skill_diagnostics,
        mut mcp_statuses,
        mut model_options,
        mut memory_scope,
        mut compaction,
    } = budget;

    state.context_tokens = context_tokens;
    state.input_context_tokens = input_context_tokens;
    state.auto_compact = auto_compact;
    state.max_output_tokens = max_output_tokens;
    state.reasoning_effort = model.config().reasoning_effort;
    state.reasoning_efforts = model_options
        .iter()
        .find(|choice| choice.qualified_name() == state.model)
        .map(|choice| choice.reasoning_efforts.clone())
        .unwrap_or_default();
    state.supports_vision = model.supports_vision();
    state.model_completions = model_options
        .iter()
        .map(ModelOption::qualified_name)
        .collect();
    let mut history_replay =
        (!session.snapshot.runs.is_empty()).then(crate::tui::replay::HistoryReplay::default);

    if history_replay.is_some() {
        state.resume_loading = Some("正在显示会话历史…".into());
    }

    state.completion_home = options.home.clone();
    state.skill_completions = skills.completions();
    state.workspace = session.snapshot.session.workspace.clone();
    state.subagent_parent = session.store.subagent_parent(session.id()).await?;
    let mut move_notice = moved.then(|| format!("Moved to {}", state.workspace));
    if history_replay.is_none()
        && let Some(notice) = move_notice.take()
    {
        state.report_local(TranscriptItem::CommandResult(notice));
    }

    for diagnostic in plugin_skill_diagnostics {
        state.report_local(TranscriptItem::Notice(format!(
            "插件 Skill 未载入：{diagnostic}"
        )));
    }

    for diagnostic in diagnostics {
        state.report_local(TranscriptItem::Notice(diagnostic));
    }

    restore_model_profile(&mut state, &mut model, &model_options, &session);
    let mut history = session.snapshot.history.clone();
    crate::instructions::inject(&mut history, &instructions);
    skills.inject(&mut history);
    crate::project_state::inject(
        &mut history,
        &session.store,
        &session.snapshot.session.project_id,
        session.id(),
    )
    .await?;
    refresh_context_usage(&mut state, &history, &tools);
    session.enable_shared();
    session_sync::sync_mode(&session.store, session.id(), &mut state).await?;
    let mut tabs = tabs::Tabs::new(&session);
    tabs.persist(&session).await?;
    let mut session_sync = session_sync::SessionSync::default();
    let mut subagent_progress = subagents::Progress::default();
    let mut sync_checked = std::time::Instant::now();
    let editor = editor::Editor::new(options.editor_command.clone());
    let mut events = EventStream::new();
    let mut pending_event = None;
    let mut ticks = time::interval(Duration::from_millis(33));
    ticks.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
    let mut command_ticks = time::interval(Duration::from_millis(50));
    command_ticks.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
    let mut receiver: Option<mpsc::Receiver<AgentEvent>> = None;
    let mut cancellation: Option<CancellationToken> = None;
    let mut task: Option<RunTask> = None;
    let mut local_task: Option<LocalTask> = None;
    let mut local_cancellation: Option<CancellationToken> = None;
    let mut resume_task: Option<ResumeTask> = None;
    let mut marketplace_task: Option<MarketplaceTask> = None;
    let mut model_refresh: Option<ModelRefreshTask> = None;
    let mut catalog_watcher =
        model_catalog::Watcher::new(options.config.clone(), options.auth_directory.clone());
    let mut pending_guidance = Vec::new();
    let mut retry_submit = false;
    let mut restore_queue = std::collections::VecDeque::new();
    let mut restore_errors = Vec::new();
    let mut refresh_before_retry = false;
    let mut active_context: Option<ChatMessage> = None;
    let mut pending: Option<orbit_tools::approval::ApprovalRequest> = None;
    let mut frames = frames::Frames::default();
    let mut wheel = wheel::Wheel::default();
    let mut branch_task: Option<JoinHandle<(String, Option<String>)>> = None;
    let mut branch_checked = std::time::Instant::now();
    let mut branch_workspace = String::new();
    let mut background_runs = background::Runs::new();
    let mut bound_session = session.id().to_owned();

    macro_rules! park_run {
        () => {
            background_runs.insert(
                session.id().to_owned(),
                background::BackgroundRun {
                    task: task.take(),
                    local_task: local_task.take(),
                    local_cancellation: local_cancellation.take(),
                    history_replay: history_replay.take(),
                    move_notice: move_notice.take(),
                    marketplace_task: marketplace_task.take(),
                    model_refresh: model_refresh.take(),
                    retry_submit: std::mem::take(&mut retry_submit),
                    refresh_before_retry: std::mem::take(&mut refresh_before_retry),
                    receiver: receiver.take(),
                    cancellation: cancellation.take(),
                    pending: pending.take(),
                    guidance: std::mem::take(&mut pending_guidance),
                    active_context: active_context.take(),
                    tools: tools.clone(),
                    undo_writer: undo_writer.clone(),
                    approvals: std::mem::replace(
                        &mut approvals,
                        orbit_tools::approval::channel().1,
                    ),
                    instructions: instructions.clone(),
                    skills: skills.clone(),
                    memory_scope: memory_scope.clone(),
                    auto_compact,
                    max_turns,
                    max_tool_calls,
                    compaction: compaction.clone(),
                    last_publish: std::time::Instant::now(),
                    mcp_statuses: mcp_statuses.clone(),
                    model_options: model_options.clone(),
                },
            );
        };
    }

    let result: Result<()> = async {
        let mut dirty = true;
        let mut completion_key = None;

        loop {
            if let Some(ids) = state.restore_selection.take() {
                restore_queue.extend(ids);
            }
            if state.resume_request.is_none() && resume_task.is_none() {
                state.resume_request = restore_queue.pop_front();
            }
            if let Some(run) = background_runs.remove(session.id()) {
                task = run.task;
                local_task = run.local_task;
                local_cancellation = run.local_cancellation;
                history_replay = run.history_replay;
                move_notice = run.move_notice;
                marketplace_task = run.marketplace_task;
                model_refresh = run.model_refresh;
                retry_submit = run.retry_submit;
                refresh_before_retry = run.refresh_before_retry;
                receiver = run.receiver;
                cancellation = run.cancellation;
                pending = run.pending;
                pending_guidance = run.guidance;
                active_context = run.active_context;
                tools = run.tools;
                undo_writer = run.undo_writer;
                approvals = run.approvals;
                instructions = run.instructions;
                skills = run.skills;
                memory_scope = run.memory_scope;
                auto_compact = run.auto_compact;
                max_turns = run.max_turns;
                max_tool_calls = run.max_tool_calls;
                compaction = run.compaction;
                mcp_statuses = run.mcp_statuses;
                model_options = run.model_options;
                bound_session = session.id().to_owned();
                session_sync.reset(session.id());
            }

            if state.resume_request.is_none() && resume_task.is_none() && history_replay.is_none() {
                for error in restore_errors.drain(..) {
                    state.report_local(TranscriptItem::CommandError(error));
                    dirty = true;
                }
            }
            // A peer can move this session while this window remains open.
            // Rebind the actual runtime before accepting the next input.
            if task.is_none() && local_task.is_none() && resume_task.is_none() && history_replay.is_none() {
                if retry_submit || std::mem::take(&mut refresh_before_retry) {
                    session_sync.reset(session.id());
                    session_sync.poll(&session.store, &mut session.snapshot, &mut state, &mut history).await?;
                    restore_model_profile(&mut state, &mut model, &model_options, &session);
                }

                let tab_workspace = std::path::Path::new(&session.snapshot.session.workspace);
                if bound_session != session.id() || session.audit_workspace.as_ref().is_none_or(|workspace| workspace.root() != tab_workspace) {
                    let prep = crate::prepare_workspace_resources(options, tab_workspace, &session).await?;
                    workspace::bind(&prep, &mut session, &mut state, &mut model, &mut history).await?;
                    bound_session = session.id().to_owned();
                    tools = prep.tools;
                    instructions = prep.budget.instructions;
                    skills = prep.budget.skills;
                    undo_writer = prep.undo_writer;
                    approvals = prep.approvals;
                    mcp_statuses = prep.budget.mcp_statuses;
                    model_options = prep.budget.model_options;
                    memory_scope = prep.budget.memory_scope;
                    auto_compact = prep.budget.auto_compact;
                    max_turns = prep.budget.max_turns;
                    max_tool_calls = prep.budget.max_tool_calls;
                    compaction = prep.budget.compaction;
                    refresh_context_usage(&mut state, &history, &tools);
                    dirty = true;
                }
            }

            if model_refresh.as_ref().is_some_and(|task| task.is_finished()) {
                let result = model_refresh.take().expect("model refresh task").await;
                let refreshed = matches!(&result, Ok(Ok(_)));
                background::model_refresh_result(result, &mut state, &mut model_options);
                if refreshed {
                    catalog_watcher.resynchronize();
                    model_catalog::publish(model_options.clone(), &mut state, &mut model_options, &mut background_runs, &mut tabs);
                }

                dirty = true;
            }

            if task.is_none() && local_task.is_none() && resume_task.is_none() && history_replay.is_none()
                && model_catalog::reconcile(&mut state, &mut model, &model_options)
            {
                refresh_context_usage(&mut state, &history, &tools);
                dirty = true;
            }

            let key = (session.id().to_owned(), state.input.text.clone());
            if completion_key.as_ref() != Some(&key) {
                state.stored_completions = completion::stored(&state.input.text, &session).await.unwrap_or_default();
                completion_key = Some(key);
            }

            if state.compact_after_model_switch && task.is_none() && local_task.is_none() && resume_task.is_none() && history_replay.is_none() {
                state.compact_after_model_switch = false;
                refresh_context_usage(&mut state, &history, &tools);

                if needs_model_switch_compaction(&state) {
                    let target_context = state.context_tokens;
                    let started = start_agent_run("/compact".into(), Vec::new(), true,
                        &mut state, &mut session, &history, &model, &tools,
                        target_context,
                        auto_compact, max_turns, max_tool_calls, &compaction).await;

                    match started {
                        Ok(started) => { receiver=Some(started.receiver); cancellation=Some(started.cancellation); task=Some(started.task); }
                        Err(error) if error.is::<crate::session::SessionRefreshRequired>() => {
                            refresh_before_retry = true;
                            state.compact_after_model_switch = true;
                            continue;
                        }

                        Err(error) => state.report_local(TranscriptItem::CommandError(error.to_string())),
                    }

                    dirty = true;
                    frames.request(time::Instant::now());
                }
            }

            if std::mem::take(&mut state.new_tab_request) {
                tabs.active_remote = None;
                if let Some(task) = resume_task.take() { task.abort(); }
                if history_replay.is_none() { state.resume_loading = None; }
                park_run!();
                if let Err(error) = new_tab(
                    &mut tabs, &mut session, &mut state, &mut model,
                    &mut history, &options.initial_directory, CommandContext {
                        tools: &tools,
                        instructions: &instructions,
                        skills: &skills,
                        mcp_statuses: &mcp_statuses,
                        model_options: &model_options,
                    },
                ).await {
                    state.report_local(TranscriptItem::CommandError(error.to_string()));
                }

                dirty = true;
                frames.request(time::Instant::now());
                continue;
            }

            if let Some(id) = state.close_tab_request.take() {
                if tabs.remote.contains_key(&id) {
                    let next = tabs.close_target(&id).map(str::to_owned);
                    tabs.remove_remote(&id);
                    if let Some(next) = next { state.resume_request = Some(next); }
                    else { state.new_tab_request = true; }
                    dirty = true;
                    frames.request(time::Instant::now());
                    continue;
                }
                let closing_active = id == session.id();
                let switching = closing_active
                    || tabs.close_target(&id).is_some_and(|target| target != session.id());
                if switching && !tabs.close_target(&id).is_some_and(|target| tabs.remote.contains_key(target)) {
                    if let Some(task) = resume_task.take() { task.abort(); }
                    if history_replay.is_none() { state.resume_loading = None; }
                    park_run!();
                }

                if !tabs.close_tab(&id, &mut session, &mut state, &mut model, &mut history)
                    && closing_active
                {
                    match new_tab(
                        &mut tabs, &mut session, &mut state, &mut model,
                        &mut history, &options.initial_directory, CommandContext {
                            tools: &tools,
                            instructions: &instructions,
                            skills: &skills,
                            mcp_statuses: &mcp_statuses,
                            model_options: &model_options,
                        },
                    ).await {
                        Ok(()) => { tabs.close_parked(&id); }
                        Err(error) => state.report_local(TranscriptItem::CommandError(error.to_string())),
                    }
                }

                for id in tabs.prune_closed(&background_runs) { background_runs.remove(&id); }

                state.tab_hover = None;
                dirty = true;
                frames.request(time::Instant::now());
                continue;
            }

            if let Some(id) = state.delete_request.take() {
                if id == session.id() {
                    if let Some(picker) = &mut state.picker {
                        picker.error = Some("无法删除当前正在使用的会话。".into());
                    }
                } else {
                    match session.store.delete_session(&id).await {
                        Ok(_) => {
                            if let Some(picker) = &mut state.picker {
                                picker.remove_session(&id);
                            }
                        }

                        Err(error) => {
                            if let Some(picker) = &mut state.picker {
                                picker.error = Some(format!("删除失败：{error}"));
                            }
                        }
                    }
                }

                dirty = true;
                frames.request(time::Instant::now());
            }

            if let Some(id) = state.resume_request.take() {
                if tabs.remote.contains_key(&id) {
                    tabs.active_remote = Some(id);
                    dirty = true;
                    frames.request(time::Instant::now());
                    continue;
                }
                tabs.active_remote = None;
                if let Some(task) = resume_task.take() { task.abort(); }
                if history_replay.is_none() { state.resume_loading = None; }
                if id != session.id() && tabs.contains(&id) { park_run!(); }
                if id == session.id() {
                    state.picker = None;
                } else if tabs.restore(&id, &mut session, &mut state, &mut model, &mut history) {
                    state.picker = None;
                    session_sync.reset(session.id());
                } else {
                    state.resume_loading = Some("正在读取会话…  Esc 取消".into());

                    if let Some(picker) = &mut state.picker { picker.error = None; }

                    let store = session.store.clone();
                    let project = session.snapshot.session.project_id.clone();
                    let instructions = instructions.clone();
                    let skills = skills.clone();
                    resume_task = Some(tokio::spawn(load_resume(store, project, id, instructions, skills)));
                }

                dirty = true;
                frames.request(time::Instant::now());
                continue;
            }

            select! {
                event = async {
                    if retry_submit {
                        Some(Ok(Event::Key(crossterm::event::KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))))
                    } else {
                        input_events::next(&mut events, &mut pending_event).await
                    }
                } => {
                    retry_submit = false;
                    let Some(event) = event else { break };
                    let mut event = event?;
                    if !wheel.accept(&event, std::time::Instant::now()) { continue; }
                    tabs.sync_ui(&mut state, &session);
                    if let Some(id) = tabs.active_remote.clone() {
                        let tab = tabs.remote.get_mut(&id).unwrap();
                        update_tab_hover(&event, &mut tab.state);
                        let action = tab_target(&event, &tab.state);
                        if let Some(action) = action {
                            match action {
                                TabAction::Select(id) => state.resume_request = Some(id),
                                TabAction::Close(id) => state.close_tab_request = Some(id),
                                TabAction::New => { let next = tab.sibling(None); tabs.add_remote(next); }
                            }
                        } else {
                            use workspace_client::Action as RemoteAction;
                            match tab.handle(event)? {
                                RemoteAction::Quit => break,
                                RemoteAction::Close => state.close_tab_request = Some(id),
                                RemoteAction::Local => state.new_tab_request = true,
                                RemoteAction::Open(spec) => {
                                    let mut spec = *spec;
                                    spec.inherit(&options.config, &options.auth_directory, Some(&state.model));
                                    let next = workspace_client::Tab::connecting(spec, state.media.clone());
                                    tabs.add_remote(next);
                                }
                                RemoteAction::SameTarget(resume) => { let next = tab.sibling(resume); tabs.add_remote(next); }
                                RemoteAction::None | RemoteAction::Redraw => {}
                            }
                        }
                        dirty = true;
                        frames.request(time::Instant::now());
                        continue;
                    }
                    if let Some(preview)=&mut state.media_preview {
                        let (consumed,close)=preview.handle(&event);
                        if close {state.media_preview=None;}
                        if consumed {dirty=true;frames.request(time::Instant::now());continue;}
                    }

                    if state.restore_picker.is_some() && restore::handle(&mut state, &event) {
                        dirty = true;
                        frames.request(time::Instant::now());
                        continue;
                    }

                    if state.btw.visible() {
                        match state.btw.handle(&event)? {
                            transcript_dialog::Action::None => {},
                            transcript_dialog::Action::Close | transcript_dialog::Action::Redraw => dirty = true,
                            transcript_dialog::Action::Link(url) => {
                                if let Err(error) = open_link_in_browser(&url) { state.btw.link_error(&error); }
                                dirty = true;
                            }
                            transcript_dialog::Action::Submit(question) => {
                                if let Err(error) = state.btw.follow_up(&session.store, session.id(), &model, &history, &question, state.max_output_tokens).await { state.btw.submit_error(&error); }
                                dirty = true;
                            }
                        }
                        if dirty { frames.request(time::Instant::now()); }
                        continue;
                    }

                    if let Some(viewer) = &mut state.subagent_dialog {
                        let action = viewer.handle(&event)?;
                        match action {
                            subagents::ViewerAction::None => {},
                            subagents::ViewerAction::Close => { state.subagent_dialog = None; dirty = true; },
                            subagents::ViewerAction::Redraw => dirty = true,
                            subagents::ViewerAction::Link(url) => {
                                if let Err(error) = open_link_in_browser(&url) { viewer.link_error(&error); }
                                dirty = true;
                            }
                            subagents::ViewerAction::Submit(message) => {
                                viewer.send(&tools, message).await;
                                dirty = true;
                            }
                        }
                        if dirty { frames.request(time::Instant::now()); }
                        continue;
                    }

                    if state.info_dialog.is_some() {
                        dialogs::handle(&mut state, event);
                        dirty = true;
                        frames.request(time::Instant::now());
                        continue;
                    }

                    if update_tab_hover(&event, &mut state) {
                        dirty = true;
                        frames.request(time::Instant::now());
                    }

                    if let Some(action) = tab_target(&event, &state)
                    {
                        match action {
                            TabAction::Select(id) => state.resume_request = Some(id),
                            TabAction::New => state.new_tab_request = true,
                            TabAction::Close(id) => state.close_tab_request = Some(id),
                        }

                        dirty = true;
                        frames.request(time::Instant::now());
                        continue;
                    }
                    if resume_task.is_some() || history_replay.is_some() {
                        match &event {
                            Event::Key(key) if key.kind != KeyEventKind::Release && key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) => break,
                            Event::Key(key) if key.kind != KeyEventKind::Release && key.code == KeyCode::Esc && resume_task.is_some() => {
                                if let Some(task) = resume_task.take() { task.abort(); }

                                state.resume_loading = None;
                            }
                            Event::Key(key) if key.code == KeyCode::PageUp => state.scroll_up(10),
                            Event::Key(key) if key.code == KeyCode::PageDown => state.scroll_down(10),
                            Event::Mouse(mouse) if mouse.kind == MouseEventKind::ScrollUp => state.scroll_up(3),
                            Event::Mouse(mouse) if mouse.kind == MouseEventKind::ScrollDown => state.scroll_down(3),
                            Event::Resize(_, _) => {},
                            _ => continue,
                        }

                        dirty = true;
                        frames.request(time::Instant::now());
                        continue;
                    }

                    if handle_input_mouse(&mut state, &event) {
                        dirty = true;
                        frames.request(time::Instant::now());
                        continue;
                    }

                    if let Some(changed) = subagents::handle_parent(&mut state, &event) {
                        if changed {dirty = true;frames.request(time::Instant::now());}
                        if !matches!(&event, Event::Mouse(mouse) if mouse.kind == MouseEventKind::Moved) {continue;}
                    }

                    // Handle the overlay before any obscured transcript hit targets.
                    if !state.dialog_open()
                        && state.approval.is_none()
                        && let Event::Mouse(mouse) = &event
                        && state.menu_area.contains((mouse.column, mouse.row).into())
                        && !state.menu().is_empty()
                    {
                        let previous = state.menu_index;
                        completion::select_with_mouse(&mut state, mouse);
                        let clicked = mouse.kind == MouseEventKind::Down(MouseButton::Left)
                            && state.menu_rows.iter().any(|(area, _)| area.contains((mouse.column, mouse.row).into()));
                        if state.menu_index != previous || mouse.kind != MouseEventKind::Moved {
                            dirty = true;
                            frames.request(time::Instant::now());
                        }
                        if clicked {
                            let candidate = &state.menu()[state.menu_index];
                            let code = if candidate.name.starts_with("/cd ") { KeyCode::Tab } else { KeyCode::Enter };
                            event = Event::Key(crossterm::event::KeyEvent::new(code, KeyModifiers::NONE));
                        } else {
                            continue;
                        }
                    }

                    if handle_list_hover(&mut state, &event) {
                        dirty = true;
                        frames.request(time::Instant::now());
                        continue;
                    }
                    let context_hover_changed = !state.selection.dragging()
                        && matches!(render::handle_context_hover(&mut state, &event), Some(true));
                    let link_hover_changed = !state.selection.dragging()
                        && !approval::file_modal(&state)
                        && !state.dialog_open()
                        && matches!(render::handle_link_hover(&mut state, &event), Some(true));
                    let transcript_hover_changed = !state.selection.dragging()
                        && !approval::file_modal(&state)
                        && !state.dialog_open()
                        && matches!(render::handle_transcript_hover(&mut state, &event), Some(true));

                    if !state.selection.dragging() && !approval::file_modal(&state) && !state.dialog_open()
                        && let Some(changed) = navigation::handle(&mut state, &event) {
                        if changed || context_hover_changed || link_hover_changed || transcript_hover_changed { dirty = true; frames.request(time::Instant::now()); }

                        continue;
                    }
                    if context_hover_changed || link_hover_changed || transcript_hover_changed { dirty = true; frames.request(time::Instant::now()); continue; }
                    // Unused pointer motion must not trigger full-frame redraws.
                    if matches!(&event, Event::Mouse(mouse) if mouse.kind == MouseEventKind::Moved) { continue; }

                    dirty = true;
                    frames.request(time::Instant::now());

                    if state.dialog_open() {
                        if let Some(id) = dialogs::handle(&mut state, event) {
                            state.resume_request = Some(id);
                        }

                        if let Some(name) = state.chosen_model.take()
                            && let Err(error) = select_model(
                                &name,
                                state.chosen_effort.take(),
                                &mut state,
                                &mut model,
                                &model_options,
                                &mut session,
                            ).await
                        {
                            state.report_local(TranscriptItem::Error(error.to_string()));
                        }

                        if let Some(source) = state.pending_marketplace_source.take() {
                            state.report_local(TranscriptItem::Notice("正在下载并检查插件市场…".into()));
                            marketplace_task = Some(tokio::spawn(crate::marketplaces::add_current(source)));
                        }

                        if let Some(change) = state.pending_project_plugin_change.take() {
                            match change {
                                dialogs::ProjectPluginChange::Enable(plugin) => {
                                    let name = plugin.plugin_name.clone();

                                    match session.store.enable_project_plugin(plugin).await {
                                        Ok(_) => state.report_local(TranscriptItem::CommandResult(format!(
                                            "已为当前项目启用 {name}。插件包保存在 Orbit 数据目录；通过校验的 Skill 会在下次启动时按需可用，命令、Hook 与 MCP 不会自动执行。"
                                        ))),
                                        Err(error) => state.report_local(TranscriptItem::CommandError(format!(
                                            "无法启用插件：{error}"
                                        ))),
                                    }
                                }
                                dialogs::ProjectPluginChange::Disable {
                                    project_id,
                                    marketplace_source,
                                    plugin_name,
                                } => match session
                                    .store
                                    .disable_project_plugin(
                                        &project_id,
                                        &marketplace_source,
                                        &plugin_name,
                                    )
                                    .await
                                {
                                    Ok(true) => state.report_local(TranscriptItem::CommandResult(format!(
                                        "已在当前项目停用 {plugin_name}。市场缓存仍保留在 Orbit 数据目录。"
                                    ))),
                                    Ok(false) => state.report_local(TranscriptItem::CommandError(format!(
                                        "插件 {plugin_name} 未处于启用状态。"
                                    ))),
                                    Err(error) => state.report_local(TranscriptItem::CommandError(format!(
                                        "无法停用插件：{error}"
                                    ))),
                                },
                            }
                        }

                        continue;
                    }


                    if let Event::Mouse(mouse) = &event
                        && mouse.kind == crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left)
                        && mouse.modifiers.contains(KeyModifiers::CONTROL) {
                        let offset = if state.follow_bottom {
                            state.max_scroll
                        } else {
                            state.max_scroll.saturating_sub(state.scroll)
                        };

                        if let Some(url) = state
                            .transcript_layout
                            .link_at(state.selection.viewport, offset, (mouse.column, mouse.row))
                            .map(str::to_owned)
                        {
                            state.selection.clear();

                            if markdown::footnotes::is_target(&url) {
                                jump_footnote_at(&mut state, (mouse.column, mouse.row));
                            } else if let Some(id) = subagent_link_id(&url) {
                                subagents::open(&mut state, id.to_owned());
                            } else if let Err(error) = open_link_in_browser(&url) {
                                state.report_local(TranscriptItem::Error(format!(
                                    "无法打开链接：{error}"
                                )));
                            }

                            continue;
                        }
                    }

                    if let Event::Mouse(mouse) = &event
                        && mouse.kind == crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left)
                        && focus_attachment_at(&mut state, mouse.column, mouse.row) {
                        state.selection.clear();
                        continue;
                    }
                    if let Some((x, y)) = state.selection.click(&event)
                        && (jump_footnote_at(&mut state, (x, y)) || open_subagent_at(&mut state, (x,y)) || toggle_transcript_at(&mut state, (x, y))) {
                        continue;
                    }

                    if pending.is_some() {
                        match approval::handle(&mut state, event.clone()) {
                            approval::Action::Approve => {
                                if let Some(request) = pending.take() {
                                    let _ = request.decision.send(true);
                                }

                                state.approval = None;
                                state.resume_clock();
                                continue;
                            }
                            approval::Action::AlwaysAllow => {
                                if let Some(request) = pending.take() {
                                    match &request.preview {
                                        orbit_tools::approval::ApprovalPreview::Write(p) => {
                                            if let Some(key) = orbit_tools::approval::file_grant_key(&p.path, std::path::Path::new(&session.snapshot.session.workspace)) {
                                                session.store.grant_tui_file(session.id(), &key).await?;
                                                state.session_allowed_files.insert(key);
                                            }
                                        }
                                        orbit_tools::approval::ApprovalPreview::Delete(p) => {
                                            if let Some(key) = orbit_tools::approval::file_grant_key(&p.path, std::path::Path::new(&session.snapshot.session.workspace)) {
                                                session.store.grant_tui_file(session.id(), &key).await?;
                                                state.session_allowed_files.insert(key);
                                            }
                                        }
                                        orbit_tools::approval::ApprovalPreview::Command(cmd) => {
                                            if let Some(git) = &cmd.git {
                                                match git.kind {
                                                    orbit_runtime::git::GitOperationKind::LocalWrite { .. }
                                                    | orbit_runtime::git::GitOperationKind::RemoteWrite { .. }
                                                    | orbit_runtime::git::GitOperationKind::ReadOnlyQuery { .. } => {
                                                        if let Some(key) = cmd.git_grant_key(std::path::Path::new(&session.snapshot.session.workspace)) { state.session_git_commands.insert(key); }
                                                    }
                                                    _ => {}
                                                }
                                            } else {
                                                state.session_commands.insert(cmd.request_grant_key(std::path::Path::new(&session.snapshot.session.workspace)));
                                            }
                                        }
                                        _ => {}
                                    }
                                    let _ = request.decision.send(true);
                                }

                                state.approval = None;
                                state.resume_clock();
                                continue;
                            }
                            approval::Action::Deny => {
                                if let Some(request) = pending.take() {
                                    let _ = request.decision.send(false);
                                }

                                state.approval = None;
                                state.resume_clock();
                                continue;
                            }
                            approval::Action::Cancel => {
                                if let Some(request) = pending.take() {
                                    let _ = request.decision.send(false);
                                }

                                state.approval = None;
                                state.resume_clock();
                                state.cancel();

                                if let Some(token) = &cancellation {
                                    token.cancel();
                                }

                                if let Some(token) = &local_cancellation {
                                    token.cancel();
                                }
                                continue;
                            }
                            approval::Action::None => {}
                        }
                    }

                    if state.selection.handle(&event)? {
                        state.cursor_motion.clear();
                        if state.selection.transcript_drag() {
                            state.follow_bottom = false;
                        }

                        terminal::draw_synchronized(&mut terminal, |frame| {
                            if !state.selection.draw_frozen(frame) {
                                render::draw(frame, &mut state);
                                state.selection.paint(frame);
                            }
                            if let Some(position) = state.ime_cursor { frame.set_cursor_position(position); }
                        })?;
                        dirty = false;
                        frames.drawn(time::Instant::now());
                        continue;
                    }
                    if matches!(&event, Event::Mouse(mouse) if matches!(mouse.kind, MouseEventKind::ScrollUp | MouseEventKind::ScrollDown)) {
                        if state.selection.dragging() {
                            keyboard_action(
                                &mut state,
                                event,
                                task.is_some() || local_task.is_some(),
                            );
                        } else if pending.is_some() {
                            approval::handle(&mut state, event);
                        } else {
                            keyboard_action(
                                &mut state,
                                event,
                                task.is_some() || local_task.is_some(),
                            );
                        }

                        continue;
                    }
                    if matches!(&event, Event::Key(key) if key.code == KeyCode::BackTab && key.kind != KeyEventKind::Release) {
                        session_sync::sync_mode(&session.store, session.id(), &mut state).await?;
                        let mode = state.permission_mode.next();
                        session.store.set_tui_mode(session.id(), mode.label()).await?;
                        state.permission_mode = mode;
                        continue;
                    }

                    match keyboard_action(&mut state, event, task.is_some() || local_task.is_some()) {
                        Action::Quit => break,
                        Action::Redraw => terminal.clear()?,

                        Action::Cancel => {
                            state.cancel();

                            if let Some(token) = &cancellation { token.cancel(); }

                            if let Some(token) = &local_cancellation { token.cancel(); }
                        }

                        Action::Steer(guidance) => {
                            if local_task.is_some() {
                                state.report_local(TranscriptItem::CommandError(
                                    "正在等待回退操作完成或取消。".into(),
                                ));
                                continue;
                            }

                            pending_guidance.push(guidance);
                            state.cancel();

                            if let Some(token) = &cancellation { token.cancel(); }
                        }

                        Action::AttachPaths(paths) => {
                            if let Err(error) = attach_paths(&mut state, &session, paths) {
                                state.report_local(TranscriptItem::Error(error.to_string()));
                            }
                        }
                        Action::CopyInput(text) => selection::copy_text(&text)?,
                        Action::PasteClipboard => {
                            if let Err(error) = paste_clipboard(&mut state, &session).await {
                                state.report_local(TranscriptItem::Error(error.to_string()));
                            }
                        }

                        Action::Submit(prompt) => {
                            if crate::tui::connection_spec::Spec::is_command(&prompt) {
                                match crate::tui::connection_spec::Spec::parse(&prompt) {
                                    Ok(mut spec) => {
                                        spec.inherit(&options.config, &options.auth_directory, Some(&state.model));
                                        tabs.add_remote(workspace_client::Tab::connecting(spec, state.media.clone()));
                                    }
                                    Err(error) => { state.input.set(prompt); state.report_local(TranscriptItem::CommandError(error.to_string())); }
                                }
                                dirty = true;
                                frames.request(time::Instant::now());
                                continue;
                            }
                            if let Some(question) = btw::question(&prompt) {
                                state.selection.clear();
                                state.input_dragging = false;
                                state.cursor_motion.clear();
                                if let Err(error) = state.btw.ask(&session.store, session.id(), &model, &history, question, state.max_output_tokens).await {
                                    state.info_dialog = Some(dialogs::InfoDialog::new("旁路提问", error.to_string()));
                                }
                                continue;
                            }

                            if matches!(prompt.trim(), "/new" | "/local") {
                                state.new_tab_request = true;
                                continue;
                            }
                            if prompt.trim() == "/close" {
                                state.close_tab_request = Some(session.id().to_owned());
                                continue;
                            }
                            if prompt.trim() == "/restore" {
                                match restore::RestorePicker::load(&session.store, tabs.restore_snapshot()).await {
                                    Ok(picker) => { state.restore_picker = Some(picker); state.input.set("/restore ".into()); }
                                    Err(error) => state.report_local(TranscriptItem::CommandError(error.to_string())),
                                }
                                continue;
                            }
                            if local_task.is_some() {
                                state.report_local(TranscriptItem::CommandError(
                                    "正在等待回退操作完成或取消。".into(),
                                ));
                                continue;
                            }

                            let prompt = if prompt.trim_start().starts_with('/') { prompt.trim().to_owned() } else { prompt };
                            let compacting = prompt == "/compact";
                            if compacting && state.is_home() {
                                continue;
                            }

                            if prompt.starts_with('/') {
                                let command = prompt
                                    .split_once(char::is_whitespace)
                                    .map_or(prompt.as_str(), |(command, _)| command);

                                if command == "/cd" {
                                    if task.is_some() || marketplace_task.is_some() || tabs.has_drafts() || !state.attachments.is_empty() {
                                        state.report_local(TranscriptItem::CommandError("请先结束运行，并处理各标签中未发送的草稿和附件。".into()));
                                        continue;
                                    }

                                    let argument = prompt.split_once(char::is_whitespace).map_or("", |(_, text)| text.trim());
                                    match directory::resolve(argument, std::path::Path::new(&state.workspace), options.home.as_deref()) {
                                        Ok(path) if path == std::path::Path::new(&state.workspace) => {}
                                        Ok(path) => match crate::prepare_workspace_resources(options, &path, &session).await {
                                            Ok(prepared) => {
                                                match session.move_to(prepared.workspace.clone(), prepared.command_limits.clone()).await {
                                                    Ok(()) => {
                                                        state.workspace = session.snapshot.session.workspace.clone();
                                                        workspace::bind(&prepared, &mut session, &mut state, &mut model, &mut history).await?;
                                                        tools = prepared.tools;
                                                        auto_compact = prepared.budget.auto_compact;
                                                        max_turns = prepared.budget.max_turns;
                                                        max_tool_calls = prepared.budget.max_tool_calls;
                                                        compaction = prepared.budget.compaction;
                                                        instructions = prepared.budget.instructions;
                                                        skills = prepared.budget.skills;
                                                        undo_writer = prepared.undo_writer;
                                                        approvals = prepared.approvals;
                                                        mcp_statuses = prepared.budget.mcp_statuses;
                                                        model_options = prepared.budget.model_options;
                                                        memory_scope = prepared.budget.memory_scope;
                                                        refresh_context_usage(&mut state, &history, &tools);
                                                        dirty = true;
                                                        frames.request(time::Instant::now());
                                                    }
                                                    Err(error) => state.report_local(TranscriptItem::CommandError(format!("无法移动会话：{error:#}"))),
                                                }
                                            }
                                            Err(error) => state.report_local(TranscriptItem::CommandError(format!("无法切换目录：{error:#}"))),
                                        },
                                        Err(error) => state.report_local(TranscriptItem::CommandError(format!("{error:#}"))),
                                    }
                                    continue;
                                }
                                if prompt.trim() == "/model refresh" {
                                    if model_refresh.is_none() {
                                        model_refresh = Some(model_catalog::refresh(options.config.clone(), options.auth_directory.clone(), true));
                                    }
                                    continue;
                                }
                                if command == "/connect" {
                                    if task.is_some() || marketplace_task.is_some() {
                                        state.report_local(TranscriptItem::CommandError("请等待当前操作结束后再连接账号。".into()));
                                        continue;
                                    }

                                    match connect::run(&mut terminal, &mut events, options).await {
                                        Ok(Some(models)) => {
                                            model_catalog::publish(models, &mut state, &mut model_options, &mut background_runs, &mut tabs);
                                            let current = model_options.iter().find(|choice| choice.qualified_name() == state.model)
                                                .or_else(|| model_options.first()).map(ModelOption::qualified_name);
                                            if let Some(current) = current {
                                                select_model(&current, None, &mut state, &mut model, &model_options, &mut session).await?;
                                            }
                                            state.model_picker = Some(dialogs::ModelPicker::new(model_options.clone(), state.model.clone(), state.reasoning_effort));
                                        }
                                        Ok(None) => {}
                                        Err(error) => state.report_local(TranscriptItem::CommandError(format!("连接账号失败：{error:#}"))),
                                    }
                                    state.menu_dismissed = true;
                                    state.selection.clear();
                                    continue;
                                }
                                if command == "/editor" {
                                    if task.is_some() {
                                        state.report_local(TranscriptItem::CommandError("请等待当前响应结束后再打开编辑器。".into()));
                                        continue;
                                    }

                                    let draft = prompt.split_once(char::is_whitespace).map_or("", |(_, text)| text).to_owned();
                                    state.input.set(draft.clone());
                                    drop(events);
                                    terminal::suspend();
                                    let edited = editor.edit(&draft, std::path::Path::new(&state.workspace)).await;
                                    state.media.invalidate();
                                    terminal::resume(&mut terminal)?;
                                    events = EventStream::new();

                                    match edited {
                                        Ok(text) => state.restore_editor_draft(text),
                                        Err(error) => state.report_local(TranscriptItem::CommandError(format!("{error:#}"))),
                                    }
                                    state.selection.clear();
                                    state.menu_dismissed = true;
                                    continue;
                                }
                                if command == "/undo" {
                                    if task.is_some() {
                                        state.report_local(TranscriptItem::CommandError(
                                            "Agent 运行中不能回退文件；请等待当前响应结束。".into(),
                                        ));
                                        continue;
                                    }

                                    let argument = prompt
                                        .split_once(char::is_whitespace)
                                        .map_or("", |(_, argument)| argument.trim());
                                    let changes = session.store.workspace_changes(session.id()).await?;
                                    let change = changes
                                        .iter()
                                        .rev()
                                        .find(|record| {
                                            !record.undone
                                                && (argument.is_empty()
                                                    || record.change.path == argument)
                                        })
                                        .cloned();
                                    let Some(change) = change else {
                                        state.report_local(TranscriptItem::CommandError(if argument.is_empty() {
                                            "当前会话没有可回退的 Orbit 文件写入。".into()
                                        } else {
                                            format!("当前会话没有 {argument} 的 Orbit 写入检查点。")
                                        }));
                                        continue;
                                    };

                                    let token = CancellationToken::new();
                                    let writer = undo_writer.clone();
                                    let task_token = token.clone();
                                    let store = session.store.clone();
                                    let session_id = session.id().to_owned();
                                    local_task = Some(tokio::spawn(async move {
                                        let result = writer.restore(change.change.clone(), task_token).await?;
                                        store
                                            .record_workspace_undo(&session_id, &change)
                                            .await
                                            .map_err(|error| orbit_agent::tool::ToolError::ExecutionFailed(format!(
                                                "undo completed but its audit record could not be saved: {error}"
                                            )))?;

                                        Ok(result)
                                    }));
                                    local_cancellation = Some(token);
                                    continue;
                                }
                                if compacting && task.is_some() {
                                    state.report_local(TranscriptItem::CommandError(
                                        "运行中不能压缩上下文；请先按 Ctrl+C 取消当前响应。".into(),
                                    ));
                                    continue;
                                }
                                if !compacting {
                                    if task.is_some()
                                        && !commands::available_while_running(command)
                                    {
                                        state.report_local(TranscriptItem::CommandError(
                                            "此命令需要当前响应结束；可先切换标签，或按 Ctrl+C 取消响应。".into(),
                                        ));
                                        continue;
                                    }

                                    let context = CommandContext {
                                        tools: &tools,
                                        instructions: &instructions,
                                        skills: &skills,
                                        mcp_statuses: &mcp_statuses,
                                        model_options: &model_options,
                                    };
                                    match run_command(
                                        &prompt, &mut state, &mut model, &mut history,
                                        &mut session, context,
                                    ).await {
                                        Ok(true) => break,
                                        Ok(false) => refresh_context_usage(&mut state, &history, &tools),
                                        Err(error) => state.report_local(TranscriptItem::Error(error.to_string())),
                                    }

                                    continue;
                                }
                            }

                            let attachments = if compacting {
                                Vec::new()
                            } else {
                                state.attachment_cursor = None;
                                std::mem::take(&mut state.attachments)
                            };

                            active_context = (!compacting).then(|| {
                                ChatMessage::user_with_attachments(prompt.clone(), attachments.clone())
                            });
                            let active_context_tokens = state.context_tokens;
                            let retry_prompt = prompt.clone();
                            let retry_attachments = attachments.clone();

                            let started = start_agent_run(
                                prompt,
                                attachments,
                                compacting,
                                &mut state,
                                &mut session,
                                &history,
                                &model,
                                &tools,
                                active_context_tokens,
                                auto_compact,
                                max_turns,
                                max_tool_calls,
                                &compaction,
                            )
                            .await;
                            let started = match started {
                                Ok(started) => started,
                                Err(error) => {
                                    state.input.set(retry_prompt);
                                    state.attachments = retry_attachments;
                                    active_context = None;
                                    if error.is::<crate::session::SessionRefreshRequired>() {
                                        retry_submit = true;
                                    } else {
                                        state.report_local(TranscriptItem::CommandError(error.to_string()));
                                    }
                                    continue;
                                }
                            };
                            receiver = Some(started.receiver);
                            cancellation = Some(started.cancellation);
                            task = Some(started.task);
                        }

                        Action::None => {}
                    }
                }
                Some(request) = approvals.recv() => {
                    if request.decision.is_closed() { continue; }
                    session_sync::sync_mode(&session.store, session.id(), &mut state).await?;
                    queue_approval(request, &mut state, &session, &mut pending);
                    dirty = true;
                }
                event = async { receiver.as_mut().unwrap().recv().await }, if receiver.is_some() => {
                    dirty = true;

                    match event {
                        Some(event) => { apply_live_event(event, &mut state); },
                        None => receiver = None,
                    }
                }
                _ = command_ticks.tick(), if state.status == app::RunStatus::Running => {
                    for (index,item) in state.transcript.iter_mut().enumerate() {
                        let TranscriptItem::Tool(row)=item else {continue;};
                        if !matches!(row.name.as_str(),"spawn_agent"|"send_agent_message") || row.status!=app::ToolStatus::Running {continue;}
                        let Ok(mut value)=serde_json::from_str::<serde_json::Value>(&row.output) else {continue;};
                        let Some(id)=value["job_id"].as_str() else {continue;};
                        let snapshot=tools.task_groups().skip(1).find_map(|jobs|jobs.snapshots(&[id.to_owned()],false).ok()).and_then(|mut jobs|jobs.pop());
                        if let Some(snapshot)=snapshot {
                            let seconds=snapshot.elapsed_ms/1000;
                            if value["elapsed_seconds"].as_u64()!=Some(seconds) {
                                value["elapsed_seconds"]=seconds.into();row.output=value.to_string();
                                state.transcript_layout.invalidate_from(index);dirty=true;frames.request(time::Instant::now());
                            }
                        }
                    }
                    if let Ok(dir) = session.command_output.read() {
                        for (index, item) in state.transcript.iter_mut().enumerate() {
                            let TranscriptItem::Tool(row) = item else { continue; };
                            if row.name != "run_command" || row.status != app::ToolStatus::Running { continue; }
                            let mut value = serde_json::from_str::<serde_json::Value>(&row.output)
                                .ok().filter(serde_json::Value::is_object).unwrap_or_else(|| serde_json::json!({}));
                            let Some(output_id) = tools.background_jobs().output_id_for_call(&row.id) else { continue; };
                            if let Some(stdout) = read_command_stdout(&dir, &output_id)
                                && !stdout.is_empty() && value["stdout"].as_str() != Some(&stdout) {
                                value["stdout"] = stdout.into();
                                value["running"] = true.into();
                                row.output = value.to_string();
                                state.transcript_layout.invalidate_from(index);
                                dirty = true;
                                frames.request(time::Instant::now());
                            }
                        }
                    }
                }
                result = async { task.as_mut().unwrap().await }, if task.is_some() => {
                    dirty = true;
                    task = None;
                    pending = None;

                    if state.approval.take().is_some() { state.selection = Default::default(); }

                    // Drain the completed run before accepting another prompt or clearing history.
                    if let Some(rx) = receiver.as_mut() {
                        while let Ok(event) = rx.try_recv() { apply_live_event(event, &mut state); }
                    }

                    let (status, final_event, completed_history) = background::outcome(result, cancellation.as_ref());

                    let finished_run = session.active_run_id();
                    session.finish(status, completed_history.clone()).await?;
                    session_sync.reset(session.id());

                    if completed_history.is_some() || session.snapshot.recovery.is_some() {
                        history = session.snapshot.history.clone();
                        active_context = None;
                        crate::instructions::inject(&mut history, &instructions);
                        skills.inject(&mut history);
                        crate::project_state::inject(
                            &mut history,
                            &session.store,
                            &session.snapshot.session.project_id,
                            session.id(),
                        )
                        .await?;
                    }

                    state.apply(final_event);
                    refresh_context_usage(&mut state, &history, &tools);
                    session_sync::publish(&session, &state, finished_run).await?;
                    task = None;
                    receiver = None;
                    cancellation = None;

                    if !pending_guidance.is_empty() {
                        let guidance = pending_guidance.join("\n\n");
                        pending_guidance.clear();
                        let steering_history = steering_history(&history, active_context.take());
                        active_context = Some(ChatMessage::user(guidance.clone()));

                        let context_tokens = state.context_tokens;
                        let started = start_agent_run(
                            guidance.clone(),
                            Vec::new(),
                            false,
                            &mut state,
                            &mut session,
                            &steering_history,
                            &model,
                            &tools,
                            context_tokens,
                            auto_compact,
                            max_turns,
                            max_tool_calls,
                            &compaction,
                        )
                        .await;
                        let started = match started {
                            Ok(started) => started,
                            Err(error) => {
                                state.input.set(guidance);
                                active_context = None;
                                if error.is::<crate::session::SessionRefreshRequired>() {
                                    retry_submit = true;
                                } else {
                                    state.report_local(TranscriptItem::CommandError(error.to_string()));
                                }
                                continue;
                            }
                        };
                        receiver = Some(started.receiver);
                        cancellation = Some(started.cancellation);
                        task = Some(started.task);
                    } else {
                        active_context = None;
                    }
                }
                result = async { resume_task.as_mut().unwrap().await }, if resume_task.is_some() => {
                    resume_task = None;
                    state.resume_loading = None;

                    let result = match result {
                        Ok(Ok(next)) => {
                            park_run!();
                            let empty_tab = tabs.empty_tab(&session, &state);
                            activate_resume(options, &mut tabs, &mut session, &mut state, &mut model, &mut history, next)
                                .await
                                .inspect(|_| {
                                    if let Some(id) = empty_tab { tabs.close_parked(&id); }
                                })
                        }
                        Ok(Err(error)) => Err(error),
                        Err(error) => Err(error.into()),
                    };

                    match result {
                        Ok(prep) => {
                            bound_session = session.id().to_owned();
                            tools = prep.tools;
                            instructions = prep.budget.instructions;
                            skills = prep.budget.skills;
                            undo_writer = prep.undo_writer;
                            approvals = prep.approvals;
                            mcp_statuses = prep.budget.mcp_statuses;
                            model_options = prep.budget.model_options;
                            memory_scope = prep.budget.memory_scope;
                            auto_compact = prep.budget.auto_compact;
                            max_turns = prep.budget.max_turns;
                            max_tool_calls = prep.budget.max_tool_calls;
                            compaction = prep.budget.compaction;
                            restore_model_profile(&mut state, &mut model, &model_options, &session);
                            history_replay = Some(crate::tui::replay::HistoryReplay::default());
                        }
                        Err(error) => {
                            let message = format!("恢复会话失败：{error:#}");

                            if let Some(picker) = &mut state.picker {
                                picker.error = Some(message);
                            } else {
                                restore_errors.push(message);
                            }
                        }
                    }

                    dirty = true;
                    frames.request(time::Instant::now());
                }
                result = async { local_task.as_mut().unwrap().await }, if local_task.is_some() => {
                    dirty = true;
                    local_task = None;
                    local_cancellation = None;

                    state.report_local(background::local_result(result));
                }
                result = async { marketplace_task.as_mut().unwrap().await }, if marketplace_task.is_some() => {
                    dirty = true;
                    marketplace_task = None;

                    state.report_local(background::marketplace_result(result));
                }
                _ = time::sleep_until(frames.deadline()), if frames.pending() => {
                    tabs.sync_ui(&mut state, &session);
                    let visible = tabs.visible(&mut state);
                    terminal::draw_synchronized(&mut terminal, |frame| {
                        render::draw(frame, visible);
                        visible.selection.paint(frame);
                        // Frozen copy selections must not retain transient cursor effects.
                        visible.cursor_motion.paint(frame.buffer_mut(), std::time::Instant::now());
                    })?;
                    let now = time::Instant::now();
                    frames.drawn(now);
                    if visible.cursor_motion.active(std::time::Instant::now()) {
                        frames.request(now + Duration::from_millis(16));
                    }
                    dirty = false;
                }
                _ = ticks.tick() => {
                    dirty |= tabs.poll_remote().await;
                    dirty |= state.media.changed();
                    if let Some(preview) = &state.media_preview { dirty |= preview.tick(); }
                    if let Some(result) = catalog_watcher.poll().await {
                        match result {
                            Ok((models, diagnostics)) => {
                                model_catalog::publish(models, &mut state, &mut model_options, &mut background_runs, &mut tabs);
                                for diagnostic in diagnostics { state.report_local(TranscriptItem::Notice(diagnostic)); }
                            }
                            Err(error) => state.report_local(TranscriptItem::CommandError(format!("模型配置更新失败，保留现有配置：{error}"))),
                        }
                        dirty = true;
                    }
                    let (background_changed, catalog_refreshed) = background::poll(&mut background_runs, &mut tabs, &mut session_sync, &mut model_options).await?;
                    if catalog_refreshed {
                        catalog_watcher.resynchronize();
                    }
                    if background_changed {
                        state.model_completions = model_options.iter().map(ModelOption::qualified_name).collect();
                        dirty = true;
                    }
                    if state.thinking_hover_started.is_some_and(|started| started.elapsed() < Duration::from_millis(160)) { dirty = true; }
                    if branch_task.as_ref().is_some_and(|task| task.is_finished())
                        && let Ok((workspace, branch)) = branch_task.take().unwrap().await
                        && workspace == state.workspace
                    {
                        dirty |= state.branch_workspace != workspace || state.branch != branch;
                        state.branch_workspace = workspace;
                        state.branch = branch;
                    }
                    if branch_task.is_none() && (branch_workspace != state.workspace || branch_checked.elapsed() >= Duration::from_secs(2)) {
                        branch_workspace = state.workspace.clone();
                        branch_checked = std::time::Instant::now();
                        let workspace = branch_workspace.clone();
                        branch_task = Some(tokio::spawn(async move {
                            let branch = project_label::branch(&workspace).await;
                            (workspace, branch)
                        }));
                    }
                    if sync_checked.elapsed() >= Duration::from_millis(200) {
                        sync_checked = std::time::Instant::now();
                        tabs.persist(&session).await?;
                        dirty |= session_sync::sync_mode(&session.store, session.id(), &mut state).await?;
                        if task.is_some() {
                            session.flush_events().await?;
                            session_sync::publish(&session, &state, session.active_run_id()).await?;
                            session_sync.reset(session.id());
                        } else if local_task.is_none() && resume_task.is_none() && history_replay.is_none()
                            && session_sync.poll(&session.store, &mut session.snapshot, &mut state, &mut history).await? {
                            let previous_profile = (state.model.clone(), state.reasoning_effort);
                            restore_model_profile(&mut state, &mut model, &model_options, &session);
                            if previous_profile != (state.model.clone(), state.reasoning_effort) {
                                refresh_context_usage(&mut state, &history, &tools);
                            }
                            dirty = true;
                        }
                        if task.is_none() && local_task.is_none() && history_replay.is_none() {
                            dirty |= session_sync::sync_runtime(&session.store, session.id(), &mut state).await?;
                        }
                        dirty |= tabs.synchronize_parked(&mut session_sync, &background_runs).await?;
                        if let Some(viewer) = &mut state.subagent_dialog {
                            dirty |= viewer.poll(&session.store, session.id()).await;
                        }
                        if history_replay.is_none() {
                            dirty |= subagent_progress.poll(&session.store, &mut state).await?;
                        }
                    }
                    dirty |= session.poll_title().await.unwrap_or(false);
                    dirty |= tabs.poll_titles().await;
                    dirty |= state.navigation.advance_preview();
                    dirty |= state.expire_footnote_focus();
                    if let Some(picker) = &mut state.picker {
                        dirty |= picker.advance_title_scroll();
                    }
                    if let Some(picker) = &mut state.restore_picker {
                        dirty |= picker.advance_title_scroll();
                    }

                    dirty |= advance_tab_hover(&mut state);

                    if let Some(replay) = &mut history_replay {
                        let done = replay.advance(&session.snapshot, &mut state, 2048, Duration::from_millis(4));

                        if done {
                            history_replay = None;
                            state.resume_loading = None;
                            session_sync::sync_mode(&session.store, session.id(), &mut state).await?;
                            if let Some(notice) = move_notice.take() {
                                state.report_local(TranscriptItem::CommandResult(notice));
                            }
                            refresh_context_usage(&mut state, &history, &tools);
                            session_sync::sync_runtime(&session.store, session.id(), &mut state).await?;
                        } else {
                            state.resume_loading = Some(format!("正在显示会话历史…  {}/{}", replay.completed_runs(), session.snapshot.runs.len()));
                        }

                        dirty = true;
                    }

                    let direction = state.selection.auto_scroll(std::time::Instant::now());
                    if let Some(viewer) = &mut state.subagent_dialog { dirty |= viewer.tick(); }
                    dirty |= state.btw.tick();
                    dirty |= tabs.poll_side_questions();

                    if direction != 0 {
                        if direction < 0 { state.scroll_up(2); } else { state.scroll_down(2); }

                        dirty = true;
                    }

                    if dirty || state.running() { frames.request(time::Instant::now()); }
                }
            }
        }

        Ok(())
    }.await;

    if let Some(task) = resume_task {
        task.abort();
    }

    drop(pending);

    if let Some(token) = cancellation {
        token.cancel();
    }

    if let Some(token) = local_cancellation {
        token.cancel();
    }

    if let Some(mut task) = task
        && time::timeout(Duration::from_millis(500), &mut task)
            .await
            .is_err()
    {
        task.abort();
        let _ = task.await;
    }

    if let Some(mut task) = local_task
        && time::timeout(Duration::from_millis(500), &mut task)
            .await
            .is_err()
    {
        task.abort();
        let _ = task.await;
    }

    if let Some(mut task) = marketplace_task
        && time::timeout(Duration::from_millis(500), &mut task)
            .await
            .is_err()
    {
        task.abort();
        let _ = task.await;
    }

    if let Some(rx) = receiver.as_mut() {
        while rx.try_recv().is_ok() {}
    }

    background::shutdown(&mut background_runs, &mut tabs).await?;
    tabs.close(&session).await?;
    let saved = session.close().await;

    result.and(saved)?;

    Ok(next_workspace.map(|resources| crate::PreparedWorkspace { session, resources }))
}
