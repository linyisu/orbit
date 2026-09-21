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
            Self::Auto => matches!(preview, ApprovalPreview::Command(_)),
            Self::Yolo => true,
        }
    }
    pub fn description(self) -> &'static str {
        match self {
            Self::Ask => "ask：文件修改、沙箱命令、MCP 调用和外部权限逐次确认",
            Self::Auto => "auto：沙箱命令自动执行，文件修改、MCP 调用和外部权限确认",
            Self::Yolo => "YOLO：全部操作自动确认通过（沙箱和权限边界限制不变）",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orbit_tools::approval::{
        CapabilityPreview, CommandPreview, DeletePreview, McpPreview, WritePreview,
    };
    use orbit_tools::capability::{EnvironmentId, FilesystemAccess};
    use std::path::PathBuf;

    #[test]
    fn policy_matrix_and_cycle() {
        let command = ApprovalPreview::Command(CommandPreview {
            command: "pwd".into(),
            timeout_seconds: 30,
            writable_snapshot: true,
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
        assert!(!Mode::Ask.allows(&command));
        assert!(!Mode::Ask.allows(&write));
        assert!(!Mode::Ask.allows(&capability));
        assert!(Mode::Auto.allows(&command));
        assert!(!Mode::Auto.allows(&write));
        assert!(!Mode::Auto.allows(&delete));
        assert!(!Mode::Ask.allows(&mcp));
        assert!(!Mode::Auto.allows(&mcp));
        assert!(!Mode::Auto.allows(&capability));
        assert!(Mode::Yolo.allows(&command));
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
