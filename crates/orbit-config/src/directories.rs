//! Native user directories shared by clients and services. Explicit XDG overrides remain supported.
use std::path::PathBuf;

fn absolute_environment(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

/// Resolve the current native user's home without requiring a Unix compatibility environment.
pub fn home() -> Option<PathBuf> {
    #[cfg(windows)]
    if let Some(path) = absolute_environment("USERPROFILE") {
        return Some(path);
    }
    absolute_environment("HOME")
}

/// Resolve configuration storage, preserving existing Unix locations.
pub fn config() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            #[cfg(windows)]
            if let Some(path) = absolute_environment("APPDATA") {
                return Some(path);
            }
            home().map(|path| {
                path.join(if cfg!(windows) {
                    "AppData/Roaming"
                } else {
                    ".config"
                })
            })
        })
}

/// Resolve cache storage; Windows does not require HOME or XDG variables.
pub fn cache() -> Option<PathBuf> {
    absolute_environment("XDG_CACHE_HOME").or_else(|| {
        #[cfg(windows)]
        if let Some(path) = absolute_environment("LOCALAPPDATA") {
            return Some(path);
        }
        home().map(|path| {
            path.join(if cfg!(windows) {
                "AppData/Local"
            } else {
                ".cache"
            })
        })
    })
}

/// Resolve local application state while retaining existing Unix state paths.
pub fn state() -> Option<PathBuf> {
    absolute_environment("XDG_STATE_HOME").or_else(|| {
        if cfg!(windows) {
            cache().map(|path| path.join("State"))
        } else {
            home().map(|path| path.join(".local/state"))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore]
    fn isolated_native_directories() {
        let Some(root) = std::env::var_os("ORBIT_DIRECTORY_TEST_ROOT").map(PathBuf::from) else {
            return;
        };
        assert_eq!(home(), Some(root.join("home")));
        #[cfg(windows)]
        {
            assert_eq!(config(), Some(root.join("roaming")));
            assert_eq!(cache(), Some(root.join("local")));
            assert_eq!(state(), Some(root.join("local/State")));
        }
        #[cfg(unix)]
        {
            assert_eq!(config(), Some(root.join("home/.config")));
            assert_eq!(cache(), Some(root.join("home/.cache")));
            assert_eq!(state(), Some(root.join("home/.local/state")));
        }
    }

    #[test]
    fn native_directories_work_without_xdg_or_unix_home_on_windows() {
        let root = tempfile::tempdir().unwrap();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--ignored",
                "--exact",
                "directories::tests::isolated_native_directories",
            ])
            .env("ORBIT_DIRECTORY_TEST_ROOT", root.path())
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("XDG_CACHE_HOME")
            .env_remove("XDG_STATE_HOME")
            .env("USERPROFILE", root.path().join("home"))
            .env("APPDATA", root.path().join("roaming"))
            .env("LOCALAPPDATA", root.path().join("local"));
        if cfg!(windows) {
            command.env_remove("HOME");
        } else {
            command.env("HOME", root.path().join("home"));
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}
