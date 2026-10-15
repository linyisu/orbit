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
    #[error("Google Cloud Code Assist 需要项目 ID，请在账号连接界面选择项目")]
    GoogleProjectRequired,
    #[error("Antigravity 账号初始化未通过：{0}")]
    AntigravityAccount(String),
    #[error("Antigravity 需要账号验证：{reason}\n验证入口：{url}")]
    AntigravityVerification { reason: String, url: String },
    #[error("ChatGPT 账号未授权套餐使用，请在账号设置中重新授权")]
    PlanUsageDisabled,
    #[error("此账号使用旧版认证接入，请在账号设置中重新登录后刷新模型目录")]
    ReauthorizationRequired,
    #[error("账号凭据不存在，请使用 /connect 或账号设置重新连接")]
    Missing,
    #[error("credential is not authorized for this endpoint or API format")]
    Scope,
    #[error("authentication network request failed")]
    Transport,
    #[error("authentication server returned HTTP {0}")]
    Http(u16),
    #[error("账号登录已过期或已撤销，请使用 /connect 或账号设置重新登录")]
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

/// Distinguish authentication failure from an external commit that was rolled back.
#[derive(Debug)]
pub enum CredentialCommitError<E> {
    Authentication(AuthError),
    Write {
        error: AuthError,
        rollback: Option<AuthError>,
    },
    Commit {
        error: E,
        rollback: Option<AuthError>,
    },
}

impl<E: std::fmt::Display> std::fmt::Display for CredentialCommitError<E> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let rollback = match self {
            Self::Authentication(error) => return error.fmt(formatter),
            Self::Write { error, rollback } => {
                write!(formatter, "{error}")?;
                rollback
            }
            Self::Commit { error, rollback } => {
                write!(formatter, "{error}")?;
                rollback
            }
        };
        if let Some(error) = rollback {
            write!(formatter, "; credential rollback failed: {error}")?;
        }
        Ok(())
    }
}

impl<E: std::fmt::Debug + std::fmt::Display> std::error::Error for CredentialCommitError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Authentication(error) | Self::Write { error, .. } => Some(error),
            Self::Commit { rollback, .. } => rollback
                .as_ref()
                .map(|error| error as _),
        }
    }
}
