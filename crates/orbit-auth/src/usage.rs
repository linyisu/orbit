//! Narrow authorization for fixed account-usage endpoints. Model scopes remain
//! unchanged; these grants never accept a caller-supplied destination.
use crate::{AuthTarget, CredentialKind, RequestAuth, Result, credential::endpoint_scope};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsageProvider {
    Chatgpt,
    Claude,
    Gemini,
    Antigravity,
    Deepseek,
    Zhipu,
}

impl UsageProvider {
    pub fn endpoint(self) -> &'static str {
        match self {
            Self::Chatgpt => "https://chatgpt.com/backend-api/wham/usage",
            Self::Claude => "https://api.anthropic.com/api/oauth/usage",
            Self::Gemini => "https://cloudcode-pa.googleapis.com/v1internal:retrieveUserQuota",
            Self::Antigravity => {
                "https://daily-cloudcode-pa.googleapis.com/v1internal:retrieveUserQuotaSummary"
            }
            Self::Deepseek => "https://api.deepseek.com/user/balance",
            Self::Zhipu => "https://open.bigmodel.cn/api/monitor/usage/quota/limit",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Chatgpt => "ChatGPT",
            Self::Claude => "Claude",
            Self::Gemini => "Gemini CLI",
            Self::Antigravity => "Antigravity",
            Self::Deepseek => "DeepSeek",
            Self::Zhipu => "智谱 Coding Plan",
        }
    }
    pub(crate) fn select(kind: CredentialKind, base: &str) -> Result<Option<(Self, AuthTarget)>> {
        let base = endpoint_scope(base)?;
        Ok(match kind {
            CredentialKind::ChatgptOAuth => Some((Self::Chatgpt, AuthTarget::ChatgptCodex)),
            CredentialKind::AnthropicOAuth => Some((Self::Claude, AuthTarget::Anthropic)),
            CredentialKind::GoogleOAuth => Some((Self::Gemini, AuthTarget::GoogleCodeAssist)),
            CredentialKind::AntigravityOAuth => {
                Some((Self::Antigravity, AuthTarget::GoogleCodeAssist))
            }
            CredentialKind::ApiKey if base.as_str() == crate::DEEPSEEK_BASE => {
                Some((Self::Deepseek, AuthTarget::Openai))
            }
            CredentialKind::ApiKey if base.as_str() == crate::ZHIPU_CODING_BASE => {
                Some((Self::Zhipu, AuthTarget::Openai))
            }
            CredentialKind::ApiKey => None,
        })
    }
}

pub struct UsageAuthorization {
    pub provider: UsageProvider,
    pub account: String,
    pub auth: RequestAuth,
}

pub fn usage_api_key(base: &str, key: &str) -> Result<Option<UsageAuthorization>> {
    let Some((provider, target)) = UsageProvider::select(CredentialKind::ApiKey, base)? else {
        return Ok(None);
    };
    let credential = crate::credential::Credential {
        version: 1,
        kind: CredentialKind::ApiKey,
        endpoint: endpoint_scope(base)?.to_string(),
        access: crate::credential::Secret(key.into()),
        refresh: None,
        expires_at: None,
        account: None,
        account_id: None,
        project_id: None,
    };
    Ok(Some(UsageAuthorization {
        provider,
        account: "配置的 API Key".into(),
        auth: credential.request_auth(&endpoint_scope(base)?, target)?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn custom_and_lookalike_endpoints_do_not_receive_account_requests() {
        for base in [
            "https://api.deepseek.com.evil/v1",
            "https://api.deepseek.com/proxy/v1",
            "http://api.deepseek.com/v1",
            "https://example.com/v1",
        ] {
            assert!(usage_api_key(base, "secret").unwrap().is_none());
        }
        let auth = usage_api_key("https://api.deepseek.com/v1", "secret")
            .unwrap()
            .unwrap();
        assert_eq!(auth.provider, UsageProvider::Deepseek);
        assert_eq!(auth.auth.headers["authorization"], "Bearer secret");
    }

    #[tokio::test]
    async fn usage_authorization_preserves_oauth_headers_without_widening_model_scope() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::AuthStore::new(root.path().join("auth")).unwrap();
        for (kind, base, target) in [
            (
                CredentialKind::ChatgptOAuth,
                "https://chatgpt.com/backend-api/codex/",
                AuthTarget::ChatgptCodex,
            ),
            (
                CredentialKind::AnthropicOAuth,
                "https://api.anthropic.com/v1/",
                AuthTarget::Anthropic,
            ),
        ] {
            let credential = crate::credential::Credential {
                version: 1,
                kind,
                endpoint: base.into(),
                access: crate::credential::Secret("fake-token".into()),
                refresh: None,
                expires_at: Some(u64::MAX),
                account: Some("test-account".into()),
                account_id: Some("test-id".into()),
                project_id: None,
            };
            store.save("test", &credential).await.unwrap();
            let usage = store.resolve_usage("test").await.unwrap().unwrap();
            assert_eq!(usage.auth.headers["authorization"], "Bearer fake-token");
            if kind == CredentialKind::ChatgptOAuth {
                assert_eq!(usage.auth.headers["chatgpt-account-id"], "test-id");
            }
            assert!(
                credential
                    .request_auth(&url::Url::parse(usage.provider.endpoint()).unwrap(), target)
                    .is_err()
            );
            let mut forged = credential;
            forged.endpoint = "https://example.com/v1/".into();
            store.save("test", &forged).await.unwrap();
            assert!(store.resolve_usage("test").await.is_err());
        }
    }
}
