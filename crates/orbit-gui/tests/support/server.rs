//! Offline protocol peer for integration and native-window checks.
//! Enabled explicitly with `test-support`; never calls a model.
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    io::{Read, Write},
    time::{SystemTime, UNIX_EPOCH},
};

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
    let mut pending = VecDeque::new();
    let mut approvals: Vec<Value> = vec![];
    let mut reject = true;
    let mut count = 0;
    let mut mode = "auto".to_string();
    let mut planning = json!({"generation":0,"mode":"build","plan":null});
    let mut user_inputs = json!([]);
    let mut side_question = String::new();
    let mut agent_updates = vec![json!({"Agent":{"Model":{"TextDelta":"子任务初始回答"}}})];
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
            writeln!(log, "{request}")?;
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
        let profile = |session: &str, model: &str, mode: &str| json!({"id":session,"persisted":session=="demo","title":if session=="demo" {"梳理项目结构"} else {"新会话"},"workspace":workspace,"model":model,"models":if session == "models" { vec!["acm::shared", "acm::qwen", "antigravity-account::claude-opus", "antigravity-account::gemini-flash", "chatgpt-account::astra", "deepseek-account::shared", "deepseek-account::deepseek-flash"] } else { vec!["fixture::local","fixture::other","fixture::gemini-flash","fixture::claude-opus"] },"mode":mode,"supports_vision":true,"reasoning_effort":null,"reasoning_efforts":[],"context_tokens":128000,"input_context_tokens":120000,"max_output_tokens":8000});
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
            }
            "sessions" => {
                json!({"type":"sessions","value":[{"id":"demo","workspace":workspace,"project_id":"fixture","model":model,"reasoning_effort":null,"title":"梳理项目结构","updated_at":0}]})
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
                count += 1;
                session = request["resume"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("draft-{count}"));
                history.entry(session.clone()).or_default();
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
                if approvals.is_empty()
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
                json!({"type":"page","value":{"next":next,"more":next < events.len(),"busy":!pending.is_empty()||!approvals.is_empty(),"model":model,"updates":&events[after.min(events.len())..next],"approvals":approvals,"planning":planning,"user_inputs":user_inputs}})
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
                    user_inputs = json!([{"id":"question-1","call_id":"question-call","source":null,"status":"pending","answers":[],"form":{"questions":[{"id":"choice","title":"实现方式","prompt":"选择实现方式","kind":"single","options":[{"id":"small","label":"小步迭代"},{"id":"full","label":"完整迁移"}],"allow_custom":true,"required":true}]}}]);
                }
                if prompt == "questions" {
                    user_inputs[0]["form"]["questions"].as_array_mut().unwrap().push(json!({
                        "id":"details","title":"补充要求","prompt":"还需要哪些内容？可选择多项，也可以补充说明。","kind":"multiple",
                        "options":[{"id":"docs","label":"说明文档","description":"记录使用方法与设计决定"},{"id":"tests","label":"回归验证"}],
                        "allow_custom":true,"required":true
                    }));
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
                    if prompt == "table" {
                        pending =
                            [json!({"Agent":{"Model":{"TextDelta":include_str!("table.md")}}})]
                                .into();
                    }
                    if prompt == "workbench" {
                        pending = workbench_events();
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
            "answer_user_input" => {
                user_inputs[0]["status"] = json!(if request["answers"].is_null() {
                    "declined"
                } else {
                    "answered"
                });
                user_inputs[0]["answers"] = if request["answers"].is_null() {
                    json!([])
                } else {
                    request["answers"].clone()
                };
                json!({"type":"ok"})
            }
            "read_file" if request["path"] == "missing.png" => {
                json!({"type":"error","value":"图片不存在"})
            }
            "read_file" => json!({"type":"file","value":"aGVsbG8gZ3VpCg=="}),
            "read_subagent" => {
                let after = request["after"].as_u64().unwrap_or_default() as usize;
                json!({"type":"subagent","value":{"next":agent_updates.len(),"more":false,"busy":false,"model":model,"updates":&agent_updates[after.min(agent_updates.len())..],"approvals":[]}})
            }
            "send_subagent" => {
                agent_updates.push(json!({"Agent":{"Model":{"TextDelta":request["message"]}}}));
                json!({"type":"ok"})
            }
            "steer" => {
                history.entry(session.clone()).or_default().push(json!({"Agent":{"InputReceived":{"message_id":8,"content":request["message"]}}}));
                json!({"type":"input_queued","value":{"message_id":8}})
            }
            "ask" => {
                side_question = request["question"].as_str().unwrap_or_default().to_string();
                json!({"type":"questions","value":{"revision":1,"selected":0,"running":true,"records":[{"thread_id":"side-1","model":model,"question":side_question,"answer":"","status":"running","error":null}]}})
            }
            "read_questions" => {
                let question_revision = 2;
                json!({"type":"questions","value":{"revision":question_revision,"selected":0,"running":false,"records":if request["after"].as_u64() == Some(question_revision) { Value::Null } else { json!([{"thread_id":"side-1","model":model,"question":side_question,"answer":"旁路回答已完成","status":"completed","error":null}]) }}})
            }
            "session_action" => {
                json!({"type":"report","value":{"title":"文件变更","text":"## 工作区变更\n\n| 文件 | 状态 |\n| --- | --- |\n| src/workspace.rs | 已修改 |\n\n```diff\n- old_layout()\n+ workspace_layout()\n```"}})
            }
            "plugins" => {
                json!({"type":"plugins","value":{"marketplaces":[],"project_id":"fixture","enabled":[],"inspections":[]}})
            }
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
