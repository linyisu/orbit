use super::*;
use orbit_protocol::resources::McpLaunch;
use std::collections::BTreeMap;

pub(super) fn candidate_directory(id: &str) -> Result<&str> {
    let (directory, _) = id.split_once('|').context("入口身份无效")?;
    ensure!(
        Path::new(directory).components().all(|part| matches!(
            part,
            std::path::Component::Normal(_) | std::path::Component::CurDir
        )),
        "入口目录无效"
    );
    Ok(directory)
}
pub(super) fn discover(root: &Path) -> Result<Vec<McpCandidate>> {
    let mut candidates = Vec::new();
    let mut pending = vec![(root.to_path_buf(), 0)];
    while let Some((directory, depth)) = pending.pop() {
        let relative = directory.strip_prefix(root)?.to_string_lossy();
        let relative = if relative.is_empty() { "." } else { &relative };
        let node = directory.join("package.json");
        let package: Option<serde_json::Value> = if node.is_file() {
            Some(serde_json::from_str(&read_metadata(&node)?)?)
        } else {
            None
        };
        let mut install = Vec::new();
        if let Some(package) = &package {
            install.push(vec![
                executable("npm"),
                if directory.join("package-lock.json").is_file() {
                    "ci"
                } else {
                    "install"
                }
                .into(),
                "--ignore-scripts".into(),
                "--no-audit".into(),
                "--no-fund".into(),
            ]);
            if package["scripts"]["build"].is_string() {
                install.push(vec![executable("npm"), "run".into(), "build".into()]);
            }
        }
        for file in [".mcp.json", "mcp.json"] {
            let path = directory.join(file);
            if !path.is_file() {
                continue;
            }
            let value: serde_json::Value = serde_json::from_str(&read_metadata(&path)?)?;
            if let Some(servers) = value["mcpServers"].as_object() {
                for (name, server) in servers {
                    if server["url"].is_string()
                        || server["type"].as_str().is_some_and(|kind| kind != "stdio")
                    {
                        continue;
                    }
                    let Some(command) = server["command"].as_str() else {
                        continue;
                    };
                    let args: Vec<String> = server
                        .get("args")
                        .cloned()
                        .map(serde_json::from_value)
                        .transpose()?
                        .unwrap_or_default();
                    let env = server
                        .get("env")
                        .cloned()
                        .map(serde_json::from_value)
                        .transpose()?
                        .unwrap_or_default();
                    let launch = McpLaunch {
                        command: resolve(command, &directory),
                        args: args
                            .into_iter()
                            .map(|arg| resolve_arg(&arg, &directory))
                            .collect(),
                        env,
                        tools: Vec::new(),
                    };
                    candidates.push(McpCandidate {
                        id: format!("{relative}|{file}:{name}"),
                        name: name_for(name),
                        launch,
                        install: install.clone(),
                        description: format!("{relative}/{file} 中的 stdio 配置"),
                    });
                }
            }
        }
        if let Some(package) = package {
            let package_name = package["name"].as_str().unwrap_or("mcp-server");
            let mcp_args = registry_args(&directory, package_name)?;
            let bins: BTreeMap<String, String> = if let Some(path) = package["bin"].as_str() {
                BTreeMap::from([(
                    package_name.rsplit('/').next().unwrap().to_owned(),
                    path.to_owned(),
                )])
            } else if package["bin"].is_object() {
                serde_json::from_value(package["bin"].clone())?
            } else {
                BTreeMap::new()
            };
            for (name, bin) in bins {
                ensure_relative(&bin)?;
                let mut args = vec![directory.join(bin).to_string_lossy().into_owned()];
                if let Some(extra) = &mcp_args {
                    args.extend(extra.iter().cloned());
                }
                candidates.push(McpCandidate {
                    id: format!("{relative}|node:{name}"),
                    name: name_for(&name),
                    launch: McpLaunch {
                        command: executable("node"),
                        args,
                        env: BTreeMap::new(),
                        tools: Vec::new(),
                    },
                    install: install.clone(),
                    description: if mcp_args.is_some() {
                        format!("{relative}/package.json 的 bin 入口，启动参数来自 server.json 的 stdio 配置")
                    } else {
                        format!("{relative}/package.json 的 bin 入口；请核对是否为 MCP 服务及所需启动参数")
                    },
                });
            }
        }
        let python = directory.join("pyproject.toml");
        if python.is_file() {
            let project: toml::Value = toml::from_str(&read_metadata(&python)?)?;
            if let Some(scripts) = project
                .get("project")
                .and_then(|project| project.get("scripts"))
                .and_then(toml::Value::as_table)
            {
                for name in scripts.keys() {
                    ensure!(
                        !name.contains('/') && !name.contains('\\') && !name.starts_with('.'),
                        "Python 启动入口名称无效"
                    );
                    let venv = directory.join(".venv");
                    candidates.push(McpCandidate {
                        id: format!("{relative}|python:{name}"),
                        name: name_for(name),
                        launch: McpLaunch {
                            command: venv.join("bin").join(name).to_string_lossy().into_owned(),
                            args: Vec::new(),
                            env: BTreeMap::new(),
                            tools: Vec::new(),
                        },
                        install: vec![
                            vec![
                                executable("python3"),
                                "-m".into(),
                                "venv".into(),
                                venv.to_string_lossy().into_owned(),
                            ],
                            vec![
                                venv.join("bin/python").to_string_lossy().into_owned(),
                                "-m".into(),
                                "pip".into(),
                                "install".into(),
                                ".".into(),
                            ],
                        ],
                        description: format!(
                            "{relative}/pyproject.toml 的脚本入口；使用独立虚拟环境"
                        ),
                    });
                }
            }
        }
        ensure!(
            candidates.len() <= 64,
            "入口超过 64 个，请选择更具体的仓库子目录"
        );
        if depth < 3 {
            for entry in fs::read_dir(&directory)? {
                let entry = entry?;
                if entry.file_type()?.is_dir() {
                    pending.push((entry.path(), depth + 1));
                }
            }
        }
    }
    candidates.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(candidates)
}

// Registry metadata distinguishes a CLI executable from its MCP subcommand.
// Read fixed arguments only; placeholders need explicit user configuration.
fn registry_args(directory: &Path, package_name: &str) -> Result<Option<Vec<String>>> {
    let path = directory.join("server.json");
    if !path.is_file() {
        return Ok(None);
    }
    let value: serde_json::Value = serde_json::from_str(&read_metadata(&path)?)?;
    let Some(packages) = value["packages"].as_array() else {
        return Ok(None);
    };
    for package in packages {
        if package["registryType"] != "npm"
            || package["identifier"] != package_name
            || package["transport"]["type"] != "stdio"
        {
            continue;
        }
        let Some(arguments) = package
            .get("packageArguments")
            .and_then(serde_json::Value::as_array)
        else {
            return Ok(Some(Vec::new()));
        };
        let mut args = Vec::new();
        for argument in arguments {
            if let Some(name) = argument["name"]
                .as_str()
                .filter(|_| argument["type"] == "named")
            {
                args.push(name.to_owned());
            } else if argument["type"] != "positional" {
                return Ok(None);
            }
            let Some(value) = argument["value"].as_str() else {
                return Ok(None);
            };
            if value.contains('{') || value.contains('}') {
                return Ok(None);
            }
            args.push(value.to_owned());
        }
        return Ok(Some(args));
    }
    Ok(None)
}
fn name_for(value: &str) -> String {
    let value = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .take(48)
        .collect::<String>();
    if value.is_empty() {
        "mcp-server".into()
    } else {
        value
    }
}
fn ensure_relative(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && !value.contains('\\')
            && Path::new(value).components().all(|part| matches!(
                part,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )),
        "启动入口必须位于仓库内"
    );
    Ok(())
}
fn resolve(value: &str, root: &Path) -> String {
    if value.starts_with("./") {
        root.join(value).to_string_lossy().into_owned()
    } else if !value.contains('/') {
        executable(value)
    } else {
        value.into()
    }
}
fn resolve_arg(value: &str, root: &Path) -> String {
    let value = value
        .replace("${MCP_SOURCE}", &root.to_string_lossy())
        .replace("${workspaceFolder}", &root.to_string_lossy());
    if !Path::new(&value).is_absolute() && (root.join(&value).is_file() || value.starts_with("./"))
    {
        root.join(value).to_string_lossy().into_owned()
    } else {
        value
    }
}
