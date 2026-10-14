//! Persisted queue peer for native UI checks; production CAS lives in service.
use orbit_protocol::input_queue::{Action, Entry, Snapshot, State};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::Path;

#[derive(Serialize, Deserialize)]
struct Queue {
    session: String,
    run: Option<i64>,
    entries: Vec<Entry>,
}

pub(super) fn enqueue(
    workspace: &str,
    session: &str,
    content: &str,
    images: Vec<orbit_protocol::MessageImage>,
    files: Vec<orbit_protocol::MessageImage>,
) -> anyhow::Result<i64> {
    let path = Path::new(workspace).join(".fixture-input-queue.json");
    let mut queue: Queue = match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Queue {
            session: session.into(),
            run: Some(71),
            entries: Vec::new(),
        },
        Err(error) => return Err(error.into()),
    };
    anyhow::ensure!(queue.session == session, "消息不属于当前会话");
    let id = queue
        .entries
        .last()
        .map_or(1, |entry| entry.id + 1);
    queue.entries.push(Entry {
        files,
        images,
        id,
        content: content.into(),
        state: State::Queued,
    });
    std::fs::write(path, serde_json::to_vec(&queue)?)?;
    Ok(id)
}

pub(super) fn consume(workspace: &str) -> anyhow::Result<Vec<Entry>> {
    let path = Path::new(workspace).join(".fixture-input-queue.json");
    let mut queue: Queue = serde_json::from_slice(&std::fs::read(&path)?)?;
    let entries = std::mem::take(&mut queue.entries);
    std::fs::write(path, serde_json::to_vec(&queue)?)?;
    Ok(entries)
}

pub(super) fn execute(workspace: &str, request: &Value) -> Value {
    match execute_inner(workspace, request) {
        Ok(snapshot) => json!({
            "type": "input_queue",
            "value": snapshot
        }),
        Err(error) => json!({
            "type": "error",
            "value": format!("{error:#}")
        }),
    }
}

fn execute_inner(workspace: &str, request: &Value) -> anyhow::Result<Snapshot> {
    anyhow::ensure!(request["expected_workspace"] == workspace, "工作区已切换");
    let directory = Path::new(workspace);
    let path = directory.join(".fixture-input-queue.json");
    let mut queue: Queue = match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Queue {
            session: "demo".into(),
            run: Some(71),
            entries: if directory
                .join(".fixture-input-queue")
                .exists()
            {
                (1..=35).map(|index| Entry {
files: Vec::new(),
                    images: vec![],
                    id: index * 7,
                    content: format!("第 {index} 条补充：先检查现有状态，再执行并验证。\n这段文本用于检查选择、修改和保留草稿。"),
                    state: match index {
                        2 => State::Claimed,
                        3 => State::Finishing,
                        4 => State::Unavailable,
                        _ => State::Queued,
                    },
                }).collect()
            } else {
                Vec::new()
            },
        },
        Err(error) => return Err(error.into()),
    };
    anyhow::ensure!(request["session"] == queue.session, "消息不属于当前会话");
    if std::fs::remove_file(directory.join(".fixture-input-queue-consume-once")).is_ok()
        && let Some(entry) = queue
            .entries
            .iter_mut()
            .find(|entry| entry.editable())
    {
        entry.state = State::Claimed;
        std::fs::write(&path, serde_json::to_vec(&queue)?)?;
    }
    let action: Action = serde_json::from_value(request["action"].clone())?;
    let mut unknown_result = false;
    let snapshot = match action {
        Action::List { after } => {
            let mut entries: Vec<_> = queue
                .entries
                .iter()
                .filter(|entry| after.is_none_or(|after| entry.id > after))
                .take(33)
                .cloned()
                .collect();
            let next = (entries.len() > 32).then(|| entries[31].id);
            entries.truncate(32);
            Snapshot::List {
                entries,
                next,
                run: queue.run,
            }
        }
        Action::Edit {
            id,
            expected,
            content,
            run,
        } => {
            anyhow::ensure!(run == queue.run, "当前运行已变化，请刷新后重试");
            let entry = queue
                .entries
                .iter_mut()
                .find(|entry| entry.id == id)
                .ok_or_else(|| anyhow::anyhow!("消息已移除"))?;
            anyhow::ensure!(entry.editable(), "消息已被取走，无法继续修改");
            anyhow::ensure!(entry.content == expected, "消息内容已变化，请刷新后重试");
            anyhow::ensure!(!content.trim().is_empty(), "消息不能为空");
            anyhow::ensure!(content.trim().len() <= 1_024 * 1_024, "消息过长");
            if std::fs::remove_file(directory.join(".fixture-input-queue-fail-once")).is_ok() {
                anyhow::bail!("测试保存未确认；草稿应保留，请刷新核对后重试。");
            }
            entry.content = content.trim().into();
            unknown_result =
                std::fs::remove_file(directory.join(".fixture-input-queue-unknown-once")).is_ok();
            Snapshot::Edited {
                entry: entry.clone(),
            }
        }
        Action::Remove { id, expected, run } => {
            anyhow::ensure!(run == queue.run, "当前运行已变化，请刷新后重试");
            let entry = queue
                .entries
                .iter()
                .find(|entry| entry.id == id)
                .ok_or_else(|| anyhow::anyhow!("消息已移除"))?;
            anyhow::ensure!(entry.editable(), "消息已被取走，无法移除");
            anyhow::ensure!(entry.content == expected, "消息内容已变化，请刷新后重试");
            queue
                .entries
                .retain(|entry| entry.id != id);
            Snapshot::Removed { id }
        }
    };
    std::fs::write(path, serde_json::to_vec(&queue)?)?;
    anyhow::ensure!(
        !unknown_result,
        "测试回执已丢失；请刷新核对服务端内容，不要自动重发。"
    );
    Ok(snapshot)
}
