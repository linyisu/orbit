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
                    project.path.as_deref().unwrap_or("（未关联目录）")
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Action::LinkProject { id } => {
            let project = store.link_project(&id, directory).await?;
            format!(
                "已关联项目 {} → {}。",
                project.id,
                project.path.unwrap_or_default()
            )
        }
        Action::Sessions => crate::session::list_text(store).await?,
        Action::SessionStorage => store
            .session_storage_usage()
            .await?
            .into_iter()
            .map(|usage| {
                format!(
                    "{}  {:>12} B  {}",
                    usage.id,
                    usage.payload_bytes,
                    usage.title.replace(['\n', '\r', '\t'], " ")
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
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
