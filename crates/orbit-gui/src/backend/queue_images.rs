//! Resolve host attachments to client assets before allowing withdrawal/editing.
use super::*;
use std::collections::HashMap;
pub(super) async fn localize(
    client: &mut Client,
    session: &str,
    response: &mut Response,
    cache: &mut HashMap<(String, String), String>,
    database: &std::path::Path,
) -> Result<()> {
    let Response::InputQueue(snapshot) = response else {
        return Ok(());
    };
    let entries = match snapshot {
        orbit_protocol::input_queue::Snapshot::List { entries, .. } => entries.as_mut_slice(),
        orbit_protocol::input_queue::Snapshot::Edited { entry } => std::slice::from_mut(entry),
        _ => return Ok(()),
    };
    for entry in entries {
        let mut images = Vec::new();
        for image in &entry.images {
            let key = (session.to_owned(), image.path.clone());
            let path = if let Some(path) = cache.get(&key) {
                path.clone()
            } else {
                let data = match client
                    .request(Request::ReadFile {
                        session: session.into(),
                        path: image.path.clone(),
                    })
                    .await?
                {
                    Response::File(data) => data,
                    // Missing historical files do not block the rest of a message.
                    Response::Error(error)
                        if error.contains("No such file") || error.contains("not found") =>
                    {
                        continue;
                    }
                    Response::Error(error) => bail!(error),
                    _ => bail!("服务未返回暂存图片"),
                };
                let name = image.name.clone();
                let database = database.to_owned();
                let path = tokio::task::spawn_blocking(move || -> Result<String> {
                    let bytes = base64::engine::general_purpose::STANDARD.decode(data)?;
                    let image = orbit_tools::image_assets::ImageAssets::for_database(&database)
                        .import_bytes(name, bytes)?;
                    Ok(image
                        .path()
                        .to_string_lossy()
                        .into_owned())
                })
                .await??;
                cache.insert(key, path.clone());
                path
            };
            images.push(orbit_protocol::MessageImage {
                name: image.name.clone(),
                path,
            });
        }
        entry.images = images;
    }
    Ok(())
}

pub(super) fn cache_database(options: &Options) -> Result<PathBuf> {
    if let Some(path) = &options.preferences {
        return Ok(path.with_extension("queue-cache"));
    }
    Ok(std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .context("找不到图片缓存目录")?
        .join("orbit/gui-queue-cache"))
}
