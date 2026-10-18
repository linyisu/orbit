//! Compatibility readers for client snapshots written before shared restoration.
use super::*;

/// Locate the pre-shared desktop view file, including explicitly isolated preference profiles.
pub(super) fn desktop_state_path(preferences: Option<&Path>) -> Result<PathBuf> {
    if let Some(preferences) = preferences {
        return Ok(preferences.with_extension("tasks.json"));
    }
    Ok(orbit_config::directories::state()
        .ok_or_else(|| anyhow::anyhow!("找不到任务保存目录，请指定 --preferences"))?
        .join("orbit/gui-tasks.json"))
}

/// Read the retired desktop session source only at migration; view-specific fields stay opaque.
pub(super) fn legacy_desktop(path: &Path, profile: &Profile) -> Result<Option<LegacyTabs>> {
    #[derive(Deserialize)]
    struct Snapshot {
        origin: Options,
        active: String,
        tasks: Vec<Task>,
    }
    #[derive(Deserialize)]
    struct Task {
        key: String,
        options: Options,
        title: String,
        text: String,
        paths: Vec<PathBuf>,
        #[serde(default)]
        attachment_names: BTreeMap<PathBuf, String>,
    }
    #[derive(Deserialize)]
    struct Options {
        config: PathBuf,
        database: Option<PathBuf>,
        auth_dir: Option<PathBuf>,
        workspace: Option<PathBuf>,
        resume: Option<String>,
        server: Option<String>,
        ssh: Option<String>,
        ssh_user: Option<String>,
        ssh_port: Option<u16>,
        identity: Option<String>,
        wsl: Option<String>,
        wsl_user: Option<String>,
    }
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let saved: Snapshot = serde_json::from_slice(&bytes)?;
    if saved.origin.config != profile.config
        || saved.origin.database != profile.database
        || crate::configuration::auth_directory(saved.origin.auth_dir)? != profile.auth
    {
        return Ok(None);
    }
    let active = saved
        .tasks
        .iter()
        .position(|task| task.key == saved.active);
    let tabs = saved
        .tasks
        .into_iter()
        .map(|task| -> Result<_> {
            let mut connection = crate::ConnectionOptions::local(
                task.options
                    .workspace
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
            );
            connection.ssh = task.options.ssh;
            connection.ssh_user = task.options.ssh_user;
            connection.ssh_port = task.options.ssh_port;
            connection.identity = task.options.identity;
            connection.wsl = task.options.wsl;
            connection.wsl_user = task.options.wsl_user;
            Ok(SavedTab {
                id: task.options.resume,
                key: task.key,
                title: task.title,
                target: connection.target()?,
                workspace: connection.workspace,
                config: task.options.config,
                auth: crate::configuration::auth_directory(task.options.auth_dir)?,
                database: task.options.database,
                server: task.options.server,
                allow_mcp_start: false,
                draft: Draft {
                    text: task.text,
                    attachments: task
                        .paths
                        .into_iter()
                        .map(|path| DraftAttachment {
                            name: task
                                .attachment_names
                                .get(&path)
                                .cloned()
                                .unwrap_or_else(|| {
                                    path.file_name()
                                        .unwrap_or_default()
                                        .to_string_lossy()
                                        .into_owned()
                                }),
                            path,
                        })
                        .collect(),
                    leases: Vec::new(),
                },
            })
        })
        .collect::<Result<_>>()?;
    Ok(Some(LegacyTabs {
        source: path.into(),
        tabs,
        active,
    }))
}
