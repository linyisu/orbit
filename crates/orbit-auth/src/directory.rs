use anyhow::{Context, Result, bail};
use std::path::PathBuf;

pub fn directory(
    explicit: Option<PathBuf>,
    config_home: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(std::path::absolute(path)?);
    }
    let config = config_home
        .filter(|path| !path.as_os_str().is_empty())
        .or_else(|| home.map(|home| home.join(".config")))
        .context("无法确定凭据目录，请设置 --auth-dir")?;
    if !config.is_absolute() {
        bail!("XDG_CONFIG_HOME 必须是绝对路径，或通过 --auth-dir 指定凭据目录");
    }

    Ok(config.join("orbit/auth"))
}
