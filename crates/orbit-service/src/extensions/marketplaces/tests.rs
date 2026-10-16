use super::*;

fn git(root: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "git {:?} failed:\nstdout: {}\nstderr: {}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    String::from_utf8(output.stdout)
        .unwrap()
        .trim()
        .to_owned()
}

#[tokio::test]
async fn registers_a_local_claude_marketplace_without_touching_a_project() {
    let temp = tempfile::tempdir().unwrap();
    let market = temp.path().join("market");
    fs::create_dir_all(market.join(".claude-plugin")).unwrap();
    fs::write(
        market.join(".claude-plugin/marketplace.json"),
        r#"{
          "name":"team-tools",
          "plugins":[{"name":"review","description":"Review code","source":"./plugins/review"}]
        }"#,
    )
    .unwrap();
    let store = Store::at(temp.path().join("orbit-data"));
    let registered = store
        .add(market.to_string_lossy().into_owned())
        .await
        .unwrap();

    assert_eq!(registered.name, "team-tools");
    assert_eq!(registered.plugins[0].name, "review");
    assert_eq!(
        registered.plugins[0].source.as_deref(),
        Some("./plugins/review")
    );
    assert_eq!(store.list().unwrap().len(), 1);
}

#[tokio::test]
async fn refresh_replaces_the_same_source_even_if_the_catalogue_name_changes() {
    let temp = tempfile::tempdir().unwrap();
    let market = temp.path().join("market");
    fs::create_dir_all(market.join(".claude-plugin")).unwrap();
    let manifest = market.join(".claude-plugin/marketplace.json");
    fs::write(&manifest, r#"{"name":"before","plugins":[]}"#).unwrap();
    let source = market.to_string_lossy().into_owned();
    let store = Store::at(temp.path().join("orbit-data"));
    store.add(source.clone()).await.unwrap();
    fs::write(
        &manifest,
        r#"{"name":"after","plugins":[{"name":"review","source":"plugins/review"}]}"#,
    )
    .unwrap();

    let refreshed = store
        .refresh(source.clone())
        .await
        .unwrap();

    assert_eq!(refreshed.name, "after");
    assert_eq!(refreshed.plugins.len(), 1);
    assert_eq!(store.list().unwrap(), vec![refreshed]);
    assert!(
        store
            .refresh("missing/source".into())
            .await
            .is_err()
    );

    fs::write(&manifest, "invalid JSON").unwrap();
    assert!(store.refresh(source).await.is_err());
    assert_eq!(store.list().unwrap()[0].name, "after");
}

#[tokio::test]
async fn remove_unregisters_only_the_requested_source_and_preserves_files() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::at(temp.path().join("orbit-data"));
    for name in ["first", "second"] {
        let market = temp.path().join(name);
        fs::create_dir_all(market.join(".claude-plugin")).unwrap();
        fs::write(
            market.join(".claude-plugin/marketplace.json"),
            format!(r#"{{"name":"{name}","plugins":[]}}"#),
        )
        .unwrap();
        store
            .add(market.to_string_lossy().into_owned())
            .await
            .unwrap();
    }
    // Cached source files are retained too; removal is not a cache deletion.
    let cached = store
        .root
        .join("source-existing-checkout");
    fs::create_dir(&cached).unwrap();
    fs::write(cached.join("package.txt"), "retained").unwrap();
    let first = temp.path().join("first");

    store
        .remove(first.to_str().unwrap())
        .unwrap();

    assert_eq!(store.list().unwrap().len(), 1);
    assert_eq!(store.list().unwrap()[0].name, "second");
    assert!(
        first
            .join(".claude-plugin/marketplace.json")
            .is_file()
    );
    assert_eq!(
        fs::read_to_string(cached.join("package.txt")).unwrap(),
        "retained"
    );
    assert!(
        store
            .remove(first.to_str().unwrap())
            .is_err()
    );
}

#[tokio::test]
async fn adding_a_same_named_source_does_not_replace_another_market() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::at(temp.path().join("orbit-data"));
    let mut sources = Vec::new();
    for directory in ["first", "second"] {
        let market = temp.path().join(directory);
        fs::create_dir_all(market.join(".claude-plugin")).unwrap();
        fs::write(
            market.join(".claude-plugin/marketplace.json"),
            r#"{"name":"shared-name","plugins":[]}"#,
        )
        .unwrap();
        sources.push(market.to_string_lossy().into_owned());
    }
    let original = store
        .add(sources[0].clone())
        .await
        .unwrap();

    assert!(
        store
            .add(sources[1].clone())
            .await
            .is_err()
    );
    assert_eq!(store.list().unwrap(), vec![original]);
}

#[test]
fn catalogue_mutations_are_serialized_until_the_lease_is_released() {
    let temp = tempfile::tempdir().unwrap();
    let first_store = Store::at(temp.path().join("orbit-data"));
    let second_store = Store::at(temp.path().join("orbit-data"));
    let first = first_store.lock().unwrap();

    assert!(second_store.lock().is_err());
    drop(first);
    second_store.lock().unwrap();
}

#[tokio::test]
async fn a_new_git_revision_does_not_replace_an_existing_checkout() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir_all(source.join(".claude-plugin")).unwrap();
    let catalog = source.join(".claude-plugin/marketplace.json");
    fs::write(&catalog, r#"{"name":"team","plugins":[]}"#).unwrap();
    git(&source, &["init", "-q"]);
    git(
        &source,
        &["config", "user.email", "orbit-test@example.invalid"],
    );
    git(&source, &["config", "user.name", "Orbit Test"]);
    git(&source, &["add", "."]);
    git(&source, &["commit", "-qm", "first"]);

    let store = Store::at(temp.path().join("cache"));
    fs::create_dir_all(&store.root).unwrap();
    let url = source.to_str().unwrap();
    let (first, first_revision) = store
        .clone_git_url("team/source", url)
        .await
        .unwrap();

    fs::write(&catalog, r#"{"name":"team","plugins":[{"name":"new"}]}"#).unwrap();
    git(&source, &["add", "."]);
    git(&source, &["commit", "-qm", "second"]);
    let (second, second_revision) = store
        .clone_git_url("team/source", url)
        .await
        .unwrap();

    assert_ne!(first, second);
    assert_ne!(first_revision, second_revision);
    assert_eq!(
        parse_catalog(&first)
            .unwrap()
            .plugins
            .len(),
        0
    );
    assert_eq!(
        parse_catalog(&second)
            .unwrap()
            .plugins
            .len(),
        1
    );
}

#[test]
fn inspects_declarations_without_executing_them() {
    let temp = tempfile::tempdir().unwrap();
    let market = temp.path();
    fs::create_dir_all(market.join(".claude-plugin")).unwrap();
    let package = market.join("plugins/review");
    fs::create_dir_all(package.join(".claude-plugin")).unwrap();
    fs::create_dir_all(package.join("skills/check")).unwrap();
    fs::create_dir_all(package.join("commands")).unwrap();
    fs::create_dir_all(package.join("agents")).unwrap();
    fs::create_dir_all(package.join("hooks")).unwrap();
    fs::write(
            package.join(".claude-plugin/plugin.json"),
            r#"{"name":"review","version":"1.2.3","description":"Review safely","mcpServers":{"docs":{}}}"#,
        )
        .unwrap();
    fs::write(package.join("skills/check/SKILL.md"), "# check").unwrap();
    fs::write(package.join("commands/review.md"), "# review").unwrap();
    fs::write(package.join("agents/reviewer.md"), "# reviewer").unwrap();
    fs::write(package.join("hooks/hooks.json"), "{}").unwrap();
    let market = Marketplace {
        name: "market".into(),
        source: "local".into(),
        directory: market.to_path_buf(),
        revision: None,
        plugins: vec![],
    };
    let plugin = Plugin {
        name: "review".into(),
        description: "Review".into(),
        source: Some("./plugins/review".into()),
    };
    let inspected = inspect_plugin(&market, &plugin).unwrap();

    assert_eq!(inspected.version.as_deref(), Some("1.2.3"));
    assert_eq!(inspected.skills, ["check"]);
    assert_eq!(inspected.commands, ["review.md"]);
    assert_eq!(inspected.agents, ["reviewer.md"]);
    assert!(inspected.declares_hooks);
    assert!(inspected.declares_mcp);
}

#[test]
fn enabled_plugin_skills_require_the_checked_package_version() {
    let temp = tempfile::tempdir().unwrap();
    let market_root = temp.path().join("market");
    let package = market_root.join("plugins/review");
    fs::create_dir_all(package.join(".claude-plugin")).unwrap();
    fs::create_dir_all(package.join("skills/check")).unwrap();
    fs::write(
        package.join(".claude-plugin/plugin.json"),
        r#"{"name":"review","version":"1.2.3"}"#,
    )
    .unwrap();
    fs::write(package.join("skills/check/SKILL.md"), "# check").unwrap();
    git(&market_root, &["init", "-q"]);
    git(
        &market_root,
        &["config", "user.email", "orbit-test@example.invalid"],
    );
    git(&market_root, &["config", "user.name", "Orbit Test"]);
    git(&market_root, &["add", "."]);
    git(&market_root, &["commit", "-qm", "fixture"]);
    let revision = git(&market_root, &["rev-parse", "HEAD"]);
    let market = Marketplace {
        name: "market".into(),
        source: "owner/market".into(),
        directory: market_root,
        revision: Some(revision.clone()),
        plugins: vec![Plugin {
            name: "review".into(),
            description: "Review".into(),
            source: Some("plugins/review".into()),
        }],
    };
    let enabled = ProjectPlugin {
        project_id: "project".into(),
        marketplace_source: "owner/market".into(),
        plugin_name: "review".into(),
        package_source: "plugins/review".into(),
        version: Some("1.2.3".into()),
        marketplace_revision: Some(revision.clone()),
        enabled_at: 0,
    };

    let roots = enabled_skill_roots_from_markets(
        std::slice::from_ref(&enabled),
        std::slice::from_ref(&market),
    );

    assert_eq!(roots.roots, vec![package.join("skills/check")]);
    assert!(roots.diagnostics.is_empty());

    let stale = ProjectPlugin {
        version: Some("1.0.0".into()),
        ..enabled.clone()
    };
    let stale_roots = enabled_skill_roots_from_markets(&[stale], std::slice::from_ref(&market));

    assert!(stale_roots.roots.is_empty());
    assert!(stale_roots.diagnostics[0].contains("重新检查后启用"));

    let updated_market = Marketplace {
        revision: Some("b".repeat(40)),
        ..market.clone()
    };
    let unchanged_version = ProjectPlugin {
        version: Some("1.2.3".into()),
        marketplace_revision: Some(revision),
        ..enabled
    };
    let updated_roots = enabled_skill_roots_from_markets(
        std::slice::from_ref(&unchanged_version),
        &[updated_market],
    );

    assert!(updated_roots.roots.is_empty());
    assert!(updated_roots.diagnostics[0].contains("Git 提交已变更"));

    fs::write(
        package.join("skills/check/SKILL.md"),
        "# changed without a commit",
    )
    .unwrap();
    let tampered_roots = enabled_skill_roots_from_markets(&[unchanged_version], &[market]);

    assert!(tampered_roots.roots.is_empty());
    assert!(tampered_roots.diagnostics[0].contains("changed after download"));
}

#[test]
fn rejects_malformed_or_duplicate_plugin_entries() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir_all(temp.path().join(".claude-plugin")).unwrap();
    fs::write(
        temp.path()
            .join(".claude-plugin/marketplace.json"),
        r#"{"name":"market","plugins":[{"name":"same"},{"name":"same"}]}"#,
    )
    .unwrap();

    assert!(parse_catalog(temp.path()).is_err());

    fs::write(
        temp.path()
            .join(".claude-plugin/marketplace.json"),
        r#"{"name":"market","plugins":[{"description":"no name"}]}"#,
    )
    .unwrap();

    assert!(parse_catalog(temp.path()).is_err());

    fs::write(
        temp.path()
            .join(".claude-plugin/marketplace.json"),
        r#"{"name":"market","plugins":[{"name":"safe","source":"../escape"}]}"#,
    )
    .unwrap();

    assert!(parse_catalog(temp.path()).is_err());
}

#[test]
fn rejects_packages_outside_the_marketplace() {
    let temp = tempfile::tempdir().unwrap();
    let market = temp.path().join("market");
    let package = market.join("package");
    fs::create_dir_all(&package).unwrap();
    fs::create_dir(temp.path().join("outside")).unwrap();
    assert_eq!(resolve_package_path(&market, "package").unwrap(), package);
    let error = resolve_package_path(&market, "../outside").unwrap_err();
    assert!(error.to_string().contains("parent"), "{error:#}");
}

#[test]
fn accepts_github_shorthand_but_not_credential_urls() {
    assert_eq!(
        git_url("owner/catalog").unwrap(),
        "https://github.com/owner/catalog.git"
    );
    assert!(git_url("https://token@example.com/catalog.git").is_err());
}

fn write_catalog(root: &Path, plugins: serde_json::Value) {
    fs::create_dir_all(root.join(".claude-plugin")).unwrap();
    fs::write(
        root.join(".claude-plugin/marketplace.json"),
        serde_json::to_vec(&serde_json::json!({
            "name": "inline-skills",
            "plugins": plugins
        }))
        .unwrap(),
    )
    .unwrap();
}

fn write_skill(root: &Path, path: &str) {
    fs::create_dir_all(root).unwrap();
    fs::create_dir_all(root.join(path)).unwrap();
    fs::write(
        root.join(path).join("SKILL.md"),
        "---\ndescription: Test skill metadata\n---\nTest body.\n",
    )
    .unwrap();
}

#[tokio::test]
async fn manifestless_cached_entries_enable_only_their_declared_skills() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("market");
    // Same shared-source layout as anthropics/skills, with disjoint bundles.
    let bundles = [
        ("document-skills", vec!["xlsx", "docx", "pptx", "pdf"]),
        ("example-skills", vec!["frontend-design", "skill-creator"]),
        ("claude-api", vec!["claude-api"]),
        ("academy-guide", vec!["academy-guide"]),
        ("discernment-nudge", vec!["discernment-nudge"]),
    ];
    let entries = bundles
        .iter()
        .map(|(name, skills)| {
            for skill in skills {
                write_skill(&root, &format!("skills/{skill}"));
            }
            serde_json::json!({
                "name": name,
                "source": "./",
                "strict": false,
                "description": "Inline skill bundle",
                "version": "1.0",
                "skills": skills
                    .iter()
                    .map(|skill| format!("./skills/{skill}"))
                    .collect::<Vec<_>>()
            })
        })
        .collect::<Vec<_>>();
    write_skill(&root, "skills/not-selected");
    write_catalog(&root, serde_json::json!(entries));
    let store = Store::at(temp.path().join("cache"));
    let registered = store
        .add(root.to_string_lossy().into_owned())
        .await
        .unwrap();
    // Stored catalogs from older Orbit versions have no strict/skills fields.
    let market: Marketplace =
        serde_json::from_slice(&serde_json::to_vec(&registered).unwrap()).unwrap();
    let repo = temp.path().join("project");
    fs::create_dir_all(repo.join(".git")).unwrap();
    let mut enabled = Vec::new();
    for (plugin, (_, expected)) in market.plugins.iter().zip(&bundles) {
        let package = inspect_plugin(&market, plugin).unwrap();
        let mut expected = expected.clone();
        expected.sort();
        assert_eq!(package.skills, expected);
        assert_eq!(package.version.as_deref(), Some("1.0"));
        assert_eq!(package.description.as_deref(), Some("Inline skill bundle"));
        let configured = ProjectPlugin {
            project_id: "project".into(),
            marketplace_source: market.source.clone(),
            plugin_name: plugin.name.clone(),
            package_source: plugin.source.clone().unwrap(),
            version: package.version,
            marketplace_revision: None,
            enabled_at: 0,
        };
        let roots = enabled_skill_roots_from_markets(
            std::slice::from_ref(&configured),
            std::slice::from_ref(&market),
        );
        assert!(roots.diagnostics.is_empty(), "{:?}", roots.diagnostics);
        let skills =
            crate::skills::Skills::discover_with_plugin_roots(&repo, None, None, roots.roots);
        assert_eq!(
            skills
                .completions()
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            expected
        );
        enabled.push(configured);
    }
    let roots = enabled_skill_roots_from_markets(&enabled, &[market]);
    let skills = crate::skills::Skills::discover_with_plugin_roots(&repo, None, None, roots.roots);
    assert_eq!(skills.len(), 9);
    assert!(skills.get("not-selected").is_none());
}

#[test]
fn manifest_and_entry_skill_paths_preserve_defaults_and_metadata_precedence() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let package = root.join("plugins/review");
    write_skill(&package, "skills/default");
    write_skill(&package, "extra/custom");
    write_skill(&package, "catalog/selected");
    fs::create_dir_all(package.join(".claude-plugin")).unwrap();
    fs::write(
        package.join(".claude-plugin/plugin.json"),
        r#"{"name":"review","version":"2.0","description":"Manifest metadata","skills":"./extra"}"#,
    )
    .unwrap();
    write_catalog(
        root,
        serde_json::json!([{
            "name": "review",
            "source": "plugins/review",
            "version": "1.0",
            "description": "Entry metadata",
            "skills": ["./catalog/selected"]
        }]),
    );
    let market = parse_catalog(root).unwrap();
    let inspected = package::inspect(&market, &market.plugins[0]).unwrap();
    assert_eq!(inspected.package.skills, ["custom", "default", "selected"]);
    assert_eq!(inspected.package.version.as_deref(), Some("2.0"));
    assert_eq!(
        inspected.package.description.as_deref(),
        Some("Manifest metadata")
    );
    assert_eq!(inspected.skill_directories.len(), 3);
    // strict=false cannot combine an inline component declaration with plugin.json.
    write_catalog(
        root,
        serde_json::json!([{
            "name": "review",
            "source": "plugins/review",
            "strict": false,
            "skills": ["./catalog/selected"]
        }]),
    );
    assert!(
        inspect_plugin(&market, &market.plugins[0])
            .unwrap_err()
            .to_string()
            .contains("conflicting")
    );
    fs::write(package.join(".claude-plugin/plugin.json"), "invalid JSON").unwrap();
    assert!(
        inspect_plugin(&market, &market.plugins[0])
            .unwrap_err()
            .to_string()
            .contains("invalid plugin JSON")
    );
}

#[test]
fn root_skills_and_manifestless_standard_layout_work_without_strict_flag() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("single-skill");
    write_skill(&root, ".");
    write_catalog(
        &root,
        serde_json::json!([{
            "name": "single",
            "source": "./",
            "skills": "."
        }]),
    );
    let market = parse_catalog(&root).unwrap();
    assert_eq!(
        inspect_plugin(&market, &market.plugins[0])
            .unwrap()
            .skills,
        ["single-skill"]
    );
    fs::remove_file(root.join("SKILL.md")).unwrap();
    write_skill(&root, "skills/standard");
    write_catalog(
        &root,
        serde_json::json!([{
            "name": "single",
            "source": "./"
        }]),
    );
    assert_eq!(
        inspect_plugin(&market, &market.plugins[0])
            .unwrap()
            .skills,
        ["standard"]
    );
}

#[test]
fn invalid_inline_skill_declarations_never_fall_back_to_loading_all_skills() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("market");
    write_skill(&root, "skills/unselected");
    for skills in [
        serde_json::json!(true),
        serde_json::json!([1]),
        serde_json::json!(["../outside"]),
        serde_json::json!(["/tmp"]),
        serde_json::json!(["./missing"]),
    ] {
        write_catalog(
            &root,
            serde_json::json!([{
                "name": "bundle",
                "source": "./",
                "strict": false,
                "skills": skills
            }]),
        );
        let market = parse_catalog(&root).unwrap();
        assert!(inspect_plugin(&market, &market.plugins[0]).is_err());
    }
    write_catalog(
        &root,
        serde_json::json!([{
            "name": "bundle",
            "source": "./",
            "strict": false,
            "skills": []
        }]),
    );
    let market = parse_catalog(&root).unwrap();
    assert!(
        inspect_plugin(&market, &market.plugins[0])
            .unwrap()
            .skills
            .is_empty()
    );
    write_catalog(
        &root,
        serde_json::json!([{
            "name": "bundle",
            "source": "./",
            "strict": "false"
        }]),
    );
    assert!(inspect_plugin(&market, &market.plugins[0]).is_err());
    write_catalog(
        &root,
        serde_json::json!([{
            "name": "bundle",
            "source": "./changed"
        }]),
    );
    assert!(
        inspect_plugin(&market, &market.plugins[0])
            .unwrap_err()
            .to_string()
            .contains("entry changed")
    );
}

#[cfg(unix)]
#[test]
fn declared_skill_symlinks_cannot_escape_the_plugin_package() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("market");
    write_skill(temp.path(), "outside");
    fs::create_dir_all(root.join("skills")).unwrap();
    std::os::unix::fs::symlink(temp.path().join("outside"), root.join("skills/escaped")).unwrap();
    write_catalog(
        &root,
        serde_json::json!([{
            "name": "bundle",
            "source": "./",
            "strict": false,
            "skills": ["./skills/escaped"]
        }]),
    );
    let market = parse_catalog(&root).unwrap();
    assert!(inspect_plugin(&market, &market.plugins[0]).is_err());
    write_catalog(
        &root,
        serde_json::json!([{
            "name": "bundle",
            "source": "./",
            "skills": ["./skills"]
        }]),
    );
    assert!(inspect_plugin(&market, &market.plugins[0]).is_err());
}
