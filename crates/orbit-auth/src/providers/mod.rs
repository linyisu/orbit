//! Provider authentication protocols. Storage and login orchestration live outside this module.
pub(crate) mod anthropic;
pub(crate) mod antigravity;
pub(crate) mod api_key;
pub(crate) mod chatgpt;
pub(crate) mod google;

use crate::{AuthTarget, CredentialKind, Result, credential::Credential};

/// OAuth registrations have fixed destinations; API keys keep their caller-reviewed scope.
pub(crate) fn binding(kind: CredentialKind) -> Option<(&'static str, AuthTarget)> {
    match kind {
        CredentialKind::ApiKey => None,
        CredentialKind::AnthropicOAuth => {
            Some(("https://api.anthropic.com/v1/", AuthTarget::Anthropic))
        }
        CredentialKind::AntigravityOAuth => Some((
            crate::providers::antigravity::API_BASE,
            AuthTarget::GoogleCodeAssist,
        )),
        CredentialKind::GoogleOAuth => Some((
            crate::providers::google::API_BASE,
            AuthTarget::GoogleCodeAssist,
        )),
        CredentialKind::ChatgptOAuth => Some((
            crate::providers::chatgpt::legacy::API_BASE,
            AuthTarget::ChatgptCodex,
        )),
        CredentialKind::ChatgptPlanOAuth => Some((
            crate::providers::chatgpt::API_BASE,
            AuthTarget::ChatgptCodex,
        )),
    }
}

pub(crate) fn authorize(
    credential: &Credential,
    headers: &mut reqwest::header::HeaderMap,
) -> Result<()> {
    match credential.kind {
        CredentialKind::ApiKey => Ok(()),
        CredentialKind::AnthropicOAuth => anthropic::authorize(credential, headers),
        CredentialKind::GoogleOAuth => google::authorize(credential, headers),
        CredentialKind::AntigravityOAuth => antigravity::authorize(credential, headers),
        CredentialKind::ChatgptOAuth => chatgpt::legacy::authorize(credential, headers),
        CredentialKind::ChatgptPlanOAuth => chatgpt::authorize(credential, headers),
    }
}

pub(crate) async fn refresh(credential: &Credential) -> Result<Credential> {
    match credential.kind {
        CredentialKind::ApiKey => Err(crate::AuthError::Scope),
        CredentialKind::AnthropicOAuth => anthropic::refresh(credential).await,
        CredentialKind::GoogleOAuth => google::refresh(credential).await,
        CredentialKind::AntigravityOAuth => antigravity::refresh(credential).await,
        CredentialKind::ChatgptOAuth => chatgpt::legacy::refresh(credential).await,
        CredentialKind::ChatgptPlanOAuth => chatgpt::refresh(credential).await,
    }
}
