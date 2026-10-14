//! Account setup coordinates authentication and local connection configuration.
use anyhow::Result;
pub use orbit_auth::{
    ApiKeyProvider, AuthError, CredentialInfo, LoginOutcome, OAuthProvider, PendingGoogleLogin,
};
use orbit_auth::{AuthStore, CallbackFlow, CallbackListener, CredentialKind, OAuthLogin};
use orbit_config::{
    config::connections::edit::{self, ConnectionDraft},
    models::ModelOption,
};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

const AUTHORIZATION_TIMEOUT: Duration = Duration::from_secs(300);

pub struct Connection {
    pub draft: ConnectionDraft,
    pub account: Option<String>,
    pub oauth: Option<OAuthProvider>,
}

/// Read connection drafts and credential metadata without returning secrets.
pub fn connections(config: &Path, auth: &Path) -> anyhow::Result<Vec<Connection>> {
    let credentials = AuthStore::new(auth)?.list()?;
    edit::saved_connections(config)?
        .into_iter()
        .map(|name| {
            let draft = ConnectionDraft::load(config, Some(&name))?;
            let credential = draft
                .provider
                .as_ref()
                .and_then(|provider| {
                    credentials
                        .iter()
                        .find(|entry| Some(&entry.name) == provider.credential.as_ref())
                });
            let oauth = credential.and_then(|entry| match entry.kind {
                CredentialKind::ApiKey => None,
                CredentialKind::AnthropicOAuth => Some(OAuthProvider::Anthropic),
                CredentialKind::GoogleOAuth => Some(OAuthProvider::Google),
                CredentialKind::AntigravityOAuth => Some(OAuthProvider::Antigravity),
                CredentialKind::ChatgptOAuth => Some(OAuthProvider::Chatgpt),
            });
            Ok(Connection {
                draft,
                account: credential.and_then(|entry| entry.account.clone()),
                oauth,
            })
        })
        .collect()
}

/// Read account metadata for connection status; credential secrets remain in the store.
pub fn credentials(auth: &Path) -> Result<Vec<CredentialInfo>> {
    Ok(AuthStore::new(auth)?.list()?)
}

/// A callback listener bound before the browser is opened. Dropping cancels this login.
pub struct Login {
    login: OAuthLogin,
    listener: CallbackListener,
    auth: PathBuf,
    provider: OAuthProvider,
    credential: Option<String>,
    url: String,
}

impl Login {
    /// Prepare one OAuth attempt without opening platform UI or changing configuration.
    pub async fn start(
        provider: OAuthProvider,
        project: Option<String>,
        auth: &Path,
        credential: Option<String>,
    ) -> Result<Self> {
        let login = OAuthLogin::new(provider, project)?;
        let listener = CallbackListener::bind_at(login.redirect_uri().clone()).await?;
        let url = login.authorization_url().to_string();
        Ok(Self {
            login,
            listener,
            auth: auth.to_owned(),
            provider,
            credential,
            url,
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// Wait for authorization, retaining a Google grant when a project still needs selection.
    pub async fn finish(self) -> Result<LoginOutcome> {
        let grant = self
            .listener
            .wait(&self.login, AUTHORIZATION_TIMEOUT)
            .await?;
        let store = AuthStore::new(&self.auth)?;
        Ok(self
            .login
            .finish_interactive(
                &grant,
                &store,
                self.credential
                    .as_deref()
                    .unwrap_or(self.provider.name()),
            )
            .await?)
    }

    /// Complete a login whose caller already supplied all required project information.
    pub async fn finish_required(self, config: &Path) -> Result<Vec<ModelOption>> {
        let provider = self.provider;
        let credential = self.credential.clone();
        let auth = self.auth.clone();
        match self.finish().await? {
            LoginOutcome::Connected => {
                save_login(config, &auth, provider, credential.as_deref()).await
            }
            LoginOutcome::GoogleProjectRequired(_) => Err(AuthError::GoogleProjectRequired.into()),
        }
    }
}

/// Persist a newly authenticated connection, or refresh an existing credential's model catalog.
pub async fn save_login(
    config: &Path,
    auth: &Path,
    provider: OAuthProvider,
    credential: Option<&str>,
) -> Result<Vec<ModelOption>> {
    if credential.is_none() {
        return orbit_config::config::connections::save_account(config, auth, provider).await;
    }
    let mut updated = orbit_config::config::FileConfig::load(config, auth)?;
    let diagnostics = orbit_config::models::updater::refresh(&mut updated, true)
        .await
        .diagnostics;
    anyhow::ensure!(diagnostics.is_empty(), "{}", diagnostics.join("\n"));
    updated.model_options()
}

/// Retry project provisioning with the same grant; failures leave it available to the caller.
pub async fn finish_project(
    pending: &mut PendingGoogleLogin,
    project: &str,
    auth: &Path,
) -> Result<()> {
    let store = AuthStore::new(auth)?;
    Ok(pending
        .finish(project, &store, OAuthProvider::Google.name())
        .await?)
}
