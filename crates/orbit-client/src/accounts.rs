//! Account setup coordinates authentication and local connection configuration.
use anyhow::Result;
pub use orbit_auth::{ApiKeyProvider, AuthError, CredentialInfo, OAuthProvider};
use orbit_auth::{AuthStore, CallbackFlow, CallbackListener, CredentialKind, OAuthLogin};
pub use orbit_config::config::connections::SaveOutcome;
use orbit_config::config::connections::edit::{self, ConnectionDraft};
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

/// Read connection drafts and account metadata without loading credential-store secrets.
pub fn connections(config: &Path, auth: &Path) -> anyhow::Result<Vec<Connection>> {
    let credentials = AuthStore::new(auth)?.list()?;
    edit::saved_connections(config)?
        .into_iter()
        .map(|name| {
            let draft = ConnectionDraft::load(config, Some(&name))?;
            let credential = draft.provider().and_then(|provider| {
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

    /// Wait for authorization, retaining both the grant and account identity across project setup.
    pub async fn finish(self) -> Result<LoginOutcome> {
        let grant = self
            .listener
            .wait(&self.login, AUTHORIZATION_TIMEOUT)
            .await?;
        let identity = AccountIdentity {
            auth: self.auth,
            provider: self.provider,
            credential: self
                .credential
                .clone()
                .unwrap_or_else(|| self.provider.name().into()),
            existing: self.credential.is_some(),
        };
        let store = AuthStore::new(&identity.auth)?;
        match self
            .login
            .finish_interactive(&grant, &store, &identity.credential)
            .await?
        {
            orbit_auth::LoginOutcome::Connected => Ok(LoginOutcome::Connected(identity)),
            orbit_auth::LoginOutcome::GoogleProjectRequired(pending) => {
                Ok(LoginOutcome::GoogleProjectRequired(PendingAccountSetup {
                    pending,
                    identity,
                }))
            }
        }
    }

    /// Complete a login whose caller already supplied all required project information.
    pub async fn finish_required(self, config: &Path) -> Result<SaveOutcome> {
        match self.finish().await? {
            LoginOutcome::Connected(identity) => identity.save(config).await,
            LoginOutcome::GoogleProjectRequired(_) => Err(AuthError::GoogleProjectRequired.into()),
        }
    }
}

/// A completed grant retains its destination so frontends never reconstruct account identity.
pub enum LoginOutcome {
    Connected(AccountIdentity),
    GoogleProjectRequired(PendingAccountSetup),
}

#[derive(Clone)]
pub struct AccountIdentity {
    auth: PathBuf,
    provider: OAuthProvider,
    credential: String,
    existing: bool,
}

impl AccountIdentity {
    /// Persist the authenticated connection and report refresh problems separately from saving.
    pub async fn save(&self, config: &Path) -> Result<SaveOutcome> {
        if !self.existing {
            return orbit_config::config::connections::save_account(
                config,
                &self.auth,
                self.provider,
            )
            .await;
        }
        Ok(orbit_config::config::connections::refresh_saved(config, &self.auth, Vec::new()).await)
    }
}

/// A resumable account setup owns the original credential destination and authorization grant.
pub struct PendingAccountSetup {
    pending: orbit_auth::PendingGoogleLogin,
    identity: AccountIdentity,
}

impl PendingAccountSetup {
    /// Retry provisioning without changing the credential name or requesting another grant.
    pub async fn finish(&mut self, project: &str) -> Result<AccountIdentity> {
        let store = AuthStore::new(&self.identity.auth)?;
        self.pending
            .finish(project, &store, &self.identity.credential)
            .await?;
        Ok(self.identity.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn existing_account_refresh_failure_keeps_login_successful() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("missing.toml");
        let identity = AccountIdentity {
            auth: directory.path().join("auth"),
            provider: OAuthProvider::Google,
            credential: "secondary-workspace".into(),
            existing: true,
        };

        let outcome = identity.save(&config).await.unwrap();

        assert!(outcome.models.is_empty());
        assert_eq!(outcome.diagnostics.len(), 1);
        assert!(!config.exists());
        assert!(!identity.auth.exists());
    }
}
