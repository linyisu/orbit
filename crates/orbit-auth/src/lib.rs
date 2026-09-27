//! User-owned credentials and OAuth flows, independent of model protocols and UI.
//!
//! Paths are injected by the host. No global environment or other clients' caches
//! are consulted. Authentication failures never select another billing method.
mod anthropic;
mod antigravity;
mod api_key;
mod callback;
mod chatgpt;
mod credential;
mod error;
mod google;
mod http;
mod oauth;
mod store;

pub use anthropic::AnthropicLogin;
pub use api_key::{ApiKeyProvider, DEEPSEEK_BASE, ZHIPU_CODING_BASE, verify_deepseek};
pub use callback::CallbackListener;
pub use credential::{AuthTarget, CredentialInfo, CredentialKind, CredentialRef, RequestAuth};
pub use error::{AuthError, Result};
pub use oauth::{CallbackFlow, LoginOutcome, OAuthLogin, OAuthProvider, PendingGoogleLogin};
pub use store::AuthStore;

pub use antigravity::refresh_client_version as refresh_antigravity_client_version;
