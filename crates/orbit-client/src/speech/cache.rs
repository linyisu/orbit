//! Verified speech assets with resumable downloads and a cancellable cross-process cache lock.
use std::{fs::File, path::Path, time::Duration};

use anyhow::{Context, Result, bail, ensure};
use reqwest::{StatusCode, header};
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::watch,
};

use super::{CancellationToken, Progress};

const DOWNLOAD_RANGE_BYTES: u64 = 8 * 1_024 * 1_024;
const READ_TIMEOUT: Duration = Duration::from_secs(30);
const LOCK_RETRY: Duration = Duration::from_millis(100);

pub(super) struct Asset<'a> {
    pub directory: &'static str,
    pub name: &'static str,
    pub source: &'a str,
    pub bytes: u64,
    pub sha256: &'static str,
}

/// Held until native loading ends, including after the caller has cancelled its wait.
pub(super) async fn lock(directory: &Path, cancel: &CancellationToken) -> Result<File> {
    ensure!(!cancel.is_cancelled(), "语音准备已取消");
    tokio::fs::create_dir_all(directory).await?;
    let file = tokio::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(directory.join("prepare.lock"))
        .await?
        .into_std()
        .await;
    loop {
        ensure!(!cancel.is_cancelled(), "语音准备已取消");
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
            Err(std::fs::TryLockError::WouldBlock) => {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => bail!("语音准备已取消"),
                    _ = tokio::time::sleep(LOCK_RETRY) => {}
                }
            }
        }
    }
}

pub(super) async fn prepare(
    root: &Path,
    assets: &[Asset<'_>],
    cancel: &CancellationToken,
    progress: &watch::Sender<Progress>,
) -> Result<()> {
    let client = reqwest::Client::builder()
        .user_agent(concat!("Orbit/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(READ_TIMEOUT)
        .read_timeout(READ_TIMEOUT)
        .build()?;
    let total = assets
        .iter()
        .map(|asset| asset.bytes)
        .sum();
    let mut completed = 0;
    for asset in assets {
        ensure!(!cancel.is_cancelled(), "语音准备已取消");
        let directory = root.join(asset.directory);
        tokio::fs::create_dir_all(&directory).await?;
        let path = directory.join(asset.name);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        if !verified(&path, asset, cancel, progress, completed, total).await? {
            download(&client, &path, asset, cancel, progress, completed, total).await?;
        }
        completed += asset.bytes;
    }
    Ok(())
}

async fn verified(
    path: &Path,
    asset: &Asset<'_>,
    cancel: &CancellationToken,
    progress: &watch::Sender<Progress>,
    completed: u64,
    total: u64,
) -> Result<bool> {
    ensure!(!cancel.is_cancelled(), "语音准备已取消");
    let mut file = match tokio::fs::File::open(path).await {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    if file.metadata().await?.len() != asset.bytes {
        return Ok(false);
    }
    let mut digest = Sha256::new();
    let mut buffer = vec![0; 64 * 1_024];
    let mut read = 0;
    loop {
        let count = tokio::select! {
            biased;
            _ = cancel.cancelled() => bail!("语音准备已取消"),
            count = file.read(&mut buffer) => count?,
        };
        if count == 0 {
            break;
        }
        read += count as u64;
        digest.update(&buffer[..count]);
        report(
            progress,
            Progress::Verifying {
                completed_bytes: completed + read,
                total_bytes: total,
            },
        );
    }
    Ok(digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
        == asset.sha256)
}

async fn download(
    client: &reqwest::Client,
    path: &Path,
    asset: &Asset<'_>,
    cancel: &CancellationToken,
    progress: &watch::Sender<Progress>,
    completed: u64,
    total: u64,
) -> Result<()> {
    let pending = path.with_file_name(format!(
        "{}.{}.part",
        path.file_name()
            .context("模型文件名无效")?
            .to_string_lossy(),
        asset.sha256
    ));
    let received = tokio::fs::metadata(&pending)
        .await
        .map(|metadata| metadata.len())
        .or_else(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Ok(0)
            } else {
                Err(error)
            }
        })?;
    if received >= asset.bytes {
        if received == asset.bytes
            && verified(&pending, asset, cancel, progress, completed, total).await?
        {
            ensure!(!cancel.is_cancelled(), "模型下载已取消");
            tokio::fs::rename(&pending, path).await?;
            return Ok(());
        }
        tokio::fs::remove_file(&pending).await?;
    }
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&pending)
        .await?;
    let result = transfer(client, &mut file, asset, cancel, progress, completed, total).await;
    // Tokio file writes can remain in flight after write_all. Complete them before releasing
    // the cache lock so a cancelled attempt cannot race the next resume operation.
    file.flush().await?;
    result?;
    file.sync_all().await?;
    drop(file);
    if !verified(&pending, asset, cancel, progress, completed, total).await? {
        tokio::fs::remove_file(&pending).await?;
        bail!("语音模型校验失败，已丢弃损坏的下载，请重试");
    }
    ensure!(!cancel.is_cancelled(), "模型下载已取消");
    tokio::fs::rename(&pending, path).await?;
    Ok(())
}

async fn transfer(
    client: &reqwest::Client,
    file: &mut tokio::fs::File,
    asset: &Asset<'_>,
    cancel: &CancellationToken,
    progress: &watch::Sender<Progress>,
    completed: u64,
    total: u64,
) -> Result<()> {
    let mut received = file.metadata().await?.len();
    report(
        progress,
        Progress::Downloading {
            received_bytes: completed + received,
            total_bytes: total,
        },
    );
    while received < asset.bytes {
        let end = (received + DOWNLOAD_RANGE_BYTES).min(asset.bytes) - 1;
        let mut url = url::Url::parse(&format!("{}/{}", asset.source, asset.name))?;
        url.query_pairs_mut()
            .append_pair("download", "true")
            .append_pair("orbit_start", &received.to_string())
            .append_pair("orbit_end", &end.to_string());
        let mut response = tokio::select! {
            biased;
            _ = cancel.cancelled() => bail!("模型下载已取消"),
            response = client.get(url)
                .header(header::ACCEPT_ENCODING, "identity")
                .header(header::RANGE, format!("bytes={received}-{end}"))
                .send() => response?.error_for_status()?,
        };
        let response_end = if response.status() == StatusCode::PARTIAL_CONTENT {
            let range = response
                .headers()
                .get(header::CONTENT_RANGE)
                .context("语音模型续传缺少 Content-Range")?
                .to_str()?;
            validate_range(range, received, end, asset.bytes)?
        } else {
            ensure!(response.status() == StatusCode::OK, "语音模型下载响应无效");
            ensure!(
                response
                    .content_length()
                    .is_none_or(|length| length == asset.bytes),
                "语音模型下载大小不符"
            );
            if received != 0 {
                file.set_len(0).await?;
                received = 0;
            }
            asset.bytes - 1
        };
        let expected = response_end - received + 1;
        ensure!(
            response
                .content_length()
                .is_none_or(|length| length == expected),
            "语音模型下载大小不符"
        );
        while let Some(chunk) = tokio::select! {
            biased;
            _ = cancel.cancelled() => bail!("模型下载已取消"),
            chunk = response.chunk() => chunk?,
        } {
            ensure!(
                received + chunk.len() as u64 <= response_end + 1,
                "语音模型下载超出预期大小"
            );
            file.write_all(&chunk).await?;
            received += chunk.len() as u64;
            report(
                progress,
                Progress::Downloading {
                    received_bytes: completed + received,
                    total_bytes: total,
                },
            );
        }
        ensure!(
            received == response_end + 1,
            "语音模型下载中断，重试将继续下载"
        );
    }
    Ok(())
}

fn validate_range(value: &str, start: u64, requested_end: u64, bytes: u64) -> Result<u64> {
    let (range, total) = value
        .strip_prefix("bytes ")
        .and_then(|value| value.split_once('/'))
        .context("语音模型续传范围无效")?;
    let (first, last) = range
        .split_once('-')
        .context("语音模型续传范围无效")?;
    let (first, last, total) = (
        first.parse::<u64>()?,
        last.parse::<u64>()?,
        total.parse::<u64>()?,
    );
    ensure!(
        first == start && last >= first && last <= requested_end && total == bytes,
        "语音模型续传范围不符，未追加响应内容"
    );
    Ok(last)
}

#[cfg(test)]
mod tests;

fn report(sender: &watch::Sender<Progress>, next: Progress) {
    fn key(progress: Progress) -> (u8, u64) {
        match progress {
            Progress::Verifying {
                completed_bytes: completed,
                total_bytes: total,
            } => (0, completed * 100 / total.max(1)),
            Progress::Downloading {
                received_bytes: received,
                total_bytes: total,
            } => (1, received * 100 / total.max(1)),
            Progress::Waiting => (2, 0),
            Progress::LoadingPreview => (3, 0),
            Progress::LoadingCorrection => (4, 0),
        }
    }
    sender.send_if_modified(|previous| {
        if key(*previous) == key(next) {
            false
        } else {
            *previous = next;
            true
        }
    });
}
