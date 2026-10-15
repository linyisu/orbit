//! Per-attempt PKCE state shared by browser authorization flows.
use crate::{AuthError, Result, credential::Secret};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};
use url::Url;

pub(crate) struct Pkce {
    pub redirect: Url,
    pub state: String,
    pub verifier: Secret,
}

impl Pkce {
    pub fn new(redirect: Url) -> Result<Self> {
        Ok(Self {
            redirect,
            state: Self::random()?,
            verifier: Secret(Self::random()?),
        })
    }

    pub fn random() -> Result<String> {
        let mut bytes = [0; 32];
        getrandom::fill(&mut bytes)
            .map_err(|_| AuthError::Configuration("secure randomness unavailable"))?;
        Ok(URL_SAFE_NO_PAD.encode(bytes))
    }

    pub fn authorization_url(&self, endpoint: &str, client: &str, scope: &str) -> Url {
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(self.verifier.0.as_bytes()));
        let mut url = Url::parse(endpoint).expect("provider authorization endpoint");
        url.query_pairs_mut().extend_pairs([
            ("response_type", "code"),
            ("client_id", client),
            ("redirect_uri", self.redirect.as_str()),
            ("scope", scope),
            ("state", self.state.as_str()),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
        ]);
        url
    }

    pub fn callback_code(&self, callback: &str) -> Result<String> {
        super::callback::authorization_code(callback, &self.redirect, &self.state)
    }
}
