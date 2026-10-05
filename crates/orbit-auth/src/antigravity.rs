//! Antigravity account provisioning; distinct from Gemini CLI's project selection.
//! Protocol reference: oh-my-pi v18.2.8 (5e0fc867f8a58dfe8812b5e99b2e7b6a0313da6c),
//! packages/ai/src/registry/oauth/google-antigravity.ts (MIT, LICENSE-REFERENCE).
use std::time::Duration;

use serde_json::{Value, json};
use url::Url;

use crate::{
    AuthError, CredentialKind, Result,
    credential::Credential,
    http::{client, json_response},
};

pub(crate) const CLIENT_ID: &str =
    "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com";
const CLIENT_SECRET: &str = "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";
pub(crate) const API_BASE: &str = "https://daily-cloudcode-pa.googleapis.com/";

// OMP v18.2.8 pins these platform fields to its reference client, independent
// of the host. Its login hook uses the default version; manifest refresh is
// invoked by model discovery, not by the browser authentication flow.
static CLIENT_VERSION: tokio::sync::OnceCell<String> = tokio::sync::OnceCell::const_new();

pub async fn refresh_client_version() {
    let _ = CLIENT_VERSION.get_or_try_init(|| async {
        let response = crate::http::client()?.get("https://antigravity-hub-auto-updater-974169037036.us-central1.run.app/manifest/latest-arm64-mac.yml")
            .header("user-agent", "electron-builder").header("cache-control", "no-cache")
            .timeout(std::time::Duration::from_secs(5)).send().await.map_err(|_| crate::AuthError::Transport)?;
        if !response.status().is_success() { return Err(crate::AuthError::Transport); }
        let text = response.text().await.map_err(|_| crate::AuthError::Transport)?;
        text.lines().find_map(|line| {
            let value = line.trim().strip_prefix("version:")?.split('#').next()?.trim().trim_matches(['\"', '\'']);
            let parts = value.split('.').collect::<Vec<_>>();
            (parts.len() == 3 && parts.iter().all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit())))
                .then(|| value.to_owned())
        }).ok_or(crate::AuthError::Transport)
    }).await;
}

pub(crate) fn user_agent() -> String {
    let version = CLIENT_VERSION.get().map(String::as_str).unwrap_or("2.8.0");
    format!("antigravity/hub/{version} (aidev_client; os_type=darwin; arch=arm64; cl=963137146)")
}

pub(crate) async fn exchange(code: &str, verifier: &str, redirect: &Url) -> Result<Credential> {
    let mut credential = crate::google::exchange_using(
        code,
        verifier,
        redirect,
        CLIENT_ID,
        CLIENT_SECRET,
        CredentialKind::AntigravityOAuth,
        API_BASE,
    )
    .await?;
    // As in OMP, requests have individual 30s timeouts and onboarding has its
    // own 30s deadline; do not impose an extra deadline on the whole login.
    credential.project_id = Some(discover_project(API_BASE, &credential.access.0).await?);

    Ok(credential)
}

pub(crate) async fn refresh(previous: &Credential) -> Result<Credential> {
    previous
        .project_id
        .as_ref()
        .filter(|id| !id.is_empty())
        .ok_or(AuthError::LoginRequired)?;

    crate::google::refresh_using(previous, CLIENT_ID, CLIENT_SECRET).await
}

async fn post(base: &str, action: &str, token: &str, body: Value) -> Result<Value> {
    let response = client()?
        .post(format!("{base}v1internal:{action}"))
        .bearer_auth(token)
        .header("user-agent", user_agent())
        .json(&body)
        .send()
        .await
        .map_err(|_| AuthError::Transport)?;
    json_response(response).await
}

async fn load(base: &str, token: &str) -> Result<Value> {
    let mut data = post(
        base,
        "loadCodeAssist",
        token,
        json!({ "metadata": { "ideType": "ANTIGRAVITY" } }),
    )
    .await?;

    if data.get("paidTier").is_none_or(Value::is_null)
        && let Some(project) = data["cloudaicompanionProject"]
            .as_str()
            .filter(|id| !id.is_empty())
    {
        data = post(
            base,
            "loadCodeAssist",
            token,
            json!({
                "cloudaicompanionProject": project,
                "metadata": { "ideType": "ANTIGRAVITY" }
            }),
        )
        .await?;
    }

    Ok(data)
}

async fn onboard(base: &str, token: &str) -> Result<()> {
    let mut operation = post(
        base,
        "onboardUser",
        token,
        json!({
            "tierId": "free-tier", "metadata": { "ideType": "ANTIGRAVITY" }
        }),
    )
    .await?;

    loop {
        if operation["done"] == true {
            if operation.get("error").is_some_and(|error| !error.is_null()) {
                return Err(AuthError::Configuration(
                    "Antigravity project initialization failed",
                ));
            }

            if !operation["response"].is_object() {
                return Err(AuthError::InvalidToken);
            }
            return Ok(());
        }

        let name = operation["name"].as_str().ok_or(AuthError::InvalidToken)?;
        if !name.starts_with("operations/")
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'_' | b'-'))
        {
            return Err(AuthError::InvalidToken);
        }

        tokio::time::sleep(Duration::from_secs(1)).await;
        let response = client()?
            .get(format!("{base}v1internal/{name}"))
            .header("content-type", "application/json")
            .bearer_auth(token)
            .header("user-agent", user_agent())
            .send()
            .await
            .map_err(|_| AuthError::Transport)?;

        operation = json_response(response).await?;
    }
}

fn check_eligibility(data: &Value, token: &str) -> Result<()> {
    let allowed = data["allowedTiers"]
        .as_array()
        .is_some_and(|tiers| tiers.iter().any(|tier| tier["id"] == "free-tier"));
    if allowed {
        return Ok(());
    }

    let tier = data["ineligibleTiers"]
        .as_array()
        .and_then(|tiers| tiers.iter().find(|tier| tier["tierId"] == "free-tier"));
    let Some(tier) = tier else {
        return Ok(());
    };
    // Like OMP, only a server-provided reason marks this response as a denial.
    // An entry without a reason is insufficient to conclude account ineligibility.
    let Some(reason) = tier["reasonMessage"]
        .as_str()
        .filter(|reason| !reason.is_empty())
    else {
        return Ok(());
    };
    let safe_text = |text: &str| {
        let redacted = if token.is_empty() {
            text.to_owned()
        } else {
            text.replace(token, "[redacted]")
        };
        redacted
            .chars()
            .filter(|character| !character.is_control())
            .take(2048)
            .collect::<String>()
    };
    let message = safe_text(reason);

    if let Some(raw) = tier["validationUrl"].as_str()
        && let Ok(url) = Url::parse(raw)
        && url.scheme() == "https"
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && !raw.chars().any(char::is_control)
        && raw.len() <= 4096
    {
        // Preserve the complete signed URL; truncating it makes verification fail.
        if token.is_empty() || !raw.contains(token) {
            return Err(AuthError::AntigravityVerification {
                reason: message,
                url: raw.to_owned(),
            });
        }
    }

    Err(AuthError::AntigravityAccount(message))
}

async fn discover_project(base: &str, token: &str) -> Result<String> {
    let data = load(base, token).await?;
    check_eligibility(&data, token)?;
    if data.get("currentTier").is_none_or(Value::is_null) {
        tokio::time::timeout(Duration::from_secs(30), onboard(base, token))
            .await
            .map_err(|_| AuthError::Timeout)??;
    }

    let refreshed = load(base, token).await?;
    let project = refreshed["cloudaicompanionProject"]
        .as_str()
        .ok_or(AuthError::Configuration(
            "Antigravity did not return a managed project; retry account initialization",
        ))?;

    crate::google::validate_project(project)?;
    Ok(project.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    // Expected wire header transcribed from OMP v18.2.8 gemini-headers.ts,
    // independently of Orbit's user_agent() implementation.
    async fn server(responses: Vec<Value>) -> (String, tokio::task::JoinHandle<Vec<Value>>) {
        crate::google::tests::server_with_headers(responses, &[
            "user-agent: antigravity/hub/2.8.0 (aidev_client; os_type=darwin; arch=arm64; cl=963137146)",
            "authorization: bearer fake-token",
            "content-type: application/json",
        ]).await
    }

    #[tokio::test]
    async fn pending_onboarding_polls_with_the_same_client_headers() {
        let (base, task) = server(vec![
            json!({ "name": "operations/setup", "done": false }),
            json!({ "done": true, "response": { "@type": "OnboardUserResponse" } }),
        ])
        .await;

        onboard(&base, "fake-token").await.unwrap();

        let requests = task.await.unwrap();
        assert_eq!(
            requests[0],
            json!({ "tierId": "free-tier", "metadata": { "ideType": "ANTIGRAVITY" } })
        );
        assert_eq!(requests[1], Value::Null);
    }

    #[tokio::test]
    async fn new_accounts_are_onboarded_and_the_managed_project_is_reloaded() {
        let (base, task) = server(vec![
            json!({ "allowedTiers": [{ "id": "free-tier" }] }),
            json!({ "done": true, "response": { "@type": "OnboardUserResponse" } }),
            json!({ "currentTier": { "id": "free-tier" }, "paidTier": { "id": "paid" }, "cloudaicompanionProject": "managed-project" }),
        ]).await;

        let project = discover_project(&base, "fake-token").await.unwrap();

        assert_eq!(project, "managed-project");
        let requests = task.await.unwrap();
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[0]["metadata"]["ideType"], "ANTIGRAVITY");
        assert_eq!(requests[1]["tierId"], "free-tier");
        assert!(requests[1].get("cloudaicompanionProject").is_none());
    }

    #[tokio::test]
    async fn existing_free_accounts_use_project_scoped_discovery_without_onboarding() {
        let loaded = json!({ "currentTier": { "id": "free-tier" }, "cloudaicompanionProject": "existing-project" });
        let (base, task) = server(vec![loaded.clone(); 4]).await;

        let project = discover_project(&base, "fake-token").await.unwrap();

        assert_eq!(project, "existing-project");
        let requests = task.await.unwrap();
        assert_eq!(requests[1]["cloudaicompanionProject"], "existing-project");
        assert_eq!(requests[3]["cloudaicompanionProject"], "existing-project");
        assert!(
            requests
                .iter()
                .all(|request| request.get("tierId").is_none())
        );
    }

    #[tokio::test]
    async fn ineligible_accounts_do_not_attempt_onboarding_or_request_a_user_project() {
        let (base, task) = server(vec![json!({ "ineligibleTiers": [{ "tierId": "free-tier", "reasonMessage": "unavailable" }] })]).await;

        let result = discover_project(&base, "fake-token").await;

        assert!(
            matches!(result, Err(AuthError::AntigravityAccount(reason)) if reason == "unavailable")
        );
        assert_eq!(task.await.unwrap().len(), 1);
    }

    #[test]
    fn eligibility_preserves_the_reason_and_validation_link_without_terminal_controls() {
        let response = json!({
            "ineligibleTiers": [{
                "tierId": "free-tier",
                "reasonMessage": "Verify account\n\u{1b} fake-token",
                "validationUrl": "https://accounts.google.com/verify?flow=account"
            }]
        });

        let error = check_eligibility(&response, "fake-token")
            .unwrap_err()
            .to_string();

        assert!(error.contains("Verify account"));
        assert!(error.contains("https://accounts.google.com/verify?flow=account"));
        assert!(!error.contains("fake-token"));
        assert!(!error.contains('\u{1b}'));
    }

    #[test]
    fn verification_urls_remain_complete_even_when_longer_than_display_messages() {
        let url = format!(
            "https://accounts.google.com/signin/continue?flow={}&authuser=0",
            "x".repeat(2500)
        );
        let response = json!({ "ineligibleTiers": [{
            "tierId": "free-tier", "reasonMessage": "Verify your account",
            "validationUrl": url
        }] });

        let error = check_eligibility(&response, "fake-token").unwrap_err();

        assert!(
            matches!(error, AuthError::AntigravityVerification { url: actual, .. } if actual == url)
        );
    }

    #[test]
    fn eligibility_does_not_invent_a_denial_or_open_unsafe_validation_urls() {
        for response in [
            json!({ "ineligibleTiers": [{ "tierId": "free-tier" }] }),
            json!({ "ineligibleTiers": [{ "tierId": "free-tier", "reasonMessage": "" }] }),
            json!({ "allowedTiers": [{ "id": "free-tier" }], "ineligibleTiers": [{ "tierId": "free-tier", "reasonMessage": "old denial" }] }),
        ] {
            assert!(check_eligibility(&response, "token").is_ok());
        }

        let response = json!({ "ineligibleTiers": [{
            "tierId": "free-tier", "reasonMessage": "verification needed",
            "validationUrl": "javascript:alert(1)"
        }] });
        let error = check_eligibility(&response, "token")
            .unwrap_err()
            .to_string();

        assert!(error.contains("verification needed"));
        assert!(!error.contains("javascript:"));
    }

    #[test]
    fn credentials_are_bound_to_antigravity_endpoint_and_use_its_project() {
        let credential = Credential {
            version: 1,
            kind: CredentialKind::AntigravityOAuth,
            endpoint: API_BASE.into(),
            access: crate::credential::Secret("fake-access".into()),
            refresh: None,
            expires_at: None,
            account: None,
            account_id: None,
            project_id: Some("managed-project".into()),
        };

        let auth = credential
            .request_auth(
                &Url::parse(API_BASE).unwrap(),
                crate::AuthTarget::GoogleCodeAssist,
            )
            .unwrap();

        assert_eq!(auth.project_id.as_deref(), Some("managed-project"));
        assert!(auth.headers["authorization"].is_sensitive());
        assert_eq!(auth.headers["user-agent"], user_agent());
        assert!(
            credential
                .request_auth(
                    &Url::parse(crate::google::API_BASE).unwrap(),
                    crate::AuthTarget::GoogleCodeAssist
                )
                .is_err()
        );
        assert!(
            credential
                .request_auth(&Url::parse(API_BASE).unwrap(), crate::AuthTarget::Openai)
                .is_err()
        );
    }
}
