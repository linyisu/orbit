use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};
use url::Url;

use crate::{AnthropicLogin, AuthError, AuthStore, Result, credential::Secret};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OAuthProvider {
    Anthropic,
    Google,
    Antigravity,
    Chatgpt,
}

impl OAuthProvider {
    pub fn name(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::Google => "google",
            Self::Antigravity => "antigravity",
            Self::Chatgpt => "chatgpt",
        }
    }

    pub fn redirect_uri(self) -> Url {
        let value = match self {
            Self::Anthropic => "http://localhost:54545/callback",
            Self::Google => "http://127.0.0.1:8085/oauth2callback",
            Self::Antigravity => "http://127.0.0.1:51121/oauth-callback",
            Self::Chatgpt => "http://localhost:1455/auth/callback",
        };
        Url::parse(value).expect("constant OAuth redirect")
    }
}

/// Shared callback contract. The listener validates state before accepting a grant.
pub trait CallbackFlow {
    fn redirect_uri(&self) -> &Url;
    fn callback_code(&self, callback: &str) -> Result<String>;
}

impl CallbackFlow for AnthropicLogin {
    fn redirect_uri(&self) -> &Url {
        self.redirect_uri()
    }

    fn callback_code(&self, callback: &str) -> Result<String> {
        self.callback_code(callback)
    }
}

/// A single browser login. Project selection is supplied explicitly by the host.
pub struct OAuthLogin {
    provider: OAuthProvider,
    redirect: Url,
    state: String,
    verifier: Secret,
    project: Option<String>,
    anthropic: Option<AnthropicLogin>,
}

impl OAuthLogin {
    pub fn new(provider: OAuthProvider, project: Option<String>) -> Result<Self> {
        if project.is_some() && provider != OAuthProvider::Google {
            return Err(AuthError::Configuration(
                "project is only supported by Google login",
            ));
        }
        if let Some(project) = &project {
            crate::google::validate_project(project)?;
        }
        let redirect = provider.redirect_uri();
        let mut random = [0; 64];
        getrandom::fill(&mut random)
            .map_err(|_| AuthError::Configuration("secure randomness unavailable"))?;
        let anthropic = if provider == OAuthProvider::Anthropic {
            Some(AnthropicLogin::new(redirect.clone())?)
        } else {
            None
        };

        Ok(Self {
            provider,
            redirect,
            state: URL_SAFE_NO_PAD.encode(&random[..32]),
            verifier: Secret(URL_SAFE_NO_PAD.encode(&random[32..])),
            project,
            anthropic,
        })
    }

    pub fn authorization_url(&self) -> Url {
        if let Some(login) = &self.anthropic {
            return login.authorization_url();
        }
        let (authorize, client, scope) = match self.provider {
            OAuthProvider::Antigravity => (
                "https://accounts.google.com/o/oauth2/v2/auth",
                crate::antigravity::CLIENT_ID,
                "https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/userinfo.email https://www.googleapis.com/auth/userinfo.profile https://www.googleapis.com/auth/cclog https://www.googleapis.com/auth/experimentsandconfigs",
            ),
            OAuthProvider::Google => (
                "https://accounts.google.com/o/oauth2/v2/auth",
                crate::google::CLIENT_ID,
                "https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/userinfo.email https://www.googleapis.com/auth/userinfo.profile",
            ),
            OAuthProvider::Chatgpt => (
                "https://auth.openai.com/oauth/authorize",
                crate::chatgpt::CLIENT_ID,
                "openid profile email offline_access",
            ),
            OAuthProvider::Anthropic => unreachable!("Anthropic has its own login flow"),
        };
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(self.verifier.0.as_bytes()));
        let mut url = Url::parse(authorize).expect("constant authorization URL");
        url.query_pairs_mut().extend_pairs([
            ("response_type", "code"),
            ("client_id", client),
            ("redirect_uri", self.redirect.as_str()),
            ("scope", scope),
            ("state", self.state.as_str()),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
        ]);
        match self.provider {
            OAuthProvider::Google | OAuthProvider::Antigravity => {
                url.query_pairs_mut()
                    .extend_pairs([("access_type", "offline"), ("prompt", "consent")]);
            }
            OAuthProvider::Chatgpt => {
                url.query_pairs_mut().extend_pairs([
                    ("id_token_add_organizations", "true"),
                    ("codex_cli_simplified_flow", "true"),
                    ("originator", "orbit"),
                ]);
            }
            OAuthProvider::Anthropic => unreachable!(),
        }
        url
    }

    pub async fn finish(self, callback: &str, store: &AuthStore, name: &str) -> Result<()> {
        match self.finish_interactive(callback, store, name).await? {
            LoginOutcome::Connected => Ok(()),
            LoginOutcome::GoogleProjectRequired(_) => Err(AuthError::GoogleProjectRequired),
        }
    }

    /// Keep a successful OAuth grant in memory while the host resolves account setup.
    pub async fn finish_interactive(
        self,
        callback: &str,
        store: &AuthStore,
        name: &str,
    ) -> Result<LoginOutcome> {
        crate::store::validate_name(name)?;
        let code = self.callback_code(callback)?;

        if let Some(login) = self.anthropic {
            login.finish(callback, store, name).await?;
            return Ok(LoginOutcome::Connected);
        }

        let credential = match self.provider {
            OAuthProvider::Antigravity => {
                crate::antigravity::exchange(&code, &self.verifier.0, &self.redirect).await?
            }
            OAuthProvider::Google => {
                let mut credential =
                    crate::google::exchange(&code, &self.verifier.0, &self.redirect).await?;
                match crate::google::provision(&mut credential, self.project.as_deref()).await {
                    Ok(()) => credential,
                    Err(AuthError::GoogleProjectRequired) => {
                        return Ok(LoginOutcome::GoogleProjectRequired(PendingGoogleLogin {
                            credential,
                        }));
                    }
                    Err(error) => return Err(error),
                }
            }
            OAuthProvider::Chatgpt => {
                crate::chatgpt::exchange(&code, &self.verifier.0, &self.redirect).await?
            }
            OAuthProvider::Anthropic => unreachable!(),
        };

        store.save(name, &credential).await?;
        Ok(LoginOutcome::Connected)
    }
}

/// An unfinished Google account setup is never written as a usable credential.
pub enum LoginOutcome {
    Connected,
    GoogleProjectRequired(PendingGoogleLogin),
}

pub struct PendingGoogleLogin {
    credential: crate::credential::Credential,
}

impl PendingGoogleLogin {
    /// On failure the grant stays available for another project selection.
    pub async fn finish(&mut self, project: &str, store: &AuthStore, name: &str) -> Result<()> {
        crate::store::validate_name(name)?;
        crate::google::validate_project(project)?;
        crate::google::provision(&mut self.credential, Some(project)).await?;
        store.save(name, &self.credential).await
    }
}

impl CallbackFlow for OAuthLogin {
    fn redirect_uri(&self) -> &Url {
        &self.redirect
    }

    fn callback_code(&self, callback: &str) -> Result<String> {
        if let Some(login) = &self.anthropic {
            return login.callback_code(callback);
        }
        crate::callback::authorization_code(callback, &self.redirect, &self.state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn antigravity_authorization_parameters_match_omp_18_2_8() {
        let login = OAuthLogin::new(OAuthProvider::Antigravity, None).unwrap();
        let url = login.authorization_url();
        let pairs = url
            .query_pairs()
            .collect::<std::collections::HashMap<_, _>>();

        assert_eq!(
            url.origin().ascii_serialization(),
            "https://accounts.google.com"
        );
        assert_eq!(url.path(), "/o/oauth2/v2/auth");
        assert_eq!(
            pairs["redirect_uri"],
            "http://127.0.0.1:51121/oauth-callback"
        );
        assert_eq!(
            pairs["client_id"],
            "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com"
        );
        assert_eq!(
            pairs["scope"],
            "https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/userinfo.email https://www.googleapis.com/auth/userinfo.profile https://www.googleapis.com/auth/cclog https://www.googleapis.com/auth/experimentsandconfigs"
        );
        assert_eq!(pairs["access_type"], "offline");
        assert_eq!(pairs["prompt"], "consent");
        assert_eq!(pairs["response_type"], "code");
        assert_eq!(pairs["code_challenge_method"], "S256");
    }

    #[test]
    fn account_flows_use_bound_pkce_callbacks_and_provider_specific_parameters() {
        for provider in [
            OAuthProvider::Google,
            OAuthProvider::Antigravity,
            OAuthProvider::Chatgpt,
        ] {
            let login = OAuthLogin::new(provider, None).unwrap();
            let url = login.authorization_url();
            let pairs = url
                .query_pairs()
                .collect::<std::collections::HashMap<_, _>>();

            assert_eq!(pairs["redirect_uri"], provider.redirect_uri().as_str());
            assert_eq!(pairs["code_challenge_method"], "S256");
            assert_eq!(
                pairs["code_challenge"],
                URL_SAFE_NO_PAD.encode(Sha256::digest(login.verifier.0.as_bytes()))
            );
            if matches!(provider, OAuthProvider::Google | OAuthProvider::Antigravity) {
                assert_eq!(pairs["access_type"], "offline");
                assert_eq!(pairs["prompt"], "consent");
            } else {
                assert_eq!(pairs["originator"], "orbit");
            }

            let callback = format!("{}?code=grant&state={}", login.redirect_uri(), login.state);

            assert_eq!(login.callback_code(&callback).unwrap(), "grant");
            assert!(
                login
                    .callback_code(&callback.replace(&login.state, "wrong"))
                    .is_err()
            );
            assert!(
                login
                    .callback_code(&format!("{callback}&state={}", login.state))
                    .is_err()
            );
            assert!(
                login
                    .callback_code(&format!("{callback}&code=other"))
                    .is_err()
            );
            assert!(
                OAuthLogin::new(provider, None)
                    .unwrap()
                    .callback_code(&callback)
                    .is_err()
            );
        }
    }

    #[tokio::test]
    async fn invalid_project_keeps_pending_grant_without_saving_credentials() {
        let directory = tempfile::tempdir().unwrap();
        let store = AuthStore::new(directory.path().join("auth")).unwrap();
        let mut pending = PendingGoogleLogin {
            credential: crate::credential::Credential {
                version: 1,
                kind: crate::CredentialKind::GoogleOAuth,
                endpoint: crate::google::API_BASE.into(),
                access: Secret("test-access".into()),
                refresh: Some(Secret("test-refresh".into())),
                expires_at: None,
                account: None,
                account_id: None,
                project_id: None,
            },
        };

        let result = pending.finish("../invalid", &store, "google").await;

        assert!(matches!(result, Err(AuthError::Configuration(_))));
        assert_eq!(pending.credential.access.0, "test-access");
        assert!(pending.credential.project_id.is_none());
        assert!(store.list().unwrap().is_empty());
        assert!(!store.directory().exists());
    }

    #[test]
    fn project_selection_is_explicit_and_google_only() {
        assert!(OAuthLogin::new(OAuthProvider::Chatgpt, Some("project".into())).is_err());
        assert!(OAuthLogin::new(OAuthProvider::Google, Some("../project".into())).is_err());
        assert!(OAuthLogin::new(OAuthProvider::Google, Some("test-project".into())).is_ok());
    }
}
