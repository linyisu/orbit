//! Opt-in evaluation against a configured real model, using the production
//! workspace actor, core instructions, tool definitions and approval policy.
//!
//! Set ORBIT_EVAL_CONFIG, ORBIT_EVAL_AUTH_DIR, ORBIT_EVAL_MODEL,
//! ORBIT_EVAL_WEB_URL and ORBIT_EVAL_WEB_MARKER, then run:
//! cargo test -p orbit-service --lib real_model_tool_selection -- --ignored --nocapture
//! ORBIT_EVAL_REPETITIONS defaults to 1. Each case has a fresh temporary workspace
//! and database. JSON reports contain tool names and verdicts, not credentials,
//! command arguments or fetched content. This is not a deterministic CI check.

use std::{env, fs, path::Path, time::Duration};

use anyhow::{Context, Result, ensure};
use orbit_agent::events::AgentEvent;
use orbit_model::types::StreamEvent;
use serde_json::json;

use super::super::{Command, Handle};
use crate::{WorkspaceOptions, prepare_workspace, workspace::protocol::Update};

const OLD_TEXT: &str = "Review status: pending.";
const NEW_TEXT: &str = "Review status: approved.";
const UNTOUCHED: &[u8] = b"\0\xffuntouched fixture\r\n";

#[derive(Clone, Copy)]
enum Case {
    ReadUrl,
    EditParagraph,
    CreateFile,
}

impl Case {
    fn name(self) -> &'static str {
        match self {
            Self::ReadUrl => "read_public_url",
            Self::EditParagraph => "edit_existing_paragraph",
            Self::CreateFile => "create_new_file",
        }
    }

    fn preferred_tool(self) -> &'static str {
        match self {
            Self::ReadUrl => "web_fetch",
            Self::EditParagraph => "edit_file",
            Self::CreateFile => "write_file",
        }
    }

    fn prompt(self, url: &str, marker: &str) -> String {
        match self {
            Self::ReadUrl => {
                format!("读取 {url}，引用其中包含“{marker}”的一行并简要解释。不要修改本地文件。")
            }
            Self::EditParagraph => {
                format!("把 notes.md 中的“{OLD_TEXT}”改成“{NEW_TEXT}”，其他内容保持原样。")
            }
            Self::CreateFile => {
                "新建 greeting.txt，内容为 Hello, Orbit!，末尾换行。其他文件保持原样。".into()
            }
        }
    }
}

fn required(name: &str) -> Result<String> {
    env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .with_context(|| format!("set {name} to run the real-model evaluation"))
}

fn notes() -> String {
    let mut text = String::from("# Review notes\n\n");
    for index in 0..32 {
        text.push_str(&format!(
            "## Section {index}\n\nKeep this paragraph unchanged: 中文、spaces  and punctuation.\n\n"
        ));
        if index == 15 {
            text.push_str(&format!("{OLD_TEXT}\n\n"));
        }
    }
    text
}

struct Settings {
    config: std::path::PathBuf,
    auth: std::path::PathBuf,
    model: String,
    url: String,
    marker: String,
}

async fn evaluate(settings: &Settings, case: Case, repetition: usize) -> Result<bool> {
    let directory = tempfile::Builder::new()
        .prefix("orbit-tool-selection-eval-")
        .tempdir()?;
    let root = directory.path();
    let workspace = root.join("workspace");
    fs::create_dir(&workspace)?;
    let workspace = workspace.canonicalize()?;
    let original = notes();
    let intended_change = match case {
        Case::EditParagraph => Some((
            "notes.md",
            Some(original.as_str()),
            original.replace(OLD_TEXT, NEW_TEXT),
        )),
        Case::CreateFile => Some(("greeting.txt", None, "Hello, Orbit!\n".into())),
        Case::ReadUrl => None,
    };
    fs::write(workspace.join("notes.md"), &original)?;
    fs::write(workspace.join("untouched.bin"), UNTOUCHED)?;
    let options = WorkspaceOptions {
        model: Some(settings.model.clone()),
        initial_directory: workspace.clone(),
        auth_directory: settings.auth.clone(),
        config: settings.config.clone(),
        database: root.join("sessions.db"),
        allow_mcp_start: false,
        home: Some(root.into()),
        config_home: Some(root.join("config")),
        instruction_dir: Some(root.join("instructions")),
        editor_command: "vi".into(),
    };
    let mut setup = prepare_workspace(&options, &workspace, None).await?;
    // Bound the cost of a bad choice without changing the available tools.
    setup.resources.budget.max_turns = 8;
    setup.resources.budget.max_tool_calls = 16;
    setup.resources.budget.max_output_tokens = setup
        .resources
        .budget
        .max_output_tokens
        .min(4_096);
    let handle = Handle::spawn(setup, None)
        .await
        .unwrap();
    handle
        .command(Command::SetMode {
            mode: orbit_protocol::Mode::Auto,
        })
        .await?;
    handle
        .command(Command::Start {
            request_id: orbit_protocol::request_id(),
            prompt: case.prompt(&settings.url, &settings.marker),
            images: Vec::new(),
            compact: false,
            model: None,
        })
        .await?;
    let mut calls = Vec::new();
    let mut succeeded = Vec::new();
    let mut failed = Vec::new();
    let mut answer = String::new();
    let mut fetched_marker = false;
    let mut native_file_change = false;
    let mut status = String::from("timeout");
    let outcome = tokio::time::timeout(Duration::from_secs(180), async {
        let mut cursor = 0;
        loop {
            let page = handle.view.lock().await.page(cursor)?;
            cursor = page.next;
            for update in page.updates {
                match update {
                    Update::Agent(event) => match *event {
                        AgentEvent::TurnStarted { .. } => answer.clear(),
                        AgentEvent::Model(StreamEvent::TextDelta(text)) => answer.push_str(&text),
                        AgentEvent::ToolStarted { call } => calls.push(call.function.name),
                        AgentEvent::ToolFailed { call, .. } => failed.push(call.function.name),
                        AgentEvent::ToolFinished {
                            call,
                            content,
                            changes,
                        } => {
                            if call.function.name == "web_fetch"
                                && let Ok(value) =
                                    serde_json::from_str::<serde_json::Value>(&content)
                            {
                                fetched_marker |= value["requested_url"].as_str()
                                    == Some(settings.url.as_str())
                                    && value["content"]
                                        .as_str()
                                        .is_some_and(|text| text.contains(&settings.marker));
                            }
                            if call.function.name == case.preferred_tool()
                                && let Some((target, before, after)) = &intended_change
                            {
                                native_file_change |= changes.iter().any(|change| {
                                    workspace
                                        .join(&change.path)
                                        .canonicalize()
                                        .ok()
                                        == Some(workspace.join(target))
                                        && change.before.as_deref() == *before
                                        && change.after == *after
                                });
                            }
                            succeeded.push(call.function.name);
                        }
                        _ => {}
                    },
                    Update::Finished {
                        status: finished, ..
                    } => status = finished,
                    _ => {}
                }
            }
            if !page.busy && !page.more {
                break;
            }
            if !page.more {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        }
        Ok::<(), anyhow::Error>(())
    })
    .await;
    if let Ok(result) = outcome {
        result?;
    } else {
        handle.command(Command::Cancel).await?;
        tokio::time::timeout(Duration::from_secs(15), async {
            while handle.running().await {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .context("evaluation cancellation did not finish")?;
    }
    handle.command(Command::Close).await?;
    let unchanged = fs::read(workspace.join("untouched.bin"))
        .ok()
        .as_deref()
        == Some(UNTOUCHED);
    let expected = match case {
        Case::EditParagraph => original.replace(OLD_TEXT, NEW_TEXT),
        _ => original,
    };
    let notes_ok = fs::read(workspace.join("notes.md"))
        .ok()
        .as_deref()
        == Some(expected.as_bytes());
    let mut files = fs::read_dir(&workspace)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<std::io::Result<Vec<_>>>()?;
    files.sort();
    let mut expected_files = vec!["notes.md", "untouched.bin"];
    if matches!(case, Case::CreateFile) {
        expected_files.push("greeting.txt");
    }
    expected_files.sort();
    let files_ok = files == expected_files;
    let task_ok = status == "completed"
        && unchanged
        && notes_ok
        && files_ok
        && match case {
            Case::ReadUrl => fetched_marker && answer.contains(&settings.marker),
            Case::EditParagraph => true,
            Case::CreateFile => {
                fs::read(workspace.join("greeting.txt"))
                    .ok()
                    .as_deref()
                    == Some(b"Hello, Orbit!\n".as_slice())
            }
        };
    let preferred = case.preferred_tool();
    // Judge the operation that fulfilled the task, not unrelated read-only
    // inspection or post-edit verification through the shell.
    let choice_ok = match case {
        Case::ReadUrl => {
            fetched_marker
                && !calls
                    .iter()
                    .any(|tool| tool == "run_command")
        }
        Case::EditParagraph => {
            native_file_change
                && !calls
                    .iter()
                    .any(|tool| tool == "write_file")
        }
        Case::CreateFile => native_file_change,
    };
    println!(
        "{}",
        json!({
            "case": case.name(),
            "repetition": repetition,
            "model": settings.model,
            "status": status,
            "task_ok": task_ok,
            "tool_choice_ok": choice_ok,
            "native_tool_failed": failed
                .iter()
                .any(|tool| tool == preferred),
            "calls": calls,
            "succeeded": succeeded,
            "failed": failed
        })
    );
    Ok(task_ok && choice_ok)
}

#[tokio::test]
#[ignore = "uses a configured real model and public URL; run explicitly with ORBIT_EVAL_* settings"]
async fn real_model_tool_selection() -> Result<()> {
    let settings = Settings {
        config: Path::new(&required("ORBIT_EVAL_CONFIG")?).canonicalize()?,
        auth: Path::new(&required("ORBIT_EVAL_AUTH_DIR")?).canonicalize()?,
        model: required("ORBIT_EVAL_MODEL")?,
        url: url::Url::parse(&required("ORBIT_EVAL_WEB_URL")?)?.to_string(),
        marker: required("ORBIT_EVAL_WEB_MARKER")?,
    };
    let repetitions = env::var("ORBIT_EVAL_REPETITIONS")
        .unwrap_or_else(|_| "1".into())
        .parse::<usize>()?;
    ensure!((1..=20).contains(&repetitions), "repetitions must be 1..20");
    let mut passed = 0;
    for repetition in 1..=repetitions {
        for case in [Case::ReadUrl, Case::EditParagraph, Case::CreateFile] {
            passed += usize::from(evaluate(&settings, case, repetition).await?);
        }
    }
    let total = repetitions * 3;
    println!(
        "{}",
        json!({
            "passed": passed,
            "total": total
        })
    );
    ensure!(
        passed == total,
        "real-model evaluation: {passed}/{total} passed; inspect JSON reports"
    );
    Ok(())
}
