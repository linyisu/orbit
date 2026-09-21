use orbit_tools::approval::ApprovalPreview;

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
    pub fn allows(self, preview: &ApprovalPreview) -> bool {
        match self {
            Self::Ask => false,
            Self::Auto => matches!(
                preview,
                ApprovalPreview::Command(cmd)
                    if cmd.workspace_access == orbit_tools::approval::CommandWorkspaceAccess::Read
            ),
            Self::Yolo => true,
        }
    }
    pub fn description(self) -> &'static str {
        match self {
            Self::Ask => "ask：文件修改、沙箱命令、MCP 调用和外部权限逐次确认",
            Self::Auto => {
                "auto：沙箱只读命令自动执行，写工作区命令、文件修改、MCP 调用和外部权限确认"
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
        let cmd_read = ApprovalPreview::Command(CommandPreview {
            command: "pwd".into(),
            timeout_seconds: 30,
            writable_snapshot: false,
            workspace_access: CommandWorkspaceAccess::Read,
        });
        let cmd_rw = ApprovalPreview::Command(CommandPreview {
            command: "cargo build".into(),
            timeout_seconds: 30,
            writable_snapshot: false,
            workspace_access: CommandWorkspaceAccess::ReadWrite,
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
        assert!(!Mode::Ask.allows(&cmd_read));
        assert!(!Mode::Ask.allows(&cmd_rw));
        assert!(!Mode::Ask.allows(&write));
        assert!(!Mode::Ask.allows(&capability));
        assert!(Mode::Auto.allows(&cmd_read));
        assert!(!Mode::Auto.allows(&cmd_rw));
        assert!(!Mode::Auto.allows(&write));
        assert!(!Mode::Auto.allows(&delete));
        assert!(!Mode::Ask.allows(&mcp));
        assert!(!Mode::Auto.allows(&mcp));
        assert!(!Mode::Auto.allows(&capability));
        assert!(Mode::Yolo.allows(&cmd_read));
        assert!(Mode::Yolo.allows(&cmd_rw));
        assert!(Mode::Yolo.allows(&write));
        assert!(Mode::Yolo.allows(&delete));
        assert!(Mode::Yolo.allows(&mcp));
        assert!(Mode::Yolo.allows(&capability));
        assert_eq!(Mode::default(), Mode::Auto);
        assert_eq!(Mode::Auto.next().next().next(), Mode::Auto);
        assert_eq!(Mode::parse("yolo"), Some(Mode::Yolo));
        assert_eq!(Mode::parse("YOLO"), Some(Mode::Yolo));
        assert_eq!(Mode::parse("unknown"), None);
    }
}
