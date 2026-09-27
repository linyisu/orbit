use std::{
    fmt,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use reqwest::header::{HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::{AuthError, Result};

/// A non-secret reference, safe to retain in model configuration.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CredentialRef {
    pub directory: PathBuf,
    pub name: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CredentialKind {
    ApiKey,
    AnthropicOAuth,
    GoogleOAuth,
    AntigravityOAuth,
    ChatgptOAuth,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthTarget {
    Openai,
    Anthropic,
    GoogleCodeAssist,
    ChatgptCodex,
}

#[derive(Debug, Serialize)]
pub struct CredentialInfo {
    pub name: String,
    pub kind: CredentialKind,
    pub endpoint: String,
    pub expires_at: Option<u64>,
    pub account: Option<String>,
    #[serde(default)]
    pub account_id: Option<String>,
    #[serde(default)]
    pub project_id: Option<String>,
}

#[derive(Debug)]
pub struct RequestAuth {
    pub kind: CredentialKind,
    pub headers: HeaderMap,
    pub project_id: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct Secret(pub String);

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Credential {
    pub version: u32,
    pub kind: CredentialKind,
    pub endpoint: String,
    pub access: Secret,
    pub refresh: Option<Secret>,
    pub expires_at: Option<u64>,
    pub account: Option<String>,
    #[serde(default)]
    pub account_id: Option<String>,
    #[serde(default)]
    pub project_id: Option<String>,
}

impl Credential {
    pub fn info(&self, name: String) -> CredentialInfo {
        CredentialInfo {
            name,
            kind: self.kind,
            endpoint: self.endpoint.clone(),
            expires_at: self.expires_at,
            account: self.account.clone(),
            account_id: self.account_id.clone(),
            project_id: self.project_id.clone(),
        }
    }

    pub fn request_auth(&self, endpoint: &Url, target: AuthTarget) -> Result<RequestAuth> {
        let required = match self.kind {
            CredentialKind::ApiKey => None,
            CredentialKind::AnthropicOAuth => {
                Some(("https://api.anthropic.com/v1/", AuthTarget::Anthropic))
            }
            CredentialKind::AntigravityOAuth => {
                Some((crate::antigravity::API_BASE, AuthTarget::GoogleCodeAssist))
            }
            CredentialKind::GoogleOAuth => {
                Some((crate::google::API_BASE, AuthTarget::GoogleCodeAssist))
            }
            CredentialKind::ChatgptOAuth => {
                Some((crate::chatgpt::API_BASE, AuthTarget::ChatgptCodex))
            }
        };
        if let Some((base, required_target)) = required {
            if self.endpoint != base || target != required_target {
                return Err(AuthError::Scope);
            }
        } else if matches!(
            target,
            AuthTarget::GoogleCodeAssist | AuthTarget::ChatgptCodex
        ) {
            return Err(AuthError::Scope);
        }
        let base = endpoint_scope(&self.endpoint)?;
        if endpoint.origin() != base.origin() || !endpoint.path().starts_with(base.path()) {
            return Err(AuthError::Scope);
        }

        let mut headers = HeaderMap::new();
        let (name, value) =
            if self.kind == CredentialKind::ApiKey && target == AuthTarget::Anthropic {
                ("x-api-key", self.access.0.clone())
            } else {
                ("authorization", format!("Bearer {}", self.access.0))
            };
        let mut value = HeaderValue::from_str(&value).map_err(|_| AuthError::InvalidToken)?;
        value.set_sensitive(true);
        headers.insert(name, value);

        if self.kind == CredentialKind::AnthropicOAuth {
            headers.insert(
                "anthropic-beta",
                HeaderValue::from_static("oauth-2025-04-20"),
            );
            headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
            headers.insert("x-app", HeaderValue::from_static("cli"));
        }

        if self.kind == CredentialKind::ChatgptOAuth {
            let account = self.account_id.as_deref().ok_or(AuthError::InvalidToken)?;
            let mut value = HeaderValue::from_str(account).map_err(|_| AuthError::InvalidToken)?;
            value.set_sensitive(true);
            headers.insert("chatgpt-account-id", value);
            headers.insert("originator", HeaderValue::from_static("orbit"));
        }
        if self.kind == CredentialKind::AntigravityOAuth {
            crate::google::validate_project(
                self.project_id.as_deref().ok_or(AuthError::InvalidToken)?,
            )?;
            headers.insert(
                "user-agent",
                HeaderValue::from_str(&crate::antigravity::user_agent())
                    .map_err(|_| AuthError::InvalidToken)?,
            );
        }

        if self.kind == CredentialKind::GoogleOAuth {
            crate::google::validate_project(
                self.project_id.as_deref().ok_or(AuthError::InvalidToken)?,
            )?;
            headers.insert(
                "client-metadata",
                HeaderValue::from_static(
                    "ideType=IDE_UNSPECIFIED,platform=PLATFORM_UNSPECIFIED,pluginType=GEMINI",
                ),
            );
        }

        Ok(RequestAuth {
            kind: self.kind,
            headers,
            project_id: self.project_id.clone(),
        })
    }
}

pub(crate) fn endpoint_scope(value: &str) -> Result<Url> {
    let mut url = Url::parse(value).map_err(|_| AuthError::Configuration("invalid base URL"))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(AuthError::Configuration(
            "base URL must be an HTTP(S) service address",
        ));
    }

    let path = format!("{}/", url.path().trim_end_matches('/'));
    url.set_path(&path);
    Ok(url)
}

pub(crate) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oauth_is_bound_to_anthropic_messages_even_if_storage_is_edited() {
        let mut credential = Credential {
            version: 1,
            kind: CredentialKind::AnthropicOAuth,
            endpoint: "https://api.anthropic.com/v1/".into(),
            access: Secret("access-secret".into()),
            refresh: Some(Secret("refresh-secret".into())),
            expires_at: Some(now() + 3600),
            account: None,
            account_id: None,
            project_id: None,
        };
        let endpoint = Url::parse("https://api.anthropic.com/v1/messages").unwrap();

        let auth = credential
            .request_auth(&endpoint, AuthTarget::Anthropic)
            .unwrap();

        assert_eq!(auth.headers["authorization"], "Bearer access-secret");
        assert_eq!(auth.headers["anthropic-beta"], "oauth-2025-04-20");
        assert!(!auth.headers.contains_key("x-api-key"));
        assert!(!format!("{auth:?}").contains("access-secret"));
        assert!(matches!(
            credential.request_auth(&endpoint, AuthTarget::Openai),
            Err(AuthError::Scope)
        ));

        credential.endpoint = "https://proxy.example/v1/".into();
        let proxy = Url::parse("https://proxy.example/v1/messages").unwrap();

        assert!(matches!(
            credential.request_auth(&proxy, AuthTarget::Anthropic),
            Err(AuthError::Scope)
        ));
    }
}
