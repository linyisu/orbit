//! Verify OpenAI identity before persisting an account registration.
use super::ISSUER;
use crate::{
    AuthError, Result,
    http::{client, json_response},
};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header, jwk::JwkSet};
use serde::Deserialize;

const ID_TOKEN_CLOCK_SKEW_SECONDS: u64 = 5;

#[derive(Deserialize)]
pub(super) struct Identity {
    pub sub: String,
    pub email: Option<String>,
    iat: u64,
    nonce: Option<String>,
}

/// Fetch only the issuer's fixed JWKS URI; token headers never choose a key server.
pub(super) async fn verify(token: &str, audience: &str, nonce: Option<&str>) -> Result<Identity> {
    let response = client()?
        .get("https://auth.openai.com/.well-known/jwks.json")
        .send()
        .await
        .map_err(|_| AuthError::Transport)?;
    let keys: JwkSet = serde_json::from_value(json_response(response).await?)
        .map_err(|_| AuthError::InvalidToken)?;
    verify_with_keys(token, audience, nonce, &keys)
}

fn verify_with_keys(
    token: &str,
    audience: &str,
    nonce: Option<&str>,
    keys: &JwkSet,
) -> Result<Identity> {
    let header = decode_header(token).map_err(|_| AuthError::InvalidToken)?;
    if !matches!(
        header.alg,
        Algorithm::RS256 | Algorithm::ES256 | Algorithm::EdDSA
    ) {
        return Err(AuthError::InvalidToken);
    }
    let kid = header
        .kid
        .as_deref()
        .ok_or(AuthError::InvalidToken)?;
    let key = keys
        .find(kid)
        .ok_or(AuthError::InvalidToken)?;
    let key = DecodingKey::from_jwk(key).map_err(|_| AuthError::InvalidToken)?;
    let mut validation = Validation::new(header.alg);
    validation.set_issuer(&[ISSUER]);
    validation.set_audience(&[audience]);
    validation.set_required_spec_claims(&["iss", "aud", "sub", "exp", "iat"]);
    validation.leeway = ID_TOKEN_CLOCK_SKEW_SECONDS;
    validation.validate_nbf = true;
    let identity = decode::<Identity>(token, &key, &validation)
        .map_err(|_| AuthError::InvalidToken)?
        .claims;
    if identity.iat > crate::credential::now().saturating_add(ID_TOKEN_CLOCK_SKEW_SECONDS)
        || identity.sub.is_empty()
        || nonce.is_some_and(|nonce| identity.nonce.as_deref() != Some(nonce))
    {
        return Err(AuthError::InvalidToken);
    }
    Ok(identity)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{EncodingKey, Header, encode, jwk::Jwk};
    use serde_json::{Value, json};

    fn signed(claims: &Value, seed: u8) -> (String, JwkSet) {
        let mut der = vec![
            0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22,
            0x04, 0x20,
        ];
        der.extend((0..32).map(|index| seed.wrapping_add(index)));
        let encoding = EncodingKey::from_ed_der(&der);
        let mut key = Jwk::from_encoding_key(&encoding, Algorithm::EdDSA).unwrap();
        key.common.key_id = Some("generated-fixture".into());
        let mut header = Header::new(Algorithm::EdDSA);
        header.kid = key.common.key_id.clone();
        (
            encode(&header, claims, &encoding).unwrap(),
            JwkSet { keys: vec![key] },
        )
    }

    #[test]
    fn identity_requires_signature_issuer_audience_expiry_and_nonce() {
        let now = crate::credential::now();
        let valid = json!({
            "iss": ISSUER, "aud": "fixture-client", "sub": "fixture-subject",
            "iat": now, "exp": now + 300, "nonce": "fixture-nonce",
        });
        let (token, keys) = signed(&valid, 1);
        assert_eq!(
            verify_with_keys(&token, "fixture-client", Some("fixture-nonce"), &keys)
                .unwrap()
                .sub,
            "fixture-subject"
        );
        assert!(verify_with_keys(&token, "other-client", Some("fixture-nonce"), &keys).is_err());
        assert!(verify_with_keys(&token, "fixture-client", Some("other-nonce"), &keys).is_err());
        let (_, other_keys) = signed(&valid, 2);
        assert!(
            verify_with_keys(&token, "fixture-client", Some("fixture-nonce"), &other_keys).is_err()
        );
        for (field, value) in [
            ("iss", json!("https://issuer.example")),
            ("exp", json!(now - 60)),
            ("iat", json!(now + 60)),
            ("sub", json!("")),
            ("nonce", Value::Null),
        ] {
            let mut invalid = valid.clone();
            invalid[field] = value;
            let (token, keys) = signed(&invalid, 1);
            assert!(
                verify_with_keys(&token, "fixture-client", Some("fixture-nonce"), &keys).is_err(),
                "{field}"
            );
        }
    }
}
