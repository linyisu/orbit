//! Controller connections and drafts for private platform storage and legacy encrypted vaults.
use super::Pairing;
use anyhow::{Result, anyhow, ensure};
use ring::{
    aead, pbkdf2,
    rand::{SecureRandom, SystemRandom},
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, num::NonZeroU32};

const ITERATIONS: u32 = 600_000;
const MAX_BYTES: usize = 4 * 1_024 * 1_024;
const AAD: &[u8] = b"orbit-controller-vault-v1";

#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contents {
    #[serde(default)]
    pub projects: BTreeMap<String, crate::projects::Projects>,
    pub devices: Vec<Pairing>,
    #[serde(default)]
    pub last_device: Option<String>,
    #[serde(default)]
    pub last_sessions: BTreeMap<String, String>,
    pub drafts: BTreeMap<String, String>,
    #[serde(default)]
    pub workspaces: BTreeMap<String, String>,
}

impl Contents {
    /// Stores one connection per authenticated host certificate, preserving the active grant.
    pub fn remember(&mut self, pairing: Pairing) {
        for old in self
            .devices
            .iter()
            .filter(|p| p.certificate == pairing.certificate)
        {
            if self.last_device.as_ref() == Some(&old.device) {
                self.last_device = Some(pairing.device.clone());
            }
            if old.device != pairing.device {
                if let Some(projects) = self.projects.remove(&old.device) {
                    self.projects
                        .entry(pairing.device.clone())
                        .or_insert(projects);
                }
                if let Some(session) = self.last_sessions.remove(&old.device) {
                    self.last_sessions
                        .insert(pairing.device.clone(), session);
                }
                self.workspaces.remove(&old.device);
                let prefix = format!("{}:{}", old.device.len(), old.device);
                let drafts: Vec<_> = self
                    .drafts
                    .keys()
                    .filter(|k| k.starts_with(&prefix))
                    .cloned()
                    .collect();
                for key in drafts {
                    let text = self.drafts.remove(&key).unwrap();
                    self.drafts
                        .entry(format!(
                            "{}:{}{}",
                            pairing.device.len(),
                            pairing.device,
                            &key[prefix.len()..]
                        ))
                        .or_insert(text);
                }
            }
        }
        self.devices
            .retain(|p| p.certificate != pairing.certificate);
        self.devices.push(pairing);
    }

    fn deduplicate(&mut self) {
        let devices = std::mem::take(&mut self.devices);
        for device in devices {
            self.remember(device);
        }
    }

    /// Remembers a server-reported project label, never an authorization scope.
    pub fn set_workspace(&mut self, device: &str, workspace: String) {
        if self
            .devices
            .iter()
            .any(|pairing| pairing.device == device)
        {
            self.workspaces
                .insert(device.to_owned(), workspace);
        }
    }

    pub fn workspace(&self, device: &str) -> Option<String> {
        self.workspaces.get(device).cloned()
    }

    /// Draft identities include the device grant so different hosts never share a draft.
    pub fn draft(&self, device: &str, session: &str) -> String {
        self.drafts
            .get(&format!("{}:{device}{session}", device.len()))
            .cloned()
            .unwrap_or_default()
    }

    pub fn set_draft(&mut self, device: &str, session: &str, text: String) {
        self.drafts
            .insert(format!("{}:{device}{session}", device.len()), text);
    }
}

/// Encode for OS-private application storage. The platform must exclude credentials from backups.
pub fn encode_private(contents: &Contents) -> Result<String> {
    let encoded = serde_json::to_string(contents)?;
    ensure!(
        encoded.len() <= MAX_BYTES,
        "saved connections exceed storage budget"
    );
    Ok(encoded)
}

pub fn decode_private(encoded: &str) -> Result<Contents> {
    ensure!(
        encoded.len() <= MAX_BYTES,
        "saved connections exceed storage budget"
    );
    let mut contents: Contents = serde_json::from_str(encoded)?;
    contents.deduplicate();
    Ok(contents)
}

#[derive(Serialize, Deserialize)]
struct Envelope {
    version: u8,
    salt: [u8; 16],
    nonce: [u8; 12],
    ciphertext: Vec<u8>,
}

fn key(password: &str, salt: &[u8]) -> Result<aead::LessSafeKey> {
    let mut bytes = [0; 32];
    pbkdf2::derive(
        pbkdf2::PBKDF2_HMAC_SHA256,
        NonZeroU32::new(ITERATIONS).unwrap(),
        salt,
        password.as_bytes(),
        &mut bytes,
    );
    let key = aead::UnboundKey::new(&aead::AES_256_GCM, &bytes)
        .map_err(|_| anyhow!("cannot initialize encrypted storage"))?;
    bytes.fill(0);
    Ok(aead::LessSafeKey::new(key))
}

/// Encrypts a complete snapshot with a fresh salt and nonce. Call away from the UI thread.
pub fn seal(contents: &Contents, password: &str) -> Result<String> {
    ensure!(password.chars().count() >= 8, "解锁密码至少需要 8 个字符");
    let random = SystemRandom::new();
    let mut envelope = Envelope {
        version: 1,
        salt: [0; 16],
        nonce: [0; 12],
        ciphertext: serde_json::to_vec(contents)?,
    };
    ensure!(
        envelope.ciphertext.len() <= MAX_BYTES,
        "saved connections exceed storage budget"
    );
    random
        .fill(&mut envelope.salt)
        .map_err(|_| anyhow!("random source unavailable"))?;
    random
        .fill(&mut envelope.nonce)
        .map_err(|_| anyhow!("random source unavailable"))?;
    key(password, &envelope.salt)?
        .seal_in_place_append_tag(
            aead::Nonce::assume_unique_for_key(envelope.nonce),
            aead::Aad::from(AAD),
            &mut envelope.ciphertext,
        )
        .map_err(|_| anyhow!("cannot encrypt saved connections"))?;
    Ok(serde_json::to_string(&envelope)?)
}

/// Authenticates before decoding; wrong passwords and damaged ciphertext have the same error.
pub fn open(encoded: &str, password: &str) -> Result<Contents> {
    ensure!(
        encoded.len() <= MAX_BYTES * 5,
        "saved connections exceed storage budget"
    );
    let mut envelope: Envelope = serde_json::from_str(encoded)?;
    ensure!(
        envelope.version == 1,
        "unsupported saved connections version"
    );
    let plaintext = key(password, &envelope.salt)?
        .open_in_place(
            aead::Nonce::assume_unique_for_key(envelope.nonce),
            aead::Aad::from(AAD),
            &mut envelope.ciphertext,
        )
        .map_err(|_| anyhow!("密码不正确，或保存的连接已损坏"))?;
    Ok(serde_json::from_slice(plaintext)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn duplicate_host_connections_keep_one_grant_and_restore_selection() {
        let first = Pairing {
            host_name: "Sample computer".into(),
            version: 1,
            address: "host".into(),
            certificate: vec![1, 2, 3],
            device: "first".into(),
            secret: "sample".into(),
        };
        let mut second = first.clone();
        second.device = "second".into();
        let mut projects = crate::projects::Projects::default();
        projects.set_pin("/sample-project", true);
        let contents = Contents {
            projects: BTreeMap::from([("first".into(), projects)]),
            devices: vec![first, second],
            last_device: Some("first".into()),
            ..Default::default()
        };
        let restored = decode_private(&encode_private(&contents).unwrap()).unwrap();
        assert_eq!(restored.devices.len(), 1);
        assert_eq!(restored.last_device.as_deref(), Some("second"));
        assert!(restored.projects["second"].is_pinned("/sample-project"));
        assert!(!restored.projects.contains_key("first"));
    }

    #[test]
    fn old_vaults_load_without_project_labels() {
        let contents: Contents = serde_json::from_str(r#"{"devices":[],"drafts":{}}"#).unwrap();
        assert_eq!(contents.workspace("unknown"), None);
        let mut contents = contents;
        contents.set_workspace("unknown", "/generated-project".into());
        assert_eq!(contents.workspace("unknown"), None);
    }

    #[test]
    fn drafts_are_scoped_to_device_and_session_without_ambiguous_concatenation() {
        let mut contents = Contents::default();
        contents.set_draft("a", "bc", "first".into());
        contents.set_draft("ab", "c", "second".into());
        assert_eq!(contents.draft("a", "bc"), "first");
        assert_eq!(contents.draft("ab", "c"), "second");
        assert!(contents.draft("other", "bc").is_empty());
    }
    #[test]
    fn encrypted_drafts_require_password_and_reject_tampering() {
        let contents = Contents {
            drafts: BTreeMap::from([("generated-session".into(), "synthetic draft".into())]),
            ..Default::default()
        };
        let encoded = seal(&contents, "test-password").unwrap();
        assert!(!encoded.contains("synthetic draft"));
        assert_eq!(
            open(&encoded, "test-password")
                .unwrap()
                .drafts,
            contents.drafts
        );
        assert!(open(&encoded, "wrong-password").is_err());
        let mut envelope: Envelope = serde_json::from_str(&encoded).unwrap();
        envelope.ciphertext[0] ^= 1;
        assert!(open(&serde_json::to_string(&envelope).unwrap(), "test-password").is_err());
    }
}
