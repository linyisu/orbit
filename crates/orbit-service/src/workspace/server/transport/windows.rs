//! Local-only named pipes with explicit user ACLs and non-impersonating clients.

use std::{
    io,
    mem::size_of,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    path::{Path, PathBuf},
    pin::Pin,
    ptr,
    task::{Context, Poll},
    time::Duration,
};

use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::windows::named_pipe::{ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions},
};
use windows_sys::Win32::{
    Foundation::{ERROR_PIPE_BUSY, LocalFree},
    Security::{
        Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1},
        EqualSid, GetTokenInformation, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
    },
    System::{
        Pipes::{GetNamedPipeClientProcessId, GetNamedPipeServerProcessId},
        Threading::{
            GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
        },
    },
};

fn pipe_name(endpoint: &Path) -> String {
    let hash: String = Sha256::digest(endpoint.as_os_str().as_encoded_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!(r"\\.\pipe\orbit-workspace-{hash}")
}

fn instance(endpoint: &Path, first: bool) -> io::Result<NamedPipeServer> {
    let sid = orbit_runtime::filesystem::private::user_id()?;
    let sddl: Vec<u16> = format!("O:{sid}D:P(A;;GA;;;{sid})")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut descriptor = ptr::null_mut();
    // SAFETY: the NUL-terminated SDDL and writable output remain live during the call.
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    // SAFETY: attributes and its descriptor outlive pipe creation; the handle is not inherited.
    let result = unsafe {
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(
                pipe_name(endpoint),
                ptr::from_mut(&mut attributes).cast(),
            )
    };
    // SAFETY: the security descriptor was allocated by the conversion API using LocalAlloc.
    unsafe {
        LocalFree(descriptor);
    }
    result
}

pub(super) struct Listener {
    endpoint: PathBuf,
    pending: NamedPipeServer,
}

impl Listener {
    pub(super) fn bind(endpoint: &Path) -> io::Result<Self> {
        Ok(Self {
            endpoint: endpoint.into(),
            pending: instance(endpoint, true)?,
        })
    }

    pub(super) async fn accept(&mut self) -> io::Result<NamedPipeServer> {
        self.pending.connect().await?;
        // Publish the next instance before releasing this one, preserving namespace ownership.
        let next = instance(&self.endpoint, false)?;
        let connected = std::mem::replace(&mut self.pending, next);
        let mut pid = 0;
        // SAFETY: the connected pipe is live and pid points to writable storage.
        check(unsafe { GetNamedPipeClientProcessId(connected.as_raw_handle(), &mut pid) })?;
        peer_process(pid)?;
        Ok(connected)
    }
}

pub(super) struct Stream {
    inner: NamedPipeClient,
    endpoint: PathBuf,
}

impl Stream {
    pub(super) async fn connect(endpoint: &Path) -> io::Result<Self> {
        loop {
            // Tokio sets SECURITY_IDENTIFICATION so a pipe server cannot impersonate this client.
            match ClientOptions::new().open(pipe_name(endpoint)) {
                Ok(inner) => {
                    return Ok(Self {
                        inner,
                        endpoint: endpoint.into(),
                    });
                }
                Err(error) if error.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(error) => return Err(error),
            }
        }
    }

    pub(super) fn peer_pid(&self) -> io::Result<u32> {
        let mut pid = 0;
        // SAFETY: the connected pipe is live and pid points to writable storage.
        if unsafe { GetNamedPipeServerProcessId(self.inner.as_raw_handle(), &mut pid) } == 0 {
            return Err(io::Error::last_os_error());
        }
        peer_process(pid)?;
        Ok(pid)
    }

    pub(super) fn endpoint(&self) -> &Path {
        &self.endpoint
    }
}

fn check(result: i32) -> io::Result<()> {
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

pub(super) fn peer_process(pid: u32) -> io::Result<OwnedHandle> {
    // SAFETY: requesting a non-inheritable query-only process handle has no borrowed pointers.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: OpenProcess transferred ownership of a new valid handle.
    let peer = unsafe { OwnedHandle::from_raw_handle(handle) };
    let mut users = Vec::new();
    // SAFETY: GetCurrentProcess returns a valid pseudo-handle that is not closed here.
    for process in [peer.as_raw_handle(), unsafe { GetCurrentProcess() }] {
        let mut token = ptr::null_mut();
        // SAFETY: process is live and token points to writable handle storage.
        check(unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) })?;
        // SAFETY: OpenProcessToken transferred ownership of a valid token handle.
        let token = unsafe { OwnedHandle::from_raw_handle(token) };
        let mut length = 0;
        // SAFETY: the zero-length request only retrieves the required allocation size.
        unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                ptr::null_mut(),
                0,
                &mut length,
            );
        }
        let mut buffer = vec![0usize; (length as usize).div_ceil(size_of::<usize>())];
        // SAFETY: buffer is aligned and contains at least length writable bytes.
        check(unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                buffer.as_mut_ptr().cast(),
                length,
                &mut length,
            )
        })?;
        users.push(buffer);
    }
    // SAFETY: both buffers contain successful TokenUser responses and remain alive.
    let equal = unsafe {
        EqualSid(
            (*users[0].as_ptr().cast::<TOKEN_USER>())
                .User
                .Sid,
            (*users[1].as_ptr().cast::<TOKEN_USER>())
                .User
                .Sid,
        )
    };
    if equal == 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "workspace peer belongs to another user",
        ));
    }
    Ok(peer)
}

impl AsyncRead for Stream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_read(cx, buffer)
    }
}

impl AsyncWrite for Stream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, buffer)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

pub(super) async fn accept(listener: &mut Listener) -> io::Result<NamedPipeServer> {
    listener.accept().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn namespace_is_exclusive_and_connected_peers_exchange_bytes() {
        let root = tempfile::tempdir().unwrap();
        let endpoint = root.path().join("service");
        let mut listener = Listener::bind(&endpoint).unwrap();
        assert!(Listener::bind(&endpoint).is_err());
        let mut client = Stream::connect(&endpoint)
            .await
            .unwrap();
        assert_eq!(client.peer_pid().unwrap(), std::process::id());
        let mut server = listener.accept().await.unwrap();
        client
            .write_all(b"request")
            .await
            .unwrap();
        let mut bytes = [0; 7];
        server
            .read_exact(&mut bytes)
            .await
            .unwrap();
        assert_eq!(&bytes, b"request");
        server
            .write_all(b"reply")
            .await
            .unwrap();
        let mut reply = [0; 5];
        client
            .read_exact(&mut reply)
            .await
            .unwrap();
        assert_eq!(&reply, b"reply");
        let second = Stream::connect(&endpoint)
            .await
            .unwrap();
        let _accepted = listener.accept().await.unwrap();
        assert_eq!(second.peer_pid().unwrap(), std::process::id());
        assert!(Listener::bind(&endpoint).is_err());
    }
}
