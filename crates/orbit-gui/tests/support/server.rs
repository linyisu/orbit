//! Offline protocol peer for integration and native-window checks.
//! Enabled explicitly with `test-support`; never calls a model.
mod disclosures;
mod files;
mod input_queue;
mod review;
mod side_questions;
mod subagents;
mod tool_output;
mod user_input_history;
use base64::Engine;
use files::{execute as fixture_files, read_file as fixture_read_file};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    io::{Read, Write},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct SessionCatalog {
    titles: HashMap<String, String>,
    deleted: std::collections::HashSet<String>,
}

fn catalog_response(mut response: Value, catalog: &SessionCatalog) -> Value {
    if let Some(sessions) = response["value"].as_array_mut() {
        sessions.retain(|session| {
            !catalog
                .deleted
                .contains(session["id"].as_str().unwrap_or_default())
        });
        for session in sessions {
            if let Some(title) = catalog
                .titles
                .get(session["id"].as_str().unwrap_or_default())
            {
                session["title"] = title.clone().into();
            }
        }
    }
    response
}

fn main() -> anyhow::Result<()> {
    if std::env::args().any(|argument| argument == "--help") {
        return Ok(());
    }
    let mut output = std::io::stdout().lock();
    let mut input = std::io::stdin().lock();
    output.write_all(orbit_protocol::PREAMBLE)?;
    output.flush()?;
    let mut model = "fixture::local".to_string();
    let mut session = "demo".to_string();
    let mut workspace = "/tmp/orbit-gui-fixture".to_owned();
    let mut relocation = None;
    let mut linked = false;
    let mut history: HashMap<String, Vec<Value>> = HashMap::from([(
        "demo".into(),
        vec![
            json!({"Started":{"prompt":"检查这个项目的结构，给我一个清晰的改进方案。"}}),
            json!({"Agent":{"Model":{"TextDelta":"我看过了项目的入口与模块关系。当前结构已经清晰，接下来可以先把会话状态和界面展示分开。\n\n### 建议从这三处开始\n\n1. **收拢会话状态**，让切换和恢复走同一条路径。\n2. **保留输入草稿**，发送失败后可以继续编辑。\n3. **统一消息呈现**，工具执行记录按需展开，正文保持连续。\n\n```rust\nlet session = client.open(workspace).await?;\nlet messages = session.subscribe();\n```\n\n这些改动可以逐步完成，已有的模型连接与工具执行继续复用。"}}}),
            json!({"Finished":{"status":"completed"}}),
        ],
    )]);
    let mut fixture_skills: HashMap<String, String> = HashMap::new();
    let mut fixture_memories: Vec<Value> = vec![];
    let mut pending: VecDeque<Value> = VecDeque::new();
    let mut approvals: Vec<Value> = vec![];
    let mut reject = true;
    let mut count = 0;
    let mut mode = "auto".to_string();
    let mut planning = json!({"generation":0,"mode":"build","plan":null});
    let mut user_inputs = user_input_history::FixtureUserInputs::default();
    let mut side_questions: HashMap<String, side_questions::FixtureQuestions> = HashMap::new();
    let mut child_agents = subagents::FixtureSubagents::default();
    loop {
        let mut header = [0; 4];
        if input.read_exact(&mut header).is_err() {
            break;
        }
        let size = u32::from_be_bytes(header) as usize;
        anyhow::ensure!(
            size <= orbit_protocol::MAX_FRAME_BYTES,
            "fixture frame too large"
        );
        let mut bytes = vec![0; size];
        input.read_exact(&mut bytes)?;
        let request: Value = serde_json::from_slice(&bytes)?;
        if request["method"] == "hello"
            && let Some(path) = request["workspace"].as_str()
        {
            workspace = path.to_owned();
            relocation =
                std::fs::read_to_string(std::path::Path::new(path).join(".fixture-relocation"))
                    .ok();
            linked = relocation.as_deref() == Some("linked");
        }
        let recovery = std::path::Path::new(&workspace)
            .join(".fixture-recovery")
            .exists();
        if relocation.is_some() || recovery {
            let mut log = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(std::path::Path::new(&workspace).join(".fixture-requests"))?;
            let mut line = serde_json::to_vec(&request)?;
            line.push(b'\n');
            log.write_all(&line)?;
        }
        if recovery
            && request["method"] == "poll"
            && std::fs::remove_file(std::path::Path::new(&workspace).join(".fixture-disconnect"))
                .is_ok()
        {
            // Terminate the transport once; the next peer still exposes demo's
            // history, as the real workspace service does across reconnects.
            return Ok(());
        }
        if recovery
            && request["method"] == "hello"
            && std::path::Path::new(&workspace)
                .join(".fixture-offline")
                .exists()
        {
            return Ok(());
        }
        let tab_layout = std::path::Path::new(&workspace)
            .join(".fixture-tabs")
            .exists();
        let catalog_path = std::path::Path::new(&workspace).join(".fixture-sessions.json");
        let mut catalog: SessionCatalog = std::fs::read(&catalog_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        let title = |session: &str| {
            if let Some(title) = catalog.titles.get(session) {
                return title.clone();
            }
            (if tab_layout {
                match session {
                    "demo" => "请检查当前项目结构，读取 Cargo.toml 和完整的依赖关系并给出建议",
                    "navigation" => {
                        "请直接使用 read_file 读取 src/main.rs，然后运行 pwd，继续检查项目"
                    }
                    _ => "",
                }
            } else if session == "demo" {
                "梳理项目结构"
            } else {
                "新会话"
            })
            .to_owned()
        };
        let profile = |session: &str, model: &str, mode: &str| json!({"id":session,"persisted":session=="demo","title":title(session),"workspace":workspace,"model":model,"models":if session == "models" { vec!["acm::shared", "acm::qwen", "antigravity-account::claude-opus", "antigravity-account::gemini-flash", "chatgpt-account::astra", "deepseek-account::shared", "deepseek-account::deepseek-flash"] } else { vec!["fixture::local","fixture::other","fixture::gemini-flash","fixture::claude-opus"] },"mode":mode,"supports_vision":true,"reasoning_effort":null,"reasoning_efforts":[],"context_tokens":128000,"input_context_tokens":120000,"max_output_tokens":8000});
        let response = match request["method"].as_str().unwrap_or_default() {
            "complete" => {
                let input = request["input"].as_str().unwrap_or_default();
                if input == "/cd denied" {
                    json!({"type":"error","value":"目录不可访问"})
                } else {
                    let entries = match input {
                        "/cd " => vec![
                            ("/cd src/", "src/", "目录", true),
                            ("/cd 'project space/'", "project space/", "目录", true),
                        ],
                        "/cd src/" => vec![("/cd src/nested/", "nested/", "目录", true)],
                        "/diff " | "/diff src" => {
                            vec![("/diff src/main.rs", "src/main.rs", "会话文件变更", false)]
                        }
                        "/tasks done " => vec![("/tasks done 42", "42", "实现参数补全", false)],
                        "/resume " => vec![("/resume demo", "demo", "梳理项目结构", false)],
                        _ => vec![],
                    };
                    json!({"type":"completions","value":{"input":input,"entries":entries.into_iter().map(|(name,display,description,continue_input)| json!({"name":name,"display":display,"description":description,"continue_input":continue_input})).collect::<Vec<_>>()}})
                }
            }
            "hello" => {
                json!({"type":"hello","value":{"version":orbit_protocol::VERSION,"workspace":workspace,"capabilities":{"system":"Linux","architecture":"x86_64","shell":"sh","native_processes":true,"files":true,"features":[]}}})
            }
            "all_sessions" => {
                let catalog_root = std::fs::read_to_string(
                    std::path::Path::new(&workspace).join(".fixture-sidebar"),
                )
                .unwrap_or_default();
                let catalog_root = if catalog_root.trim().is_empty() {
                    &workspace
                } else {
                    catalog_root.trim()
                };
                let response = if std::path::Path::new(&workspace)
                    .join(".fixture-sidebar")
                    .exists()
                {
                    json!({"type":"sessions","value":[
                        {"id":"demo","workspace":catalog_root,"project_id":"fixture","model":model,"title":"请检查当前项目结构，读取 Cargo.toml 和完整的依赖关系并给出建议","updated_at":3},
                        {"id":"navigation","workspace":catalog_root,"project_id":"fixture","model":model,"title":"请直接使用 read_file 读取 /tmp\n/orbit-e2e/README.md\n并继续检查","updated_at":2},
                        {"id":"unavailable","workspace":catalog_root,"project_id":"fixture","model":model,"title":"验证打开失败的会话","updated_at":1},
                        {"id":"notes","workspace":"/tmp/orbit-gui-notes","project_id":"notes","model":model,"title":"整理开发笔记","updated_at":1}
                    ]})
                } else {
                    let saved_path = if relocation.is_none() {
                        "/tmp/orbit-gui-fixture"
                    } else if linked {
                        &workspace
                    } else {
                        "/missing/original"
                    };
                    json!({"type":"sessions","value":[
                        {"id":"demo","workspace":saved_path,"project_id":"fixture","model":model,"title":"梳理项目结构","updated_at":3},
                        {"id":"navigation","workspace":saved_path,"project_id":"fixture","model":model,"title":"调整项目导航","updated_at":2},
                        {"id":"notes","workspace":"/tmp/orbit-gui-notes","project_id":"notes","model":model,"title":"整理开发笔记","updated_at":1}
                    ]})
                };
                catalog_response(response, &catalog)
            }
            "sessions" => catalog_response(
                json!({"type":"sessions","value":[{"id":"demo","workspace":workspace,"project_id":"fixture","model":model,"reasoning_effort":null,"title":"梳理项目结构","updated_at":0}]}),
                &catalog,
            ),
            "admin" if request["action"]["action"] == "rename_session" => {
                let id = request["action"]["id"].as_str().unwrap_or_default();
                let title = request["action"]["title"].as_str().unwrap_or_default();
                if title.trim().is_empty() || title.chars().count() > 120 {
                    json!({"type":"error","value":"会话标题应为 1–120 个字符"})
                } else if catalog.deleted.contains(id) {
                    json!({"type":"error","value":"会话已删除"})
                } else {
                    catalog.titles.insert(id.to_owned(), title.to_owned());
                    std::fs::write(&catalog_path, serde_json::to_vec(&catalog)?)?;
                    json!({"type":"report","value":{"title":"会话标题","text":title}})
                }
            }
            "admin" if request["action"]["action"] == "delete_session" => {
                let id = request["action"]["id"].as_str().unwrap_or_default();
                if (id == session && (!pending.is_empty() || !approvals.is_empty()))
                    || side_questions
                        .get(id)
                        .is_some_and(|questions| questions.running())
                {
                    json!({"type":"error","value":"会话正在运行，无法删除"})
                } else {
                    catalog.deleted.insert(id.to_owned());
                    catalog.titles.remove(id);
                    history.remove(id);
                    std::fs::write(&catalog_path, serde_json::to_vec(&catalog)?)?;
                    json!({"type":"report","value":{"title":"工作区管理","text":"会话及私有文件已删除。"}})
                }
            }
            "admin" if request["action"]["action"] == "link_project" => {
                if relocation.as_deref() == Some("reject") {
                    json!({"type":"error","value":"this directory already has another project's conversations"})
                } else if request["action"]["id"] != "fixture" {
                    json!({"type":"error","value":"wrong project identity"})
                } else {
                    linked = true;
                    json!({"type":"report","value":{"title":"工作区管理","text":"已关联项目"}})
                }
            }
            "open" if relocation.is_some() && !linked => {
                json!({"type":"error","value":"session belongs to workspace /missing/original"})
            }
            "open" => {
                if request["resume"] == "unavailable" {
                    let response = json!({"type":"error","value":"测试会话暂时无法打开，请重试。"});
                    let bytes = serde_json::to_vec(&response)?;
                    output.write_all(&(bytes.len() as u32).to_be_bytes())?;
                    output.write_all(&bytes)?;
                    output.flush()?;
                    continue;
                }
                count += 1;
                session = request["resume"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("draft-{count}"));
                user_inputs.prepare(&workspace, &session);
                child_agents.prepare(&workspace, &session);
                history.entry(session.clone()).or_default();
                if session == "demo"
                    && std::path::Path::new(&workspace)
                        .join("preview.png")
                        .is_file()
                {
                    history.get_mut("demo").unwrap()[0]["Started"]["presentation"] =
                        json!({"image_assets":[{"name":"preview.png","path":"preview.png"}]});
                }
                if session == "quota" {
                    model = "fixture::gemini-flash".into();
                    std::fs::write(
                        std::path::Path::new(&workspace).join(".fixture-quota-model"),
                        &model,
                    )?;
                }
                if session == "tool-history" {
                    history.insert(session.clone(), tool_history());
                }
                if session == "navigation" {
                    history.insert(session.clone(), (1..=160).flat_map(|index| [
                        json!({"Started":{"prompt":format!("第 {index} 轮：检查项目功能与验证结果")}}),
                        json!({"Agent":{"Model":{"TextDelta":format!("## 第 {index} 轮答复\n\n这一轮保留真实用户提问作为定位点。\n\n{}", "详细说明与后续步骤。\n\n".repeat(index % 4 + 1))}}}),
                        json!({"Finished":{"status":"completed"}}),
                    ]).collect());
                }
                if session == "paged" {
                    history.insert(session.clone(), (0..100).map(|index|
                        json!({"Agent":{"Model":{"TextDelta":format!("{index}\n")}}})
                    ).collect());
                }
                json!({"type":"opened","value":profile(&session,&model,&mode)})
            }
            "poll" => {
                let events = history.entry(session.clone()).or_default();
                let hold_command = pending.front().is_some_and(|event| {
                    event["Agent"]["ToolFinished"]["call"]["id"] == "output-command"
                }) && std::path::Path::new(&workspace)
                    .join(".fixture-command-output-hold")
                    .exists();
                let hold_disclosure = pending.front().is_some_and(|event| {
                    event["Agent"]["ToolStarted"]["call"]["id"] == "disclosure-02"
                }) && std::path::Path::new(&workspace)
                    .join(".fixture-disclosure-hold")
                    .exists();
                if approvals.is_empty()
                    && !hold_command
                    && !hold_disclosure
                    && let Some(event) = pending.pop_front()
                {
                    events.push(event);
                }
                let after = request["after"].as_u64().unwrap_or_default() as usize;
                let next = if session == "paged" {
                    (after + 1).min(events.len())
                } else {
                    events.len()
                };
                json!({"type":"page","value":{"next":next,"more":next < events.len(),"busy":!pending.is_empty()||!approvals.is_empty(),"model":model,"updates":&events[after.min(events.len())..next],"approvals":approvals,"planning":planning,"user_inputs":user_inputs.live(&session),"tool_progress":tool_output::progress(events)}})
            }
            "usage" => {
                let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
                if request["session"] == "quota" {
                    let model = std::fs::read_to_string(
                        std::path::Path::new(&workspace).join(".fixture-quota-model"),
                    )?;
                    let limits = [
                        ("Gemini Models · Weekly Limit Remaining", 0.89, "Account"),
                        ("模型 · chat_20706", 1., "Model"),
                        ("Gemini Models · Five Hour Limit Remaining", 0.73, "Account"),
                        ("Claude and GPT models · Weekly Limit Remaining", 1., "Account"),
                        ("Claude and GPT models · Five Hour Limit Remaining", 0.5, "Account"),
                    ].map(|(label, fraction, scope)| json!({"label":label,"scope":scope,"remaining_fraction":fraction,"resets_at":(now+13200).to_string()}));
                    json!({"type":"usage","value":{"model":model,"account":{"identity":"fixture","provider":"Antigravity","name":"演示账号","fetched_at":now,"limits":limits},"session":{"runs":1,"requests":1,"runs_with_usage":1}}})
                } else {
                    json!({"type":"usage","value":{"model":model,"account":{"identity":"fixture","provider":"本地测试","name":"演示账号","fetched_at":now,"limits":[{"label":"Gemini Models · Weekly Limit Remaining","remaining_fraction":0.89,"resets_at":(now+580000).to_string()},{"label":"Gemini Models · Five Hour Limit Remaining","remaining_fraction":0.73,"resets_at":(now+13200).to_string()}]},"session":{"runs":1,"requests":1,"runs_with_usage":1,"input":8192,"output":1024}}})
                }
            }
            "start" => {
                let prompt = request["prompt"].as_str().unwrap_or_default();
                if prompt == "question" || prompt == "questions" {
                    user_inputs.start(&workspace, &session, prompt == "questions");
                }
                if prompt.trim() == "fail" && reject {
                    reject = false;
                    json!({"type":"error","value":"测试服务拒绝了这次发送；草稿已保留，可以重试。"})
                } else {
                    history
                        .entry(session.clone())
                        .or_default()
                        .push(json!({"Started":{"prompt":prompt}}));
                    pending = [
                        "已收到任务。\n\n",
                        "**流式输出**",
                        "正在通过客户端显示。",
                        "\n\n可以继续检查草稿和主题切换。",
                    ]
                    .into_iter()
                    .map(|text| json!({"Agent":{"Model":{"TextDelta":text}}}))
                    .collect();
                    if prompt == "math" {
                        pending = [json!({"Agent":{"Model":{"TextDelta":"行内公式（$WE=1\\quad$）与文字连续。\n\n$$\n\\frac{a+b}{c} = \\sqrt{x^2+y^2}\n$$\n\n公式后面的正文。"}}})].into();
                    }
                    if prompt == "rendering" {
                        pending = [json!({"Agent":{"Model":{"TextDelta":"你目前遇到的**“高位无法复位”和“进位不正常”**，需要先检查输入。\n\n使能计数（$EN_{in}=1$），满模了，下一拍你该加一。\n\n**使能计数（$EN_{in}=1$）**，观察输出。\n\n行内公式 $\\frac{a+b}{c}$ 和 $x_i^2$ 应与正文自然对齐。\n\n`**原样保留**` 与 **正常加粗**。"}}})].into();
                    }
                    if prompt == "code" {
                        pending =
                            [json!({"Agent":{"Model":{"TextDelta":include_str!("code.md")}}})]
                                .into();
                    }
                    if prompt == "font-fallback" {
                        pending = [json!({"Agent":{"Model":{"TextDelta":include_str!("font-fallback.md")}}})].into();
                    }
                    if prompt == "selection" {
                        pending = [json!({"Agent":{"Model":{"TextDelta":"### 3. `bner0` 指令\n\n普通文字 `inline code` 与**加粗文字**连续选中。\n\n1. 混合 `code` 的列表项\n2. 下一项\n\n公式 $EN_{in}=1$ 和后面的文字。"}}})].into();
                    }
                    if prompt == "table" {
                        pending =
                            [json!({"Agent":{"Model":{"TextDelta":include_str!("table.md")}}})]
                                .into();
                    }
                    if prompt == "workbench" {
                        pending = workbench_events();
                    }
                    if matches!(prompt, "command-output" | "command-output-fast") {
                        pending = tool_output::events(
                            &workspace,
                            if prompt == "command-output" { 120 } else { 5 },
                        );
                    }
                    if prompt == "command-output-long" {
                        pending = tool_output::long_events(&workspace);
                    }
                    if matches!(
                        prompt.lines().next(),
                        Some("disclosures" | "disclosures-live")
                    ) {
                        pending = disclosures::events(
                            &workspace,
                            prompt.lines().next() == Some("disclosures-live"),
                        );
                    }
                    if prompt == "sidebar" {
                        pending = (0..80)
                            .map(|_| json!({"Agent":{"Model":{"TextDelta":"检查中。"}}}))
                            .collect();
                    }
                    if prompt.trim() == "approve" {
                        approvals = vec![
                            json!({"id":7,"preview":{"Command":{"command":"printf 'Orbit GUI test\\n'","cwd":workspace,"timeout_seconds":30,"workspace_access":"read","network_allowed":false,"external_capabilities":[],"external_write":false,"git":null}}}),
                        ];
                    }
                    pending.push_back(json!({"Finished":{"status":"completed"}}));
                    json!({"type":"ok"})
                }
            }
            "approve" => {
                approvals.clear();
                json!({"type":"ok"})
            }
            "cancel" => {
                pending.clear();
                approvals.clear();
                history
                    .entry(session.clone())
                    .or_default()
                    .push(json!({"Finished":{"status":"cancelled"}}));
                json!({"type":"ok"})
            }
            "set_model" => {
                model = request["model"].as_str().unwrap().to_string();
                if session == "quota" {
                    std::fs::write(
                        std::path::Path::new(&workspace).join(".fixture-quota-model"),
                        &model,
                    )?;
                }
                json!({"type":"profile","value":profile(&session,&model,&mode)})
            }
            "set_mode" => {
                mode = request["mode"].as_str().unwrap().to_string();
                json!({"type":"profile","value":profile(&session,&model,&mode)})
            }
            "plan" => {
                match request["action"]["action"].as_str() {
                    Some("enter") => {
                        planning = json!({"generation":1,"mode":"plan","plan":{"id":"plan-1","workspace":workspace,"revision":1,"title":"完善桌面客户端","markdown":"## 实现步骤\n\n1. 统一窗口布局\n2. 接入工作区功能\n3. 验证交互","steps":["统一窗口布局","接入功能","验证交互"],"status":"review"}})
                    }
                    Some("approve")
                        if request["action"]["generation"] == planning["generation"] =>
                    {
                        planning["mode"] = json!("build");
                        planning["plan"]["status"] = json!("approved");
                    }
                    Some("leave") => planning["mode"] = json!("build"),
                    _ => {}
                }
                json!({"type":"ok"})
            }
            "answer_user_input" => user_inputs.answer(&request),
            "user_input_history" => user_inputs.history(
                &workspace,
                request["session"].as_str().unwrap_or_default(),
                request["before"].as_i64(),
            ),
            "read_file" if request["path"] == "missing.png" => {
                json!({"type":"error","value":"图片不存在"})
            }
            "read_file" if request["path"] == "preview.png" => {
                let bytes = std::fs::read(std::path::Path::new(&workspace).join("preview.png"))?;
                json!({"type":"file","value":base64::engine::general_purpose::STANDARD.encode(bytes)})
            }
            "read_file" => {
                fixture_read_file(&workspace, request["path"].as_str().unwrap_or_default())
            }
            "files" => fixture_files(&workspace, &request),
            "input_queue" => input_queue::execute(&workspace, &request),
            "review" => review::execute(&workspace, &request),
            "subagents" => child_agents.execute(&workspace, &request),
            "read_subagent" => child_agents.read(&workspace, &request),
            "send_subagent" => child_agents.send_legacy(&workspace, &request),
            "steer" => {
                history.entry(session.clone()).or_default().push(json!({"Agent":{"InputReceived":{"message_id":8,"content":request["message"]}}}));
                json!({"type":"input_queued","value":{"message_id":8}})
            }
            "ask" | "open_questions" | "read_questions" | "cancel_question" | "close_questions" => {
                side_questions
                    .entry(request["session"].as_str().unwrap_or(&session).to_owned())
                    .or_default()
                    .execute(request["method"].as_str().unwrap(), &request, &model)
            }
            "session_action" => {
                json!({"type":"report","value":{"title":"文件变更","text":"## 工作区变更\n\n| 文件 | 状态 |\n| --- | --- |\n| src/workspace.rs | 已修改 |\n\n```diff\n- old_layout()\n+ workspace_layout()\n```"}})
            }
            "tasks" => {
                if request["expected_workspace"]
                    .as_str()
                    .is_some_and(|expected| expected != workspace.as_str())
                {
                    json!({"type":"error","value":"工作区已切换，请重新打开当前项目的任务页后重试"})
                } else {
                    task_snapshot(&workspace, &request["action"])
                }
            }
            "resources" => {
                if request["expected_workspace"]
                    .as_str()
                    .is_some_and(|expected| expected != workspace.as_str())
                    || request["session"].as_str() != Some(session.as_str())
                {
                    json!({"type":"error","value":"工作区已切换，请重新打开当前项目的设置后重试"})
                } else {
                    let action = &request["action"];
                    if let Some(save) = action.get("SaveSkill") {
                        fixture_skills.insert(
                            save["name"].as_str().unwrap_or_default().into(),
                            save["content"].as_str().unwrap_or_default().into(),
                        );
                    }
                    if let Some(memory) = action.get("Memory") {
                        if memory["action"] == "add" {
                            let id = fixture_memories.len() + 1;
                            let category = memory["category"].as_str().unwrap_or("fact");
                            let title = match category {
                                "constraint" => "约束",
                                "decision" => "决策",
                                "preference" => "偏好",
                                _ => "事实",
                            };
                            fixture_memories.push(json!({"id":id,"session_id":session,"category":category,"title":title,"content":memory["content"],"active":true,"created_at":0,"updated_at":0}));
                        }
                        if memory["action"] == "forget" {
                            fixture_memories.retain(|m| m["id"] != memory["id"]);
                        }
                    }
                    let snapshot = if let Some(read) = action.get("ReadSkill") {
                        let name = read["name"].as_str().unwrap_or_default();
                        json!({"Skill":{"name":name,"content":fixture_skills.get(name).cloned().unwrap_or_default(),"editable":true}})
                    } else if action == "Memories" || action.get("Memory").is_some() {
                        json!({"Memories":fixture_memories})
                    } else if action == "Mcp" {
                        json!(orbit_protocol::resources::Snapshot::Mcp(vec![
                            orbit_protocol::resources::McpServer {
                                name: "fixture".into(),
                                connected: true,
                                detail: "已连接".into(),
                                tools: vec![orbit_protocol::resources::McpTool {
                                    name: "mcp__fixture__echo".into(),
                                    description: "Echo input".into(),
                                }],
                            }
                        ]))
                    } else {
                        json!({"Skills":fixture_skills.keys().map(|name|json!({"name":name,"title":name,"description":"项目技能","source":"项目技能","editable":true})).collect::<Vec<_>>()})
                    };
                    json!({"type":"resources","value":snapshot})
                }
            }
            "plugins" => plugin_catalog(&workspace, &request["action"])?,
            _ => json!({"type":"ok"}),
        };
        let bytes = serde_json::to_vec(&response)?;
        output.write_all(&(bytes.len() as u32).to_be_bytes())?;
        output.write_all(&bytes)?;
        output.flush()?;
    }
    Ok(())
}

fn workbench_events() -> VecDeque<Value> {
    let read = json!({"id":"read-entry","function":{"name":"read_file","arguments":"{\"path\":\"src/main.rs\"}"}});
    let write = json!({"id":"write-entry","function":{"name":"write_file","arguments":"{\"path\":\"src/workbench.rs\"}"}});
    let child = json!({"id":"spawn-review","function":{"name":"spawn_agent","arguments":"{\"task\":\"审查任务切换与状态保留\"}"}});
    let mut events: VecDeque<_> = [
        json!({"Agent":{"TurnStarted":{"turn":1}}}),
        json!({"Agent":{"Model":{"TextDelta":"先检查会话的状态归属，再验证任务切换。"}}}),
        json!({"Agent":{"ToolStarted":{"call":read}}}),
        json!({"Agent":{"ToolFinished":{"call":read,"content":"入口已读取；会话通过独立的客户端连接运行。","changes":[]}}}),
        json!({"Agent":{"ToolStarted":{"call":child}}}),
        json!({"Agent":{"ToolBackgrounded":{"call":child,"job_id":"review-job","content":"{\"agent_id\":\"review-1\",\"role\":\"review\",\"status\":\"running\"}"}}}),
        json!({"Agent":{"ToolStarted":{"call":write}}}),
    ].into();
    // Long enough to switch tasks while this fixture is still producing events.
    events.extend((0..120).map(|_| json!({"Agent":{"Model":{"ReasoningDelta":"检查状态。"}}})));
    events.extend([
        json!({"Agent":{"ToolFinished":{"call":write,"content":"已更新任务宿主。","changes":[{"path":"src/workbench.rs","before":null,"after":"// task host\n"}]}}}),
        json!({"Agent":{"Model":{"TextDelta":"任务视图已独立保存。\n\n- 切换任务保留输入和工具过程。\n- 修改记录可以在右侧查看。\n- 子 Agent 有单独的执行记录入口。\n\n这一轮修改涉及 `src/workbench.rs`。"}}}),
    ]);
    events
}

fn tool_history() -> Vec<Value> {
    let mut events = vec![json!({"Started":{"prompt":"检查项目结构，说明需要改进的地方。"}})];
    for index in 0..8 {
        let call = json!({"id":format!("read-{index}"),"function":{"name":"read_file","arguments":format!("{{\"path\":\"src/module_{index}.rs\"}}")}});
        events.extend([
            json!({"Agent":{"TurnStarted":{"turn":index}}}),
            json!({"Agent":{"Model":{"TextDelta":""}}}),
            json!({"Agent":{"Model":{"ReasoningDelta":"检查模块的依赖方向与状态归属。"}}}),
            json!({"Agent":{"ToolStarted":{"call":call}}}),
            json!({"Agent":{"ToolFinished":{"call":call,"content":"模块已读取。状态通过实体持有，界面通过事件订阅更新。","changes":[]}}}),
        ]);
    }
    events.extend([
        json!({"Agent":{"Model":{"TextDelta":"检查完成。当前项目按工作区、会话和工具执行分层。\n\n### 建议调整\n\n- **会话状态**由独立实体持有，切换任务时保留草稿和执行状态。\n- **工具过程**归入所属用户回合，完整参数与结果按需展开。\n- **最终答复**单独呈现，避免工具日志打断阅读。\n\n本次只读取了源码，没有修改项目文件。"}}}),
        json!({"Finished":{"status":"completed"}}),
    ]);
    events
}

fn plugin_catalog(workspace: &str, action: &Value) -> anyhow::Result<Value> {
    let path = std::path::Path::new(workspace).join(".fixture-plugins.json");
    let mut catalog: orbit_protocol::plugins::Catalog = match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(
                json!({"type":"plugins","value":{"marketplaces":[],"project_id":"fixture","enabled":[],"inspections":[]}}),
            );
        }
        Err(error) => return Err(error.into()),
    };
    match action["action"].as_str() {
        Some("enable") => {
            let mut plugin: orbit_protocol::project::ProjectPlugin =
                serde_json::from_value(action["plugin"].clone())?;
            plugin.project_id = catalog.project_id.clone();
            catalog.enabled.retain(|p| {
                p.marketplace_source != plugin.marketplace_source
                    || p.plugin_name != plugin.plugin_name
            });
            catalog.enabled.push(plugin);
        }
        Some("disable") => catalog.enabled.retain(|p| {
            Some(p.marketplace_source.as_str()) != action["marketplace_source"].as_str()
                || Some(p.plugin_name.as_str()) != action["plugin_name"].as_str()
        }),
        Some("remove") => {
            if catalog
                .enabled
                .iter()
                .any(|p| Some(p.marketplace_source.as_str()) == action["source"].as_str())
            {
                return Ok(
                    json!({"type":"error","value":"这个市场仍有已启用插件，请先停用后再移除"}),
                );
            }
            catalog
                .marketplaces
                .retain(|m| Some(m.source.as_str()) != action["source"].as_str());
        }
        Some("add") => return Ok(json!({"type":"error","value":"测试市场目录不存在，请检查来源"})),
        _ => {}
    }
    std::fs::write(path, serde_json::to_vec(&catalog)?)?;
    Ok(json!({"type":"plugins","value":catalog}))
}

fn task_snapshot(workspace: &str, action: &Value) -> Value {
    use orbit_protocol::project::{ProjectTask, TaskAction, TaskSnapshot, TaskStatus};
    let result = (|| -> anyhow::Result<TaskSnapshot> {
        let path = std::path::Path::new(workspace).join(".fixture-tasks.json");
        let mut tasks: Vec<ProjectTask> = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => vec![],
            Err(error) => return Err(error.into()),
        };
        let action: TaskAction = serde_json::from_value(action.clone())?;
        let changed = match action {
            TaskAction::List { include_completed } => {
                let tasks = tasks
                    .into_iter()
                    .filter(|task| {
                        include_completed
                            || !matches!(task.status, TaskStatus::Completed | TaskStatus::Cancelled)
                    })
                    .collect();
                return Ok(TaskSnapshot {
                    tasks,
                    changed: None,
                });
            }
            TaskAction::Add { title } => {
                let id = tasks.iter().map(|task| task.id).max().unwrap_or(0) + 1;
                tasks.push(ProjectTask {
                    id,
                    project_id: "fixture".into(),
                    parent_id: None,
                    title,
                    details: String::new(),
                    priority: 0,
                    status: TaskStatus::Pending,
                    owner_session_id: None,
                    depends_on: vec![],
                    blocked_by: vec![],
                    created_at: 0,
                    updated_at: 0,
                });
                Some(id)
            }
            TaskAction::Create {
                title,
                details,
                priority,
                parent_id,
                depends_on,
            } => {
                let id = tasks.iter().map(|task| task.id).max().unwrap_or(0) + 1;
                tasks.push(ProjectTask {
                    id,
                    project_id: "fixture".into(),
                    parent_id,
                    title,
                    details,
                    priority,
                    status: TaskStatus::Pending,
                    owner_session_id: None,
                    depends_on,
                    blocked_by: vec![],
                    created_at: 0,
                    updated_at: 0,
                });
                Some(id)
            }
            TaskAction::Update {
                id,
                title,
                details,
                status,
                priority,
                depends_on,
            } => {
                let task = tasks
                    .iter_mut()
                    .find(|task| task.id == id)
                    .ok_or_else(|| anyhow::anyhow!("项目中没有这个任务"))?;
                if let Some(title) = title {
                    task.title = title;
                }
                if let Some(details) = details {
                    task.details = details;
                }
                if let Some(status) = status {
                    task.status = status;
                }
                if let Some(priority) = priority {
                    task.priority = priority;
                }
                if let Some(depends_on) = depends_on {
                    task.depends_on = depends_on;
                }
                Some(id)
            }
            TaskAction::SetStatus { id, status } => {
                tasks
                    .iter_mut()
                    .find(|task| task.id == id)
                    .ok_or_else(|| anyhow::anyhow!("项目中没有这个任务"))?
                    .status = status;
                Some(id)
            }
            TaskAction::Release { id } => {
                let task = tasks
                    .iter_mut()
                    .find(|task| task.id == id)
                    .ok_or_else(|| anyhow::anyhow!("项目中没有这个任务"))?;
                task.owner_session_id = None;
                if task.status == TaskStatus::InProgress {
                    task.status = TaskStatus::Pending;
                }
                Some(id)
            }
        };
        for task in &tasks {
            anyhow::ensure!(
                !task.title.trim().is_empty() && task.title.len() <= 512,
                "请填写有效任务标题"
            );
            anyhow::ensure!(
                task.details.len() <= 8192 && (-100..=100).contains(&task.priority),
                "任务说明或优先级超出范围"
            );
            anyhow::ensure!(task.depends_on.len() <= 64, "一个任务最多选择 64 个依赖");
            let mut pending = task.depends_on.clone();
            let mut visited = std::collections::BTreeSet::new();
            while let Some(id) = pending.pop() {
                anyhow::ensure!(id != task.id, "任务依赖不能形成循环");
                let dependency = tasks
                    .iter()
                    .find(|task| task.id == id)
                    .ok_or_else(|| anyhow::anyhow!("依赖任务不存在"))?;
                if visited.insert(id) {
                    pending.extend(dependency.depends_on.iter().copied());
                }
            }
            if matches!(task.status, TaskStatus::InProgress | TaskStatus::Completed) {
                anyhow::ensure!(
                    task.depends_on.iter().all(|id| tasks
                        .iter()
                        .any(|task| task.id == *id && task.status == TaskStatus::Completed)),
                    "还有未完成的依赖任务"
                );
            }
        }
        let completed: std::collections::BTreeSet<_> = tasks
            .iter()
            .filter(|task| task.status == TaskStatus::Completed)
            .map(|task| task.id)
            .collect();
        for task in &mut tasks {
            task.blocked_by = task
                .depends_on
                .iter()
                .copied()
                .filter(|id| !completed.contains(id))
                .collect();
        }
        tasks.sort_by_key(|task| {
            (
                match task.status {
                    TaskStatus::InProgress => 0,
                    TaskStatus::Blocked => 1,
                    _ => 2,
                },
                std::cmp::Reverse(task.priority),
                task.id,
            )
        });
        std::fs::write(path, serde_json::to_vec(&tasks)?)?;
        Ok(TaskSnapshot { tasks, changed })
    })();
    match result {
        Ok(tasks) => json!({"type":"tasks","value":tasks}),
        Err(error) => json!({"type":"error","value":format!("{error:#}")}),
    }
}
