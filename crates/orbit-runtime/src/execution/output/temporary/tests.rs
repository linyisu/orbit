use super::*;

#[tokio::test]
async fn task_end_expires_access_but_keeps_outstanding_io_alive() {
    let outputs = CommandOutputs::default();
    assert!(outputs.lease().is_none());
    assert!(outputs.create_log().await.is_err());
    let task = outputs.begin_run().unwrap();
    assert!(
        outputs.lease().is_none(),
        "starting a task allocates no files"
    );
    assert!(outputs.begin_run().is_err());
    let log = outputs.create_log().await.unwrap();
    let lease = outputs.lease().unwrap();
    let path = lease.path().to_owned();
    assert!(path.starts_with(CommandOutputs::root()));
    assert!(path.starts_with(std::env::temp_dir()));
    assert_eq!(path.metadata().unwrap().permissions().mode() & 0o777, 0o700);
    let (id, mut stdout, stderr) = log.split();
    stdout.append(b"first page").await.unwrap();
    let file = path.join(format!("{id}.stdout"));
    assert_eq!(std::fs::read(&file).unwrap(), b"first page");

    drop(task);
    assert!(outputs.lease().is_none());
    assert!(outputs.create_log().await.is_err());
    // A late writer or an already-started page read owns its directory.
    stdout.append(b"; final page").await.unwrap();
    drop(stdout);
    drop(stderr);
    assert_eq!(std::fs::read(&file).unwrap(), b"first page; final page");

    let next = outputs.begin_run().unwrap();
    let next_log = outputs.create_log().await.unwrap();
    assert_ne!(outputs.lease().unwrap().path(), path);
    assert!(
        !outputs
            .lease()
            .unwrap()
            .path()
            .join(file.file_name().unwrap())
            .exists()
    );
    drop(lease);
    assert!(!path.exists());
    drop(next_log);
    let next_path = outputs.lease().unwrap().path().to_owned();
    drop(next);
    assert!(!next_path.exists());
}

#[test]
fn orphan_collection_keeps_live_directories_unknown_files_and_symlinks() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("outputs");
    let live = create_directory(&root).unwrap();
    let live_path = live.directory.path().to_owned();
    let RunDirectory { directory, _lock } = create_directory(&root).unwrap();
    let orphan = directory.keep();
    std::fs::write(orphan.join("cmd-old.stdout"), b"orphan").unwrap();
    drop(_lock); // Model abrupt process exit while leaving the files behind.

    let external = tempfile::tempdir().unwrap();
    std::fs::write(external.path().join("keep"), b"user file").unwrap();
    std::os::unix::fs::symlink(external.path(), root.join("run-link")).unwrap();
    std::fs::create_dir(root.join("run-unknown")).unwrap();
    std::fs::write(root.join("unknown"), b"keep").unwrap();
    let fresh = create_directory(&root).unwrap();
    assert!(!orphan.exists());
    assert!(live_path.exists());
    assert!(
        root.join("run-link")
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(external.path().join("keep").exists());
    assert!(root.join("run-unknown").exists() && root.join("unknown").exists());
    drop(fresh);
    drop(live);
    assert!(!live_path.exists());
}

#[test]
fn ownership_root_rejects_public_directories_and_symlinks() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("public");
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(create_directory(&root).is_err());
    let link = temporary.path().join("link");
    std::os::unix::fs::symlink(temporary.path(), &link).unwrap();
    assert!(create_directory(&link).is_err());
}
