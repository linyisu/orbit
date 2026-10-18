//! Human-readable projections of durable state and command-management receipts.
use serde_json::Value;

use super::describe;

pub(super) fn present(name: &str, value: &Value) -> Option<String> {
    let mut blocks = Vec::new();
    match name {
        "ListTasks" | "UpdateTask" => {
            let tasks = records(value, "tasks", "task")?;
            for task in tasks {
                blocks.push(format!(
                    "#{} · {} · {}\n{}",
                    task["id"],
                    state(text(task, "status")),
                    text(task, "title"),
                    text(task, "details")
                ));
                if let Some(ids) = task["blocked_by"]
                    .as_array()
                    .filter(|ids| !ids.is_empty())
                {
                    blocks.push(format!(
                        "等待任务：{}",
                        ids.iter()
                            .map(Value::to_string)
                            .collect::<Vec<_>>()
                            .join("、")
                    ));
                }
            }
            if blocks.is_empty() {
                blocks.push("没有任务".into());
            }
        }
        "ListMemories" | "SearchMemory" | "SaveMemory" | "ForgetMemory" => {
            let memories = records(value, "memories", "memory")?;
            for memory in memories {
                let category = match text(memory, "category") {
                    "constraint" => "约束",
                    "decision" => "决定",
                    "fact" => "事实",
                    "preference" => "偏好",
                    other => other,
                };
                blocks.push(format!(
                    "#{} · {category}{} · {}\n{}",
                    memory["id"],
                    if memory["active"] == false {
                        " · 已停用"
                    } else {
                        ""
                    },
                    text(memory, "title"),
                    text(memory, "content")
                ));
            }
            if blocks.is_empty() {
                blocks.push("没有记忆".into());
            }
        }
        "CommandStatus" | "WaitCommands" | "StopCommands" => {
            for job in value["jobs"].as_array()? {
                blocks.push(format!(
                    "{}\n{} · {} ms",
                    text(job, "command"),
                    state(text(job, "status")),
                    job["elapsed_ms"]
                ));
                if let Some(result) = job["result"].as_object() {
                    for key in ["stdout", "stderr"] {
                        if let Some(content) = result
                            .get(key)
                            .and_then(Value::as_str)
                            .filter(|text| !text.is_empty())
                        {
                            blocks.push(format!("{key}\n{content}"));
                        }
                    }
                    if let Some(error) = result.get("error") {
                        let mut detail = String::new();
                        describe(error, 0, &mut detail);
                        blocks.push(format!("错误\n{detail}"));
                    }
                    if let Some(code) = result
                        .get("exit_code")
                        .filter(|code| !code.is_null())
                    {
                        blocks.push(format!("退出码 {code}"));
                    }
                }
            }
            if blocks.is_empty() {
                blocks.push("没有后台命令".into());
            }
            if value["wait_timed_out"] == true {
                blocks.push("等待已超时，命令状态见上方".into());
            }
        }
        "CheckEvidence" => {
            let commands = value["commands"].as_array()?;
            let changes = value["changes"].as_array()?;
            blocks.push(format!(
                "{} 项文件变更 · {} 条命令记录",
                changes.len(),
                value["command_count"]
            ));
            if value["baseline_available"] == false {
                blocks.push("未记录会话基线".into());
            }
            for change in changes {
                blocks.push(format!(
                    "{} · {} · {}",
                    text(change, "path"),
                    text(change, "change_kind"),
                    state(text(change, "origin"))
                ));
            }
            for command in commands {
                blocks.push(format!(
                    "{}\n{} · 退出码 {}",
                    text(command, "command"),
                    state(text(command, "input_state")),
                    command["exit_code"]
                ));
            }
            if let Some(candidates) = value["verification_candidates"].as_array() {
                for candidate in candidates {
                    if let Some(argv) = candidate["argv"].as_array() {
                        blocks.push(format!(
                            "建议验证（{}）\n{}",
                            text(candidate, "cwd"),
                            argv.iter()
                                .filter_map(Value::as_str)
                                .collect::<Vec<_>>()
                                .join(" ")
                        ));
                    }
                }
            }
            blocks.push("命令退出码和文件指纹仅供核对，不能单独证明测试通过。".into());
        }
        "ReadHistory" => {
            if let Some(archives) = value["archives"].as_array() {
                for archive in archives {
                    blocks.push(format!(
                        "归档 #{} · {} 条消息",
                        archive["archive_id"], archive["message_count"]
                    ));
                }
                if archives.is_empty() {
                    blocks.push("没有已归档的历史消息".into());
                }
            } else {
                for item in value["items"].as_array()? {
                    let serialized = item["serialized_message"].as_str()?;
                    let message = serde_json::from_str::<Value>(serialized).ok();
                    blocks.push(format!(
                        "消息 {}\n{}",
                        item["message"],
                        message
                            .as_ref()
                            .and_then(|message| message["content"].as_str())
                            .unwrap_or(serialized)
                    ));
                }
            }
        }
        _ => return None,
    }
    // Cursors and fingerprints are transport details; preserve actual diagnostic errors.
    if let Some(error) = value.get("error") {
        let mut text = String::new();
        describe(error, 0, &mut text);
        blocks.push(text);
    }
    Some(
        blocks
            .into_iter()
            .map(|block| block.trim_end().to_owned())
            .collect::<Vec<_>>()
            .join("\n\n"),
    )
}

fn records<'a>(value: &'a Value, plural: &str, single: &str) -> Option<Vec<&'a Value>> {
    if let Some(records) = value[plural].as_array() {
        Some(records.iter().collect())
    } else {
        value
            .get(single)
            .filter(|record| record.is_object())
            .map(|record| vec![record])
    }
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or_default()
}

fn state(value: &str) -> &str {
    match value {
        "pending" => "待处理",
        "in_progress" | "running" => "执行中",
        "blocked" => "受阻",
        "completed" => "已完成",
        "cancelling" => "正在停止",
        "cancelled" => "已取消",
        "failed" => "失败",
        "input_unchanged" => "输入未变化",
        "stale" => "验证后已有变更",
        "unknown" => "未记录输入版本",
        "content_matches_orbit_receipt" => "与工具变更一致",
        "content_modified_after_orbit" => "工具变更后再次修改",
        "external_change" => "外部变更",
        other => other,
    }
}
