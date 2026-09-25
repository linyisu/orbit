use orbit_tools::approval::ApprovalPreview;
use std::path::Path;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    Ask,
    #[default]
    Auto,
    Yolo,
}

impl Mode {
    pub fn parse(text: &str) -> Option<Self> {
        match text.to_ascii_lowercase().as_str() {
            "ask" => Some(Self::Ask),
            "auto" => Some(Self::Auto),
            "yolo" => Some(Self::Yolo),
            _ => None,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Auto => "auto",
            Self::Yolo => "YOLO",
        }
    }
    pub fn next(self) -> Self {
        match self {
            Self::Ask => Self::Auto,
            Self::Auto => Self::Yolo,
            Self::Yolo => Self::Ask,
        }
    }
    pub fn allows(self, preview: &ApprovalPreview, workspace: &Path) -> bool {
        match self {
            Self::Ask => false,
            Self::Auto => orbit_tools::approval::auto_allows(preview, workspace),
            Self::Yolo => true,
        }
    }
    pub fn description(self) -> &'static str {
        match self {
            Self::Ask => "ask：文件修改、沙箱命令、MCP 调用和外部权限逐次确认",
            Self::Auto => {
                "auto：工作区文件工具和离线只读命令自动执行；联网、Shell 写入和外部权限需确认"
            }
            Self::Yolo => "YOLO：全部操作自动确认通过（沙箱和权限边界限制不变）",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orbit_runtime::capability::{EnvironmentId, FilesystemAccess};
    use orbit_tools::approval::{
        CapabilityPreview, CommandPreview, CommandWorkspaceAccess, DeletePreview, McpPreview,
        WritePreview,
    };
    use std::path::PathBuf;

    #[test]
    fn policy_matrix_and_cycle() {
        let workspace = tempfile::tempdir().unwrap();
        let root = workspace.path();
        let cmd_read = ApprovalPreview::Command(CommandPreview {
            command: "pwd".into(),
            timeout_seconds: 30,
            workspace_access: CommandWorkspaceAccess::Read,
            network_allowed: true,
            external_capabilities: Vec::new(),
            external_write: false,
        });
        let cmd_rw = ApprovalPreview::Command(CommandPreview {
            command: "cargo build".into(),
            timeout_seconds: 30,
            workspace_access: CommandWorkspaceAccess::ReadWrite,
            network_allowed: true,
            external_capabilities: Vec::new(),
            external_write: false,
        });
        let write = ApprovalPreview::Write(WritePreview {
            path: "file".into(),
            before: None,
            after: "text".into(),
        });
        let delete = ApprovalPreview::Delete(DeletePreview {
            path: "file".into(),
            before: "text".into(),
        });
        let mcp = ApprovalPreview::Mcp(McpPreview {
            server: "files".into(),
            tool: "read_file".into(),
            arguments: r#"{\"path\":\"README.md\"}"#.into(),
        });
        let capability = ApprovalPreview::Capability(CapabilityPreview {
            environment: EnvironmentId::Local,
            path: PathBuf::from("/tmp/data"),
            access: FilesystemAccess::ReadWrite,
            reason: "testing".into(),
        });
        assert!(!Mode::Ask.allows(&cmd_read, root));
        assert!(!Mode::Ask.allows(&cmd_rw, root));
        assert!(!Mode::Ask.allows(&write, root));
        assert!(!Mode::Ask.allows(&capability, root));
        assert!(!Mode::Auto.allows(&cmd_read, root));
        assert!(!Mode::Auto.allows(&cmd_rw, root));
        let offline_read = ApprovalPreview::Command(CommandPreview {
            network_allowed: false,
            ..match cmd_read.clone() {
                ApprovalPreview::Command(command) => command,
                _ => unreachable!(),
            }
        });
        assert!(Mode::Auto.allows(&offline_read, root));
        assert!(Mode::Auto.allows(&write, root));
        assert!(Mode::Auto.allows(&delete, root));
        assert!(!Mode::Ask.allows(&mcp, root));
        assert!(!Mode::Auto.allows(&mcp, root));
        assert!(!Mode::Auto.allows(&capability, root));
        assert!(Mode::Yolo.allows(&cmd_read, root));
        assert!(Mode::Yolo.allows(&cmd_rw, root));
        assert!(Mode::Yolo.allows(&write, root));
        assert!(Mode::Yolo.allows(&delete, root));
        assert!(Mode::Yolo.allows(&mcp, root));
        assert!(Mode::Yolo.allows(&capability, root));
        let outside = ApprovalPreview::Write(WritePreview {
            path: workspace
                .path()
                .parent()
                .unwrap()
                .join("outside.txt")
                .display()
                .to_string(),
            before: None,
            after: "text".into(),
        });
        assert!(!Mode::Auto.allows(&outside, root));
        let external_command = ApprovalPreview::Command(CommandPreview {
            command: "write outside".into(),
            timeout_seconds: 30,
            workspace_access: CommandWorkspaceAccess::ReadWrite,
            network_allowed: true,
            external_capabilities: Vec::new(),
            external_write: true,
        });
        assert!(!Mode::Auto.allows(&external_command, root));
        assert_eq!(Mode::default(), Mode::Auto);
        assert_eq!(Mode::Auto.next().next().next(), Mode::Auto);
        assert_eq!(Mode::parse("yolo"), Some(Mode::Yolo));
        assert_eq!(Mode::parse("YOLO"), Some(Mode::Yolo));
        assert_eq!(Mode::parse("unknown"), None);
    }
}
