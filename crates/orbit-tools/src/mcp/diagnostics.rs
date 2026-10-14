//! Drain stderr continuously without allowing a noisy server to grow memory.
use std::sync::{Arc, Mutex};
use tokio::{io::AsyncReadExt, process::ChildStderr};
use tokio_util::task::AbortOnDropHandle;

pub(super) struct Stderr {
    tail: Arc<Mutex<Vec<u8>>>,
    task: AbortOnDropHandle<()>,
}

impl Stderr {
    pub fn capture(mut stderr: ChildStderr) -> Self {
        let tail = Arc::new(Mutex::new(Vec::new()));
        let buffer = tail.clone();
        let task = AbortOnDropHandle::new(tokio::spawn(async move {
            let mut chunk = [0; 4096];
            while let Ok(count) = stderr.read(&mut chunk).await {
                if count == 0 {
                    break;
                }
                let mut bytes = buffer.lock().unwrap();
                bytes.extend_from_slice(&chunk[..count]);
                let excess = bytes.len().saturating_sub(8192);
                bytes.drain(..excess);
            }
        }));
        Self { tail, task }
    }

    pub async fn finish(&mut self) -> String {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), &mut self.task).await;
        String::from_utf8_lossy(&self.tail.lock().unwrap())
            .trim()
            .to_owned()
    }
}
