//! Bubblewrap namespace, system mount and seccomp configuration.
use std::{
    io::{Seek, Write},
    path::Path,
    process::Stdio,
};

use seccompiler::{BpfProgram, SeccompAction, SeccompFilter};
use tokio::process::Command;

use crate::{
    capability::NetworkPolicy,
    error::{RuntimeError, failed},
    execution::process::TMP_SIZE_BYTES,
};

pub(super) fn configure_isolation(command: &mut Command, network: NetworkPolicy) {
    command.args([
        "--seccomp",
        "0",
        "--unshare-user",
        "--disable-userns",
        "--die-with-parent",
        "--new-session",
        "--cap-drop",
        "ALL",
        "--unshare-pid",
        "--unshare-ipc",
        "--unshare-uts",
        "--unshare-cgroup-try",
    ]);

    if network == NetworkPolicy::Deny {
        command.arg("--unshare-net");
    }
}

/// A browser reader gets system libraries and one disposable profile, never the workspace or host home.
pub fn browser_reader_command(profile: &Path) -> Result<Command, RuntimeError> {
    let mut command = Command::new("/usr/bin/bwrap");
    command
        .env_clear()
        .arg("--clearenv")
        .stdin(Stdio::from(seccomp_filter()?))
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    configure_isolation(&mut command, NetworkPolicy::Allow);
    mount_system_paths(&mut command, NetworkPolicy::Allow);
    command
        .arg("--bind")
        .arg(profile)
        .arg("/orbit-browser")
        .args([
            "--size",
            &TMP_SIZE_BYTES.to_string(),
            "--tmpfs",
            "/tmp",
            "--proc",
            "/proc",
            "--dev",
            "/dev",
        ])
        .args([
            "--setenv",
            "PATH",
            "/usr/bin:/bin",
            "--setenv",
            "HOME",
            "/orbit-browser",
            "--setenv",
            "XDG_CONFIG_HOME",
            "/orbit-browser",
            "--setenv",
            "XDG_CACHE_HOME",
            "/orbit-browser",
            "--setenv",
            "LANG",
            "C.UTF-8",
            "--chdir",
            "/orbit-browser",
            "--",
            "/usr/bin/firefox",
        ]);
    Ok(command)
}

pub(super) fn seccomp_filter() -> Result<std::fs::File, RuntimeError> {
    let blocked = [
        libc::SYS_ptrace,
        libc::SYS_bpf,
        libc::SYS_perf_event_open,
        libc::SYS_keyctl,
        libc::SYS_add_key,
        libc::SYS_request_key,
        libc::SYS_mount,
        libc::SYS_umount2,
        libc::SYS_pivot_root,
        libc::SYS_setns,
        libc::SYS_unshare,
        libc::SYS_init_module,
        libc::SYS_finit_module,
        libc::SYS_delete_module,
        libc::SYS_kexec_load,
        libc::SYS_open_by_handle_at,
    ];
    let rules = blocked
        .into_iter()
        .map(|syscall| (syscall, vec![]))
        .collect();
    let architecture = std::env::consts::ARCH.try_into().map_err(failed)?;
    let filter = SeccompFilter::new(
        rules,
        SeccompAction::Allow,
        SeccompAction::Errno(libc::EPERM as u32),
        architecture,
    )
    .map_err(failed)?;
    let program: BpfProgram = filter.try_into().map_err(failed)?;
    let mut file = tempfile::tempfile().map_err(failed)?;

    for instruction in program {
        file.write_all(&instruction.code.to_ne_bytes())
            .map_err(failed)?;
        file.write_all(&[instruction.jt, instruction.jf])
            .map_err(failed)?;
        file.write_all(&instruction.k.to_ne_bytes())
            .map_err(failed)?;
    }

    file.rewind().map_err(failed)?;

    Ok(file)
}

pub(super) fn mount_system_paths(command: &mut Command, network: NetworkPolicy) {
    for path in ["/usr", "/bin", "/sbin", "/lib", "/lib64"] {
        if Path::new(path).exists() {
            command.args(["--ro-bind", path, path]);
        }
    }

    if Path::new("/etc/ld.so.cache").exists() {
        command.args(["--ro-bind", "/etc/ld.so.cache", "/etc/ld.so.cache"]);
    }

    if network == NetworkPolicy::Allow {
        for path in [
            "/etc/resolv.conf",
            "/etc/hosts",
            "/etc/ssl",
            "/etc/pki",
            "/etc/ca-certificates",
        ] {
            if Path::new(path).exists() {
                command.args(["--ro-bind", path, path]);
            }
        }
    }
}
