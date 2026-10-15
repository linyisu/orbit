//! Sign in with ChatGPT registrations; legacy Codex grants remain endpoint-scoped.
mod host;
mod identity;
pub(crate) mod legacy;
mod login;

use crate::{
    AuthError, CredentialKind, Result,
    credential::{Credential, Secret},
    http::{Tokens, form_token},
};
pub(crate) use login::ChatgptLogin;
use serde::{Deserialize, Serialize};

pub(crate) const API_BASE: &str = "https://api.openai.com/v1/";
const RESOURCE: &str = "https://api.openai.com/v1";
const ISSUER: &str = "https://auth.openai.com";
const TOKEN_URL: &str = "https://auth.openai.com/api/accounts/oauth/token";
const PLAN_SCOPE: &str = "chatgpt.tokens.use.direct";

/// Validated registration metadata belongs to one user and one issued OAuth client.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Registration {
    pub client_id: String,
    pub subject: String,
    pub id_token: Secret,
    pub scopes: Vec<String>,
}

impl Registration {
    pub fn authorize(&self) -> Result<()> {
        if !self
            .scopes
            .iter()
            .any(|scope| scope == PLAN_SCOPE)
        {
            return Err(AuthError::PlanUsageDisabled);
        }
        Ok(())
    }
}

pub(crate) async fn refresh(previous: &Credential) -> Result<Credential> {
    let registration = previous
        .chatgpt
        .as_ref()
        .ok_or(AuthError::InvalidToken)?;
    let refresh = previous
        .refresh
        .as_ref()
        .ok_or(AuthError::LoginRequired)?;
    let payload = form_token(
        TOKEN_URL,
        &[
            ("grant_type", "refresh_token"),
            ("client_id", &registration.client_id),
            ("refresh_token", &refresh.0),
            ("resource", RESOURCE),
        ],
    )
    .await?;
    let mut registration = registration.clone();
    if let Some(scope) = payload["scope"].as_str() {
        registration.scopes = scope
            .split_whitespace()
            .map(str::to_owned)
            .collect();
    }
    let tokens: Tokens = serde_json::from_value(payload).map_err(|_| AuthError::InvalidToken)?;
    let mut credential = previous.clone();
    if let Some(id_token) = &tokens.id_token {
        let identity = identity::verify(&id_token.0, &registration.client_id, None).await?;
        if identity.sub != registration.subject {
            return Err(AuthError::LoginRequired);
        }
        credential.account = identity
            .email
            .or_else(|| previous.account.clone());
        registration.id_token = id_token.clone();
    }
    credential.chatgpt = Some(registration);
    tokens.apply(&mut credential)?;
    Ok(credential)
}

fn credential(
    registration: Registration,
    email: Option<String>,
    tokens: Tokens,
) -> Result<Credential> {
    let mut credential = Credential {
        version: 1,
        kind: CredentialKind::ChatgptPlanOAuth,
        endpoint: API_BASE.into(),
        access: Secret(String::new()),
        refresh: None,
        expires_at: None,
        account: email,
        account_id: Some(registration.subject.clone()),
        project_id: None,
        chatgpt: Some(Box::new(registration)),
    };
    tokens.apply(&mut credential)?;
    Ok(credential)
}

/// Add provider authentication metadata after the common endpoint scope check.
pub(crate) fn authorize(
    credential: &crate::credential::Credential,
    _headers: &mut reqwest::header::HeaderMap,
) -> crate::Result<()> {
    let registration = credential
        .chatgpt
        .as_ref()
        .ok_or(AuthError::InvalidToken)?;
    registration.authorize()?;

    Ok(())
}
