//! Official API-key provider identities; discovery belongs to orbit-model.

pub const DEEPSEEK_BASE: &str = "https://api.deepseek.com/v1/";

pub const ZHIPU_CODING_BASE: &str = "https://open.bigmodel.cn/api/coding/paas/v4/";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApiKeyProvider {
    Deepseek,
    ZhipuCodingPlan,
}

impl ApiKeyProvider {
    pub fn name(self) -> &'static str {
        match self {
            Self::Deepseek => "deepseek",
            Self::ZhipuCodingPlan => "zhipu-coding-plan",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Deepseek => "DeepSeek API",
            Self::ZhipuCodingPlan => "智谱 Coding Plan",
        }
    }

    pub fn base_url(self) -> &'static str {
        match self {
            Self::Deepseek => DEEPSEEK_BASE,
            Self::ZhipuCodingPlan => ZHIPU_CODING_BASE,
        }
    }
}
