//! Dynamic OAuth registration and returning-account authorization.
use super::{RESOURCE, Registration, TOKEN_URL, credential, identity};
use crate::{
    AuthError, AuthStore, CredentialKind, Result,
    credential::Credential,
    http::{Tokens, form_token},
    oauth::pkce::Pkce,
};
use url::Url;

const REGISTRATION_CLIENT: &str = "dynamic_agent_client";
const REQUESTED_SCOPES: &str =
    "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";

pub(crate) struct ChatgptLogin {
    host: String,
    nonce: String,
    previous: Option<Registration>,
}

impl ChatgptLogin {
    pub async fn new(store: &AuthStore, existing: Option<&str>) -> Result<Self> {
        let previous = match existing {
            Some(name) => {
                let snapshot = store.snapshot(name).await?;
                match snapshot.0.kind {
                    CredentialKind::ChatgptPlanOAuth => Some(
                        *snapshot
                            .0
                            .chatgpt
                            .ok_or(AuthError::InvalidToken)?,
                    ),
                    CredentialKind::ChatgptOAuth => None,
                    _ => return Err(AuthError::Scope),
                }
            }
            None => None,
        };
        Ok(Self {
            host: super::host::identifier(store).await?,
            nonce: Pkce::random()?,
            previous,
        })
    }

    pub fn authorization_url(&self, grant: &Pkce) -> Url {
        let client = self
            .previous
            .as_ref()
            .map_or(REGISTRATION_CLIENT, |value| &value.client_id);
        let mut url = grant.authorization_url(
            "https://auth.openai.com/api/accounts/authorize",
            client,
            REQUESTED_SCOPES,
        );
        url.query_pairs_mut().extend_pairs([
            ("ext_agent_host_id", self.host.as_str()),
            ("nonce", self.nonce.as_str()),
            ("resource", RESOURCE),
        ]);
        if self.previous.is_none() {
            url.query_pairs_mut()
                .append_pair("agent_name_hint", "Orbit");
        }
        url
    }

    fn client_id(&self, callback: &str) -> Result<String> {
        const MAX_CLIENT_ID_BYTES: usize = 256;
        let url = Url::parse(callback).map_err(|_| AuthError::InvalidCallback)?;
        let clients = url
            .query_pairs()
            .filter(|(key, _)| key == "client_id")
            .map(|(_, value)| value.into_owned())
            .collect::<Vec<_>>();
        if clients.len() > 1 {
            return Err(AuthError::InvalidCallback);
        }
        let client = match (&self.previous, clients.first()) {
            (Some(previous), None) => previous.client_id.as_str(),
            (Some(previous), Some(client)) if client == &previous.client_id => client,
            (None, Some(client)) if !client.is_empty() && client != REGISTRATION_CLIENT => client,
            _ => return Err(AuthError::InvalidCallback),
        };
        if client.len() > MAX_CLIENT_ID_BYTES || client.chars().any(char::is_control) {
            return Err(AuthError::InvalidCallback);
        }
        Ok(client.to_owned())
    }

    pub fn callback_code(&self, grant: &Pkce, callback: &str) -> Result<String> {
        let code = grant.callback_code(callback)?;
        self.client_id(callback)?;
        Ok(code)
    }

    pub async fn finish(self, grant: &Pkce, callback: &str) -> Result<Credential> {
        let code = self.callback_code(grant, callback)?;
        let client_id = self.client_id(callback)?;
        let payload = form_token(
            TOKEN_URL,
            &[
                ("grant_type", "authorization_code"),
                ("client_id", &client_id),
                ("code", &code),
                ("code_verifier", &grant.verifier.0),
                ("redirect_uri", grant.redirect.as_str()),
                ("resource", RESOURCE),
            ],
        )
        .await?;
        let scopes = payload["scope"]
            .as_str()
            .ok_or(AuthError::InvalidToken)?
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        let tokens: Tokens =
            serde_json::from_value(payload).map_err(|_| AuthError::InvalidToken)?;
        let id_token = tokens
            .id_token
            .clone()
            .ok_or(AuthError::InvalidToken)?;
        let identity = identity::verify(&id_token.0, &client_id, Some(&self.nonce)).await?;
        if self
            .previous
            .as_ref()
            .is_some_and(|previous| previous.subject != identity.sub)
        {
            return Err(AuthError::LoginRequired);
        }
        credential(
            Registration {
                client_id,
                subject: identity.sub,
                id_token,
                scopes,
            },
            identity.email,
            tokens,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credential::Secret;

    fn returned_registration() -> Registration {
        Registration {
            client_id: "issued-fixture-client".into(),
            subject: "generated-subject".into(),
            id_token: Secret("retained-identity-token".into()),
            scopes: vec![super::super::PLAN_SCOPE.into()],
        }
    }

    #[tokio::test]
    async fn host_is_stable_but_each_attempt_has_fresh_nonce_and_pkce() {
        let directory = tempfile::tempdir().unwrap();
        let store = AuthStore::new(directory.path().join("auth")).unwrap();
        let first = ChatgptLogin::new(&store, None)
            .await
            .unwrap();
        let second = ChatgptLogin::new(&store, None)
            .await
            .unwrap();
        assert_eq!(first.host, second.host);
        assert_ne!(first.nonce, second.nonce);
        assert!(store.list().unwrap().is_empty());
        let redirect = Url::parse("http://127.0.0.1:54321/auth/callback").unwrap();
        let grant = Pkce::new(redirect.clone()).unwrap();
        let prefix = format!("{redirect}?code=generated-code&state={}", grant.state);
        for suffix in [
            "",
            "&client_id=dynamic_agent_client",
            "&client_id=a&client_id=b",
        ] {
            assert!(
                first
                    .callback_code(&grant, &format!("{prefix}{suffix}"))
                    .is_err()
            );
        }
        assert_eq!(
            first
                .callback_code(&grant, &format!("{prefix}&client_id=issued-fixture-client"))
                .unwrap(),
            "generated-code"
        );
        let returning = ChatgptLogin {
            host: first.host,
            nonce: first.nonce,
            previous: Some(returned_registration()),
        };
        assert!(
            returning
                .callback_code(&grant, &prefix)
                .is_ok()
        );
        assert!(
            returning
                .callback_code(&grant, &format!("{prefix}&client_id=other-client"))
                .is_err()
        );
        assert!(
            returning
                .previous
                .unwrap()
                .authorize()
                .is_ok()
        );
        let mut disabled = returned_registration();
        disabled.scopes.clear();
        assert!(matches!(
            disabled.authorize(),
            Err(AuthError::PlanUsageDisabled)
        ));
    }
}
