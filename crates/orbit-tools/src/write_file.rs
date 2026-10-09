use std::{
    fs::File,
    io::{self, Read, Write},
    os::unix::fs::MetadataExt,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

use orbit_agent::tool::{FileChange, Tool, ToolError, ToolFuture, ToolOutput};
use orbit_model::types::{ToolCall, ToolDefinition};
use orbit_runtime::filesystem::{Workspace, WorkspaceError};
use rustix::fs::{
    AtFlags, Mode, OFlags, RenameFlags, fchmod, openat, renameat, renameat_with, unlinkat,
};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::approval::{
    ApprovalPreview, ApprovalRequest, ApprovalSender, DeletePreview, WritePreview,
};

const MAX_BYTES: usize = 1024 * 1024;

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    path: String,
    content: String,
}

#[derive(Clone)]
pub struct WriteFile {
    workspace: Workspace,
    approvals: ApprovalSender,
}

impl WriteFile {
    pub fn new(workspace: Workspace, approvals: ApprovalSender) -> Self {
        Self {
            workspace,
            approvals,
        }
    }

    pub(crate) async fn edit(
        &self,
        path: String,
        transform: impl FnOnce(&str) -> Result<String, ToolError> + Send + 'static,
        cancellation: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        check_cancelled(&cancellation)?;
        let workspace = self.workspace.clone();
        let prepared = tokio::task::spawn_blocking(move || {
            let mut prepared = Prepared::new(
                workspace,
                Arguments {
                    path,
                    content: String::new(),
                },
            )?;
            let before = prepared.preview.before.as_deref().ok_or_else(|| {
                failed("edit requires an existing file; use write_file to create one")
            })?;
            let after = transform(before)?;

            if after.len() > MAX_BYTES || after.contains('\0') {
                return Err(failed(
                    "edited content must be UTF-8 text without NUL, at most 1 MiB",
                ));
            }

            if after == before {
                return Err(failed("edit makes no change"));
            }

            prepared.preview.after = after;

            Ok::<_, ToolError>(prepared)
        })
        .await
        .map_err(failed)??;

        prepared
            .approve_and_commit(&self.approvals, cancellation)
            .await
    }

    /// Restore a file to an exact, centrally recorded pre-write checkpoint.
    /// The current contents must still equal the checkpoint's post-write text;
    /// otherwise a human or later tool changed it and the restore refuses.
    pub async fn restore(
        &self,
        change: FileChange,
        cancellation: CancellationToken,
    ) -> Result<String, ToolError> {
        if let Some(before) = change.before {
            check_cancelled(&cancellation)?;
            let workspace = self.workspace.clone();
            let path = change.path.clone();
            let expected = change.after;
            let prepared = tokio::task::spawn_blocking(move || {
                let prepared = Prepared::new(
                    workspace,
                    Arguments {
                        path,
                        content: before,
                    },
                )?;

                if prepared.preview.before.as_deref() != Some(expected.as_str()) {
                    return Err(failed(
                        "cannot undo: current file differs from Orbit's checkpoint; it was changed after the recorded write",
                    ));
                }

                Ok::<_, ToolError>(prepared)
            })
            .await
            .map_err(failed)??;

            prepared
                .approve_and_commit(&self.approvals, cancellation)
                .await
                .map(|output| {
                    json!({ "path": change.path, "restored": true, "created_file_removed": false, "result": output.content })
                    .to_string()
                })
        } else {
            self.delete_created(change, cancellation).await
        }
    }

    async fn delete_created(
        &self,
        change: FileChange,
        cancellation: CancellationToken,
    ) -> Result<String, ToolError> {
        check_cancelled(&cancellation)?;
        let workspace = self.workspace.clone();
        let path = change.path.clone();
        let expected = change.after;
        let prepared =
            tokio::task::spawn_blocking(move || DeletePrepared::new(workspace, path, expected))
                .await
                .map_err(failed)??;

        prepared
            .approve_and_commit(&self.approvals, cancellation)
            .await
            .map(|result| {
                json!({ "path": change.path, "restored": true, "created_file_removed": true, "result": result })
                .to_string()
            })
    }
}

struct DeletePrepared {
    workspace: Workspace,
    path: String,
    before: Fingerprint,
    text: String,
    parent: (u64, u64),
}

impl DeletePrepared {
    fn new(workspace: Workspace, path: String, expected: String) -> Result<Self, ToolError> {
        let (parent, _) = workspace
            .open_granted_write_parent(Path::new(&path))
            .map_err(failed)?;
        let parent_metadata = parent.metadata().map_err(failed)?;
        let (before, text) = snapshot(&workspace, &path)?;
        let Some(before) = before else {
            return Err(failed("cannot undo: the created file is already absent"));
        };
        let Some(text) = text else {
            return Err(failed("cannot undo: the created file is already absent"));
        };
        if text != expected {
            return Err(failed(
                "cannot undo: current file differs from Orbit's checkpoint; it was changed after the recorded write",
            ));
        }

        Ok(Self {
            workspace,
            path,
            before,
            text,
            parent: (parent_metadata.dev(), parent_metadata.ino()),
        })
    }

    async fn approve_and_commit(
        self,
        approvals: &ApprovalSender,
        cancellation: CancellationToken,
    ) -> Result<String, ToolError> {
        check_cancelled(&cancellation)?;
        let (sender, receiver) = oneshot::channel();
        let preview = ApprovalPreview::Delete(DeletePreview {
            path: self.path.clone(),
            before: self.text.clone(),
        });
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(ToolError::Cancelled),
            sent = approvals.send(ApprovalRequest { preview, decision: sender }) => {
                sent.map_err(|_| failed("undo approval unavailable; no file was changed"))?;
            }
        }
        let approved = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(ToolError::Cancelled),
            answer = receiver => answer.unwrap_or(false),
        };

        if !approved {
            return Err(failed("user denied undo; no file was changed"));
        }

        tokio::task::spawn_blocking(move || self.commit(cancellation))
            .await
            .map_err(failed)?
    }

    fn commit(self, cancellation: CancellationToken) -> Result<String, ToolError> {
        check_cancelled(&cancellation)?;
        let path = Path::new(&self.path);
        let (parent, name) = self
            .workspace
            .open_granted_write_parent(path)
            .map_err(failed)?;
        let parent_metadata = parent.metadata().map_err(failed)?;

        if (parent_metadata.dev(), parent_metadata.ino()) != self.parent {
            return Err(failed(
                "cannot undo: parent directory changed; request a new preview",
            ));
        }

        let (current, text) = snapshot(&self.workspace, &self.path)?;

        if current.as_ref() != Some(&self.before) || text.as_deref() != Some(self.text.as_str()) {
            return Err(failed(
                "cannot undo: current file differs from Orbit's checkpoint; it was changed after the recorded write",
            ));
        }

        check_cancelled(&cancellation)?;
        unlinkat(&parent, &name, AtFlags::empty()).map_err(failed)?;
        let synced = parent.sync_all().is_ok();

        Ok(json!({ "path": self.path, "deleted": true, "directory_synced": synced }).to_string())
    }
}

impl Tool for WriteFile {
    fn capabilities(&self) -> orbit_agent::tool::ToolCapabilities {
        orbit_agent::tool::ToolCapabilities::WRITE
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "write_file",
            "Create or intentionally replace a whole UTF-8 file; prefer edit_file for localized changes to existing files. Write a file in an existing workspace directory (or outside path granted via request_capability) under the selected approval policy. Auto mode allows workspace-local edits; external writes still require approval. Use relative paths or granted outside paths. Old and new content must each be at most 1 MiB. Internal symlinks are resolved safely; escaping links, hard-linked files and protected metadata are rejected. Changes during approval cause a conflict; never automatically retry a denied write.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"],
                "additionalProperties": false
            }),
        )
    }

    fn execute<'a>(&'a self, call: ToolCall, cancellation: CancellationToken) -> ToolFuture<'a> {
        Box::pin(async move {
            let args: Arguments = serde_json::from_str(&call.function.arguments).map_err(|e| {
                ToolError::InvalidArguments {
                    tool: "write_file".into(),
                    message: e.to_string(),
                }
            })?;

            if args.path.trim().is_empty()
                || args.content.len() > MAX_BYTES
                || args.content.contains('\0')
            {
                return Err(failed(
                    "path must be nonempty and content must be UTF-8 text without NUL, at most 1 MiB",
                ));
            }

            check_cancelled(&cancellation)?;
            let workspace = self.workspace.clone();
            let prepared = tokio::task::spawn_blocking(move || Prepared::new(workspace, args))
                .await
                .map_err(failed)??;

            prepared
                .approve_and_commit(&self.approvals, cancellation)
                .await
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Fingerprint {
    device: u64,
    inode: u64,
    modified: (i64, i64),
    changed: (i64, i64),
    mode: u32,
}

fn fingerprint(file: &File) -> Result<Fingerprint, ToolError> {
    let metadata = file.metadata().map_err(failed)?;

    Ok(Fingerprint {
        device: metadata.dev(),
        inode: metadata.ino(),
        modified: (metadata.mtime(), metadata.mtime_nsec()),
        changed: (metadata.ctime(), metadata.ctime_nsec()),
        mode: metadata.mode(),
    })
}

struct Prepared {
    workspace: Workspace,
    preview: WritePreview,
    before: Option<Fingerprint>,
    parent: (u64, u64),
}

impl Prepared {
    fn new(workspace: Workspace, args: Arguments) -> Result<Self, ToolError> {
        let (parent, _) = workspace
            .authorize_write_parent(Path::new(&args.path))
            .map_err(failed)?;
        let metadata = parent.metadata().map_err(failed)?;
        let (before, text) = snapshot(&workspace, &args.path)?;

        Ok(Self {
            workspace,
            preview: WritePreview {
                path: args.path,
                before: text,
                after: args.content,
            },
            before,
            parent: (metadata.dev(), metadata.ino()),
        })
    }

    async fn approve_and_commit(
        self,
        approvals: &ApprovalSender,
        cancellation: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let prepared = self;
        check_cancelled(&cancellation)?;
        let (sender, receiver) = oneshot::channel();
        let approval = ApprovalRequest {
            preview: ApprovalPreview::Write(prepared.preview.clone()),
            decision: sender,
        };
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(ToolError::Cancelled),
            sent = approvals.send(approval) => { sent.map_err(|_| failed("write approval unavailable; no file was changed"))?; }
        }
        let approved = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(ToolError::Cancelled),
            answer = receiver => answer.unwrap_or(false),
        };

        if !approved {
            return Err(failed(
                "user denied write; do not retry without a new user request",
            ));
        }

        check_cancelled(&cancellation)?;
        // Once the blocking commit starts, await its actual result rather than report
        // cancellation while a detached write may still be completing.
        let change = FileChange {
            path: prepared.preview.path.clone(),
            before: prepared.preview.before.clone(),
            after: prepared.preview.after.clone(),
        };

        tokio::task::spawn_blocking(move || prepared.commit(cancellation))
            .await
            .map_err(failed)?
            .map(|receipt| ToolOutput::with_changes(receipt, vec![change]))
    }

    fn commit(self, cancellation: CancellationToken) -> Result<String, ToolError> {
        check_cancelled(&cancellation)?;
        let path = Path::new(&self.preview.path);
        let (parent, name) = self
            .workspace
            .authorize_write_parent(path)
            .map_err(failed)?;
        let metadata = parent.metadata().map_err(failed)?;

        if (metadata.dev(), metadata.ino()) != self.parent {
            return Err(failed(
                "write conflict: parent directory changed; request a new preview",
            ));
        }

        let temp_name = format!(
            ".orbit-write-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        );
        let fd = openat(
            &parent,
            temp_name.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(failed)?;
        let mut file = File::from(fd);
        let _cleanup = TempFile {
            parent: &parent,
            name: &temp_name,
        };
        file.write_all(self.preview.after.as_bytes())
            .map_err(failed)?;

        if let Some(before) = &self.before {
            fchmod(&file, Mode::from_bits_truncate(before.mode & 0o777)).map_err(failed)?;
        }

        file.sync_all().map_err(failed)?;
        let (now, text) = snapshot(&self.workspace, &self.preview.path)?;
        let (current_parent, _) = self
            .workspace
            .open_granted_write_parent(path)
            .map_err(failed)?;
        let current = current_parent.metadata().map_err(failed)?;

        if now != self.before
            || text != self.preview.before
            || (current.dev(), current.ino()) != self.parent
        {
            return Err(failed(
                "write conflict: file changed after preview; request a new preview",
            ));
        }

        check_cancelled(&cancellation)?;

        if self.before.is_none() {
            renameat_with(
                &parent,
                temp_name.as_str(),
                &parent,
                &name,
                RenameFlags::NOREPLACE,
            )
            .map_err(failed)?;
        } else {
            renameat(&parent, temp_name.as_str(), &parent, &name).map_err(failed)?;
        }

        // The rename has committed; do not turn a subsequent cancellation into a
        // misleading 'not written' result. Report directory sync separately.
        let sync_error = parent.sync_all().err().map(|error| error.to_string());

        Ok(json!({
            "path": self.preview.path,
            "written_bytes": self.preview.after.len(),
            "created": self.before.is_none(),
            "directory_synced": sync_error.is_none(),
            "directory_sync_error": sync_error
        })
        .to_string())
    }
}

struct TempFile<'a> {
    parent: &'a File,
    name: &'a str,
}

impl Drop for TempFile<'_> {
    fn drop(&mut self) {
        let _ = unlinkat(self.parent, self.name, AtFlags::empty());
    }
}

fn snapshot(
    workspace: &Workspace,
    path: &str,
) -> Result<(Option<Fingerprint>, Option<String>), ToolError> {
    let file = match workspace.authorize_read(Path::new(path)) {
        Ok(file) => file,
        Err(WorkspaceError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            return Ok((None, None));
        }
        Err(error) => return Err(failed(error)),
    };

    if file.metadata().map_err(failed)?.nlink() != 1 {
        return Err(failed("refusing to replace a hard-linked file"));
    }

    let before = fingerprint(&file)?;
    let mut bytes = Vec::new();
    (&file)
        .take(MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(failed)?;

    if bytes.len() > MAX_BYTES {
        return Err(failed("existing file exceeds the 1 MiB write limit"));
    }

    let text = String::from_utf8(bytes).map_err(|_| failed("existing file is not UTF-8 text"))?;

    if text.contains('\0') {
        return Err(failed("existing file contains binary data"));
    }
    if fingerprint(&file)? != before {
        return Err(failed("file changed while preparing preview"));
    }

    Ok((Some(before), Some(text)))
}

fn check_cancelled(token: &CancellationToken) -> Result<(), ToolError> {
    if token.is_cancelled() {
        Err(ToolError::Cancelled)
    } else {
        Ok(())
    }
}

fn failed(error: impl std::fmt::Display) -> ToolError {
    ToolError::ExecutionFailed(error.to_string())
}
