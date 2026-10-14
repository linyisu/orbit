//! Shared command argument vocabulary and project command parsing for desktop and terminal.
use anyhow::{Result, bail, ensure};
use orbit_protocol::Completion as Candidate;
use orbit_protocol::{
    SessionAction,
    project::{MemoryAction, MemoryCategory, TaskAction, TaskStatus},
};
use std::{
    cell::RefCell,
    path::{Path, PathBuf},
};

pub fn project_action(command: &str, argument: &str) -> Result<SessionAction> {
    let (action, rest) = argument
        .trim()
        .split_once(char::is_whitespace)
        .unwrap_or((argument.trim(), ""));
    let rest = rest.trim();
    Ok(match command {
        "/tasks" => SessionAction::Task {
            task: match action {
                "" | "list" | "all" => TaskAction::List {
                    include_completed: action == "all",
                },
                "add" => {
                    ensure!(!rest.is_empty(), "用法：/tasks add <标题>");
                    TaskAction::Add { title: rest.into() }
                }
                "start" | "done" | "block" | "cancel" => TaskAction::SetStatus {
                    id: rest
                        .parse()
                        .map_err(|_| anyhow::anyhow!("请输入任务编号"))?,
                    status: match action {
                        "start" => TaskStatus::InProgress,
                        "done" => TaskStatus::Completed,
                        "block" => TaskStatus::Blocked,
                        _ => TaskStatus::Cancelled,
                    },
                },
                "release" => TaskAction::Release {
                    id: rest
                        .parse()
                        .map_err(|_| anyhow::anyhow!("请输入任务编号"))?,
                },
                _ => bail!("用法：/tasks [list|all|add|start|done|block|cancel|release]"),
            },
        },
        "/memory" => SessionAction::Memory {
            memory: match action {
                "" | "list" | "all" => MemoryAction::List {
                    include_inactive: action == "all",
                },
                "search" => MemoryAction::Search { query: rest.into() },
                "forget" => MemoryAction::Forget {
                    id: rest
                        .parse()
                        .map_err(|_| anyhow::anyhow!("请输入记忆编号"))?,
                },
                "add" => {
                    let (category, content) = rest
                        .split_once(char::is_whitespace)
                        .unwrap_or((rest, ""));
                    let category = MemoryCategory::parse(category).ok_or_else(|| {
                        anyhow::anyhow!(
                            "用法：/memory add constraint|decision|fact|preference <内容>"
                        )
                    })?;
                    MemoryAction::Add {
                        category,
                        content: content.trim().into(),
                    }
                }
                _ => bail!("用法：/memory [list|all|search|add|forget]"),
            },
        },
        _ => bail!("未知项目命令"),
    })
}

pub fn arguments(input: &str, models: &[String], skills: &[(String, String)]) -> Vec<Candidate> {
    let Some((command, argument)) = input.split_once(char::is_whitespace) else {
        return Vec::new();
    };
    let argument = argument.trim_start();
    if command == "/model" && !argument.contains("::") {
        let mut groups = std::collections::BTreeMap::<String, usize>::new();
        let mut entries = Vec::new();
        for model in models {
            if orbit_protocol::search::rank(model, argument).is_none() {
                continue;
            }
            if let Some((provider, _)) = model.split_once("::") {
                *groups
                    .entry(provider.into())
                    .or_default() += 1;
            } else {
                entries.push(Candidate {
                    name: format!("/model {model}"),
                    display: Some(model.clone()),
                    description: "切换模型".into(),
                    continue_input: false,
                });
            }
        }
        entries.extend(
            groups
                .into_iter()
                .map(|(provider, count)| Candidate {
                    name: format!("/model {provider}::"),
                    display: Some(provider),
                    description: format!("{count} 个匹配模型"),
                    continue_input: true,
                }),
        );
        if orbit_protocol::search::rank("refresh", argument).is_some() {
            entries.push(Candidate {
                name: "/model refresh".into(),
                display: Some("refresh".into()),
                description: "更新公共模型规格及账号目录".into(),
                continue_input: false,
            });
        }
        return entries;
    }
    let candidates: Vec<(String, String, bool)> = match command {
        "/model" => models
            .iter()
            .map(|name| (name.clone(), "切换模型".into(), false))
            .chain(std::iter::once((
                "refresh".into(),
                "更新公共模型规格及账号目录".into(),
                false,
            )))
            .collect(),

        "/skill" => skills
            .iter()
            .map(|(name, description)| (name.clone(), description.clone(), false))
            .collect(),

        "/mode" => [("plan", "只读规划", false), ("build", "执行模式", false)]
            .into_iter()
            .map(|(n, d, c)| (n.into(), d.into(), c))
            .collect(),
        "/plan" => vec![("view".into(), "查看计划与反馈".into(), false)],
        "/permissions" => [
            ("ask", "逐次确认", false),
            ("auto", "工作区命令自动执行（含联网）", false),
            ("YOLO", "自动确认", false),
        ]
        .into_iter()
        .map(|(n, d, c)| (n.into(), d.into(), c))
        .collect(),

        "/tasks" => options(
            argument,
            &[
                ("list", "待办任务", false),
                ("all", "全部任务", false),
                ("add", "输入任务标题", true),
                ("start", "输入任务编号", true),
                ("done", "输入任务编号", true),
                ("block", "输入任务编号", true),
                ("cancel", "输入任务编号", true),
            ],
        ),

        "/memory" => options(
            argument,
            &[
                ("list", "有效记忆", false),
                ("all", "全部记忆", false),
                ("search", "输入关键词", true),
                ("add constraint", "输入约束", true),
                ("add decision", "输入决策", true),
                ("add fact", "输入事实", true),
                ("add preference", "输入偏好", true),
                ("forget", "输入记忆编号", true),
            ],
        ),

        _ => Vec::new(),
    };

    orbit_protocol::search::ranked(candidates, |(value, _, _)| {
        let rank = orbit_protocol::search::rank(value, argument);
        if command == "/model" {
            rank.into_iter()
                .chain(
                    value
                        .split_once("::")
                        .and_then(|(_, model)| orbit_protocol::search::rank(model, argument)),
                )
                .min()
        } else {
            rank
        }
    })
    .into_iter()
    .map(|(value, description, continue_input)| Candidate {
        name: format!("{command} {value}"),
        display: None,
        description,
        continue_input,
    })
    .collect()
}

fn options(_argument: &str, values: &[(&str, &str, bool)]) -> Vec<(String, String, bool)> {
    values
        .iter()
        .map(|(value, description, more)| (value.to_string(), description.to_string(), *more))
        .collect()
}

pub fn cached_directories(
    input: &str,
    previous: &str,
    entries: &[Candidate],
) -> Option<Vec<Candidate>> {
    fn query(input: &str) -> Option<(&str, &str)> {
        let typed = input.strip_prefix("/cd ")?.trim_start();
        let typed = match typed.chars().next() {
            Some(quote @ ('\'' | '"')) => typed.trim_matches(quote),
            _ => typed,
        };
        Some(
            typed
                .rsplit_once('/')
                .map_or(("", typed), |(parent, partial)| {
                    (&typed[..parent.len() + 1], partial)
                }),
        )
    }

    let (parent, partial) = query(input)?;
    if parent != query(previous)?.0 {
        return None;
    }
    let mut entries: Vec<_> = entries.iter().collect();
    entries.sort_by(|left, right| left.display.cmp(&right.display));
    Some(
        orbit_protocol::search::ranked(entries, |entry| {
            let name = entry
                .display
                .as_deref()?
                .trim_end_matches('/');
            if name.starts_with('.') && name != ".." && !partial.starts_with('.') {
                return None;
            }
            orbit_protocol::search::rank(name, partial)
        })
        .into_iter()
        .cloned()
        .collect(),
    )
}

#[derive(Clone, Debug, Default)]
pub struct DirectoryCache {
    entries: RefCell<Option<DirectorySnapshot>>,
}

#[derive(Clone, Debug)]
struct DirectorySnapshot {
    input: String,
    workspace: PathBuf,
    home: Option<PathBuf>,
    candidates: Vec<Candidate>,
}

impl DirectoryCache {
    pub fn matching(&self, input: &str, workspace: &Path, home: Option<&Path>) -> Vec<Candidate> {
        let mut cache = self.entries.borrow_mut();
        if let Some(snapshot) = cache.as_ref()
            && snapshot.input == input
            && snapshot.workspace == workspace
            && snapshot.home.as_deref() == home
        {
            return snapshot.candidates.clone();
        }

        let candidates = directories(input, workspace, home);
        *cache = Some(DirectorySnapshot {
            input: input.into(),
            workspace: workspace.into(),
            home: home.map(Path::to_path_buf),
            candidates: candidates.clone(),
        });
        candidates
    }
}

fn directories(input: &str, workspace: &Path, home: Option<&Path>) -> Vec<Candidate> {
    let typed = input.trim_start();
    if typed == "~" && home.is_some() {
        return vec![Candidate {
            name: "/cd ~/".into(),
            display: Some("~/".into()),
            description: String::new(),
            continue_input: true,
        }];
    }

    let quote = typed
        .chars()
        .next()
        .filter(|ch| matches!(ch, '\'' | '"'));
    let typed = if let Some(quote) = quote {
        typed.trim_matches(quote)
    } else {
        typed
    };
    let (prefix, partial) = typed
        .rsplit_once('/')
        .map_or(("", typed), |(parent, partial)| {
            (&typed[..parent.len() + 1], partial)
        });
    let directory = if prefix == "~/" || prefix.starts_with("~/") {
        let Some(home) = home else {
            return Vec::new();
        };
        home.join(&prefix[2..])
    } else {
        workspace.join(prefix)
    };
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut names = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| !name.starts_with('.') || partial.starts_with('.'))
        .filter(|name| !name.chars().any(char::is_control))
        .collect::<Vec<_>>();

    names.sort();

    if prefix.is_empty() && "..".starts_with(partial) {
        names.insert(0, "..".into());
    }

    orbit_protocol::search::ranked(names, |name| orbit_protocol::search::rank(name, partial))
        .into_iter()
        .map(|name| {
            let path = format!("{prefix}{name}/");
            let path = if quote.is_some() || path.starts_with(['\'', '"']) {
                shlex::try_quote(&path).map_or(path.clone(), |value| value.into_owned())
            } else {
                path
            };
            Candidate {
                name: format!("/cd {path}"),
                display: Some(format!("{name}/")),
                description: String::new(),
                continue_input: true,
            }
        })
        .collect()
}

#[cfg(test)]
mod directory_tests {
    use super::*;
    #[test]
    fn directories_use_explicit_roots_and_preserve_spaces_and_quotes() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("project space")).unwrap();
        std::fs::write(root.path().join("project file"), "text").unwrap();
        std::fs::create_dir(root.path().join("project space/nested")).unwrap();
        std::fs::create_dir(root.path().join(".hidden")).unwrap();

        let candidates = directories("pro", root.path(), None);
        assert_eq!(candidates.len(), 1);
        assert!(candidates[0].description.is_empty());
        assert!(!candidates[0].name.contains("nested"));
        assert_eq!(candidates[0].name, "/cd project space/");
        assert_eq!(candidates[0].display.as_deref(), Some("project space/"));
        let quoted = directories("\"pro", root.path(), None);
        assert_eq!(
            crate::directories::resolve(&quoted[0].name[4..], root.path(), None).unwrap(),
            root.path().join("project space")
        );
        assert_eq!(
            directories("~/pro", Path::new("/"), Some(root.path()))[0].name,
            "/cd ~/project space/"
        );
        assert!(
            directories("", root.path(), None)
                .iter()
                .all(|entry| !entry.name.contains(".hidden"))
        );
        assert!(
            directories(".", root.path(), None)
                .iter()
                .any(|entry| entry.name.contains(".hidden"))
        );
        assert!(directories("missing/", root.path(), None).is_empty());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_arguments_expand_connections_without_losing_qualified_identity() {
        let models = vec![
            "alpha::fast".into(),
            "alpha::slow".into(),
            "beta::fast".into(),
        ];
        let entries = arguments("/model ", &models, &[]);
        assert_eq!(
            entries
                .iter()
                .map(|e| e.name.as_str())
                .collect::<Vec<_>>(),
            ["/model alpha::", "/model beta::", "/model refresh"]
        );
        assert!(entries[0].continue_input);
        let entries = arguments("/model beta::", &models, &[]);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "/model beta::fast");
        assert!(!entries[0].continue_input);
    }

    #[test]
    fn nested_project_arguments_parse_to_the_intended_action() {
        assert!(arguments("/tasks done ", &[], &[]).is_empty());
        assert!(arguments("/tasks add write docs", &[], &[]).is_empty());
        let entries = arguments("/memory add ", &[], &[]);
        assert_eq!(entries.len(), 4);
        assert!(
            entries
                .iter()
                .all(|entry| entry.continue_input)
        );
        assert!(matches!(
            project_action("/tasks", "done 42").unwrap(),
            SessionAction::Task {
                task: TaskAction::SetStatus {
                    id: 42,
                    status: TaskStatus::Completed
                }
            }
        ));
        assert!(matches!(
            project_action("/memory", "forget 7").unwrap(),
            SessionAction::Memory {
                memory: MemoryAction::Forget { id: 7 }
            }
        ));
        assert!(matches!(
            project_action("/memory", "add preference 中文 偏好").unwrap(),
            SessionAction::Memory {
                memory:
                    MemoryAction::Add {
                        category: MemoryCategory::Preference,
                        content,
                    },
            } if content == "中文 偏好"
        ));
        assert!(project_action("/tasks", "done invalid").is_err());
    }
}
