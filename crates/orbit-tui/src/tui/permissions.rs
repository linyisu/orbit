pub use orbit_protocol::Mode;

/// Confirmed permissions and local intent have different lifetimes.
#[derive(Debug, Default)]
pub struct State {
    pub mode: Mode,
    pub pending: Option<Mode>,
    revision: u64,
}

impl State {
    pub fn apply(&mut self, mode: Mode, revision: u64) {
        if revision >= self.revision {
            self.mode = mode;
            self.revision = revision;
        }
    }

    pub fn next(&self) -> Mode {
        self.pending.unwrap_or(self.mode).next()
    }

    pub fn label(&self) -> String {
        match self.pending {
            Some(mode) => format!("{}…", mode.label()),
            None => self.mode.label().into(),
        }
    }
}

pub trait ModeExt {
    fn next(self) -> Mode;
    fn label(self) -> &'static str;
    fn description(self) -> &'static str;
}

impl ModeExt for Mode {
    fn next(self) -> Self {
        match self {
            Self::Ask => Self::Auto,
            Self::Auto => Self::Yolo,
            Self::Yolo => Self::Ask,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Auto => "auto",
            Self::Yolo => "yolo",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Ask => "ask：文件修改、命令、MCP 调用和外部权限逐次确认",
            Self::Auto => {
                "auto：工作区文件操作及从工作区启动的所有命令自动执行（含联网）；MCP 和新增外部权限需确认。命令使用目标用户的真实环境与系统权限"
            }
            Self::Yolo => "YOLO：全部操作自动确认通过（隔离方式由工作区配置决定）",
        }
    }
}
