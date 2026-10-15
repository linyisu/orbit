//! Account setup coordinates authentication and local connection configuration.
use anyhow::Result;
pub use orbit_auth::{ApiKeyProvider, AuthError, CredentialInfo, OAuthProvider};
use orbit_auth::{AuthStore, CallbackListener, OAuthLogin};
pub use orbit_config::config::connections::SaveOutcome;
use orbit_config::config::connections::edit::{self, ConnectionDraft};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

const AUTHORIZATION_TIMEOUT: Duration = Duration::from_secs(300);

/// Verify key access and fetch live model choices without writing credentials.
pub async fn verify_api_key(provider: ApiKeyProvider, key: &str) -> Result<Vec<String>> {
    Ok(orbit_model::discovery::verify_api_key(provider, key).await?)
}

/// Verify and persist one official API connection using its live model catalog as evidence.
pub async fn connect_api_key(
    config: &Path,
    auth: &Path,
    provider: ApiKeyProvider,
    secret: String,
) -> Result<SaveOutcome> {
    let ids = verify_api_key(provider, &secret).await?;
    orbit_config::config::connections::save_api_key(config, auth, provider, secret, ids).await
}

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
            let oauth = credential.and_then(|entry| entry.kind.oauth_provider());
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
    destination: AccountDestination,
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
        let listener = CallbackListener::bind_at(provider.redirect_uri()).await?;
        let store = AuthStore::new(auth)?;
        let entries = store.list()?;
        let destination = if let Some(name) = credential {
            let info = entries
                .iter()
                .find(|entry| entry.name == name)
                .ok_or(AuthError::Missing)?;
            if info.kind.oauth_provider() != Some(provider) {
                return Err(AuthError::Scope.into());
            }
            AccountDestination::Existing(name)
        } else {
            let mut name = provider.name().to_owned();
            let mut suffix = 2;
            while entries
                .iter()
                .any(|entry| entry.name == name)
            {
                name = format!("{}-{suffix}", provider.name());
                suffix += 1;
            }
            AccountDestination::New(name)
        };
        let login = OAuthLogin::new(
            provider,
            project,
            &store,
            destination.existing(),
            listener.redirect_uri(),
        )
        .await?;
        let url = login.authorization_url().to_string();
        Ok(Self {
            login,
            listener,
            auth: auth.to_owned(),
            provider,
            destination,
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
            destination: self.destination,
        };
        let store = AuthStore::new(&identity.auth)?;
        match self
            .login
            .finish_interactive(&grant, &store, identity.destination.name())
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
    destination: AccountDestination,
}

#[derive(Clone)]
enum AccountDestination {
    New(String),
    Existing(String),
}

impl AccountDestination {
    fn name(&self) -> &str {
        match self {
            Self::New(name) | Self::Existing(name) => name,
        }
    }

    fn existing(&self) -> Option<&str> {
        match self {
            Self::New(_) => None,
            Self::Existing(name) => Some(name),
        }
    }
}

impl AccountIdentity {
    /// Persist the authenticated connection and report refresh problems separately from saving.
    pub async fn save(&self, config: &Path) -> Result<SaveOutcome> {
        if let AccountDestination::New(name) = &self.destination {
            return orbit_config::config::connections::save_account(
                config,
                &self.auth,
                self.provider,
                name,
            )
            .await;
        }
        orbit_config::config::connections::refresh_account(
            config,
            &self.auth,
            self.destination.name(),
        )
        .await
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
            .finish(project, &store, self.identity.destination.name())
            .await?;
        Ok(self.identity.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn missing_account_is_not_reported_as_successful_configuration_save() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("missing.toml");
        let identity = AccountIdentity {
            auth: directory.path().join("auth"),
            provider: OAuthProvider::Google,
            destination: AccountDestination::Existing("secondary-workspace".into()),
        };

        assert!(identity.save(&config).await.is_err());
        assert!(!config.exists());
        assert!(!identity.auth.exists());
    }
}
