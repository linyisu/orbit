use std::{
    io::{self, IsTerminal, Write},
    path::Path,
};

use clap::ValueEnum;
use orbit_tools::approval::{ApprovalPreview, ApprovalReceiver};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum ApprovalMode {
    Auto,
    #[default]
    Ask,
    Deny,
}

impl ApprovalMode {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Ask => "ask",
            Self::Deny => "deny",
        }
    }

    pub const fn next(self) -> Self {
        match self {
            Self::Auto => Self::Ask,
            Self::Ask => Self::Deny,
            Self::Deny => Self::Auto,
        }
    }
}

fn workspace_local_file(path: &str, workspace: &Path) -> bool {
    let path = Path::new(path);
    if path.is_absolute() {
        return false;
    }
    let Some(parent) = path.parent() else {
        return false;
    };
    workspace
        .join(parent)
        .canonicalize()
        .is_ok_and(|resolved| resolved.starts_with(workspace))
}

pub(crate) fn auto_allows(preview: &ApprovalPreview, workspace: &Path) -> bool {
    match preview {
        ApprovalPreview::Write(write) => workspace_local_file(&write.path, workspace),
        ApprovalPreview::Command(_) => true,
    }
}

fn show_preview(preview: &ApprovalPreview) {
    match preview {
        ApprovalPreview::Write(write) => {
            eprintln!("File change: {}", write.path);
            eprintln!("--- before");
            eprintln!("{}", write.before.as_deref().unwrap_or("(new file)"));
            eprintln!("+++ after");
            eprintln!("{}", write.after);
        }
        ApprovalPreview::Command(command) => {
            eprintln!(
                "Disposable workspace command (timeout {}s; network allowed):",
                command.timeout_seconds
            );
            eprintln!("{}", command.command);
        }
    }
}

async fn ask(preview: ApprovalPreview) -> bool {
    if !io::stdin().is_terminal() {
        eprintln!("approval requires a terminal; request denied");
        return false;
    }

    tokio::task::spawn_blocking(move || {
        show_preview(&preview);
        eprint!("Approve? [y/N] ");
        if io::stderr().flush().is_err() {
            return false;
        }

        let mut answer = String::new();
        io::stdin()
            .read_line(&mut answer)
            .is_ok_and(|_| matches!(answer.trim(), "y" | "Y" | "yes" | "YES"))
    })
    .await
    .unwrap_or(false)
}

pub async fn serve(mut receiver: ApprovalReceiver, mode: ApprovalMode, workspace: &Path) {
    while let Some(request) = receiver.recv().await {
        let approved = match mode {
            ApprovalMode::Auto => auto_allows(&request.preview, workspace),
            ApprovalMode::Ask => ask(request.preview).await,
            ApprovalMode::Deny => false,
        };
        let _ = request.decision.send(approved);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orbit_tools::approval::{CommandPreview, WritePreview};

    #[test]
    fn auto_mode_stays_inside_workspace() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("src")).unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("linked")).unwrap();
        let write = |path: &str| {
            ApprovalPreview::Write(WritePreview {
                path: path.to_owned(),
                before: None,
                after: "content".to_owned(),
            })
        };

        assert!(auto_allows(&write("src/main.rs"), root.path()));
        assert!(!auto_allows(&write("linked/outside.rs"), root.path()));
        assert!(!auto_allows(&write("../outside.rs"), root.path()));
        assert!(auto_allows(
            &ApprovalPreview::Command(CommandPreview {
                command: "pwd".to_owned(),
                timeout_seconds: 10,
            }),
            root.path()
        ));
    }
}
