//! Unix socket transport; filesystem permissions protect the local endpoint.
use std::io;

pub use tokio::net::{UnixListener as Listener, UnixStream as Stream};

pub async fn accept(listener: &mut Listener) -> io::Result<Stream> {
    listener
        .accept()
        .await
        .map(|(stream, _)| stream)
}
