use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "details")]
pub enum GitOperationKind {
    /// Read-only inspection (status, diff, log, branch --list, etc.)
    /// Allowed automatically without interactive confirmation.
    ReadOnlyQuery {
        subcommand: String,
    },
    /// Normal local modification (add, normal commit, create branch, stash, etc.)
    /// Requires local Git write authorization; not prompted repeatedly in the same session.
    LocalWrite {
        subcommand: String,
        description: String,
    },
    /// Branch switching, merge, or rebase.
    /// Shows impact scope and alerts if there is risk of uncommitted changes being affected.
    BranchSwitchOrRebase {
        subcommand: String,
        target: String,
        risk_notice: Option<String>,
    },
    /// Push to remote repository.
    /// Requires explicit remote write authorization (network permission does NOT imply push).
    RemoteWrite {
        remote: String,
        source_branch: Option<String>,
        target_branch: Option<String>,
    },
    /// High-risk or destructive operation (force push, reset --hard, clean -fd, delete branch).
    /// Always requires explicit confirmation, never bypassed by ordinary write permissions.
    Dangerous {
        subcommand: String,
        reason: String,
    },
    /// Modification to repository configuration (.git/config), worktree config, or hooks.
    /// Requires independent dedicated authorization.
    ConfigMutation {
        key: Option<String>,
        value: Option<String>,
    },
    /// Compound commands (&&, ||, ;, pipes, subshells) or unrecognized git commands/scripts.
    /// Must fall back to regular Shell approval and not claim to be a safe Git operation.
    CompoundOrShell {
        reason: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitCommandAnalysis {
    pub raw_command: String,
    pub subcommand: String,
    pub kind: GitOperationKind,
}

impl GitCommandAnalysis {
    /// Classify a command string into a structured Git analysis, or return None if not a git command.
    pub fn parse(command: &str) -> Option<Self> {
        let trimmed = command.trim();
        if trimmed.is_empty() {
            return None;
        }

        // Fast check: if it doesn't contain "git", it's not a git command.
        if !contains_git_word(trimmed) {
            return None;
        }

        // Check for compound shell operators, subshells, pipelines, or redirects.
        if contains_compound_shell_syntax(trimmed) {
            return Some(Self {
                raw_command: trimmed.to_string(),
                subcommand: String::new(),
                kind: GitOperationKind::CompoundOrShell {
                    reason: "包含复合命令、管道或 Shell 控制符，按通用 Shell 审批处理".into(),
                },
            });
        }

        let words = match shlex::split(trimmed) {
            Some(w) if !w.is_empty() => w,
            _ => {
                return Some(Self {
                    raw_command: trimmed.to_string(),
                    subcommand: String::new(),
                    kind: GitOperationKind::CompoundOrShell {
                        reason: "无法安全解析命令行词法".into(),
                    },
                });
            }
        };

        let first = &words[0];
        if first != "git" && !first.ends_with("/git") {
            return None;
        }

        // Parse global options before the subcommand
        let mut idx = 1;
        let mut inline_config_keys = Vec::new();

        while idx < words.len() {
            let arg = &words[idx];
            if !arg.starts_with('-') {
                break;
            }

            match arg.as_str() {
                "--help" | "--version" | "--paginate" | "-p" | "--no-pager"
                | "--no-replace-objects" | "--literal-pathspecs" | "--bare" => {
                    idx += 1;
                }
                "-C" | "--git-dir" | "--work-tree" | "--namespace" => {
                    idx += 2; // skip flag and directory argument
                }
                "-c" => {
                    if idx + 1 < words.len() {
                        inline_config_keys.push(words[idx + 1].clone());
                        idx += 2;
                    } else {
                        idx += 1;
                    }
                }
                _ if arg.starts_with("-C") => {
                    idx += 1;
                }
                _ if arg.starts_with("--git-dir=") || arg.starts_with("--work-tree=") => {
                    idx += 1;
                }
                _ if arg.starts_with("-c") => {
                    inline_config_keys.push(arg[2..].to_string());
                    idx += 1;
                }
                _ => {
                    // Unknown global flag
                    idx += 1;
                }
            }
        }

        // Check if inline config attempts to mutate hooks or execution config
        for key_val in inline_config_keys {
            let key = key_val.split('=').next().unwrap_or("").trim().to_lowercase();
            if key == "core.hookspath"
                || key == "core.sshcommand"
                || key.starts_with("credential.")
            {
                return Some(Self {
                    raw_command: trimmed.to_string(),
                    subcommand: "config".into(),
                    kind: GitOperationKind::ConfigMutation {
                        key: Some(key),
                        value: None,
                    },
                });
            }
        }

        if idx >= words.len() {
            // Just `git` or `git --version`
            return Some(Self {
                raw_command: trimmed.to_string(),
                subcommand: "version".into(),
                kind: GitOperationKind::ReadOnlyQuery {
                    subcommand: "version".into(),
                },
            });
        }

        let subcommand = words[idx].clone();
        let sub_args = &words[idx + 1..];

        let kind = classify_subcommand(&subcommand, sub_args);
        Some(Self {
            raw_command: trimmed.to_string(),
            subcommand,
            kind,
        })
    }
}

fn contains_git_word(text: &str) -> bool {
    text.split_whitespace().any(|word| {
        word == "git" || word.starts_with("git ") || word.ends_with("/git")
    })
}

fn contains_compound_shell_syntax(text: &str) -> bool {
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '\'' if !in_double_quote => in_single_quote = !in_single_quote,
            '"' if !in_single_quote => in_double_quote = !in_double_quote,
            '\\' => {
                chars.next(); // skip escaped char
            }
            _ if in_single_quote || in_double_quote => {}
            '&' | '|' | ';' | '\n' | '`' | '>' | '<' => return true,
            '$' if chars.peek() == Some(&'(') => return true,
            _ => {}
        }
    }

    false
}

fn classify_subcommand(subcmd: &str, args: &[String]) -> GitOperationKind {
    match subcmd {
        "status" | "diff" | "log" | "show" | "rev-parse" | "describe" | "cat-file"
        | "ls-files" | "ls-tree" | "grep" | "shortlog" | "blame" | "check-ignore"
        | "check-attr" | "whatchanged" | "count-objects" => {
            GitOperationKind::ReadOnlyQuery {
                subcommand: subcmd.to_string(),
            }
        }

        "remote" => {
            if args.is_empty()
                || args.iter().any(|a| a == "-v" || a == "--verbose")
                || args.first().is_some_and(|a| a == "show" || a == "get-url")
            {
                GitOperationKind::ReadOnlyQuery {
                    subcommand: subcmd.to_string(),
                }
            } else {
                GitOperationKind::LocalWrite {
                    subcommand: subcmd.to_string(),
                    description: format!("修改远端配置: git remote {}", args.join(" ")),
                }
            }
        }

        "branch" => {
            if args.iter().any(|a| a == "-D" || a == "-d" || a == "--delete") {
                return GitOperationKind::Dangerous {
                    subcommand: "branch".into(),
                    reason: "删除分支".into(),
                };
            }
            if args.iter().any(|a| a == "-m" || a == "-M" || a == "--move") {
                return GitOperationKind::LocalWrite {
                    subcommand: "branch".into(),
                    description: "重命名分支".into(),
                };
            }
            let is_listing = args.is_empty()
                || args.iter().all(|a| {
                    a.starts_with('-')
                        && (a == "-a"
                            || a == "-r"
                            || a == "--all"
                            || a == "--remotes"
                            || a == "--list"
                            || a == "-v"
                            || a == "-vv"
                            || a == "--show-current")
                });
            if is_listing {
                GitOperationKind::ReadOnlyQuery {
                    subcommand: "branch".into(),
                }
            } else {
                GitOperationKind::LocalWrite {
                    subcommand: "branch".into(),
                    description: format!("创建分支: {}", args.join(" ")),
                }
            }
        }

        "tag" => {
            if args.iter().any(|a| a == "-d" || a == "--delete") {
                return GitOperationKind::Dangerous {
                    subcommand: "tag".into(),
                    reason: "删除标签".into(),
                };
            }
            if args.is_empty() || args.iter().any(|a| a == "-l" || a == "--list") {
                GitOperationKind::ReadOnlyQuery {
                    subcommand: "tag".into(),
                }
            } else {
                GitOperationKind::LocalWrite {
                    subcommand: "tag".into(),
                    description: format!("创建标签: {}", args.join(" ")),
                }
            }
        }

        "add" => GitOperationKind::LocalWrite {
            subcommand: "add".into(),
            description: format!("暂存文件: git add {}", args.join(" ")),
        },

        "rm" => GitOperationKind::LocalWrite {
            subcommand: "rm".into(),
            description: format!("删除已跟踪文件: git rm {}", args.join(" ")),
        },

        "mv" => GitOperationKind::LocalWrite {
            subcommand: "mv".into(),
            description: format!("移动/重命名文件: git mv {}", args.join(" ")),
        },

        "commit" => GitOperationKind::LocalWrite {
            subcommand: "commit".into(),
            description: "提交代码到本地仓库".into(),
        },

        "stash" => {
            if args.iter().any(|a| a == "drop" || a == "clear") {
                GitOperationKind::Dangerous {
                    subcommand: "stash".into(),
                    reason: "丢弃或清空暂存区 (stash drop/clear)".into(),
                }
            } else {
                GitOperationKind::LocalWrite {
                    subcommand: "stash".into(),
                    description: "操作暂存栈 (git stash)".into(),
                }
            }
        }

        "checkout" => {
            if args.iter().any(|a| a == "-f" || a == "--force") {
                return GitOperationKind::Dangerous {
                    subcommand: "checkout".into(),
                    reason: "强制检出 (checkout --force) 将丢弃未保存修改".into(),
                };
            }
            if args.iter().any(|a| a == "-b" || a == "-B") {
                return GitOperationKind::LocalWrite {
                    subcommand: "checkout".into(),
                    description: format!("创建并切换新分支: git checkout {}", args.join(" ")),
                };
            }
            let target = args
                .iter()
                .find(|a| !a.starts_with('-'))
                .cloned()
                .unwrap_or_else(|| "HEAD".into());
            GitOperationKind::BranchSwitchOrRebase {
                subcommand: "checkout".into(),
                target,
                risk_notice: Some("切换分支可能影响未提交的工作区变更".into()),
            }
        }

        "switch" => {
            if args.iter().any(|a| a == "-f" || a == "--force" || a == "--discard-changes") {
                return GitOperationKind::Dangerous {
                    subcommand: "switch".into(),
                    reason: "强制切换分支将丢弃未保存修改".into(),
                };
            }
            if args.iter().any(|a| a == "-c" || a == "-C" || a == "--create") {
                return GitOperationKind::LocalWrite {
                    subcommand: "switch".into(),
                    description: format!("创建并切换新分支: git switch {}", args.join(" ")),
                };
            }
            let target = args
                .iter()
                .find(|a| !a.starts_with('-'))
                .cloned()
                .unwrap_or_else(|| "HEAD".into());
            GitOperationKind::BranchSwitchOrRebase {
                subcommand: "switch".into(),
                target,
                risk_notice: Some("切换分支可能影响未提交的工作区变更".into()),
            }
        }

        "merge" => {
            let target = args
                .iter()
                .find(|a| !a.starts_with('-'))
                .cloned()
                .unwrap_or_else(|| "unknown".into());
            GitOperationKind::BranchSwitchOrRebase {
                subcommand: "merge".into(),
                target,
                risk_notice: Some("合并分支可能引入合并冲突或改动工作区".into()),
            }
        }

        "rebase" => {
            let target = args
                .iter()
                .find(|a| !a.starts_with('-'))
                .cloned()
                .unwrap_or_else(|| "upstream".into());
            GitOperationKind::BranchSwitchOrRebase {
                subcommand: "rebase".into(),
                target,
                risk_notice: Some("变基操作将重写提交历史".into()),
            }
        }

        "push" => {
            // Check for force flags or delete flags
            let is_force = args.iter().any(|a| {
                a == "--force"
                    || a == "-f"
                    || a == "--force-with-lease"
                    || a == "--force-if-includes"
            });
            let is_delete = args.iter().any(|a| a == "--delete" || a == "-d");
            let has_plus_ref = args
                .iter()
                .filter(|a| !a.starts_with('-'))
                .any(|a| a.starts_with('+'));

            if is_force || has_plus_ref {
                return GitOperationKind::Dangerous {
                    subcommand: "push".into(),
                    reason: "强制推送到远端仓库 (可能覆盖或丢失历史提交)".into(),
                };
            }
            if is_delete {
                return GitOperationKind::Dangerous {
                    subcommand: "push".into(),
                    reason: "删除远端分支".into(),
                };
            }

            // Extract remote and branch refspec
            let non_flag_args: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
            let remote = non_flag_args
                .first()
                .map(|s| s.to_string())
                .unwrap_or_else(|| "origin".into());

            let (source_branch, target_branch) = if non_flag_args.len() > 1 {
                let refspec = non_flag_args[1];
                if let Some((src, dst)) = refspec.split_once(':') {
                    (Some(src.to_string()), Some(dst.to_string()))
                } else {
                    (Some(refspec.to_string()), Some(refspec.to_string()))
                }
            } else {
                (None, None)
            };

            GitOperationKind::RemoteWrite {
                remote,
                source_branch,
                target_branch,
            }
        }

        "reset" => {
            if args.iter().any(|a| a == "--hard") {
                GitOperationKind::Dangerous {
                    subcommand: "reset".into(),
                    reason: "硬重置 (git reset --hard) 将彻底丢弃所有工作区和暂存区修改".into(),
                }
            } else {
                GitOperationKind::LocalWrite {
                    subcommand: "reset".into(),
                    description: "重置 HEAD 或暂存区 (保持工作区不变)".into(),
                }
            }
        }

        "clean" => {
            if args.iter().any(|a| a.contains('f') || a == "--force") {
                GitOperationKind::Dangerous {
                    subcommand: "clean".into(),
                    reason: "强制清理未跟踪文件 (git clean -f)".into(),
                }
            } else {
                GitOperationKind::ReadOnlyQuery {
                    subcommand: "clean".into(),
                }
            }
        }

        "restore" => {
            if args.iter().any(|a| a == "--worktree" || a == "-W")
                || !args.iter().any(|a| a == "--staged" || a == "-S")
            {
                GitOperationKind::Dangerous {
                    subcommand: "restore".into(),
                    reason: "丢弃工作区已修改的文件内容".into(),
                }
            } else {
                GitOperationKind::LocalWrite {
                    subcommand: "restore".into(),
                    description: "将已暂存文件取消暂存".into(),
                }
            }
        }

        "config" => {
            let is_query = args.iter().any(|a| {
                a == "--get"
                    || a == "--get-all"
                    || a == "--list"
                    || a == "-l"
                    || a == "--get-regexp"
            }) || (args.len() == 1 && !args[0].starts_with('-'));

            if is_query {
                GitOperationKind::ReadOnlyQuery {
                    subcommand: "config".into(),
                }
            } else {
                let key = args.iter().find(|a| !a.starts_with('-')).cloned();
                GitOperationKind::ConfigMutation { key, value: None }
            }
        }

        "cherry-pick" | "revert" => GitOperationKind::LocalWrite {
            subcommand: subcmd.to_string(),
            description: format!("应用提交: git {subcmd}"),
        },

        _ => GitOperationKind::CompoundOrShell {
            reason: format!("未分类或自定义 Git 子命令 '{subcmd}'，按通用 Shell 审批处理"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_read_only_queries() {
        for cmd in [
            "git status",
            "git status -s",
            "git diff",
            "git diff --cached",
            "git log -n 5 --oneline",
            "git show HEAD",
            "git branch",
            "git branch -a",
            "git branch --list",
            "git tag -l",
            "git remote -v",
            "git rev-parse HEAD",
        ] {
            let analysis = GitCommandAnalysis::parse(cmd).expect(cmd);
            assert!(
                matches!(analysis.kind, GitOperationKind::ReadOnlyQuery { .. }),
                "Expected ReadOnlyQuery for '{cmd}', got {:?}",
                analysis.kind
            );
        }
    }

    #[test]
    fn parses_local_writes() {
        let add = GitCommandAnalysis::parse("git add src/main.rs").unwrap();
        assert!(matches!(add.kind, GitOperationKind::LocalWrite { ref subcommand, .. } if subcommand == "add"));

        let commit = GitCommandAnalysis::parse("git commit -m 'feat: test'").unwrap();
        assert!(matches!(commit.kind, GitOperationKind::LocalWrite { ref subcommand, .. } if subcommand == "commit"));

        let new_branch = GitCommandAnalysis::parse("git checkout -b feature/xyz").unwrap();
        assert!(matches!(new_branch.kind, GitOperationKind::LocalWrite { .. }));

        let new_branch2 = GitCommandAnalysis::parse("git switch -c feature/xyz").unwrap();
        assert!(matches!(new_branch2.kind, GitOperationKind::LocalWrite { .. }));
    }

    #[test]
    fn parses_remote_writes() {
        let push = GitCommandAnalysis::parse("git push origin main").unwrap();
        match push.kind {
            GitOperationKind::RemoteWrite {
                remote,
                source_branch,
                target_branch,
            } => {
                assert_eq!(remote, "origin");
                assert_eq!(source_branch.as_deref(), Some("main"));
                assert_eq!(target_branch.as_deref(), Some("main"));
            }
            other => panic!("Unexpected kind: {other:?}"),
        }

        let push_colon = GitCommandAnalysis::parse("git push upstream feat:main").unwrap();
        match push_colon.kind {
            GitOperationKind::RemoteWrite {
                remote,
                source_branch,
                target_branch,
            } => {
                assert_eq!(remote, "upstream");
                assert_eq!(source_branch.as_deref(), Some("feat"));
                assert_eq!(target_branch.as_deref(), Some("main"));
            }
            other => panic!("Unexpected kind: {other:?}"),
        }
    }

    #[test]
    fn detects_dangerous_commands() {
        for (cmd, expected_sub) in [
            ("git push --force origin main", "push"),
            ("git push -f origin main", "push"),
            ("git push origin +main", "push"),
            ("git push origin --delete old-branch", "push"),
            ("git reset --hard HEAD~1", "reset"),
            ("git clean -fd", "clean"),
            ("git clean -f", "clean"),
            ("git branch -D old-feature", "branch"),
            ("git checkout -f", "checkout"),
            ("git restore .", "restore"),
        ] {
            let analysis = GitCommandAnalysis::parse(cmd).expect(cmd);
            assert!(
                matches!(analysis.kind, GitOperationKind::Dangerous { ref subcommand, .. } if subcommand == expected_sub),
                "Expected Dangerous for '{cmd}', got {:?}",
                analysis.kind
            );
        }
    }

    #[test]
    fn falls_back_on_compound_and_pipeline_commands() {
        for cmd in [
            "git status && git add .",
            "git commit -m 'done'; git push",
            "git status | grep modified",
            "git status `rm -rf .git`",
            "git log $(cat /tmp/arg)",
            "git diff > /tmp/diff.patch",
        ] {
            let analysis = GitCommandAnalysis::parse(cmd).expect(cmd);
            assert!(
                matches!(analysis.kind, GitOperationKind::CompoundOrShell { .. }),
                "Expected CompoundOrShell for '{cmd}', got {:?}",
                analysis.kind
            );
        }
    }

    #[test]
    fn detects_config_and_hooks_injection() {
        let inline_hook = GitCommandAnalysis::parse("git -c core.hooksPath=/tmp/pwn status").unwrap();
        assert!(matches!(inline_hook.kind, GitOperationKind::ConfigMutation { .. }));

        let inline_ssh = GitCommandAnalysis::parse("git -c core.sshCommand=evil push").unwrap();
        assert!(matches!(inline_ssh.kind, GitOperationKind::ConfigMutation { .. }));

        let config_set = GitCommandAnalysis::parse("git config user.name 'Test'").unwrap();
        assert!(matches!(config_set.kind, GitOperationKind::ConfigMutation { .. }));

        let config_get = GitCommandAnalysis::parse("git config user.name").unwrap();
        assert!(matches!(config_get.kind, GitOperationKind::ReadOnlyQuery { .. }));
    }
}
