//! Share one daemon per database, with an orderly handover between builds.
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::{
    io::Read,
    process::{Command, Stdio},
};

use super::*;

#[cfg(all(test, unix))]
mod tests;

pub(super) fn build_generation() -> u64 {
    env!("ORBIT_SERVICE_BUILD_GENERATION")
        .parse()
        .expect("build generation")
}

pub(super) async fn connect_or_start(
    socket: &Path,
    args: &ServiceOptions,
    config: &Path,
    database: &Path,
    auth: &Path,
) -> Result<Stream> {
    tokio::time::timeout(START_TIMEOUT, async {
        // Keep the database endpoint stable so all terminals share session
        // actors. Serialize replacement rather than creating a daemon per build.
        let startup = private_file(&socket.with_extension("startup"))?;
        acquire(&startup).await?;
        let root = socket
            .parent()
            .context("workspace endpoint has no parent")?;
        let legacy = endpoints::legacy(root, database)?;
        // Hold every released launcher's startup lock through publication of
        // the new endpoint and aliases, including concurrent old terminals.
        let mut legacy_startup = Vec::new();
        for (_, address) in &legacy {
            let lock = private_file(&address.with_extension("startup"))?;
            acquire(&lock).await?;
            legacy_startup.push(lock);
        }
        for (version, address) in &legacy {
            if fs::read_link(address).ok().as_deref() == Some(socket) {
                continue;
            }
            match Stream::connect(address).await {
                Ok(mut stream) => {
                    verify_owner(&stream)?;
                    ensure!(
                        retire_version(&mut stream, args, config, auth, *version).await?,
                        "旧版地址上运行着更新的工作区服务，请使用更新的 Orbit 客户端"
                    );
                    drop(stream);
                    let daemon = private_file(&address.with_extension("lock"))?;
                    acquire(&daemon).await?;
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                    ) => {}
                Err(error) => return Err(error.into()),
            }
        }
        if let Ok(mut stream) = Stream::connect(socket).await {
            if same_executable(&stream).await? {
                return Ok(stream);
            }
            if !retire(&mut stream, args, config, auth).await? {
                // Negotiation consumed its own Hello. The actual client needs
                // a fresh stream for its connection context and handshake.
                return Ok(Stream::connect(socket).await?);
            }
            drop(stream);
            // Shutdown acknowledges before its daemon releases the endpoint.
            // Wait for that release before launching the replacement.
            let daemon = private_file(&socket.with_extension("lock"))?;
            acquire(&daemon).await?;
            drop(daemon);
        }
        launch(socket, args, config, database, auth)?;
        loop {
            if let Ok(stream) = Stream::connect(socket).await {
                ensure!(
                    same_executable(&stream).await?,
                    "工作区服务切换失败：连接到了其他构建的服务"
                );
                return Ok(stream);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .with_context(|| {
        format!(
            "workspace server did not start or finish updating; see {}",
            socket.with_extension("log").display()
        )
    })?
}

pub(super) fn private_file(path: &Path) -> Result<std::fs::File> {
    #[cfg(windows)]
    return Ok(orbit_runtime::filesystem::private::open_file(
        path, true, false,
    )?);
    #[cfg(unix)]
    Ok(OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(path)?)
}

async fn acquire(file: &std::fs::File) -> Result<()> {
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(()),
            Err(std::fs::TryLockError::WouldBlock) => {
                tokio::time::sleep(Duration::from_millis(50)).await
            }
            Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
        }
    }
}

#[cfg(unix)]
fn verify_owner(stream: &Stream) -> Result<i32> {
    let credentials = stream.peer_cred()?;
    ensure!(
        credentials.uid() == unsafe { libc::geteuid() },
        "workspace daemon belongs to another user"
    );
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        credentials
            .pid()
            .context("workspace daemon PID is unavailable")
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd;
        let mut pid: libc::pid_t = 0;
        let mut length = std::mem::size_of_val(&pid) as libc::socklen_t;
        // SAFETY: the connected socket is live and the output buffer and length match pid_t.
        let result = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_LOCAL,
                libc::LOCAL_PEERPID,
                std::ptr::from_mut(&mut pid).cast(),
                &mut length,
            )
        };
        if result != 0 {
            return Err(std::io::Error::last_os_error())
                .context("cannot identify workspace daemon");
        }
        ensure!(
            length as usize == std::mem::size_of_val(&pid) && pid > 0,
            "workspace daemon PID is unavailable"
        );
        Ok(pid)
    }
}

#[cfg(windows)]
fn verify_owner(stream: &Stream) -> Result<u32> {
    Ok(stream.peer_pid()?)
}

async fn same_executable(stream: &Stream) -> Result<bool> {
    let pid = verify_owner(stream)?;
    tokio::task::spawn_blocking(move || {
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            compare_executables(
                Path::new("/proc/self/exe"),
                &PathBuf::from(format!("/proc/{pid}/exe")),
            )
        }
        #[cfg(windows)]
        {
            use std::os::windows::{ffi::OsStringExt, io::AsRawHandle};
            use windows_sys::Win32::System::Threading::QueryFullProcessImageNameW;
            const IMAGE_PATH_CHARS: usize = 32_768;
            let peer = transport::peer_process(pid)?;
            let mut path = vec![0u16; IMAGE_PATH_CHARS];
            let mut length = path.len() as u32;
            // SAFETY: peer is live and the UTF-16 buffer has the provided capacity.
            if unsafe {
                QueryFullProcessImageNameW(peer.as_raw_handle(), 0, path.as_mut_ptr(), &mut length)
            } == 0
            {
                return Err(std::io::Error::last_os_error())
                    .context("cannot inspect workspace daemon");
            }
            let path = PathBuf::from(std::ffi::OsString::from_wide(&path[..length as usize]));
            compare_executables(&std::env::current_exe()?, &path)
        }
        #[cfg(target_os = "macos")]
        {
            use std::{ffi::CStr, os::unix::ffi::OsStrExt};
            let mut path = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
            // SAFETY: the writable buffer has the documented maximum path size.
            let length =
                unsafe { libc::proc_pidpath(pid, path.as_mut_ptr().cast(), path.len() as u32) };
            if length <= 0 {
                return Err(std::io::Error::last_os_error())
                    .context("cannot inspect workspace daemon");
            }
            let path = CStr::from_bytes_until_nul(&path)
                .context("workspace daemon path is not terminated")?;
            let peer = Path::new(std::ffi::OsStr::from_bytes(path.to_bytes()));
            compare_executables(&std::env::current_exe()?, peer)
        }
    })
    .await?
}

fn compare_executables(current: &Path, peer: &Path) -> Result<bool> {
    let mut current =
        std::fs::File::open(current).context("cannot inspect current workspace executable")?;
    let mut peer =
        std::fs::File::open(peer).context("cannot inspect running workspace executable")?;
    let current_metadata = current.metadata()?;
    let peer_metadata = peer.metadata()?;
    // Normal connections use the same immutable cache file: no hashing needed.
    #[cfg(unix)]
    let identical = (current_metadata.dev(), current_metadata.ino())
        == (peer_metadata.dev(), peer_metadata.ino());
    #[cfg(windows)]
    let identical = orbit_runtime::filesystem::private::identity(&current)?
        == orbit_runtime::filesystem::private::identity(&peer)?;
    if identical {
        return Ok(true);
    }
    if current_metadata.len() != peer_metadata.len() {
        return Ok(false);
    }

    fn digest(file: &mut std::fs::File) -> Result<sha2::digest::Output<Sha256>> {
        let mut hash = Sha256::new();
        let mut buffer = [0; 64 * 1_024];
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hash.update(&buffer[..count]);
        }
        Ok(hash.finalize())
    }
    Ok(digest(&mut current)? == digest(&mut peer)?)
}

async fn retire(
    stream: &mut Stream,
    args: &ServiceOptions,
    config: &Path,
    auth: &Path,
) -> Result<bool> {
    retire_version(stream, args, config, auth, protocol::VERSION).await
}

async fn retire_version(
    stream: &mut Stream,
    args: &ServiceOptions,
    config: &Path,
    auth: &Path,
    mut version: u32,
) -> Result<bool> {
    let owner = verify_owner(stream)?;
    // macOS may no longer expose the peer address after a released daemon
    // closes a rejected handshake. Retain it while the connection is live.
    #[cfg(unix)]
    let address = stream.peer_addr()?;
    #[cfg(windows)]
    let address = stream.endpoint().to_path_buf();
    let mut retried = false;
    let hello = loop {
        protocol::write(
            stream,
            &ConnectionContext {
                config: config.into(),
                auth: auth.into(),
                allow_mcp_start: args.allow_mcp_start,
                workspace: expand_home(&args.workspace)?.canonicalize()?,
                remote_scope: false,
            },
        )
        .await?;
        protocol::write(
            stream,
            &Request::Hello {
                version,
                workspace: None,
            },
        )
        .await?;
        let hello: Response = protocol::read(stream).await?;
        if let Response::Error(error) = &hello
            && !retried
            && let Some(expected) = error
                .strip_prefix("workspace protocol mismatch; expected ")
                .and_then(|value| value.parse::<u32>().ok())
                .filter(|expected| *expected > 0 && *expected != version)
        {
            // Released daemons close a mismatched Hello. Reconnect using the
            // announced version only for Hello/Shutdown, never for sessions.
            #[cfg(windows)]
            let socket = address.as_path();
            #[cfg(unix)]
            let socket = address
                .as_pathname()
                .context("后台服务没有可重连的地址")?;
            let replacement = Stream::connect(socket).await?;
            ensure!(
                verify_owner(&replacement)? == owner,
                "协商更新时后台服务发生了变化，请重试"
            );
            *stream = replacement;
            version = expected;
            retried = true;
            continue;
        }
        break hello;
    };
    let Response::Hello {
        version: negotiated,
        build_generation: active,
        ..
    } = hello
    else {
        bail!("无法与已有后台服务协商更新：{hello:?}");
    };
    ensure!(negotiated == version, "workspace update protocol mismatch");
    if active >= build_generation() {
        ensure!(
            orbit_protocol::supports_version(negotiated),
            "后台服务使用协议 {negotiated}，当前客户端使用协议 {}；请更新客户端",
            protocol::VERSION
        );
        // Every compatible frontend uses the newest shared daemon. A terminal
        // with an older embedded server must never downgrade it.
        return Ok(false);
    }
    eprintln!("检测到旧版后台服务，等待空闲服务切换");
    protocol::write(stream, &Request::Shutdown).await?;
    match protocol::read::<Response>(stream).await? {
        Response::Ok => Ok(true),
        Response::Error(error) => {
            bail!("工作区后台服务待更新，等待运行任务结束后自动重连：{error}")
        }
        other => bail!("unexpected workspace shutdown response: {other:?}"),
    }
}

/// Keep bridge pipes usable locally without leaking them into the persistent daemon.
#[cfg(windows)]
pub(super) fn isolate_stdio() -> Result<()> {
    use std::os::windows::io::BorrowedHandle;
    use windows_sys::Win32::{
        Foundation::INVALID_HANDLE_VALUE,
        System::Console::{GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE},
    };

    for stream in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: GetStdHandle borrows a process standard handle; it does not transfer ownership.
        let handle = unsafe { GetStdHandle(stream) };
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            continue;
        }
        // SAFETY: the standard handle remains owned by this process throughout the call.
        prevent_inheritance(unsafe { BorrowedHandle::borrow_raw(handle) })
            .context("cannot isolate workspace bridge standard handles")?;
    }
    Ok(())
}

#[cfg(windows)]
fn prevent_inheritance(handle: std::os::windows::io::BorrowedHandle<'_>) -> std::io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{HANDLE_FLAG_INHERIT, SetHandleInformation};

    // SAFETY: the borrowed handle is live; only its inheritance flag is cleared.
    if unsafe { SetHandleInformation(handle.as_raw_handle(), HANDLE_FLAG_INHERIT, 0) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn launch(
    socket: &Path,
    args: &ServiceOptions,
    config: &Path,
    database: &Path,
    auth: &Path,
) -> Result<()> {
    #[cfg(windows)]
    let log = orbit_runtime::filesystem::private::open_append(&socket.with_extension("log"))?;
    #[cfg(unix)]
    let log = OpenOptions::new()
        .create(true)
        .truncate(false)
        .append(true)
        .mode(0o600)
        .open(socket.with_extension("log"))?;
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(["workspace-service", "--daemon", "--workspace"])
        .arg(&args.workspace)
        .arg("--config")
        .arg(config)
        .arg("--database")
        .arg(database)
        .arg("--auth-dir")
        .arg(auth)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log);
    if args.allow_mcp_start {
        command.arg("--allow-mcp-start");
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS};
        command.creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS);
    }
    #[cfg(unix)]
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command.spawn()?;
    std::thread::spawn(move || {
        let mut child = child;
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::os::windows::io::{AsHandle, AsRawHandle};
    use windows_sys::Win32::Foundation::{
        GetHandleInformation, HANDLE_FLAG_INHERIT, SetHandleInformation,
    };

    #[test]
    fn bridge_handles_stop_inheriting_without_losing_access() {
        use std::io::{Read, Seek, SeekFrom, Write};

        let mut file = tempfile::tempfile().unwrap();
        // SAFETY: file owns a live handle; this fixture enables inheritance before isolation.
        assert_ne!(
            unsafe {
                SetHandleInformation(
                    file.as_raw_handle(),
                    HANDLE_FLAG_INHERIT,
                    HANDLE_FLAG_INHERIT,
                )
            },
            0
        );
        prevent_inheritance(file.as_handle()).unwrap();
        let mut flags = 0;
        // SAFETY: file is live and flags is a writable DWORD output.
        assert_ne!(
            unsafe { GetHandleInformation(file.as_raw_handle(), &mut flags) },
            0
        );
        assert_eq!(flags & HANDLE_FLAG_INHERIT, 0);
        file.write_all(b"isolated bridge")
            .unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        let mut content = String::new();
        file.read_to_string(&mut content)
            .unwrap();
        assert_eq!(content, "isolated bridge");
    }
}
