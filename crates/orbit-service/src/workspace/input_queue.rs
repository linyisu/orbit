//! User instructions use the existing steering policy, not file-write policy.
use anyhow::{Result, ensure};
use orbit_agent::{inbox::AgentInbox, tool::ToolError};
use orbit_protocol::input_queue::{Action, Entry, Snapshot, State};

use crate::PreparedWorkspace;

const PAGE_INPUT_BYTES: usize = 4 * 1_024 * 1_024;

pub(super) async fn execute(
    setup: &mut PreparedWorkspace,
    inbox: Option<AgentInbox>,
    expected_workspace: &str,
    action: Action,
) -> Result<Snapshot> {
    ensure!(
        expected_workspace == setup.session.snapshot.session.workspace,
        "工作区已切换，请刷新输入队列"
    );
    let run = setup.session.active_run_id();
    match action {
        Action::List { after } => {
            ensure!(
                after.is_none_or(|after| after >= 0),
                "invalid input queue cursor"
            );
            if !setup.session.is_persisted() {
                return Ok(Snapshot::List {
                    entries: vec![],
                    next: None,
                    run: None,
                });
            }
            let live = match inbox {
                Some(inbox) => Some(inbox.snapshot().await),
                None => None,
            };
            let stored = setup
                .session
                .store
                .input_queue(setup.session.id())
                .await?;
            ensure!(
                stored.workspace == expected_workspace,
                "工作区已切换，请刷新输入队列"
            );
            let mut entries = Vec::new();
            let mut bytes = 0;
            let mut next = None;
            for message in stored
                .messages
                .into_iter()
                .filter(|message| message.id > after.unwrap_or(0))
            {
                let state = if let Some(live) = &live {
                    if stored.run != run {
                        State::Unavailable
                    } else if live.closed {
                        State::Finishing
                    } else if let Some(queued) = live
                        .messages
                        .iter()
                        .find(|queued| queued.id == message.id)
                    {
                        if queued.content == message.content {
                            State::Queued
                        } else {
                            State::Unavailable
                        }
                    } else {
                        State::Claimed
                    }
                } else if stored.run.is_some() {
                    State::Unavailable
                } else {
                    State::Idle
                };
                let metadata =
                    |attachment: &orbit_model::types::Attachment| orbit_protocol::MessageImage {
                        name: attachment.name().into(),
                        path: attachment
                            .path()
                            .to_string_lossy()
                            .into_owned(),
                    };
                let entry = Entry {
                    delivery: if live
                        .as_ref()
                        .is_none_or(|live| live.deferred_ids.contains(&message.id))
                    {
                        orbit_protocol::input_queue::Delivery::NextRun
                    } else {
                        orbit_protocol::input_queue::Delivery::AtBoundary
                    },
                    files: message
                        .attachments
                        .iter()
                        .filter(|a| !a.is_image())
                        .map(metadata)
                        .collect(),
                    images: message
                        .attachments
                        .iter()
                        .filter(|a| a.is_image())
                        .map(|image| orbit_protocol::MessageImage {
                            name: image.name().into(),
                            path: image
                                .path()
                                .to_string_lossy()
                                .into_owned(),
                        })
                        .collect(),
                    id: message.id,
                    content: message.content,
                    state,
                };
                let size = serde_json::to_vec(&entry)?.len();
                // A legal 1MiB input may expand to 6MiB under JSON escaping;
                // allow one such row, while keeping a multi-row page bounded.
                ensure!(
                    size < orbit_protocol::MAX_FRAME_BYTES - 4_096,
                    "queued input is too large to inspect safely"
                );
                if !entries.is_empty() && bytes + size > PAGE_INPUT_BYTES {
                    next = entries
                        .last()
                        .map(|entry: &Entry| entry.id);
                    break;
                }
                bytes += size;
                entries.push(entry);
            }
            Ok(Snapshot::List { entries, next, run })
        }
        Action::Steer {
            id,
            expected,
            run: expected_run,
        } => {
            ensure!(
                run == expected_run && run.is_some(),
                "任务轮次已改变，请刷新队列"
            );
            let inbox = inbox.ok_or_else(|| anyhow::anyhow!("当前没有正在运行的任务"))?;
            inbox.steer(id, &expected).await?;
            Ok(Snapshot::Steered { id })
        }
        Action::Edit {
            id,
            expected,
            content,
            run: expected_run,
        } => {
            ensure!(
                run == expected_run,
                "任务轮次已改变，请刷新输入队列后重新编辑"
            );
            let content = content.trim().to_owned();
            ensure!(
                !content.is_empty() && content.len() <= 1_024 * 1_024,
                "消息需要 1–1048576 字节"
            );
            let (state, delivery) = if let Some(inbox) = inbox {
                inbox
                    .persist_edit(id, &expected, content.clone(), || async {
                        setup
                            .session
                            .edit_queued_input(expected_workspace, id, &expected, content.clone())
                            .await
                            .map_err(tool_error)
                    })
                    .await?;
                (
                    State::Queued,
                    if inbox
                        .snapshot()
                        .await
                        .deferred_ids
                        .contains(&id)
                    {
                        orbit_protocol::input_queue::Delivery::NextRun
                    } else {
                        orbit_protocol::input_queue::Delivery::AtBoundary
                    },
                )
            } else {
                // ensure_writer reserves the process-wide writer lease before
                // touching an idle input; a foreign live service makes it fail.
                setup
                    .session
                    .edit_queued_input(expected_workspace, id, &expected, content.clone())
                    .await?;
                (State::Idle, orbit_protocol::input_queue::Delivery::NextRun)
            };
            let attachments = setup
                .session
                .store
                .pending_agent_messages(setup.session.id())
                .await?
                .into_iter()
                .find(|message| message.id == id)
                .map(|message| message.attachments)
                .unwrap_or_default();
            let metadata =
                |attachment: &orbit_model::types::Attachment| orbit_protocol::MessageImage {
                    name: attachment.name().into(),
                    path: attachment
                        .path()
                        .to_string_lossy()
                        .into_owned(),
                };
            Ok(Snapshot::Edited {
                entry: Entry {
                    delivery,
                    id,
                    content,
                    state,
                    images: attachments
                        .iter()
                        .filter(|a| a.is_image())
                        .map(metadata)
                        .collect(),
                    files: attachments
                        .iter()
                        .filter(|a| !a.is_image())
                        .map(metadata)
                        .collect(),
                },
            })
        }
        Action::Remove {
            id,
            expected,
            run: expected_run,
        } => {
            ensure!(
                run == expected_run,
                "任务轮次已改变，请刷新输入队列后重新撤销"
            );
            if let Some(inbox) = inbox {
                inbox
                    .persist_remove(id, &expected, || async {
                        setup
                            .session
                            .remove_queued_input(expected_workspace, id, &expected)
                            .await
                            .map_err(tool_error)
                    })
                    .await?;
            } else {
                setup
                    .session
                    .remove_queued_input(expected_workspace, id, &expected)
                    .await?;
            }
            Ok(Snapshot::Removed { id })
        }
    }
}

fn tool_error(error: anyhow::Error) -> ToolError {
    ToolError::ExecutionFailed(format!("{error:#}"))
}

#[cfg(test)]
mod tests;
