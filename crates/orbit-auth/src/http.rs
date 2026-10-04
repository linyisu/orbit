use std::time::Duration;

use reqwest::{Client, Response};
use serde::Deserialize;
use serde_json::Value;

use crate::{
    AuthError, Result,
    credential::{Credential, Secret, now},
};

pub(crate) const AUTH_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const MAX_TOKEN_RESPONSE_BYTES: usize = 64 * 1024;

pub(crate) fn client() -> Result<Client> {
    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(AUTH_REQUEST_TIMEOUT)
        .user_agent(concat!("orbit/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|_| AuthError::Transport)
}

pub(crate) async fn json_response(mut response: Response) -> Result<Value> {
    let status = response.status();
    let mut bytes = Vec::new();

    while let Some(chunk) = response.chunk().await.map_err(|_| AuthError::Transport)? {
        if bytes.len().saturating_add(chunk.len()) > MAX_TOKEN_RESPONSE_BYTES {
            return Err(AuthError::InvalidToken);
        }
        bytes.extend_from_slice(&chunk);
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| {
        if status.is_success() {
            AuthError::InvalidToken
        } else {
            AuthError::Http(status.as_u16())
        }
    })?;
    if !status.is_success() {
        if value["error"].as_str() == Some("invalid_grant") {
            return Err(AuthError::LoginRequired);
        }
        return Err(AuthError::Http(status.as_u16()));
    }

    Ok(value)
}

pub(crate) async fn form_token(endpoint: &str, form: &[(&str, &str)]) -> Result<Value> {
    let body = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(form.iter().copied())
        .finish();
    let response = client()?
        .post(endpoint)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await
        .map_err(|_| AuthError::Transport)?;
    json_response(response).await
}

#[derive(Deserialize)]
pub(crate) struct Tokens {
    pub access_token: Secret,
    pub refresh_token: Option<Secret>,
    pub expires_in: u64,
    pub id_token: Option<Secret>,
}

impl Tokens {
    pub fn apply(self, credential: &mut Credential) -> Result<()> {
        if self.access_token.0.is_empty()
            || self.access_token.0.chars().any(char::is_control)
            || self.expires_in == 0
        {
            return Err(AuthError::InvalidToken);
        }
        let refresh = self
            .refresh_token
            .or_else(|| credential.refresh.clone())
            .filter(|value| !value.0.is_empty() && !value.0.chars().any(char::is_control))
            .ok_or(AuthError::InvalidToken)?;
        credential.access = self.access_token;
        credential.refresh = Some(refresh);
        credential.expires_at = Some(
            now()
                .checked_add(self.expires_in)
                .ok_or(AuthError::InvalidToken)?,
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn grant_forms_escape_codes_and_token_errors_do_not_echo_the_body() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let url = format!("http://{}/token", listener.local_addr().unwrap());
        let server = async {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            while !bytes.ends_with(b"\r\n\r\n") {
                bytes.push(socket.read_u8().await.unwrap());
            }
            let headers = String::from_utf8(bytes).unwrap().to_ascii_lowercase();
            let length: usize = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length: "))
                .unwrap()
                .parse()
                .unwrap();
            let mut body = vec![0; length];
            socket.read_exact(&mut body).await.unwrap();
            let error = r#"{"error":"invalid_grant","error_description":"private-token"}"#;
            let response = format!(
                "HTTP/1.1 400 Bad Request\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{error}",
                error.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            (headers, body)
        };

        let (result, (headers, body)) = tokio::join!(
            form_token(
                &url,
                &[("grant_type", "authorization_code"), ("code", "a+b&=")]
            ),
            server
        );

        assert!(headers.contains("content-type: application/x-www-form-urlencoded"));
        let pairs = url::form_urlencoded::parse(&body).collect::<std::collections::HashMap<_, _>>();
        assert_eq!(pairs["code"], "a+b&=");
        let error = result.unwrap_err();
        assert!(matches!(error, AuthError::LoginRequired));
        assert!(!format!("{error:?}").contains("private-token"));
    }
}
