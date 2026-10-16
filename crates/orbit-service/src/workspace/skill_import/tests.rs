use super::*;

fn isolated_home(root: &Path) -> PathBuf {
    let home = root.join("home");
    fs::create_dir(&home).unwrap();
    home
}

fn skill(root: &Path, relative: &str, name: &str) {
    let directory = root.join(relative);
    fs::create_dir_all(&directory).unwrap();
    fs::write(
        directory.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: Review changes carefully\n---\nRead the diff.\n"),
    )
    .unwrap();
}

async fn scan(source: &Path, home: &Path) -> Scan {
    Scan::acquire_to(
        "global-library".into(),
        home.into(),
        source.display().to_string(),
        None,
        HashSet::new(),
        home.join(".config/orbit/skills"),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn discovery_freezes_complete_packages_and_installs_without_executing_scripts() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let home = isolated_home(root.path());
    skill(&source, "skills/review", "review");
    for (path, content) in [
        ("scripts/check.sh", "#!/bin/sh\nexit 99"),
        ("references/guide.md", "original"),
        ("assets/icon.svg", "<svg/>"),
    ] {
        let file = source.join("skills/review").join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, content).unwrap();
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            source.join("skills/review/scripts/check.sh"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    let scan = scan(&source, &home).await;
    assert_eq!(scan.candidates.len(), 1);
    assert_eq!(scan.candidates[0].path, "skills/review");
    fs::write(
        source.join("skills/review/references/guide.md"),
        "changed after scan",
    )
    .unwrap();
    scan.install(
        &scan.token,
        "global-library",
        &home,
        &["skills/review".into()],
        &HashSet::new(),
    )
    .unwrap();
    let installed = home.join(".config/orbit/skills/review");
    assert_eq!(
        fs::read_to_string(installed.join("references/guide.md")).unwrap(),
        "original"
    );
    assert!(
        installed
            .join("assets/icon.svg")
            .is_file()
    );
    assert_eq!(
        fs::read_to_string(installed.join("scripts/check.sh")).unwrap(),
        "#!/bin/sh\nexit 99"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(installed.join("scripts/check.sh"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
    }
    let skills = crate::skills::Skills::global(Some(home.clone()), Some(home.join(".config")));
    assert_eq!(skills.len(), 1);
    assert!(skills.get("review").is_some());
    assert!(
        scan.install(
            &scan.token,
            "global-library",
            &home,
            &["skills/review".into()],
            &HashSet::new()
        )
        .is_err()
    );
}

#[tokio::test]
async fn conflicts_preflight_whole_selection_and_snapshot_identity_is_bound() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let home = isolated_home(root.path());
    let other = root.path().join("other");
    fs::create_dir(&other).unwrap();
    skill(&source, "first", "first");
    skill(&source, "second", "second");
    let mut scan = scan(&source, &home).await;
    let paths = vec!["first".into(), "second".into()];
    for (token, session, target, selection) in [
        ("wrong", "global-library", home.as_path(), paths.as_slice()),
        (
            scan.token.as_str(),
            "other-library",
            home.as_path(),
            paths.as_slice(),
        ),
        (
            scan.token.as_str(),
            "global-library",
            other.as_path(),
            paths.as_slice(),
        ),
        (
            scan.token.as_str(),
            "global-library",
            home.as_path(),
            &["../first".into()],
        ),
        (
            scan.token.as_str(),
            "global-library",
            home.as_path(),
            &["/first".into()],
        ),
        (
            scan.token.as_str(),
            "global-library",
            home.as_path(),
            &["first".into(), "first".into()],
        ),
    ] {
        assert!(
            scan.install(token, session, target, selection, &HashSet::new())
                .is_err()
        );
    }
    assert!(!home.join(".config").exists());
    // A conflict appearing after scanning cannot leave an earlier selection installed.
    let existing = home.join(".config/orbit/skills/second");
    fs::create_dir_all(&existing).unwrap();
    fs::write(existing.join("keep.txt"), "do not replace").unwrap();
    assert!(
        scan.install(
            &scan.token,
            "global-library",
            &home,
            &paths,
            &HashSet::new()
        )
        .is_err()
    );
    assert!(
        !home
            .join(".config/orbit/skills/first")
            .exists()
    );
    assert_eq!(
        fs::read_to_string(existing.join("keep.txt")).unwrap(),
        "do not replace"
    );
    scan.created = Instant::now() - LIFETIME - Duration::from_secs(1);
    assert!(
        scan.install(
            &scan.token,
            "global-library",
            &home,
            &["first".into()],
            &HashSet::new()
        )
        .unwrap_err()
        .to_string()
        .contains("过期")
    );
}

#[tokio::test]
async fn duplicate_names_and_installed_user_skills_are_reported_as_conflicts() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let home = isolated_home(root.path());
    skill(&source, "alpha", "duplicate");
    skill(&source, "beta", "duplicate");
    skill(&source, "user", "personal");
    let scan = Scan::acquire_to(
        "global-library".into(),
        home.clone(),
        source.display().to_string(),
        None,
        HashSet::from(["personal".into()]),
        home.join(".config/orbit/skills"),
    )
    .await
    .unwrap();
    assert_eq!(scan.candidates.len(), 3);
    assert!(
        scan.candidates
            .iter()
            .all(|candidate| candidate.conflict)
    );
    assert!(
        scan.install(
            &scan.token,
            "global-library",
            &home,
            &["alpha".into()],
            &HashSet::new()
        )
        .is_err()
    );
}

#[tokio::test]
async fn direct_skill_file_and_relative_home_source_are_supported() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path();
    skill(home, "incoming/review", "review");
    let scan = Scan::acquire_to(
        "global-library".into(),
        root.path().into(),
        "incoming/review/SKILL.md".into(),
        None,
        HashSet::new(),
        home.join(".config/orbit/skills"),
    )
    .await
    .unwrap();
    assert_eq!(scan.candidates[0].path, ".");
    assert_eq!(scan.candidates[0].name, "review");
    scan.install(
        &scan.token,
        "global-library",
        root.path(),
        &[".".into()],
        &HashSet::new(),
    )
    .unwrap();
    assert!(
        root.path()
            .join(".config/orbit/skills/review/SKILL.md")
            .is_file()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn symlink_sources_resources_and_destinations_are_rejected() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let home = isolated_home(root.path());
    skill(&source, "review", "review");
    let link = source.join("review/link");
    symlink("/etc/passwd", &link).unwrap();
    assert!(
        Scan::acquire_to(
            "global-library".into(),
            home.clone(),
            source.display().to_string(),
            None,
            HashSet::new(),
            home.join(".config/orbit/skills"),
        )
        .await
        .is_err()
    );
    fs::remove_file(link).unwrap();
    symlink(&source, root.path().join("alias")).unwrap();
    assert!(
        Scan::acquire_to(
            "global-library".into(),
            home.clone(),
            root.path()
                .join("alias")
                .display()
                .to_string(),
            None,
            HashSet::new(),
            home.join(".config/orbit/skills"),
        )
        .await
        .is_err()
    );
    let scan = scan(&source, &home).await;
    symlink(&source, home.join(".config")).unwrap();
    assert!(
        scan.install(
            &scan.token,
            "global-library",
            &home,
            &["review".into()],
            &HashSet::new()
        )
        .is_err()
    );
    assert!(!source.join("skills").exists());
}

#[tokio::test]
async fn oversized_resources_fail_discovery_without_installing_anything() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let home = isolated_home(root.path());
    skill(&source, "review", "review");
    fs::File::create(source.join("review/large.bin"))
        .unwrap()
        .set_len(MAX_FILE_BYTES + 1)
        .unwrap();
    let error = Scan::acquire_to(
        "global-library".into(),
        home.clone(),
        source.display().to_string(),
        None,
        HashSet::new(),
        home.join(".config/orbit/skills"),
    )
    .await
    .err()
    .unwrap();
    assert!(error.to_string().contains("8 MiB"));
    assert!(!home.join(".config").exists());
}

#[tokio::test]
async fn empty_or_heading_only_skills_are_rejected_before_selection() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source/review");
    let home = isolated_home(root.path());
    fs::create_dir_all(&source).unwrap();
    for content in ["", "# Heading", "---\nname: review\n---\n# Heading"] {
        fs::write(source.join("SKILL.md"), content).unwrap();
        let error = Scan::acquire_to(
            "global-library".into(),
            home.clone(),
            source.display().to_string(),
            None,
            HashSet::new(),
            home.join(".config/orbit/skills"),
        )
        .await
        .err()
        .unwrap();
        assert!(
            error
                .to_string()
                .contains("缺少说明或正文")
        );
        assert!(!home.join(".config").exists());
    }
}
