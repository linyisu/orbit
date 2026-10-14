use std::time::Duration;

use serde_json::{Value, json};
use url::Url;

use crate::{
    AuthError, CredentialKind, Result,
    credential::{Credential, Secret},
    http::{Tokens, client, form_token, json_response},
};

// Public installed-app credentials from Gemini CLI / oh-my-pi; not a user secret.
pub(crate) const CLIENT_ID: &str =
    "681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com";
const CLIENT_SECRET: &str = "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";
pub(crate) const API_BASE: &str = "https://cloudcode-pa.googleapis.com/";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

pub(crate) async fn exchange(code: &str, verifier: &str, redirect: &Url) -> Result<Credential> {
    exchange_using(
        code,
        verifier,
        redirect,
        CLIENT_ID,
        CLIENT_SECRET,
        CredentialKind::GoogleOAuth,
        API_BASE,
    )
    .await
}

pub(crate) async fn exchange_using(
    code: &str,
    verifier: &str,
    redirect: &Url,
    client_id: &str,
    client_secret: &str,
    kind: CredentialKind,
    endpoint: &str,
) -> Result<Credential> {
    let value = form_token(
        TOKEN_URL,
        &[
            ("grant_type", "authorization_code"),
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("code", code),
            ("code_verifier", verifier),
            ("redirect_uri", redirect.as_str()),
        ],
    )
    .await?;
    let tokens: Tokens = serde_json::from_value(value).map_err(|_| AuthError::InvalidToken)?;
    let mut credential = Credential {
        version: 1,
        kind,
        endpoint: endpoint.into(),
        access: Secret(String::new()),
        refresh: None,
        expires_at: None,
        account: None,
        account_id: None,
        project_id: None,
    };

    tokens.apply(&mut credential)?;
    // Profile lookup is optional; a userinfo outage must not invalidate a grant.
    if let Ok(response) = client()?
        .get("https://www.googleapis.com/oauth2/v1/userinfo?alt=json")
        .bearer_auth(&credential.access.0)
        .send()
        .await
        && let Ok(value) = json_response(response).await
    {
        credential.account = value["email"]
            .as_str()
            .map(str::to_owned);
    }

    Ok(credential)
}

pub(crate) async fn provision(credential: &mut Credential, project: Option<&str>) -> Result<()> {
    credential.project_id = Some(
        tokio::time::timeout(
            Duration::from_secs(120),
            discover_project(API_BASE, &credential.access.0, project),
        )
        .await
        .map_err(|_| AuthError::Timeout)??,
    );

    Ok(())
}

pub(crate) async fn refresh(previous: &Credential) -> Result<Credential> {
    refresh_using(previous, CLIENT_ID, CLIENT_SECRET).await
}

pub(crate) async fn refresh_using(
    previous: &Credential,
    client_id: &str,
    client_secret: &str,
) -> Result<Credential> {
    let refresh = previous
        .refresh
        .as_ref()
        .ok_or(AuthError::LoginRequired)?;
    let value = form_token(
        TOKEN_URL,
        &[
            ("grant_type", "refresh_token"),
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("refresh_token", &refresh.0),
        ],
    )
    .await?;
    let tokens: Tokens = serde_json::from_value(value).map_err(|_| AuthError::InvalidToken)?;
    let mut credential = previous.clone();
    tokens.apply(&mut credential)?;
    Ok(credential)
}

pub(crate) fn validate_project(project: &str) -> Result<()> {
    if project.is_empty()
        || project.len() > 256
        || !project
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        return Err(AuthError::Configuration("invalid Google Cloud project ID"));
    }

    Ok(())
}

async fn post(base: &str, action: &str, token: &str, body: Value) -> Result<Value> {
    let response = client()?
        .post(format!("{base}v1internal:{action}"))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .map_err(|_| AuthError::Transport)?;
    json_response(response).await
}

async fn discover_project(base: &str, token: &str, project: Option<&str>) -> Result<String> {
    let mut metadata = json!({
        "ideType": "IDE_UNSPECIFIED",
        "platform": "PLATFORM_UNSPECIFIED",
        "pluginType": "GEMINI"
    });
    let mut body = json!({
        "metadata": metadata
    });
    if let Some(project) = project {
        validate_project(project)?;
        metadata["duetProject"] = json!(project);
        body["metadata"] = metadata.clone();
        body["cloudaicompanionProject"] = json!(project);
    }

    let loaded = post(base, "loadCodeAssist", token, body).await?;
    if loaded
        .get("currentTier")
        .is_some_and(|tier| !tier.is_null())
    {
        let resolved = loaded["cloudaicompanionProject"]
            .as_str()
            .or(project)
            .ok_or(AuthError::GoogleProjectRequired)?;
        validate_project(resolved)?;
        return Ok(resolved.to_owned());
    }

    let default_tier = loaded["allowedTiers"]
        .as_array()
        .and_then(|tiers| {
            tiers
                .iter()
                .find(|tier| tier["isDefault"] == true)
        });
    let tier = match default_tier {
        Some(tier) => tier["id"]
            .as_str()
            .unwrap_or("free-tier"),
        None => "legacy-tier",
    };
    if tier != "free-tier" && project.is_none() {
        return Err(AuthError::GoogleProjectRequired);
    }
    let mut body = json!({
        "tierId": tier,
        "metadata": metadata
    });
    if tier != "free-tier"
        && let Some(project) = project
    {
        body["cloudaicompanionProject"] = json!(project);
    }

    let mut operation = post(base, "onboardUser", token, body).await?;

    for _ in 0..24 {
        if operation["done"] == true {
            if operation.get("error").is_some() {
                return Err(AuthError::Configuration("Google project onboarding failed"));
            }
            let resolved = operation["response"]["cloudaicompanionProject"]["id"]
                .as_str()
                .or(project)
                .ok_or(AuthError::Configuration(
                    "Google did not return a Cloud Code Assist project",
                ))?;

            validate_project(resolved)?;
            return Ok(resolved.to_owned());
        }

        let name = operation["name"]
            .as_str()
            .ok_or(AuthError::InvalidToken)?;
        if !name.starts_with("operations/")
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'_' | b'-'))
        {
            return Err(AuthError::InvalidToken);
        }

        tokio::time::sleep(Duration::from_secs(5)).await;
        let response = client()?
            .get(format!("{base}v1internal/{name}"))
            .bearer_auth(token)
            .send()
            .await
            .map_err(|_| AuthError::Transport)?;

        operation = json_response(response).await?;
    }

    Err(AuthError::Timeout)
}

#[cfg(test)]
pub(crate) mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    pub(crate) async fn server(
        responses: Vec<Value>,
    ) -> (String, tokio::task::JoinHandle<Vec<Value>>) {
        server_with_headers(responses, &[]).await
    }

    pub(crate) async fn server_with_headers(
        responses: Vec<Value>,
        expected_headers: &[&str],
    ) -> (String, tokio::task::JoinHandle<Vec<Value>>) {
        let expected_headers = expected_headers
            .iter()
            .map(|header| header.to_string())
            .collect::<Vec<_>>();
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let base = format!("http://{}/", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let mut requests = Vec::new();
            for response in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                while !bytes.ends_with(b"\r\n\r\n") {
                    bytes.push(socket.read_u8().await.unwrap());
                }
                let header = String::from_utf8(bytes)
                    .unwrap()
                    .to_ascii_lowercase();
                for expected in &expected_headers {
                    assert!(
                        header
                            .lines()
                            .any(|line| line == expected),
                        "missing expected header {expected:?} in {header:?}"
                    );
                }

                let length: usize = header
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length: "))
                    .unwrap_or("0")
                    .parse()
                    .unwrap();
                let mut body = vec![0; length];
                socket
                    .read_exact(&mut body)
                    .await
                    .unwrap();
                requests.push(if body.is_empty() {
                    Value::Null
                } else {
                    serde_json::from_slice(&body).unwrap()
                });
                let body = response.to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket
                    .write_all(response.as_bytes())
                    .await
                    .unwrap();
            }

            requests
        });
        (base, task)
    }

    #[tokio::test]
    async fn existing_projects_and_free_tier_onboarding_are_both_resolved() {
        let (base, task) = server(vec![json!({
            "currentTier": {
                "id": "free-tier"
            },
            "cloudaicompanionProject": "existing-project"
        })])
        .await;

        let project = discover_project(&base, "fake-token", None)
            .await
            .unwrap();

        assert_eq!(project, "existing-project");
        assert_eq!(task.await.unwrap().len(), 1);

        let (base, task) = server(vec![
            json!({
                "allowedTiers": [{
                    "id": "free-tier",
                    "isDefault": true
                }]
            }),
            json!({
                "done": true,
                "response": {
                    "cloudaicompanionProject": {
                        "id": "new-project"
                    }
                }
            }),
        ])
        .await;

        let project = discover_project(&base, "fake-token", None)
            .await
            .unwrap();

        assert_eq!(project, "new-project");
        let requests = task.await.unwrap();
        assert_eq!(requests[1]["tierId"], "free-tier");
    }

    #[tokio::test]
    async fn default_tier_without_id_uses_free_onboarding() {
        let (base, task) = server(vec![
            json!({
                "allowedTiers": [{
                    "isDefault": true
                }]
            }),
            json!({
                "done": true,
                "response": {
                    "cloudaicompanionProject": {
                        "id": "managed-project"
                    }
                }
            }),
        ])
        .await;

        let project = discover_project(&base, "fake-token", None)
            .await
            .unwrap();

        assert_eq!(project, "managed-project");
        let requests = task.await.unwrap();
        assert_eq!(requests[1]["tierId"], "free-tier");
        assert!(
            requests[1]
                .get("cloudaicompanionProject")
                .is_none()
        );
    }

    #[tokio::test]
    async fn explicit_project_is_forwarded_to_standard_tier_onboarding() {
        let (base, task) = server(vec![
            json!({
                "allowedTiers": [{
                    "id": "standard-tier",
                    "isDefault": true
                }]
            }),
            json!({
                "done": true,
                "response": {}
            }),
        ])
        .await;

        let project = discover_project(&base, "fake-token", Some("owned-project"))
            .await
            .unwrap();

        assert_eq!(project, "owned-project");
        let requests = task.await.unwrap();
        assert_eq!(requests[0]["cloudaicompanionProject"], "owned-project");
        assert_eq!(requests[1]["cloudaicompanionProject"], "owned-project");
        assert_eq!(requests[1]["metadata"]["duetProject"], "owned-project");
        assert_eq!(requests[1]["tierId"], "standard-tier");
    }

    #[tokio::test]
    async fn missing_tiers_do_not_silently_select_free_access() {
        let (base, task) = server(vec![json!({})]).await;

        let result = discover_project(&base, "fake-token", None).await;

        assert!(matches!(result, Err(AuthError::GoogleProjectRequired)));
        assert_eq!(task.await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn workspace_accounts_require_an_explicit_project_when_discovery_has_none() {
        let (base, task) = server(vec![json!({
            "currentTier": {
                "id": "standard-tier"
            }
        })])
        .await;

        let result = discover_project(&base, "fake-token", None).await;

        assert!(matches!(result, Err(AuthError::GoogleProjectRequired)));
        task.await.unwrap();
    }
}
