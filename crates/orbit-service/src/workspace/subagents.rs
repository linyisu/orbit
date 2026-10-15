//! The native child catalogue shares the store's ownership boundary and the
//! existing subagent tools' policy, inbox, and cancellation implementation.
use anyhow::{Context, Result, ensure};
use orbit_protocol::{
    Capability,
    subagents::{Action, Agent, Snapshot},
};

const PAGE_SIZE: usize = 32;
const MAX_MESSAGE_BYTES: usize = 32_768;

#[cfg(test)]
mod tests;

pub(super) async fn execute(
    setup: &mut crate::PreparedWorkspace,
    action: Action,
) -> Result<Snapshot> {
    ensure!(
        !setup
            .resources
            .policy
            .disabled(Capability::Subagents),
        "子 Agent 功能已禁用"
    );
    let parent = setup.session.id();
    let store = &setup.session.store;
    match action {
        Action::List { offset } => {
            ensure!(offset <= 20_000, "子 Agent 列表页码超出限制");
            let children = store.subagents(parent).await?;
            let total = children.len();
            let mut agents = Vec::new();
            for child in children
                .into_iter()
                .skip(offset)
                .take(PAGE_SIZE)
            {
                let revision = store
                    .session_revision(&child.id)
                    .await?
                    .context("子 Agent 已不存在，请刷新列表")?;
                agents.push(Agent {
                    id: child.id,
                    role: child.role,
                    model: revision.model,
                    task: child.task.chars().take(200).collect(),
                    workspace: revision.workspace,
                    status: if revision.status.is_empty() {
                        "idle".into()
                    } else {
                        revision.status
                    },
                    run: revision.run,
                });
            }
            let end = offset.saturating_add(agents.len());
            Ok(Snapshot::List {
                agents,
                next: (end < total).then_some(end),
            })
        }
        Action::Send { id, message } => {
            store
                .owned_subagent(parent, &id)
                .await?;
            let message = message.trim().to_owned();
            ensure!(
                !message.is_empty() && message.len() <= MAX_MESSAGE_BYTES,
                "消息需要 1–32768 字节"
            );
            setup
                .resources
                .subagents
                .as_ref()
                .context("当前会话不能向子 Agent 发送消息")?
                .send(&setup.resources.tools, id, message)
                .await
        }
        Action::Cancel { id, run } => {
            store
                .owned_subagent(parent, &id)
                .await?;
            let revision = store
                .session_revision(&id)
                .await?
                .context("子 Agent 已不存在，请刷新列表")?;
            ensure!(
                revision.run == run && run > 0,
                "子 Agent 已开始另一轮任务，请刷新后重试"
            );
            ensure!(
                revision.status == "running",
                "子 Agent 当前任务已结束，请刷新列表"
            );
            setup
                .resources
                .subagents
                .as_ref()
                .context("当前会话不能停止子 Agent")?
                .cancel(&setup.resources.tools, id.clone(), run)
                .await?;
            Ok(Snapshot::Stopping { id, run })
        }
    }
}
