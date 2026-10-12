//! A single-file index transaction, independent of Orbit's write ledger.
//!
//! Build an index privately, then publish it under Git's index lock after
//! checking the complete reviewed state again. No checkout/add/reset is used.
use std::{
    fs::{File, Metadata},
    io::{self, Read, Write},
    os::{fd::AsRawFd, unix::fs::MetadataExt},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};

use orbit_agent::tool::ToolError;
use orbit_runtime::filesystem::Workspace;
use rustix::fs::{
    AtFlags, Mode, OFlags, RenameFlags, fchmod, mkdirat, openat, renameat, renameat_with, unlinkat,
};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use super::{
    GIT_TIMEOUT, MAX_PATCH_BYTES, checked_review_path, failed, git_with_objects, invalid,
    repository_root,
};

const MAX_INDEX_BYTES: usize = 32 * 1024 * 1024;
const MAX_FILE_BYTES: usize = 8 * 1024 * 1024;
const MAX_ADMIN_BYTES: usize = 1024 * 1024;

pub struct ReviewedFile {
    pub patch: String,
    pub index_receipt: Option<String>,
    pub index_note: Option<String>,
}

/// Display the exact raw bytes/modes that the index transaction will use.
/// Unsupported index formats retain their read-only review surface.
pub fn review_index_patch(
    workspace: &Workspace,
    path: &str,
    staged: bool,
    untracked: bool,
) -> Result<ReviewedFile, ToolError> {
    let cancellation = CancellationToken::new();
    let context = IndexContext::new(workspace, &cancellation);
    let before = context
        .as_ref()
        .map_err(|error| error.to_string())
        .and_then(|context| context.snapshot(path).map_err(|error| error.to_string()));
    match before {
        Ok(before) => {
            let context = context.as_ref().map_err(failed)?;
            // Do not diff the live worktree: A -> B -> A changes could display
            // B while issuing a receipt for A. Only immutable captured inputs
            // participate, without clean filters, attributes or EOL conversion.
            let patch = context.raw_patch(&before, path, staged, untracked)?;
            let after = context.snapshot(path)?;
            if before.receipt != after.receipt {
                return Err(conflict());
            }
            Ok(ReviewedFile {
                patch,
                index_receipt: Some(before.receipt),
                index_note: None,
            })
        }
        Err(reason) => Ok(ReviewedFile {
            patch: super::review_patch(workspace, path, staged, untracked)?,
            index_receipt: None,
            index_note: Some(format!("仅供审查，无法修改暂存区：{reason}")),
        }),
    }
}

/// Stage (`staged=true`) or unstage one exact reviewed path. This updates only
/// the index. Blob storage is immutable; no user file or write receipt changes.
pub fn update_review_index(
    workspace: &Workspace,
    path: &str,
    receipt: &str,
    staged: bool,
    cancellation: CancellationToken,
) -> Result<(), ToolError> {
    if receipt.len() != 64 || !receipt.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid("缺少有效审查凭据，请刷新差异"));
    }
    let context = IndexContext::new(workspace, &cancellation)?;
    let before = context.snapshot(path)?;
    if before.receipt != receipt {
        return Err(conflict());
    }
    let mut lock = GitLock::acquire(&context.admin, "index.lock")?;
    // Ref locks keep ordinary checkout/commit/update-ref operations from moving
    // the base while an unstage is prepared. Index locking also covers add/reset.
    let _head_lock = GitLock::acquire(&context.admin, "HEAD.lock")?;
    let _ref_lock = before
        .head_ref
        .as_deref()
        .map(|name| context.ref_lock(name))
        .transpose()?;
    if context.snapshot(path)?.receipt != receipt {
        return Err(conflict());
    }

    // Keep temporary files outside the working tree and inaccessible to other
    // users. Git sees only this private index and the pinned metadata directory.
    let temporary = tempfile::tempdir().map_err(failed)?;
    let index = temporary.path().join("index");
    if let Some(bytes) = &before.index {
        let mut file = private_file(&index)?;
        file.write_all(bytes).map_err(failed)?;
        file.sync_all().map_err(failed)?;
    }
    if staged {
        if let Some(worktree) = &before.worktree {
            let blob = temporary.path().join("blob");
            private_file(&blob)?
                .write_all(&worktree.bytes)
                .map_err(failed)?;
            let objects = temporary.path().join("objects");
            std::fs::create_dir(&objects).map_err(failed)?;
            let oid = context.command(
                vec![
                    "hash-object".into(),
                    "-w".into(),
                    "--no-filters".into(),
                    "--".into(),
                    path_text(&blob)?,
                ],
                None,
                Some(&objects),
                false,
            )?;
            let oid = std::str::from_utf8(&oid).map_err(failed)?.trim().to_owned();
            validate_oid(&oid, before.hash_bytes)?;
            context.install_object(&objects, &oid)?;
            context.command(
                vec![
                    "update-index".into(),
                    "--add".into(),
                    "--cacheinfo".into(),
                    format!("{:o}", worktree.mode),
                    oid,
                    path.into(),
                ],
                Some(&index),
                None,
                false,
            )?;
        } else {
            context.remove_entry(&index, path)?;
        }
    } else if let Some(head) = &before.head {
        let entry = context.command(
            vec![
                "ls-tree".into(),
                "-z".into(),
                head.clone(),
                "--".into(),
                path.into(),
            ],
            None,
            None,
            false,
        )?;
        if entry.is_empty() {
            context.remove_entry(&index, path)?;
        } else {
            let (metadata, filename) = entry
                .split_once_byte(b'\t')
                .ok_or_else(|| failed("Git tree entry is invalid"))?;
            if filename != [path.as_bytes(), b"\0"].concat() {
                return Err(failed("Git tree entry does not match the reviewed file"));
            }
            let metadata = std::str::from_utf8(metadata).map_err(failed)?;
            let values: Vec<_> = metadata.split_whitespace().collect();
            if values.len() != 3 || values[1] != "blob" || !matches!(values[0], "100644" | "100755")
            {
                return Err(failed("此 Git 对象类型不支持取消暂存"));
            }
            validate_oid(values[2], before.hash_bytes)?;
            context.command(
                vec![
                    "update-index".into(),
                    "--add".into(),
                    "--cacheinfo".into(),
                    values[0].into(),
                    values[2].into(),
                    path.into(),
                ],
                Some(&index),
                None,
                false,
            )?;
        }
    } else {
        context.remove_entry(&index, path)?;
    }
    let intended = read_path(&index, MAX_INDEX_BYTES)?
        .ok_or_else(|| failed("Git did not produce an index"))?;
    validate_index(&intended, before.hash_bytes, path)?;
    // Check that Git did not alter any other entry, including staging flags.
    let old_entries = index_entries(before.index.as_deref(), before.hash_bytes)?;
    let new_entries = index_entries(Some(&intended), before.hash_bytes)?;
    if old_entries
        .iter()
        .filter(|entry| entry.path != path.as_bytes())
        .collect::<Vec<_>>()
        != new_entries
            .iter()
            .filter(|entry| entry.path != path.as_bytes())
            .collect::<Vec<_>>()
    {
        return Err(failed("Git 尝试修改其他暂存条目，操作已取消"));
    }
    if context.snapshot(path)?.receipt != receipt || cancellation.is_cancelled() {
        return Err(if cancellation.is_cancelled() {
            ToolError::Cancelled
        } else {
            conflict()
        });
    }
    lock.file.write_all(&intended).map_err(failed)?;
    lock.file.sync_all().map_err(failed)?;
    if context.snapshot(path)?.receipt != receipt {
        return Err(conflict());
    }
    context.check_admin()?;
    lock.publish(&context.admin)?;
    // A failed directory sync follows the atomic publish; clearly report that
    // the result may already be visible instead of suggesting a blind retry.
    context.admin.sync_all().map_err(|error| {
        failed(format!(
            "暂存区可能已更新，但持久化未确认；请刷新后再操作：{error}"
        ))
    })?;
    Ok(())
}

struct IndexContext<'a> {
    workspace: &'a Workspace,
    root: File,
    admin: File,
    objects: File,
    cancellation: &'a CancellationToken,
    deadline: Instant,
}

impl<'a> IndexContext<'a> {
    fn new(
        workspace: &'a Workspace,
        cancellation: &'a CancellationToken,
    ) -> Result<Self, ToolError> {
        let deadline = Instant::now() + GIT_TIMEOUT;
        repository_root(workspace.root(), cancellation)?;
        let root = workspace.open_directory(Path::new("")).map_err(failed)?;
        let admin = directory_at(&root, ".git")
            .map_err(|_| failed("暂存操作需要本工作区内的普通 .git 目录；链接工作树仅供审查"))?;
        let objects = directory_at(&admin, "objects")?;
        let context = Self {
            workspace,
            root,
            admin,
            objects,
            cancellation,
            deadline,
        };
        context.check_admin()?;
        Ok(context)
    }

    fn check_admin(&self) -> Result<(), ToolError> {
        if self.cancellation.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        if Instant::now() >= self.deadline {
            return Err(failed("Git index operation timed out after 10 seconds"));
        }
        let root = open_directory_absolute(self.workspace.root())?;
        let admin = directory_at(&root, ".git")?;
        let objects = directory_at(&admin, "objects")?;
        if identity(&root.metadata().map_err(failed)?)
            != identity(&self.root.metadata().map_err(failed)?)
            || identity(&admin.metadata().map_err(failed)?)
                != identity(&self.admin.metadata().map_err(failed)?)
            || identity(&objects.metadata().map_err(failed)?)
                != identity(&self.objects.metadata().map_err(failed)?)
        {
            return Err(conflict());
        }
        // Git may consult these administrative files before each command. Do
        // not allow symlink/hardlink aliases to turn a local index action into
        // writes or reads through arbitrary administrative paths.
        for name in [
            "HEAD",
            "config",
            "config.worktree",
            "commondir",
            "objects/info/alternates",
        ] {
            read_relative_admin(&self.admin, name, MAX_ADMIN_BYTES)?;
        }
        if read_at(&self.admin, "commondir", MAX_ADMIN_BYTES)?.is_some() {
            return Err(failed("共享 Git 管理目录仅供审查，暂不支持修改暂存区"));
        }
        Ok(())
    }

    fn command(
        &self,
        args: Vec<String>,
        index: Option<&Path>,
        objects: Option<&Path>,
        allow_one: bool,
    ) -> Result<Vec<u8>, ToolError> {
        self.command_with_limit(args, index, objects, allow_one, MAX_PATCH_BYTES)
    }

    fn command_with_limit(
        &self,
        args: Vec<String>,
        index: Option<&Path>,
        objects: Option<&Path>,
        allow_one: bool,
        limit: usize,
    ) -> Result<Vec<u8>, ToolError> {
        self.check_admin()?;
        let mut pinned = vec![
            format!("--git-dir={}", fd_path(&self.admin).display()),
            format!("--work-tree={}", fd_path(&self.root).display()),
        ];
        pinned.extend(args);
        git_with_objects(
            self.workspace.root(),
            pinned,
            self.cancellation,
            limit,
            false,
            allow_one,
            index,
            objects,
            self.deadline,
        )
    }

    fn blob(&self, oid: &str, mode: u32) -> Result<Worktree, ToolError> {
        if !matches!(mode, 0o100644 | 0o100755) {
            return Err(failed("非普通 Git 对象仅供审查，暂不支持修改暂存区"));
        }
        let bytes = self.command_with_limit(
            vec!["cat-file".into(), "blob".into(), oid.into()],
            None,
            None,
            false,
            MAX_FILE_BYTES,
        )?;
        Ok(Worktree { bytes, mode })
    }

    fn raw_patch(
        &self,
        snapshot: &Snapshot,
        path: &str,
        staged: bool,
        untracked: bool,
    ) -> Result<String, ToolError> {
        let entries = index_entries(snapshot.index.as_deref(), snapshot.hash_bytes)?;
        let entry = entries.iter().find(|entry| entry.path == path.as_bytes());
        if untracked && (entry.is_some() || snapshot.worktree.is_none() || staged) {
            return Err(invalid("文件已不属于未跟踪列表，请刷新审查"));
        }
        let indexed = entry
            .map(|entry| {
                let oid: String = entry.oid.iter().map(|byte| format!("{byte:02x}")).collect();
                self.blob(&oid, entry.mode)
            })
            .transpose()?;
        let head = if staged {
            snapshot
                .head
                .as_ref()
                .map(|head| {
                    let entry = self.command(
                        vec![
                            "ls-tree".into(),
                            "-z".into(),
                            head.clone(),
                            "--".into(),
                            path.into(),
                        ],
                        None,
                        None,
                        false,
                    )?;
                    if entry.is_empty() {
                        return Ok(None);
                    }
                    let (metadata, filename) = entry
                        .split_once_byte(b'\t')
                        .ok_or_else(|| failed("Git tree entry is invalid"))?;
                    if filename != [path.as_bytes(), b"\0"].concat() {
                        return Err(failed("Git tree entry does not match the reviewed file"));
                    }
                    let metadata = std::str::from_utf8(metadata).map_err(failed)?;
                    let values: Vec<_> = metadata.split_whitespace().collect();
                    if values.len() != 3 || values[1] != "blob" {
                        return Err(failed("此 Git 对象类型不支持原始暂存审查"));
                    }
                    validate_oid(values[2], snapshot.hash_bytes)?;
                    let mode = u32::from_str_radix(values[0], 8).map_err(failed)?;
                    self.blob(values[2], mode).map(Some)
                })
                .transpose()?
                .flatten()
        } else {
            None
        };
        let (base, target) = if staged {
            (head.as_ref(), indexed.as_ref())
        } else {
            (indexed.as_ref(), snapshot.worktree.as_ref())
        };
        raw_patch(base, target, path, self.cancellation, self.deadline)
    }

    fn snapshot(&self, path: &str) -> Result<Snapshot, ToolError> {
        self.check_admin()?;
        let checked = checked_review_path(self.workspace, path)?;
        if checked != path {
            return Err(invalid("请使用审查列表中的相对文件路径"));
        }
        let format = self.command(
            vec!["rev-parse".into(), "--show-object-format".into()],
            None,
            None,
            false,
        )?;
        let hash_bytes = match format.as_slice() {
            b"sha1\n" => 20,
            b"sha256\n" => 32,
            _ => return Err(failed("不支持此 Git 对象格式")),
        };
        let index = read_at(&self.admin, "index", MAX_INDEX_BYTES)?;
        if let Some(index) = &index {
            validate_index(index, hash_bytes, path)?;
        }
        let head_bytes = read_at(&self.admin, "HEAD", MAX_ADMIN_BYTES)?
            .ok_or_else(|| failed("Git HEAD is missing"))?;
        let head_text = std::str::from_utf8(&head_bytes).map_err(failed)?.trim();
        let head_ref = head_text.strip_prefix("ref: ").map(str::to_owned);
        if let Some(name) = &head_ref {
            validate_ref(name)?;
            read_relative_admin(&self.admin, name, MAX_ADMIN_BYTES)?;
        }
        let head = self.command(
            vec![
                "rev-parse".into(),
                "--verify".into(),
                "--quiet".into(),
                "HEAD".into(),
            ],
            None,
            None,
            true,
        )?;
        let head = std::str::from_utf8(&head).map_err(failed)?.trim();
        let head = if head.is_empty() {
            None
        } else {
            validate_oid(head, hash_bytes)?;
            Some(head.to_owned())
        };
        let worktree = read_worktree(self.workspace, path)?;
        let mut digest = Sha256::new();
        for bytes in [
            b"orbit-review-index-v1".as_slice(),
            self.workspace.root().as_os_str().as_encoded_bytes(),
            path.as_bytes(),
            &head_bytes,
        ] {
            digest_field(&mut digest, Some(bytes));
        }
        digest_field(&mut digest, index.as_deref());
        digest_field(&mut digest, head.as_deref().map(str::as_bytes));
        for name in [
            "config",
            "config.worktree",
            "packed-refs",
            "info/attributes",
            "objects/info/alternates",
        ] {
            digest_field(
                &mut digest,
                read_relative_admin(&self.admin, name, MAX_ADMIN_BYTES)?.as_deref(),
            );
        }
        let root_identity = identity(&self.root.metadata().map_err(failed)?);
        let admin_identity = identity(&self.admin.metadata().map_err(failed)?);
        for value in [
            root_identity.0,
            root_identity.1,
            admin_identity.0,
            admin_identity.1,
        ] {
            digest.update(value.to_be_bytes());
        }
        if let Some(worktree) = &worktree {
            digest.update(worktree.mode.to_be_bytes());
            digest_field(&mut digest, Some(&worktree.bytes));
        } else {
            digest.update(0_u32.to_be_bytes());
            digest_field(&mut digest, None);
        }
        // Re-read all mutable inputs after the potentially lengthy Git call.
        if read_at(&self.admin, "index", MAX_INDEX_BYTES)? != index
            || read_at(&self.admin, "HEAD", MAX_ADMIN_BYTES)?.as_deref()
                != Some(head_bytes.as_slice())
        {
            return Err(conflict());
        }
        Ok(Snapshot {
            index,
            worktree,
            head,
            head_ref,
            hash_bytes,
            receipt: digest
                .finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        })
    }

    fn remove_entry(&self, index: &Path, path: &str) -> Result<(), ToolError> {
        self.command(
            vec![
                "update-index".into(),
                "--force-remove".into(),
                "--".into(),
                path.into(),
            ],
            Some(index),
            None,
            false,
        )
        .map(|_| ())
    }

    fn ref_lock(&self, name: &str) -> Result<GitLock, ToolError> {
        validate_ref(name)?;
        let (parent, leaf) = admin_parent(&self.admin, name)?
            .ok_or_else(|| failed("该未创建分支目录暂不支持暂存，请先创建分支"))?;
        GitLock::acquire(&parent, &format!("{leaf}.lock"))
    }

    fn install_object(&self, temporary: &Path, oid: &str) -> Result<(), ToolError> {
        self.check_admin()?;
        let (prefix, suffix) = oid.split_at(2);
        match mkdirat(&self.objects, prefix, Mode::from_raw_mode(0o755)) {
            Ok(()) => {}
            Err(error) if error == rustix::io::Errno::EXIST => {}
            Err(error) => return Err(failed(error)),
        }
        let shard = directory_at(&self.objects, prefix)?;
        // Publish a complete object atomically, without replacing an existing
        // object or following a hostile shard/file symlink.
        static OBJECT_SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let name = format!(
            ".orbit-object-{}-{}",
            std::process::id(),
            OBJECT_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        let mut object = GitLock::acquire(&shard, &name)?;
        let mut source = File::open(temporary.join(prefix).join(suffix)).map_err(failed)?;
        io::copy(&mut source, &mut object.file).map_err(failed)?;
        fchmod(&object.file, Mode::from_raw_mode(0o444)).map_err(failed)?;
        object.file.sync_all().map_err(failed)?;
        if !object.owns_path() {
            return Err(conflict());
        }
        match renameat_with(
            &shard,
            name.as_str(),
            &shard,
            suffix,
            RenameFlags::NOREPLACE,
        ) {
            Ok(()) => {
                object.published = true;
                shard.sync_all().map_err(failed)?;
            }
            Err(error) if error == rustix::io::Errno::EXIST => {
                let existing = read_at(&shard, suffix, MAX_FILE_BYTES + MAX_ADMIN_BYTES)?
                    .ok_or_else(conflict)?;
                let intended = read_path(
                    &temporary.join(prefix).join(suffix),
                    MAX_FILE_BYTES + MAX_ADMIN_BYTES,
                )?
                .ok_or_else(conflict)?;
                if existing != intended {
                    return Err(failed("Git 对象已存在且编码不同，请先检查仓库完整性"));
                }
            }
            Err(error) => return Err(failed(error)),
        }
        Ok(())
    }
}

struct Snapshot {
    index: Option<Vec<u8>>,
    worktree: Option<Worktree>,
    head: Option<String>,
    head_ref: Option<String>,
    hash_bytes: usize,
    receipt: String,
}
struct Worktree {
    bytes: Vec<u8>,
    mode: u32,
}

struct GitLock {
    parent: File,
    name: String,
    file: File,
    published: bool,
}
impl GitLock {
    fn acquire(parent: &File, name: &str) -> Result<Self, ToolError> {
        // A descriptor exhaustion after CREATE must not strand a lock.
        let parent = parent.try_clone().map_err(failed)?;
        let fd = openat(
            &parent,
            name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .map_err(|error| {
            if error == rustix::io::Errno::EXIST {
                failed("Git 操作正在进行（锁已存在），请等待完成后刷新")
            } else {
                failed(error)
            }
        })?;
        Ok(Self {
            parent,
            name: name.into(),
            file: File::from(fd),
            published: false,
        })
    }
    fn owns_path(&self) -> bool {
        let Ok(fd) = openat(
            &self.parent,
            self.name.as_str(),
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        ) else {
            return false;
        };
        let Ok(current) = File::from(fd).metadata() else {
            return false;
        };
        self.file.metadata().is_ok_and(|owned| {
            identity(&current) == identity(&owned) && current.is_file() && current.nlink() == 1
        })
    }
    fn publish(&mut self, admin: &File) -> Result<(), ToolError> {
        if !self.owns_path() {
            return Err(conflict());
        }
        renameat(&self.parent, self.name.as_str(), admin, "index").map_err(failed)?;
        self.published = true;
        Ok(())
    }
}
impl Drop for GitLock {
    fn drop(&mut self) {
        if !self.published && self.owns_path() {
            let _ = unlinkat(&self.parent, self.name.as_str(), AtFlags::empty());
        }
    }
}

fn private_file(path: &Path) -> Result<File, ToolError> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(failed)
}

fn raw_patch(
    base: Option<&Worktree>,
    target: Option<&Worktree>,
    path: &str,
    cancellation: &CancellationToken,
    deadline: Instant,
) -> Result<String, ToolError> {
    if base.is_none() && target.is_none() {
        return Ok(String::new());
    }
    // Fixed filenames in an isolated directory ensure that even a reviewed
    // .gitattributes file cannot supply attributes for either input. No actual
    // repository configuration, clean filter or EOL conversion participates.
    let temporary = tempfile::tempdir().map_err(failed)?;
    for (side, file) in [("a", base), ("b", target)] {
        if let Some(file) = file {
            let directory = temporary.path().join(side);
            std::fs::create_dir(&directory).map_err(failed)?;
            let mut output = private_file(&directory.join("review"))?;
            output.write_all(&file.bytes).map_err(failed)?;
            if file.mode == 0o100755 {
                fchmod(&output, Mode::from_raw_mode(0o700)).map_err(failed)?;
            }
        }
    }
    let left = if base.is_some() {
        "a/review"
    } else {
        "/dev/null"
    };
    let right = if target.is_some() {
        "b/review"
    } else {
        "/dev/null"
    };
    let patch = git_with_objects(
        temporary.path(),
        vec![
            "-c".into(),
            "core.autocrlf=false".into(),
            "-c".into(),
            "core.filemode=true".into(),
            "diff".into(),
            "--no-index".into(),
            "--no-prefix".into(),
            "--no-ext-diff".into(),
            "--no-textconv".into(),
            "--no-renames".into(),
            "--unified=3".into(),
            "--".into(),
            left.into(),
            right.into(),
        ],
        cancellation,
        MAX_PATCH_BYTES,
        false,
        true,
        None,
        None,
        deadline,
    )?;
    let patch =
        String::from_utf8(patch).map_err(|_| failed("Git patch contains non-UTF-8 content"))?;
    let old = quoted_patch_path(&format!("a/{path}"));
    let new = quoted_patch_path(&format!("b/{path}"));
    let mut result = String::with_capacity(patch.len() + path.len() * 4);
    for line in patch.split_inclusive('\n') {
        let body = line.strip_suffix('\n').unwrap_or(line);
        let replacement = if body.starts_with("diff --git ") {
            Some(format!("diff --git {old} {new}"))
        } else if body == "--- a/review" {
            Some(format!("--- {old}"))
        } else if body == "+++ b/review" {
            Some(format!("+++ {new}"))
        } else if body.starts_with("Binary files ") {
            Some(format!(
                "Binary files {} and {} differ",
                if base.is_some() {
                    old.as_str()
                } else {
                    "/dev/null"
                },
                if target.is_some() {
                    new.as_str()
                } else {
                    "/dev/null"
                },
            ))
        } else {
            None
        };
        if let Some(replacement) = replacement {
            result.push_str(&replacement);
            if line.ends_with('\n') {
                result.push('\n');
            }
        } else {
            // Preserve CRLF in hunk contents verbatim.
            result.push_str(line);
        }
    }
    Ok(result)
}

fn quoted_patch_path(path: &str) -> String {
    if path
        .bytes()
        .all(|byte| (33..127).contains(&byte) && !matches!(byte, b'"' | b'\\'))
    {
        return path.into();
    }
    let mut quoted = String::from("\"");
    for byte in path.bytes() {
        match byte {
            b'\t' => quoted.push_str("\\t"),
            b'\n' => quoted.push_str("\\n"),
            b'\r' => quoted.push_str("\\r"),
            b'\\' => quoted.push_str("\\\\"),
            b'"' => quoted.push_str("\\\""),
            32..=126 => quoted.push(char::from(byte)),
            _ => quoted.push_str(&format!("\\{byte:03o}")),
        }
    }
    quoted.push('"');
    quoted
}
fn directory_at(parent: &File, name: impl rustix::path::Arg) -> Result<File, ToolError> {
    openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(failed)
}
fn open_directory_absolute(path: &Path) -> Result<File, ToolError> {
    let mut directory = File::open("/").map_err(failed)?;
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => directory = directory_at(&directory, name)?,
            _ => return Err(failed("invalid repository directory")),
        }
    }
    Ok(directory)
}
fn fd_path(file: &File) -> PathBuf {
    PathBuf::from(format!(
        "/proc/{}/fd/{}",
        std::process::id(),
        file.as_raw_fd()
    ))
}
fn path_text(path: &Path) -> Result<String, ToolError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| failed("temporary Git path is not UTF-8"))
}
fn identity(metadata: &Metadata) -> (u64, u64) {
    (metadata.dev(), metadata.ino())
}
fn fingerprint(metadata: &Metadata) -> (u64, u64, u64, u32, i64, i64, i64, i64) {
    (
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.mode(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    )
}

fn read_file(mut file: File, limit: usize) -> Result<Vec<u8>, ToolError> {
    let before = file.metadata().map_err(failed)?;
    if !before.is_file() || before.nlink() != 1 {
        return Err(failed("Git 管理文件必须是无硬链接的普通文件"));
    }
    if before.len() > limit as u64 {
        return Err(failed(format!("文件超过 {limit} 字节的安全上限")));
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(failed)?;
    if bytes.len() > limit || fingerprint(&before) != fingerprint(&file.metadata().map_err(failed)?)
    {
        return Err(conflict());
    }
    Ok(bytes)
}
fn read_at(parent: &File, name: &str, limit: usize) -> Result<Option<Vec<u8>>, ToolError> {
    match openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    ) {
        Ok(fd) => read_file(File::from(fd), limit).map(Some),
        Err(error) if error == rustix::io::Errno::NOENT => Ok(None),
        Err(error) => Err(failed(error)),
    }
}
fn read_path(path: &Path, limit: usize) -> Result<Option<Vec<u8>>, ToolError> {
    let parent =
        open_directory_absolute(path.parent().ok_or_else(|| failed("invalid index path"))?)?;
    read_at(
        &parent,
        path.file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| failed("invalid index name"))?,
        limit,
    )
}
fn admin_parent(admin: &File, name: &str) -> Result<Option<(File, String)>, ToolError> {
    let mut parent = admin.try_clone().map_err(failed)?;
    let path = Path::new(name);
    let mut components = path.components().peekable();
    while let Some(component) = components.next() {
        let Component::Normal(component) = component else {
            return Err(failed("invalid Git administrative path"));
        };
        if components.peek().is_none() {
            return Ok(Some((
                parent,
                component
                    .to_str()
                    .ok_or_else(|| failed("Git administrative path is not UTF-8"))?
                    .into(),
            )));
        }
        match openat(
            &parent,
            component,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => parent = File::from(fd),
            Err(error) if error == rustix::io::Errno::NOENT => return Ok(None),
            Err(error) => return Err(failed(error)),
        }
    }
    Err(failed("empty Git administrative path"))
}
fn read_relative_admin(
    admin: &File,
    name: &str,
    limit: usize,
) -> Result<Option<Vec<u8>>, ToolError> {
    let Some((parent, leaf)) = admin_parent(admin, name)? else {
        return Ok(None);
    };
    read_at(&parent, &leaf, limit)
}
fn read_worktree(workspace: &Workspace, path: &str) -> Result<Option<Worktree>, ToolError> {
    // Unlike read-only review, index updates intentionally refuse all symlink
    // components so a regular file snapshot can never stage a symlink target.
    let root = workspace.open_directory(Path::new("")).map_err(failed)?;
    let Some((parent, leaf)) = admin_parent(&root, path)? else {
        return Ok(None);
    };
    match openat(
        &parent,
        leaf.as_str(),
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => {
            let mut file = File::from(fd);
            let before = file.metadata().map_err(failed)?;
            if !before.is_file() {
                return Err(failed("非普通文件仅供审查，暂不支持修改暂存区"));
            }
            // The strict nofollow descriptor supplies the bytes; the retained
            // Workspace separately authorizes that exact inode (including
            // protected hardlink aliases), rather than a newly built registry.
            let authorized = workspace.open_read(Path::new(path)).map_err(failed)?;
            if identity(&before) != identity(&authorized.metadata().map_err(failed)?) {
                return Err(conflict());
            }
            if before.len() > MAX_FILE_BYTES as u64 {
                return Err(failed("文件超过 8MiB，暂不支持原生暂存"));
            }
            let mut bytes = Vec::new();
            (&mut file)
                .take(MAX_FILE_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(failed)?;
            if bytes.len() > MAX_FILE_BYTES
                || fingerprint(&before) != fingerprint(&file.metadata().map_err(failed)?)
            {
                return Err(conflict());
            }
            Ok(Some(Worktree {
                bytes,
                mode: if before.mode() & 0o111 != 0 {
                    0o100755
                } else {
                    0o100644
                },
            }))
        }
        Err(error) if error == rustix::io::Errno::NOENT => Ok(None),
        Err(error) if error == rustix::io::Errno::LOOP => {
            Err(failed("符号链接仅供审查，暂不支持修改暂存区"))
        }
        Err(error) => Err(failed(error)),
    }
}
fn validate_ref(name: &str) -> Result<(), ToolError> {
    if !name.starts_with("refs/heads/")
        || name.ends_with('/')
        || name.contains("..")
        || name.contains('@')
        || name.contains('\\')
        || name.contains(':')
        || name.bytes().any(|byte| byte <= 32 || byte == 127)
        || name
            .split('/')
            .any(|part| part.is_empty() || part.starts_with('.') || part.ends_with(".lock"))
    {
        return Err(failed("此 HEAD 引用不支持安全暂存"));
    }
    Ok(())
}
fn validate_oid(oid: &str, bytes: usize) -> Result<(), ToolError> {
    if oid.len() != bytes * 2 || !oid.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(failed("Git object id is invalid"));
    }
    Ok(())
}
fn conflict() -> ToolError {
    failed("文件、Git HEAD 或暂存区已变更，请刷新差异后重新审查；此次操作未覆盖工作区文件")
}
fn digest_field(digest: &mut Sha256, bytes: Option<&[u8]>) {
    digest.update([u8::from(bytes.is_some())]);
    if let Some(bytes) = bytes {
        digest.update((bytes.len() as u64).to_be_bytes());
        digest.update(bytes);
    }
}

#[derive(PartialEq, Eq)]
struct IndexEntry<'a> {
    path: &'a [u8],
    mode: u32,
    oid: &'a [u8],
    flags: u16,
    extended: u16,
}
fn validate_index(bytes: &[u8], hash_bytes: usize, path: &str) -> Result<(), ToolError> {
    for entry in index_entries(Some(bytes), hash_bytes)? {
        if entry.path == path.as_bytes()
            && (entry.flags & 0x3000 != 0
                || entry.extended & 0x4000 != 0
                || !matches!(entry.mode, 0o100644 | 0o100755))
        {
            return Err(failed(
                "冲突、稀疏条目或非普通文件仅供审查，暂不支持修改暂存区",
            ));
        }
    }
    Ok(())
}
fn index_entries(
    bytes: Option<&[u8]>,
    hash_bytes: usize,
) -> Result<Vec<IndexEntry<'_>>, ToolError> {
    let Some(bytes) = bytes else {
        return Ok(Vec::new());
    };
    if bytes.len() < 12 + hash_bytes || &bytes[..4] != b"DIRC" {
        return Err(failed("Git index 格式无效"));
    }
    let version = u32::from_be_bytes(bytes[4..8].try_into().unwrap());
    if !matches!(version, 2 | 3) {
        return Err(failed("暂仅支持 Git index v2/v3；其他格式仍可审查"));
    }
    let count = u32::from_be_bytes(bytes[8..12].try_into().unwrap()) as usize;
    let end = bytes.len() - hash_bytes;
    let mut cursor: usize = 12;
    let mut entries = Vec::new();
    for _ in 0..count {
        let start = cursor;
        let fixed = 40 + hash_bytes + 2;
        if cursor.saturating_add(fixed) > end {
            return Err(failed("Git index 条目不完整"));
        }
        let mode = u32::from_be_bytes(bytes[cursor + 24..cursor + 28].try_into().unwrap());
        let oid = &bytes[cursor + 40..cursor + 40 + hash_bytes];
        let flags = u16::from_be_bytes(
            bytes[cursor + 40 + hash_bytes..cursor + fixed]
                .try_into()
                .unwrap(),
        );
        cursor += fixed;
        let extended = if flags & 0x4000 != 0 {
            if version != 3 || cursor + 2 > end {
                return Err(failed("Git index 扩展条目无效"));
            }
            let value = u16::from_be_bytes(bytes[cursor..cursor + 2].try_into().unwrap());
            cursor += 2;
            value
        } else {
            0
        };
        let length = bytes[cursor..end]
            .iter()
            .position(|byte| *byte == 0)
            .ok_or_else(|| failed("Git index 路径不完整"))?;
        let path = &bytes[cursor..cursor + length];
        cursor = start + (cursor + length + 1 - start).div_ceil(8) * 8;
        if cursor > end {
            return Err(failed("Git index padding is invalid"));
        }
        entries.push(IndexEntry {
            path,
            mode,
            oid,
            flags,
            extended,
        });
    }
    while cursor < end {
        if cursor + 8 > end {
            return Err(failed("Git index extension is incomplete"));
        }
        let signature = &bytes[cursor..cursor + 4];
        let length = u32::from_be_bytes(bytes[cursor + 4..cursor + 8].try_into().unwrap()) as usize;
        if !signature[0].is_ascii_uppercase() {
            return Err(failed("split/sparse 或未知必需 index 扩展仅供审查"));
        }
        cursor = cursor
            .checked_add(8 + length)
            .filter(|cursor| *cursor <= end)
            .ok_or_else(|| failed("Git index extension is invalid"))?;
    }
    Ok(entries)
}

trait SplitOnceByte {
    fn split_once_byte(&self, byte: u8) -> Option<(&[u8], &[u8])>;
}
impl SplitOnceByte for [u8] {
    fn split_once_byte(&self, byte: u8) -> Option<(&[u8], &[u8])> {
        let position = self.iter().position(|value| *value == byte)?;
        Some((&self[..position], &self[position + 1..]))
    }
}
