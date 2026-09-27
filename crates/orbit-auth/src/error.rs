use thiserror::Error;

pub type Result<T> = std::result::Result<T, AuthError>;

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("credential storage: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid credential storage data")]
    Storage,
    #[error("invalid authentication configuration: {0}")]
    Configuration(&'static str),
    #[error(
        "Google Cloud Code Assist requires a project ID; use --project <id> or continue in /connect"
    )]
    GoogleProjectRequired,
    #[error("Antigravity 账号初始化未通过：{0}")]
    AntigravityAccount(String),
    #[error("Antigravity 需要账号验证：{reason}\n验证入口：{url}")]
    AntigravityVerification { reason: String, url: String },
    #[error("credential not found; run orbit auth login or orbit auth set-key")]
    Missing,
    #[error("credential is not authorized for this endpoint or API format")]
    Scope,
    #[error("authentication network request failed")]
    Transport,
    #[error("authentication server returned HTTP {0}")]
    Http(u16),
    #[error("login expired or revoked; run orbit auth login again")]
    LoginRequired,
    #[error("authentication server returned an invalid token response")]
    InvalidToken,
    #[error("OAuth callback has an invalid state, code, or redirect address")]
    InvalidCallback,
    #[error("OAuth authorization was denied")]
    Denied,
    #[error("OAuth login timed out")]
    Timeout,
}
