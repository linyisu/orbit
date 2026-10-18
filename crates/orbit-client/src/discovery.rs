use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use tokio::process::Command;

const CACHE_TTL: Duration = Duration::from_secs(30);
const MAX_CONFIG_FILES: usize = 64;

#[derive(Clone, Debug, Default)]
pub struct Host {
    pub name: String,
    pub hostname: String,
    pub user: String,
    pub port: String,
    pub identity: String,
}

#[derive(Default)]
struct Cache {
    hosts: Vec<Host>,
    distributions: Vec<String>,
    checked: Option<Instant>,
    loading: bool,
}
static REVISION: AtomicU64 = AtomicU64::new(0);

pub fn revision() -> u64 {
    REVISION.load(Ordering::Relaxed)
}

static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();

pub fn snapshot() -> (Vec<Host>, Vec<String>) {
    let cache = CACHE.get_or_init(Mutex::default);
    let Ok(mut state) = cache.lock() else {
        return Default::default();
    };
    if !state.loading
        && state
            .checked
            .is_none_or(|time| time.elapsed() >= CACHE_TTL)
        && let Ok(runtime) = tokio::runtime::Handle::try_current()
    {
        state.loading = true;
        runtime.spawn(async {
            let hosts = tokio::task::spawn_blocking(|| {
                let Some(home) = orbit_config::directories::home() else {
                    return Vec::new();
                };
                hosts(&home.join(".ssh/config"), &home)
            })
            .await
            .unwrap_or_default();
            // Publish SSH aliases before waiting for WSL, which may not be installed.
            if let Ok(mut state) = cache.lock() {
                state.hosts = hosts;
                REVISION.fetch_add(1, Ordering::Relaxed);
            }
            let distributions = distributions().await;
            if let Ok(mut state) = cache.lock() {
                state.distributions = distributions;
                state.checked = Some(Instant::now());
                state.loading = false;
                REVISION.fetch_add(1, Ordering::Relaxed);
            }
        });
    }
    (state.hosts.clone(), state.distributions.clone())
}

pub fn hosts(config: &Path, home: &Path) -> Vec<Host> {
    let mut lines = Vec::new();
    let mut visited = BTreeSet::new();
    collect(config, home, &mut visited, &mut lines);
    let names: BTreeSet<_> = lines
        .iter()
        .filter(|(key, _)| key == "host")
        .flat_map(|(_, words)| words.iter())
        .filter(|word| !word.is_empty() && !word.contains(['*', '?', '!', '[', ']']))
        .cloned()
        .collect();
    names
        .into_iter()
        .map(|name| {
            let mut host = Host {
                name,
                ..Host::default()
            };
            let mut active = true;
            for (key, words) in &lines {
                if key == "host" {
                    active = words
                        .iter()
                        .any(|pattern| matches_host(pattern, &host.name))
                        && !words.iter().any(|pattern| {
                            pattern
                                .strip_prefix('!')
                                .is_some_and(|pattern| matches_host(pattern, &host.name))
                        });
                } else if key == "match" {
                    active = false;
                } else if active && let Some(value) = words.first() {
                    let field = match key.as_str() {
                        "hostname" => &mut host.hostname,
                        "user" => &mut host.user,
                        "port" => &mut host.port,
                        "identityfile" => &mut host.identity,
                        _ => continue,
                    };
                    if field.is_empty() {
                        field.clone_from(value);
                    }
                }
            }
            host
        })
        .collect()
}

fn matches_host(pattern: &str, host: &str) -> bool {
    !pattern.starts_with('!')
        && globset::Glob::new(pattern).is_ok_and(|glob| glob.compile_matcher().is_match(host))
}

fn collect(
    path: &Path,
    home: &Path,
    visited: &mut BTreeSet<PathBuf>,
    lines: &mut Vec<(String, Vec<String>)>,
) {
    let Ok(path) = path.canonicalize() else {
        return;
    };
    if visited.len() >= MAX_CONFIG_FILES || !visited.insert(path.clone()) {
        return;
    }
    let Ok(bytes) = std::fs::read(&path) else {
        return;
    };
    if bytes.len() > 1_024 * 1_024 {
        return;
    }
    for line in String::from_utf8_lossy(&bytes).lines() {
        let line = line.trim();
        let Some(split) = line.find(|c: char| c.is_whitespace() || c == '=') else {
            continue;
        };
        let key = line[..split].to_ascii_lowercase();
        let words =
            shlex::split(line[split..].trim_start_matches(|c: char| c.is_whitespace() || c == '='))
                .unwrap_or_default();
        if key == "include" {
            for word in words {
                let path = if let Some(suffix) = word.strip_prefix("~/") {
                    home.join(suffix)
                } else if Path::new(&word).is_absolute() {
                    word.into()
                } else {
                    home.join(".ssh").join(word)
                };
                for path in expand(&path) {
                    collect(&path, home, visited, lines);
                }
            }
        } else {
            lines.push((key, words));
        }
    }
}

fn expand(pattern: &Path) -> Vec<PathBuf> {
    let mut paths = vec![PathBuf::new()];
    for component in pattern.components() {
        let part = component.as_os_str().to_string_lossy();
        if part.contains(['*', '?', '[']) {
            let Ok(glob) = globset::Glob::new(&part) else {
                return vec![];
            };
            let matcher = glob.compile_matcher();
            paths = paths
                .into_iter()
                .flat_map(|path| {
                    std::fs::read_dir(path)
                        .into_iter()
                        .flatten()
                        .filter_map(Result::ok)
                        .filter(|entry| matcher.is_match(entry.file_name()))
                        .map(|entry| entry.path())
                })
                .take(MAX_CONFIG_FILES)
                .collect();
        } else {
            for path in &mut paths {
                path.push(component);
            }
        }
    }
    paths.sort();
    paths
}

async fn distributions() -> Vec<String> {
    let mut command = Command::new("wsl.exe");
    command
        .args(["--list", "--quiet"])
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let Ok(Ok(output)) = tokio::time::timeout(Duration::from_secs(3), command.output()).await
    else {
        return vec![];
    };
    if !output.status.success() {
        return vec![];
    }
    decode_distributions(&output.stdout)
}

fn decode_distributions(bytes: &[u8]) -> Vec<String> {
    let text = if bytes.contains(&0) {
        String::from_utf16_lossy(
            &bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect::<Vec<_>>(),
        )
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    };
    text.trim_start_matches('\u{feff}')
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn config_aliases_follow_includes_and_first_values_without_suggesting_patterns() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path();
        std::fs::create_dir_all(home.join(".ssh/conf.d")).unwrap();
        std::fs::write(
            home.join(".ssh/config"),
            "Include conf.d/*\nHost 204 other\n \
            HostName=10.0.0.2\n User alice\n Port 2222\nHost *\n User default\n",
        )
        .unwrap();
        std::fs::write(
            home.join(".ssh/conf.d/dev"),
            "Host dev\n User bob\n IdentityFile ~/.ssh/id_dev\n",
        )
        .unwrap();
        let hosts = hosts(&home.join(".ssh/config"), home);
        assert_eq!(
            hosts
                .iter()
                .map(|host| host.name.as_str())
                .collect::<Vec<_>>(),
            ["204", "dev", "other"]
        );
        assert_eq!(hosts[0].user, "alice");
        assert_eq!(hosts[0].port, "2222");
        assert_eq!(hosts[1].user, "bob");
    }

    #[test]
    fn wsl_list_handles_utf16_and_utf8() {
        let bytes: Vec<_> = "\u{feff}Ubuntu\r\nDebian\r\n"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(decode_distributions(&bytes), ["Ubuntu", "Debian"]);
        assert_eq!(decode_distributions(b"Ubuntu\n"), ["Ubuntu"]);
    }
}
