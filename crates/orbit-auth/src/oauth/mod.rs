//! Login orchestration and resumable setup; provider protocols live in `providers`.
pub(crate) mod callback;
pub(crate) mod pkce;
pub use callback::CallbackListener;

use crate::{AnthropicLogin, AuthError, AuthStore, Result, providers};
use pkce::Pkce;
use url::Url;

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

    pub fn endpoint(self) -> &'static str {
        match self {
            Self::Anthropic => providers::anthropic::API_BASE,
            Self::Google => providers::google::API_BASE,
            Self::Antigravity => providers::antigravity::API_BASE,
            Self::Chatgpt => providers::chatgpt::API_BASE,
        }
    }

    /// Loopback bind URI; port zero selects a free port before constructing authorization.
    pub fn redirect_uri(self) -> Url {
        let value = match self {
            Self::Anthropic => "http://localhost:54545/callback",
            Self::Google => "http://127.0.0.1:8085/oauth2callback",
            Self::Antigravity => "http://127.0.0.1:51121/oauth-callback",
            Self::Chatgpt => "http://127.0.0.1:0/auth/callback",
        };
        Url::parse(value).expect("constant OAuth redirect")
    }
}

/// Shared callback contract. Validate the bound state before accepting a grant or denial.
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

pub struct OAuthLogin {
    flow: LoginFlow,
    save: SaveMode,
}

#[derive(Clone, Copy)]
enum SaveMode {
    Create,
    Replace,
}

impl SaveMode {
    async fn persist(
        self,
        store: &AuthStore,
        name: &str,
        credential: &crate::credential::Credential,
    ) -> Result<()> {
        match self {
            Self::Create => store.save_new(name, credential).await,
            Self::Replace => store.save(name, credential).await,
        }
    }
}

enum LoginFlow {
    Anthropic(AnthropicLogin),
    Google {
        grant: Pkce,
        project: Option<String>,
    },
    Antigravity(Pkce),
    Chatgpt {
        grant: Pkce,
        login: providers::chatgpt::ChatgptLogin,
    },
}

impl OAuthLogin {
    /// Prepare authorization for the listener's exact URI without modifying active credentials.
    /// ChatGPT reuses a persisted host ID and the selected registration when available.
    pub async fn new(
        provider: OAuthProvider,
        project: Option<String>,
        store: &AuthStore,
        existing: Option<&str>,
        redirect: Url,
    ) -> Result<Self> {
        if project.is_some() && provider != OAuthProvider::Google {
            return Err(AuthError::Configuration(
                "project is only supported by Google login",
            ));
        }
        if let Some(project) = &project {
            providers::google::validate_project(project)?;
        }
        let expected = provider.redirect_uri();
        if redirect.scheme() != expected.scheme()
            || redirect.host_str() != expected.host_str()
            || redirect.path() != expected.path()
            || redirect.query().is_some()
            || redirect.fragment().is_some()
            || !redirect.username().is_empty()
            || redirect.password().is_some()
            || (provider != OAuthProvider::Chatgpt && redirect.port() != expected.port())
        {
            return Err(AuthError::InvalidCallback);
        }
        let flow = match provider {
            OAuthProvider::Anthropic => LoginFlow::Anthropic(AnthropicLogin::new(redirect)?),
            OAuthProvider::Google => LoginFlow::Google {
                grant: Pkce::new(redirect)?,
                project,
            },
            OAuthProvider::Antigravity => LoginFlow::Antigravity(Pkce::new(redirect)?),
            OAuthProvider::Chatgpt => LoginFlow::Chatgpt {
                grant: Pkce::new(redirect)?,
                login: providers::chatgpt::ChatgptLogin::new(store, existing).await?,
            },
        };
        Ok(Self {
            flow,
            save: if existing.is_some() {
                SaveMode::Replace
            } else {
                SaveMode::Create
            },
        })
    }

    fn grant(&self) -> Option<&Pkce> {
        match &self.flow {
            LoginFlow::Anthropic(_) => None,
            LoginFlow::Google { grant, .. }
            | LoginFlow::Antigravity(grant)
            | LoginFlow::Chatgpt { grant, .. } => Some(grant),
        }
    }

    pub fn authorization_url(&self) -> Url {
        match &self.flow {
            LoginFlow::Anthropic(login) => login.authorization_url(),
            LoginFlow::Google { grant, .. } => providers::google::authorization_url(grant),
            LoginFlow::Antigravity(grant) => providers::antigravity::authorization_url(grant),
            LoginFlow::Chatgpt { grant, login } => login.authorization_url(grant),
        }
    }

    pub async fn finish(self, callback: &str, store: &AuthStore, name: &str) -> Result<()> {
        match self
            .finish_interactive(callback, store, name)
            .await?
        {
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
        let credential = match self.flow {
            LoginFlow::Anthropic(login) => login.exchange_grant(callback).await?,
            LoginFlow::Antigravity(grant) => {
                providers::antigravity::exchange(&code, &grant.verifier.0, &grant.redirect).await?
            }
            LoginFlow::Google { grant, project } => {
                let mut credential =
                    providers::google::exchange(&code, &grant.verifier.0, &grant.redirect).await?;
                match providers::google::provision(&mut credential, project.as_deref()).await {
                    Ok(()) => credential,
                    Err(AuthError::GoogleProjectRequired) => {
                        return Ok(LoginOutcome::GoogleProjectRequired(PendingGoogleLogin {
                            credential: Box::new(credential),
                            save: self.save,
                        }));
                    }
                    Err(error) => return Err(error),
                }
            }
            LoginFlow::Chatgpt { grant, login } => login.finish(&grant, callback).await?,
        };
        self.save
            .persist(store, name, &credential)
            .await?;
        Ok(LoginOutcome::Connected)
    }
}

/// An unfinished Google account setup is never written as a usable credential.
pub enum LoginOutcome {
    Connected,
    GoogleProjectRequired(PendingGoogleLogin),
}

pub struct PendingGoogleLogin {
    credential: Box<crate::credential::Credential>,
    save: SaveMode,
}

impl PendingGoogleLogin {
    /// On failure the grant stays available for another project selection.
    pub async fn finish(&mut self, project: &str, store: &AuthStore, name: &str) -> Result<()> {
        crate::store::validate_name(name)?;
        providers::google::validate_project(project)?;
        providers::google::provision(&mut self.credential, Some(project)).await?;
        self.save
            .persist(store, name, &self.credential)
            .await
    }
}

impl CallbackFlow for OAuthLogin {
    fn redirect_uri(&self) -> &Url {
        match &self.flow {
            LoginFlow::Anthropic(login) => login.redirect_uri(),
            _ => {
                &self
                    .grant()
                    .expect("PKCE login")
                    .redirect
            }
        }
    }
    fn callback_code(&self, callback: &str) -> Result<String> {
        match &self.flow {
            LoginFlow::Anthropic(login) => login.callback_code(callback),
            LoginFlow::Chatgpt { grant, login } => login.callback_code(grant, callback),
            _ => self
                .grant()
                .expect("PKCE login")
                .callback_code(callback),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credential::Secret;
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use sha2::{Digest, Sha256};

    async fn new_login(provider: OAuthProvider, project: Option<String>) -> Result<OAuthLogin> {
        let directory = tempfile::tempdir().unwrap();
        let store = AuthStore::new(directory.path()).unwrap();
        OAuthLogin::new(provider, project, &store, None, provider.redirect_uri()).await
    }

    #[tokio::test]
    async fn antigravity_authorization_parameters_match_omp_18_2_8() {
        let login = new_login(OAuthProvider::Antigravity, None)
            .await
            .unwrap();
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
            "https://www.googleapis.com/auth/cloud-platform \
                https://www.googleapis.com/auth/userinfo.email \
                https://www.googleapis.com/auth/userinfo.profile \
                https://www.googleapis.com/auth/cclog https://www.googleapis.com/auth/experimentsa\
                ndconfigs"
        );
        assert_eq!(pairs["access_type"], "offline");
        assert_eq!(pairs["prompt"], "consent");
        assert_eq!(pairs["response_type"], "code");
        assert_eq!(pairs["code_challenge_method"], "S256");
    }

    #[tokio::test]
    async fn account_flows_use_bound_pkce_callbacks_and_provider_specific_parameters() {
        for provider in [
            OAuthProvider::Google,
            OAuthProvider::Antigravity,
            OAuthProvider::Chatgpt,
        ] {
            let login = new_login(provider, None).await.unwrap();
            let url = login.authorization_url();
            let pairs = url
                .query_pairs()
                .collect::<std::collections::HashMap<_, _>>();

            assert_eq!(pairs["redirect_uri"], provider.redirect_uri().as_str());
            assert_eq!(pairs["code_challenge_method"], "S256");
            assert_eq!(
                pairs["code_challenge"],
                URL_SAFE_NO_PAD.encode(Sha256::digest(
                    login
                        .grant()
                        .unwrap()
                        .verifier
                        .0
                        .as_bytes()
                ))
            );
            if matches!(provider, OAuthProvider::Google | OAuthProvider::Antigravity) {
                assert_eq!(pairs["access_type"], "offline");
                assert_eq!(pairs["prompt"], "consent");
            } else {
                assert_eq!(pairs["client_id"], "dynamic_agent_client");
                assert_eq!(pairs["resource"], "https://api.openai.com/v1");
                assert_eq!(pairs["agent_name_hint"], "Orbit");
            }

            let client = if provider == OAuthProvider::Chatgpt {
                "&client_id=oaiapp_fixture"
            } else {
                ""
            };
            let callback = format!(
                "{}?code=grant&state={}{client}",
                login.redirect_uri(),
                login.grant().unwrap().state
            );

            assert_eq!(login.callback_code(&callback).unwrap(), "grant");
            assert!(
                login
                    .callback_code(&callback.replace(&login.grant().unwrap().state, "wrong"))
                    .is_err()
            );
            assert!(
                login
                    .callback_code(&format!(
                        "{callback}&state={}",
                        login.grant().unwrap().state
                    ))
                    .is_err()
            );
            assert!(
                login
                    .callback_code(&format!("{callback}&code=other"))
                    .is_err()
            );
            assert!(
                new_login(provider, None)
                    .await
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
            save: SaveMode::Create,
            credential: Box::new(crate::credential::Credential {
                version: 1,
                kind: crate::CredentialKind::GoogleOAuth,
                endpoint: crate::providers::google::API_BASE.into(),
                access: Secret("test-access".into()),
                refresh: Some(Secret("test-refresh".into())),
                expires_at: None,
                account: None,
                account_id: None,
                project_id: None,
                chatgpt: None,
            }),
        };

        let result = pending
            .finish("../invalid", &store, "google")
            .await;

        assert!(matches!(result, Err(AuthError::Configuration(_))));
        assert_eq!(pending.credential.access.0, "test-access");
        assert!(pending.credential.project_id.is_none());
        assert!(store.list().unwrap().is_empty());
        assert!(!store.directory().exists());
    }

    #[tokio::test]
    async fn project_selection_is_explicit_and_google_only() {
        assert!(
            new_login(OAuthProvider::Chatgpt, Some("project".into()))
                .await
                .is_err()
        );
        assert!(
            new_login(OAuthProvider::Google, Some("../project".into()))
                .await
                .is_err()
        );
        assert!(
            new_login(OAuthProvider::Google, Some("test-project".into()))
                .await
                .is_ok()
        );
    }
}
