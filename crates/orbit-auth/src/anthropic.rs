use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::Client;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use url::Url;

use crate::{
    AuthError, AuthStore, CredentialKind, Result,
    credential::{Credential, Secret, now},
};

// Public OAuth application identity and grant shape from oh-my-pi's Anthropic rule.
const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
const TOKEN_URL: &str = "https://api.anthropic.com/v1/oauth/token";
const API_BASE: &str = "https://api.anthropic.com/v1/";
const SCOPES: &str = "org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";

/// One-use PKCE state; tokens and the verifier are never exposed in Debug output.
pub struct AnthropicLogin {
    redirect: Url,
    state: String,
    verifier: Secret,
    challenge: String,
}

impl AnthropicLogin {
    pub fn new(redirect: Url) -> Result<Self> {
        if redirect.scheme() != "http"
            || !matches!(redirect.host_str(), Some("localhost" | "127.0.0.1"))
            || redirect.path() != "/callback"
            || redirect.query().is_some()
            || redirect.fragment().is_some()
            || !redirect.username().is_empty()
            || redirect.password().is_some()
        {
            return Err(AuthError::Configuration(
                "OAuth redirect must be an HTTP loopback /callback URL",
            ));
        }
        let verifier = random(96)?;
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));

        Ok(Self {
            redirect,
            state: random(32)?,
            verifier: Secret(verifier),
            challenge,
        })
    }

    pub fn authorization_url(&self) -> Url {
        let mut url = Url::parse(AUTHORIZE_URL).expect("constant authorize URL");
        url.query_pairs_mut().extend_pairs([
            ("response_type", "code"),
            ("client_id", CLIENT_ID),
            ("redirect_uri", self.redirect.as_str()),
            ("scope", SCOPES),
            ("code_challenge", self.challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("state", self.state.as_str()),
            ("code", "true"),
        ]);
        url
    }

    pub fn redirect_uri(&self) -> &Url {
        &self.redirect
    }

    pub(crate) fn callback_code(&self, callback: &str) -> Result<String> {
        crate::callback::authorization_code(callback, &self.redirect, &self.state)
    }

    pub async fn finish(self, callback: &str, store: &AuthStore, name: &str) -> Result<()> {
        crate::store::validate_name(name)?;
        let code = self.callback_code(callback)?;
        let body = json!({
            "grant_type": "authorization_code",
            "client_id": CLIENT_ID,
            "code": code,
            "state": self.state,
            "code_verifier": self.verifier.0,
            "redirect_uri": self.redirect.as_str()
        });
        let credential = exchange(TOKEN_URL, body, None).await?;

        store.save(name, &credential).await
    }
}

pub(crate) async fn refresh(previous: &Credential) -> Result<Credential> {
    if previous.endpoint != API_BASE {
        return Err(AuthError::Scope);
    }
    let refresh = previous.refresh.as_ref().ok_or(AuthError::LoginRequired)?;
    let body = json!({
        "grant_type": "refresh_token",
        "client_id": CLIENT_ID,
        "refresh_token": refresh.0
    });

    exchange(TOKEN_URL, body, Some(previous)).await
}

async fn exchange(
    endpoint: &str,
    body: Value,
    previous: Option<&Credential>,
) -> Result<Credential> {
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(crate::http::AUTH_REQUEST_TIMEOUT)
        .build()
        .map_err(|_| AuthError::Transport)?;
    let mut response = client
        .post(endpoint)
        .header("anthropic-beta", "oauth-2025-04-20")
        .json(&body)
        .send()
        .await
        .map_err(|_| AuthError::Transport)?;
    let status = response.status();
    let mut bytes = Vec::new();

    while let Some(chunk) = response.chunk().await.map_err(|_| AuthError::Transport)? {
        if bytes.len().saturating_add(chunk.len()) > crate::http::MAX_TOKEN_RESPONSE_BYTES {
            return Err(AuthError::InvalidToken);
        }
        bytes.extend_from_slice(&chunk);
    }

    if !status.is_success() {
        let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        if value["error"].as_str() == Some("invalid_grant") {
            return Err(AuthError::LoginRequired);
        }
        return Err(AuthError::Http(status.as_u16()));
    }

    token_response(&bytes, previous)
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: Secret,
    refresh_token: Option<Secret>,
    expires_in: u64,
    account: Option<Account>,
}

#[derive(Deserialize)]
struct Account {
    email_address: Option<String>,
    uuid: Option<String>,
}

fn token_response(bytes: &[u8], previous: Option<&Credential>) -> Result<Credential> {
    let response: TokenResponse =
        serde_json::from_slice(bytes).map_err(|_| AuthError::InvalidToken)?;
    if response.access_token.0.is_empty() || response.expires_in == 0 {
        return Err(AuthError::InvalidToken);
    }
    let refresh = response
        .refresh_token
        .or_else(|| previous.and_then(|credential| credential.refresh.clone()))
        .filter(|token| !token.0.is_empty())
        .ok_or(AuthError::InvalidToken)?;
    let expires_at = now()
        .checked_add(response.expires_in)
        .ok_or(AuthError::InvalidToken)?;
    let account = previous
        .and_then(|credential| credential.account.clone())
        .or_else(|| {
            response
                .account
                .and_then(|account| account.email_address.or(account.uuid))
        });

    Ok(Credential {
        version: 1,
        kind: CredentialKind::AnthropicOAuth,
        endpoint: API_BASE.into(),
        access: response.access_token,
        refresh: Some(refresh),
        expires_at: Some(expires_at),
        account,
        account_id: None,
        project_id: None,
    })
}

fn random(size: usize) -> Result<String> {
    let mut bytes = vec![0; size];
    getrandom::fill(&mut bytes)
        .map_err(|_| AuthError::Configuration("secure randomness unavailable"))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn token_exchange_handles_rotation_redirects_and_sanitized_errors() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        for (status, body, expected) in [
            (
                "200 OK",
                r#"{"access_token":"new-access","refresh_token":"new-refresh","expires_in":3600}"#,
                "success",
            ),
            (
                "400 Bad Request",
                r#"{"error":"invalid_grant","description":"private-token"}"#,
                "login",
            ),
            ("302 Found", "private-token", "redirect"),
            ("200 OK", "private-token", "invalid"),
        ] {
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
                .await
                .unwrap();
            let endpoint = format!("http://{}/token", listener.local_addr().unwrap());
            let server = async {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();

                loop {
                    request.push(socket.read_u8().await.unwrap());
                    if request.ends_with(b"\r\n\r\n") {
                        break;
                    }
                }

                let headers = String::from_utf8(request).unwrap();
                let length: usize = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .map(str::to_owned)
                    })
                    .unwrap()
                    .parse()
                    .unwrap();
                let mut request_body = vec![0; length];
                socket.read_exact(&mut request_body).await.unwrap();
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nLocation: https://example.invalid/token\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
                serde_json::from_slice::<Value>(&request_body).unwrap()
            };
            let grant = json!({ "grant_type": "refresh_token", "refresh_token": "old-refresh" });

            let (result, received) = tokio::join!(exchange(&endpoint, grant.clone(), None), server);

            assert_eq!(received, grant);
            match expected {
                "success" => {
                    let credential = result.unwrap();
                    assert_eq!(credential.access.0, "new-access");
                    assert_eq!(credential.refresh.unwrap().0, "new-refresh");
                }
                "login" => assert!(matches!(result, Err(AuthError::LoginRequired))),
                "redirect" => assert!(matches!(result, Err(AuthError::Http(302)))),
                _ => assert!(matches!(result, Err(AuthError::InvalidToken))),
            }
        }
    }

    #[test]
    fn pkce_and_callbacks_bind_the_grant_to_one_login() {
        let login =
            AnthropicLogin::new(Url::parse("http://localhost:54545/callback").unwrap()).unwrap();
        let url = login.authorization_url();
        let pairs: std::collections::HashMap<_, _> = url.query_pairs().collect();
        assert_eq!(
            pairs["code_challenge"],
            URL_SAFE_NO_PAD.encode(Sha256::digest(login.verifier.0.as_bytes()))
        );
        assert_eq!(login.verifier.0.len(), 128);

        let callback = format!("{}?code=example&state={}", login.redirect, login.state);
        assert_eq!(login.callback_code(&callback).unwrap(), "example");
        assert!(
            login
                .callback_code(&format!("{callback}&state=wrong"))
                .is_err()
        );
        assert!(
            login
                .callback_code(&callback.replace("localhost", "attacker.example"))
                .is_err()
        );
        assert!(
            login
                .callback_code(&callback.replace(&login.state, "wrong"))
                .is_err()
        );
        assert!(matches!(
            login.callback_code(&format!(
                "{}?error=access_denied&state={}",
                login.redirect, login.state
            )),
            Err(AuthError::Denied)
        ));
    }

    #[test]
    fn refresh_preserves_identity_and_retains_unrotated_refresh_tokens() {
        let initial = json!({
            "access_token": "access-one",
            "refresh_token": "refresh-one",
            "expires_in": 3600,
            "account": { "uuid": "account-one" }
        });
        let first = token_response(&serde_json::to_vec(&initial).unwrap(), None).unwrap();
        let update = json!({
            "access_token": "access-two",
            "expires_in": 7200,
            "account": { "uuid": "different" }
        });

        let second = token_response(&serde_json::to_vec(&update).unwrap(), Some(&first)).unwrap();

        assert_eq!(second.refresh.unwrap().0, "refresh-one");
        assert_eq!(second.account.as_deref(), Some("account-one"));
        assert!(second.expires_at > first.expires_at);
        assert!(!format!("{first:?}").contains("access-one"));
        assert!(!format!("{first:?}").contains("refresh-one"));
        assert!(token_response(br#"{"access_token":"token","expires_in":1}"#, None).is_err());
    }
}
