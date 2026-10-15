//! User-owned credentials and OAuth flows, independent of model protocols and UI.
//!
//! Paths are injected by the host. No global environment or other clients' caches
//! are consulted. Authentication failures never select another billing method.
mod credential;
mod error;
mod http;
mod oauth;
mod providers;
mod store;
mod usage;
pub use usage::{UsageAuthorization, UsageProvider, usage_api_key};

pub use credential::{
    AuthTarget, CredentialInfo, CredentialKind, CredentialRef, CredentialSnapshot, RequestAuth,
};
pub use error::{AuthError, CredentialCommitError, Result};
pub use oauth::CallbackListener;
pub use oauth::{CallbackFlow, LoginOutcome, OAuthLogin, OAuthProvider, PendingGoogleLogin};
pub use providers::anthropic::AnthropicLogin;
pub use providers::api_key::{ApiKeyProvider, DEEPSEEK_BASE, ZHIPU_CODING_BASE};
pub use store::AuthStore;

pub use providers::antigravity::refresh_client_version as refresh_antigravity_client_version;

mod directory;
pub use directory::directory;
