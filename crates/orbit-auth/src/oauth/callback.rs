use std::time::Duration;

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use url::Url;

use crate::{AuthError, CallbackFlow, Result};

const MAX_AUTHORIZATION_CODE_BYTES: usize = 4_096;
const MAX_CALLBACK_HEADER_BYTES: usize = 8 * 1_024;
const CALLBACK_IO_TIMEOUT: Duration = Duration::from_secs(2);

/// Validate the redirect and state before accepting either a code or denial.
pub(crate) fn authorization_code(callback: &str, redirect: &Url, state: &str) -> Result<String> {
    let url = Url::parse(callback).map_err(|_| AuthError::InvalidCallback)?;
    if url.origin() != redirect.origin()
        || url.path() != redirect.path()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(AuthError::InvalidCallback);
    }

    let pairs: Vec<_> = url.query_pairs().collect();
    let states: Vec<_> = pairs
        .iter()
        .filter(|(key, _)| key == "state")
        .collect();
    if states.len() != 1 || states[0].1 != state {
        return Err(AuthError::InvalidCallback);
    }
    if pairs
        .iter()
        .any(|(key, _)| key == "error")
    {
        return Err(AuthError::Denied);
    }
    let codes: Vec<_> = pairs
        .iter()
        .filter(|(key, _)| key == "code")
        .collect();
    if codes.len() != 1 || codes[0].1.is_empty() || codes[0].1.len() > MAX_AUTHORIZATION_CODE_BYTES
    {
        return Err(AuthError::InvalidCallback);
    }

    Ok(codes[0].1.to_string())
}

/// A loopback-only listener. The host owns browser launching and cancellation.
pub struct CallbackListener {
    listener: TcpListener,
    redirect: Url,
}

impl CallbackListener {
    pub async fn bind(port: u16) -> Result<Self> {
        Self::bind_at(
            Url::parse(&format!("http://localhost:{port}/callback")).expect("loopback URL"),
        )
        .await
    }

    pub async fn bind_at(mut redirect: Url) -> Result<Self> {
        if redirect.scheme() != "http"
            || !matches!(redirect.host_str(), Some("localhost" | "127.0.0.1"))
            || !redirect.username().is_empty()
            || redirect.password().is_some()
            || redirect.query().is_some()
            || redirect.fragment().is_some()
        {
            return Err(AuthError::InvalidCallback);
        }

        let port = redirect
            .port()
            .ok_or(AuthError::InvalidCallback)?;
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).await?;
        let port = listener.local_addr()?.port();
        redirect
            .set_port(Some(port))
            .map_err(|_| AuthError::InvalidCallback)?;

        Ok(Self { listener, redirect })
    }

    pub fn redirect_uri(&self) -> Url {
        self.redirect.clone()
    }

    pub async fn wait(
        &self,
        login: &(impl CallbackFlow + Sync),
        timeout: Duration,
    ) -> Result<String> {
        if login.redirect_uri() != &self.redirect {
            return Err(AuthError::InvalidCallback);
        }

        tokio::time::timeout(timeout, self.receive(login))
            .await
            .map_err(|_| AuthError::Timeout)?
    }

    async fn receive(&self, login: &(impl CallbackFlow + Sync)) -> Result<String> {
        loop {
            let (mut stream, _) = self.listener.accept().await?;
            let read = async {
                let mut bytes = Vec::new();
                while bytes.len() < MAX_CALLBACK_HEADER_BYTES {
                    let byte = stream.read_u8().await?;
                    bytes.push(byte);
                    if bytes.ends_with(b"\r\n\r\n") {
                        return Ok::<_, std::io::Error>(bytes);
                    }
                }

                Err(std::io::Error::other("callback headers too large"))
            };
            let Ok(Ok(bytes)) = tokio::time::timeout(CALLBACK_IO_TIMEOUT, read).await else {
                continue;
            };
            let request = String::from_utf8_lossy(&bytes);
            let mut line = request
                .lines()
                .next()
                .unwrap_or("")
                .split_whitespace();
            if line.next() != Some("GET") {
                continue;
            }
            let target = line.next().unwrap_or("");
            if !target.starts_with(&format!("{}?", self.redirect.path())) {
                continue;
            }
            let callback = format!(
                "{}{}",
                self.redirect
                    .origin()
                    .ascii_serialization(),
                target
            );
            let result = login.callback_code(&callback);
            let accepted = result.is_ok();
            let (status, body) = if accepted {
                (
                    "200 OK",
                    "Authorization callback received. Login is not complete yet. Return to Orbit \
                        for token exchange and account setup.",
                )
            } else {
                ("400 Bad Request", "Invalid authorization callback.")
            };
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nCache-Control: \
                    no-store\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            let _ =
                tokio::time::timeout(CALLBACK_IO_TIMEOUT, stream.write_all(response.as_bytes()))
                    .await;

            match result {
                Ok(_) => return Ok(callback),
                Err(AuthError::Denied) => return Err(AuthError::Denied),
                _ => continue,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AnthropicLogin;

    #[tokio::test]
    async fn listener_ignores_forged_state_then_accepts_the_bound_callback() {
        let listener = CallbackListener::bind(0).await.unwrap();
        let login = AnthropicLogin::new(listener.redirect_uri()).unwrap();
        let state = login
            .authorization_url()
            .query_pairs()
            .find(|(key, _)| key == "state")
            .unwrap()
            .1
            .into_owned();
        let port = listener.redirect_uri().port().unwrap();
        let requests = async {
            for (state, expected) in [("forged", "400"), (state.as_str(), "200")] {
                let mut socket = tokio::net::TcpStream::connect(("127.0.0.1", port))
                    .await
                    .unwrap();
                socket
                    .write_all(
                        format!(
                            "GET /callback?code=test&state={state} HTTP/1.1\r\nHost: \
                    localhost\r\n\r\n"
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
                let mut response = String::new();
                socket
                    .read_to_string(&mut response)
                    .await
                    .unwrap();
                assert!(
                    response.starts_with(&format!("HTTP/1.1 {expected}")),
                    "{response}"
                );
            }
        };

        let (result, ()) = tokio::join!(listener.wait(&login, Duration::from_secs(5)), requests);

        assert_eq!(
            login
                .callback_code(&result.unwrap())
                .unwrap(),
            "test"
        );
    }

    #[tokio::test]
    async fn missing_callback_has_a_bounded_wait() {
        let listener = CallbackListener::bind(0).await.unwrap();
        let login = AnthropicLogin::new(listener.redirect_uri()).unwrap();

        let result = listener
            .wait(&login, Duration::from_millis(10))
            .await;

        assert!(matches!(result, Err(AuthError::Timeout)));
    }
}
