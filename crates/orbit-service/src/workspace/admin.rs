use anyhow::Result;
use orbit_protocol::{
    Report,
    admin::{Action, SessionSection},
};
use orbit_store::store::Store;

pub(super) async fn execute(
    store: &Store,
    directory: &std::path::Path,
    action: Action,
) -> Result<Report> {
    let text = match action {
        Action::Projects => store
            .projects()
            .await?
            .into_iter()
            .map(|project| {
                format!(
                    "{}  {}",
                    project.id,
                    project
                        .path
                        .as_deref()
                        .unwrap_or("（未关联目录）")
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Action::LinkProject { id } => {
            let project = store
                .link_project(&id, directory)
                .await?;
            format!(
                "已关联项目 {} → {}。",
                project.id,
                project.path.unwrap_or_default()
            )
        }
        Action::Sessions => crate::session::list_text(store).await?,
        Action::SessionStorage => {
            let footprint = store.storage_footprint().await?;
            let mut lines = vec![format!(
                "文件大小快照（B）：数据库 {} · WAL {} · 共享内存 {} · 附件 {} · 会话命令日志 {}\n当前用户临时命令日志 {} \
                    B（跨数据库，任务结束清理）\n会话 ID  数据库载荷 B  关联图片 B  \
                    标题\n载荷不包含索引及空闲页；共享图片会出现在各个引用会话中，不能直接相加。",
                footprint.database_bytes,
                footprint.wal_bytes,
                footprint.shared_memory_bytes,
                footprint.attachment_bytes,
                footprint.old_command_output_bytes,
                footprint.temporary_command_output_bytes,
            )];
            lines.extend(
                store
                    .session_storage_usage()
                    .await?
                    .into_iter()
                    .map(|usage| {
                        format!(
                            "{}  {:>12}  {:>12}  {}",
                            usage.id,
                            usage.payload_bytes,
                            usage.attachment_bytes,
                            usage
                                .title
                                .replace(['\n', '\r', '\t'], " ")
                        )
                    }),
            );
            lines.join("\n")
        }
        Action::RenameSession { id, title } => {
            ensure_title(&title)?;
            store
                .acquire(&id)
                .await?
                .set_title(title.clone())
                .await?;
            title
        }
        Action::DeleteSession { id } => {
            let deleted = store.delete_session(&id).await?;
            let mut text = if deleted {
                "会话及私有文件已删除。"
            } else {
                "会话不存在；同 ID 的私有残留文件已清理。"
            }
            .to_owned();
            if let Err(error) = store.reclaim_unused_space().await {
                text.push_str(&format!("\n数据库空间回收失败：{error}"));
            }
            text
        }
        Action::InspectSession { id, section } => match section {
            SessionSection::History => serde_json::to_string_pretty(&store.load(&id).await?)?,
            SessionSection::Compactions => {
                serde_json::to_string_pretty(&store.compactions(&id).await?)?
            }
            SessionSection::Diagnostics => {
                orbit_store::diagnostics::text(&store.diagnostics(&id).await?)
            }
        },
    };
    Ok(Report {
        title: "工作区管理".into(),
        text,
    })
}

pub(super) fn ensure_title(title: &str) -> Result<()> {
    anyhow::ensure!(
        orbit_protocol::admin::valid_title(title),
        "会话标题应为 1–120 个字符"
    );
    Ok(())
}
