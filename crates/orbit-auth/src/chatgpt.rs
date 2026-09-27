use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::Value;
use url::Url;

use crate::{
    AuthError, CredentialKind, Result,
    credential::{Credential, Secret},
    http::{Tokens, form_token},
};

pub(crate) const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub(crate) const API_BASE: &str = "https://chatgpt.com/backend-api/codex/";
const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";

pub(crate) async fn exchange(code: &str, verifier: &str, redirect: &Url) -> Result<Credential> {
    let value = form_token(
        TOKEN_URL,
        &[
            ("grant_type", "authorization_code"),
            ("client_id", CLIENT_ID),
            ("code", code),
            ("code_verifier", verifier),
            ("redirect_uri", redirect.as_str()),
        ],
    )
    .await?;

    parse_tokens(value, None)
}

pub(crate) async fn refresh(previous: &Credential) -> Result<Credential> {
    let token = previous.refresh.as_ref().ok_or(AuthError::LoginRequired)?;
    let value = form_token(
        TOKEN_URL,
        &[
            ("grant_type", "refresh_token"),
            ("client_id", CLIENT_ID),
            ("refresh_token", &token.0),
        ],
    )
    .await?;

    parse_tokens(value, Some(previous))
}

fn parse_tokens(value: Value, previous: Option<&Credential>) -> Result<Credential> {
    let tokens: Tokens = serde_json::from_value(value).map_err(|_| AuthError::InvalidToken)?;
    // These claims only select the upstream account header. They are not locally
    // trusted authorization: the service still validates the bearer token.
    let access = claims(&tokens.access_token.0);
    let identity = tokens
        .id_token
        .as_ref()
        .map(|token| claims(&token.0))
        .unwrap_or(Value::Null);
    let account_id = account_id(&access)
        .or_else(|| account_id(&identity))
        .or_else(|| previous.and_then(|credential| credential.account_id.as_deref()))
        .filter(|id| {
            !id.is_empty() && id.len() <= 256 && id.bytes().all(|byte| byte.is_ascii_graphic())
        })
        .ok_or(AuthError::InvalidToken)?
        .to_owned();
    if previous
        .and_then(|credential| credential.account_id.as_deref())
        .is_some_and(|old| old != account_id)
    {
        return Err(AuthError::LoginRequired);
    }
    let email = access["https://api.openai.com/profile"]["email"]
        .as_str()
        .or_else(|| identity["email"].as_str())
        .or_else(|| identity["https://api.openai.com/profile"]["email"].as_str())
        .map(str::to_owned)
        .or_else(|| previous.and_then(|credential| credential.account.clone()));
    let mut credential = Credential {
        version: 1,
        kind: CredentialKind::ChatgptOAuth,
        endpoint: API_BASE.into(),
        access: Secret(String::new()),
        refresh: previous.and_then(|credential| credential.refresh.clone()),
        expires_at: None,
        account: email,
        account_id: Some(account_id),
        project_id: None,
    };
    tokens.apply(&mut credential)?;
    Ok(credential)
}

fn account_id(value: &Value) -> Option<&str> {
    value["https://api.openai.com/auth"]["chatgpt_account_id"].as_str()
}

fn claims(token: &str) -> Value {
    let parts: Vec<_> = token.split('.').collect();
    if parts.len() != 3 {
        return Value::Null;
    }
    URL_SAFE_NO_PAD
        .decode(parts[1].trim_end_matches('='))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn response(account: &str, refresh: Option<&str>) -> Value {
        let claims = json!({ "https://api.openai.com/auth": { "chatgpt_account_id": account } });
        json!({
            "access_token": format!("header.{}.signature", URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap())),
            "refresh_token": refresh,
            "expires_in": 3600
        })
    }

    #[test]
    fn refresh_retains_tokens_but_cannot_silently_switch_subscription_accounts() {
        let first = parse_tokens(response("test-account", Some("refresh-one")), None).unwrap();

        let updated = parse_tokens(response("test-account", None), Some(&first)).unwrap();

        assert_eq!(updated.account_id.as_deref(), Some("test-account"));
        assert_eq!(updated.refresh.unwrap().0, "refresh-one");
        assert!(matches!(
            parse_tokens(response("other-account", Some("refresh-two")), Some(&first)),
            Err(AuthError::LoginRequired)
        ));
        assert!(parse_tokens(json!({ "access_token": "not-a-jwt", "refresh_token": "refresh", "expires_in": 3600 }), None).is_err());
        assert!(parse_tokens(response("test-account", None), None).is_err());
    }
}
