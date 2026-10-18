use std::{collections::BTreeMap, sync::OnceLock};
#[cfg(unix)]
use std::{path::Path, process::Stdio, time::Duration};

#[cfg(unix)]
use tokio::{io::AsyncReadExt, process::Command};

use super::environment::is_sensitive_key;
#[cfg(unix)]
use super::process::ProcessGroup;

#[cfg(any(unix, test))]
const MARKER: &[u8] = b"\0ORBIT-ENVIRONMENT\0";
#[cfg(unix)]
const MAX_BYTES: u64 = 1_024 * 1_024;
static ENVIRONMENT: OnceLock<BTreeMap<String, String>> = OnceLock::new();
static INITIALIZED: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();

pub fn snapshot() -> BTreeMap<String, String> {
    ENVIRONMENT
        .get()
        .cloned()
        .unwrap_or_else(inherited)
}

fn inherited() -> BTreeMap<String, String> {
    std::env::vars()
        .filter(|(key, _)| !is_sensitive_key(key))
        .collect()
}

/// Load the target user's login environment once without changing process-global variables.
pub async fn initialize() {
    INITIALIZED
        .get_or_init(|| async {
            let environment = inherited();
            #[cfg(unix)]
            let environment = {
                let mut environment = environment;
                let shell = environment
                    .get("SHELL")
                    .cloned()
                    .unwrap_or_else(|| "/bin/sh".into());
                let home = environment
                    .get("HOME")
                    .cloned()
                    .unwrap_or_else(|| "/".into());
                if let Ok(discovered) =
                    discover(Path::new(&shell), Path::new(&home), &environment).await
                {
                    environment.extend(discovered);
                }
                environment
            };
            let _ = ENVIRONMENT.set(environment);
        })
        .await;
}

#[cfg(unix)]
async fn discover(
    shell: &Path,
    home: &Path,
    environment: &BTreeMap<String, String>,
) -> std::io::Result<BTreeMap<String, String>> {
    let mut command = Command::new(shell);
    command
        .args([
            "-l",
            "-c",
            "printf '\\000ORBIT-ENVIRONMENT\\000'; /usr/bin/env -0",
        ])
        .current_dir(home)
        .env_clear()
        .envs(environment)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let (mut child, _group) = ProcessGroup::spawn(&mut command)?;
    let mut stdout = child
        .stdout
        .take()
        .unwrap()
        .take(MAX_BYTES + 1);
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).await?;
        let status = child.wait().await?;
        if !status.success() || bytes.len() as u64 > MAX_BYTES {
            return Err(std::io::Error::other("login environment unavailable"));
        }
        parse(&bytes)
    })
    .await;
    result.map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::TimedOut, "login environment timed out")
    })?
}

#[cfg(any(unix, test))]
fn parse(bytes: &[u8]) -> std::io::Result<BTreeMap<String, String>> {
    let start = bytes
        .windows(MARKER.len())
        .rposition(|window| window == MARKER)
        .ok_or_else(|| std::io::Error::other("login environment marker missing"))?
        + MARKER.len();
    Ok(bytes[start..]
        .split(|byte| *byte == 0)
        .filter_map(|item| {
            let item = std::str::from_utf8(item).ok()?;
            let (key, value) = item.split_once('=')?;
            (!key.is_empty() && !is_sensitive_key(key)).then(|| (key.to_owned(), value.to_owned()))
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_output_preserves_values_and_filters_credentials() {
        let environment = parse(
            b"startup banner\n\0ORBIT-ENVIRONMENT\0PATH=/tools:/usr/bin\0CUSTOM\
            =a=b\nnext\0MODEL_API_KEY=hidden\0ORBIT_INTERNAL=hidden\0SSH_AUTH_SOCK=/agent\0",
        )
        .unwrap();
        assert_eq!(environment["PATH"], "/tools:/usr/bin");
        assert_eq!(environment["CUSTOM"], "a=b\nnext");
        assert_eq!(environment["SSH_AUTH_SOCK"], "/agent");
        assert!(!environment.contains_key("MODEL_API_KEY"));
        assert!(!environment.contains_key("ORBIT_INTERNAL"));
        assert!(parse(b"no marker").is_err());
    }
}
