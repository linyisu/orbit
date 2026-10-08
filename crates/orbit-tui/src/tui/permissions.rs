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
            Self::Yolo => "yolo",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Ask => "ask：文件修改、命令、MCP 调用和外部权限逐次确认",
            Self::Auto => {
                "auto：工作区文件操作及从工作区启动的所有命令自动执行（含联网）；MCP 和新增外部权限需确认。命令隔离由沙箱配置决定"
            }
            Self::Yolo => "YOLO：全部操作自动确认通过（隔离方式由工作区配置决定）",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_supported_permission_modes() {
        assert_eq!(Mode::default(), Mode::Auto);
        for (text, mode) in [
            ("ask", Mode::Ask),
            ("auto", Mode::Auto),
            ("yolo", Mode::Yolo),
            ("YOLO", Mode::Yolo),
        ] {
            assert_eq!(Mode::parse(text), Some(mode));
        }
        assert_eq!(Mode::parse("unknown"), None);
    }
}
