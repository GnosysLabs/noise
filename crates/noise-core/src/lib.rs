use std::{
    collections::{HashMap, HashSet},
    time::{SystemTime, UNIX_EPOCH},
};

use argon2::{Algorithm, Argon2, Params, Version};
use base64::{Engine as _, engine::general_purpose::STANDARD_NO_PAD};
use chacha20poly1305::{
    KeyInit, XChaCha20Poly1305, XNonce,
    aead::{Aead, Payload},
};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand::random;
use serde::{Deserialize, Serialize};
use thiserror::Error;

const FREQUENCY_SPACE: u64 = 1_000_000_000_000;
const NOISE_ID_SPACE: u64 = 1_000_000_000_000;
const INVITE_KDF_CONTEXT: &str = "xyz.gnosyslabs.noise.frequency-invite.v1";
const DIRECT_KEY_CONTEXT: &str = "xyz.gnosyslabs.noise.direct-key.v1";
const DIRECT_MAILBOX_CONTEXT: &str = "xyz.gnosyslabs.noise.direct-mailbox.v1";
const DIRECT_SCOPE_CONTEXT: &str = "xyz.gnosyslabs.noise.direct-scope.v1";
pub const DEFAULT_GROUP_ACCENT_COLOR: &str = "#7758ED";

fn default_group_accent_color() -> String {
    DEFAULT_GROUP_ACCENT_COLOR.to_owned()
}

#[derive(Debug, Error)]
pub enum NoiseError {
    #[error("invalid {0} encoding")]
    InvalidEncoding(&'static str),
    #[error("invalid {0} length")]
    InvalidLength(&'static str),
    #[error("invalid signature")]
    InvalidSignature,
    #[error("cryptographic operation failed")]
    Crypto,
    #[error("invalid frequency")]
    InvalidFrequency,
    #[error("invalid Noise ID")]
    InvalidNoiseId,
    #[error("the invitation belongs to a different frequency")]
    FrequencyMismatch,
    #[error("the invitation issuer does not match its payload")]
    IdentityMismatch,
    #[error("the event belongs to a different group")]
    GroupMismatch,
    #[error("the encrypted blob does not match its identifier")]
    BlobMismatch,
    #[error("invalid group authority")]
    InvalidGroupAuthority,
    #[error("serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
}

#[derive(Clone)]
pub struct Identity {
    signing_key: SigningKey,
}

impl Identity {
    pub fn generate() -> Self {
        Self {
            signing_key: SigningKey::from_bytes(&random()),
        }
    }

    pub fn from_secret_base64(encoded: &str) -> Result<Self, NoiseError> {
        Ok(Self {
            signing_key: SigningKey::from_bytes(&decode_array(encoded, "identity secret")?),
        })
    }

    pub fn secret_base64(&self) -> String {
        STANDARD_NO_PAD.encode(self.signing_key.to_bytes())
    }

    pub fn public_key_base64(&self) -> String {
        STANDARD_NO_PAD.encode(self.signing_key.verifying_key().to_bytes())
    }

    fn sign(&self, bytes: &[u8]) -> String {
        STANDARD_NO_PAD.encode(self.signing_key.sign(bytes).to_bytes())
    }

    pub fn direct_mailbox(
        &self,
        peer_public_key: &str,
        mailbox_public_key: &str,
    ) -> Result<GroupMembership, NoiseError> {
        let self_public_key = self.public_key_base64();
        if mailbox_public_key != self_public_key && mailbox_public_key != peer_public_key {
            return Err(NoiseError::IdentityMismatch);
        }
        let peer_bytes = decode_array::<32>(peer_public_key, "direct peer public key")?;
        let peer =
            VerifyingKey::from_bytes(&peer_bytes).map_err(|_| NoiseError::InvalidSignature)?;
        let shared = peer
            .to_montgomery()
            .mul_clamped(self.signing_key.to_scalar_bytes())
            .to_bytes();
        if shared == [0; 32] {
            return Err(NoiseError::Crypto);
        }
        let mut participants = [
            decode_array::<32>(&self_public_key, "identity public key")?,
            peer_bytes,
        ];
        participants.sort();
        let mut material = Vec::with_capacity(96);
        material.extend_from_slice(&shared);
        material.extend_from_slice(&participants[0]);
        material.extend_from_slice(&participants[1]);
        let secret = blake3::derive_key(DIRECT_KEY_CONTEXT, &material);
        Ok(GroupMembership {
            group_id: direct_mailbox_id(mailbox_public_key)?,
            name: String::new(),
            description: String::new(),
            rules: String::new(),
            avatar: None,
            background: None,
            accent_color: default_group_accent_color(),
            members_can_send_messages: true,
            members_can_send_media: true,
            owner_public_key: String::new(),
            authority_nonce_base64: String::new(),
            secret_base64: STANDARD_NO_PAD.encode(secret),
        })
    }

    pub fn direct_scope_id(&self, peer_public_key: &str) -> Result<String, NoiseError> {
        direct_scope_id(&self.public_key_base64(), peer_public_key)
    }
}

pub fn direct_mailbox_id(public_key: &str) -> Result<String, NoiseError> {
    let bytes = decode_array::<32>(public_key, "direct mailbox public key")?;
    VerifyingKey::from_bytes(&bytes).map_err(|_| NoiseError::InvalidSignature)?;
    Ok(blake3::derive_key(DIRECT_MAILBOX_CONTEXT, &bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

pub fn direct_scope_id(
    left_public_key: &str,
    right_public_key: &str,
) -> Result<String, NoiseError> {
    let mut participants = [
        decode_array::<32>(left_public_key, "direct participant public key")?,
        decode_array::<32>(right_public_key, "direct participant public key")?,
    ];
    VerifyingKey::from_bytes(&participants[0]).map_err(|_| NoiseError::InvalidSignature)?;
    VerifyingKey::from_bytes(&participants[1]).map_err(|_| NoiseError::InvalidSignature)?;
    participants.sort();
    let mut material = Vec::with_capacity(64);
    material.extend_from_slice(&participants[0]);
    material.extend_from_slice(&participants[1]);
    Ok(blake3::derive_key(DIRECT_SCOPE_CONTEXT, &material)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

pub fn direct_message_id(author_public_key: &str, author_sequence: u64) -> String {
    let mut hasher = blake3::Hasher::new_derive_key("xyz.gnosyslabs.noise.direct-message.v1");
    hasher.update(author_public_key.as_bytes());
    hasher.update(&author_sequence.to_be_bytes());
    hasher.finalize().to_hex().to_string()
}

#[derive(Clone, Debug)]
pub struct AccountCredentials {
    pub noise_id: String,
    pub locator: String,
    pub vault_key_base64: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccountVault {
    pub locator: String,
    pub revision: u64,
    pub identity_public_key: String,
    pub nonce_base64: String,
    pub ciphertext_base64: String,
    #[serde(default)]
    pub deleted: bool,
    pub signature_base64: String,
}

#[derive(Serialize)]
struct UnsignedAccountVault<'a> {
    locator: &'a str,
    revision: u64,
    identity_public_key: &'a str,
    nonce_base64: &'a str,
    ciphertext_base64: &'a str,
    deleted: bool,
}

impl AccountVault {
    pub fn seal(
        identity: &Identity,
        credentials: &AccountCredentials,
        revision: u64,
        plaintext: &[u8],
    ) -> Result<Self, NoiseError> {
        if revision == 0 || plaintext.is_empty() || plaintext.len() > 2_000_000 {
            return Err(NoiseError::Crypto);
        }
        let nonce: [u8; 24] = random();
        let identity_public_key = identity.public_key_base64();
        let key = decode_array::<32>(&credentials.vault_key_base64, "account vault key")?;
        let aad = account_vault_aad(&credentials.locator, revision, &identity_public_key, false);
        let ciphertext = XChaCha20Poly1305::new((&key).into())
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: plaintext,
                    aad: &aad,
                },
            )
            .map_err(|_| NoiseError::Crypto)?;
        let mut vault = Self {
            locator: credentials.locator.clone(),
            revision,
            identity_public_key,
            nonce_base64: STANDARD_NO_PAD.encode(nonce),
            ciphertext_base64: STANDARD_NO_PAD.encode(ciphertext),
            deleted: false,
            signature_base64: String::new(),
        };
        vault.signature_base64 = identity.sign(&vault.signing_bytes()?);
        Ok(vault)
    }

    pub fn tombstone(
        identity: &Identity,
        locator: impl Into<String>,
        revision: u64,
    ) -> Result<Self, NoiseError> {
        if revision == 0 {
            return Err(NoiseError::Crypto);
        }
        let mut vault = Self {
            locator: locator.into(),
            revision,
            identity_public_key: identity.public_key_base64(),
            nonce_base64: String::new(),
            ciphertext_base64: String::new(),
            deleted: true,
            signature_base64: String::new(),
        };
        vault.signature_base64 = identity.sign(&vault.signing_bytes()?);
        Ok(vault)
    }

    pub fn verify(&self) -> Result<(), NoiseError> {
        if self.revision == 0
            || self.locator.len() != 64
            || !self.locator.bytes().all(|byte| byte.is_ascii_hexdigit())
            || (self.deleted
                && (!self.nonce_base64.is_empty() || !self.ciphertext_base64.is_empty()))
            || (!self.deleted
                && (decode_array::<24>(&self.nonce_base64, "account vault nonce").is_err()
                    || self.ciphertext_base64.is_empty()
                    || self.ciphertext_base64.len() > 2_700_000))
        {
            return Err(NoiseError::Crypto);
        }
        verify_signature(
            &self.identity_public_key,
            &self.signature_base64,
            &self.signing_bytes()?,
        )
    }

    pub fn open(&self, credentials: &AccountCredentials) -> Result<Vec<u8>, NoiseError> {
        self.verify()?;
        if self.deleted || self.locator != credentials.locator {
            return Err(NoiseError::Crypto);
        }
        let nonce = decode_array::<24>(&self.nonce_base64, "account vault nonce")?;
        let ciphertext = decode(&self.ciphertext_base64, "account vault ciphertext")?;
        let key = decode_array::<32>(&credentials.vault_key_base64, "account vault key")?;
        let aad = account_vault_aad(
            &self.locator,
            self.revision,
            &self.identity_public_key,
            false,
        );
        XChaCha20Poly1305::new((&key).into())
            .decrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: &ciphertext,
                    aad: &aad,
                },
            )
            .map_err(|_| NoiseError::Crypto)
    }

    fn signing_bytes(&self) -> Result<Vec<u8>, NoiseError> {
        Ok(serde_json::to_vec(&UnsignedAccountVault {
            locator: &self.locator,
            revision: self.revision,
            identity_public_key: &self.identity_public_key,
            nonce_base64: &self.nonce_base64,
            ciphertext_base64: &self.ciphertext_base64,
            deleted: self.deleted,
        })?)
    }
}

pub fn generate_noise_id() -> String {
    format!("{:012}", random::<u64>() % NOISE_ID_SPACE)
}

pub fn normalize_noise_id(value: &str) -> Result<String, NoiseError> {
    let digits: String = value
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    if digits.len() != 12 || !digits.chars().all(|character| character.is_ascii_digit()) {
        return Err(NoiseError::InvalidNoiseId);
    }
    Ok(digits)
}

pub fn display_noise_id(value: &str) -> Result<String, NoiseError> {
    let digits = normalize_noise_id(value)?;
    Ok(format!(
        "{} {} {}",
        &digits[0..4],
        &digits[4..8],
        &digits[8..12]
    ))
}

pub fn derive_account_credentials(
    noise_id: &str,
    password: &str,
) -> Result<AccountCredentials, NoiseError> {
    let noise_id = normalize_noise_id(noise_id)?;
    let salt = blake3::derive_key("xyz.gnosyslabs.noise.account-salt.v1", noise_id.as_bytes());
    let params = Params::new(64 * 1024, 3, 1, Some(64)).map_err(|_| NoiseError::Crypto)?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut master = [0u8; 64];
    argon
        .hash_password_into(password.as_bytes(), &salt[..16], &mut master)
        .map_err(|_| NoiseError::Crypto)?;
    let locator = blake3::derive_key("xyz.gnosyslabs.noise.account-locator.v1", &master)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let key = blake3::derive_key("xyz.gnosyslabs.noise.account-vault-key.v1", &master);
    master.fill(0);
    Ok(AccountCredentials {
        noise_id,
        locator,
        vault_key_base64: STANDARD_NO_PAD.encode(key),
    })
}

fn account_vault_aad(
    locator: &str,
    revision: u64,
    identity_public_key: &str,
    deleted: bool,
) -> Vec<u8> {
    format!("noise-account-v1:{locator}:{revision}:{identity_public_key}:{deleted}").into_bytes()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Profile {
    pub username: String,
    #[serde(default)]
    pub bio: String,
    #[serde(default)]
    pub avatar: Option<ProfileImage>,
    #[serde(default = "default_true")]
    pub accepts_direct_messages: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileImage {
    pub blob_id: String,
    pub key_base64: String,
    pub mime_type: String,
    pub byte_length: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaChunk {
    pub blob_id: String,
    pub key_base64: String,
    pub byte_length: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaAttachment {
    pub file_name: String,
    pub mime_type: String,
    pub byte_length: u64,
    pub chunks: Vec<MediaChunk>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EncryptedBlob {
    pub blob_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
    pub nonce_base64: String,
    pub ciphertext_base64: String,
}

impl EncryptedBlob {
    pub fn create(plaintext: &[u8]) -> Result<(Self, String), NoiseError> {
        Self::create_inner(plaintext, None)
    }

    pub fn create_for_group(
        plaintext: &[u8],
        group_id: impl Into<String>,
    ) -> Result<(Self, String), NoiseError> {
        Self::create_inner(plaintext, Some(group_id.into()))
    }

    fn create_inner(
        plaintext: &[u8],
        group_id: Option<String>,
    ) -> Result<(Self, String), NoiseError> {
        let key: [u8; 32] = random();
        let nonce: [u8; 24] = random();
        let ciphertext = XChaCha20Poly1305::new((&key).into())
            .encrypt(XNonce::from_slice(&nonce), plaintext)
            .map_err(|_| NoiseError::Crypto)?;
        let blob_id = blob_id(group_id.as_deref(), &nonce, &ciphertext);
        Ok((
            Self {
                blob_id,
                group_id,
                nonce_base64: STANDARD_NO_PAD.encode(nonce),
                ciphertext_base64: STANDARD_NO_PAD.encode(ciphertext),
            },
            STANDARD_NO_PAD.encode(key),
        ))
    }

    pub fn verify(&self) -> Result<(), NoiseError> {
        let nonce = decode_array::<24>(&self.nonce_base64, "blob nonce")?;
        let ciphertext = decode(&self.ciphertext_base64, "blob ciphertext")?;
        if blob_id(self.group_id.as_deref(), &nonce, &ciphertext) != self.blob_id {
            return Err(NoiseError::BlobMismatch);
        }
        Ok(())
    }

    pub fn open(&self, key_base64: &str) -> Result<Vec<u8>, NoiseError> {
        self.verify()?;
        let key = decode_array::<32>(key_base64, "blob key")?;
        let nonce = decode_array::<24>(&self.nonce_base64, "blob nonce")?;
        let ciphertext = decode(&self.ciphertext_base64, "blob ciphertext")?;
        XChaCha20Poly1305::new((&key).into())
            .decrypt(XNonce::from_slice(&nonce), ciphertext.as_ref())
            .map_err(|_| NoiseError::Crypto)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupProfile {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub rules: String,
    #[serde(default)]
    pub avatar: Option<ProfileImage>,
    #[serde(default)]
    pub background: Option<ProfileImage>,
    #[serde(default = "default_group_accent_color")]
    pub accent_color: String,
    #[serde(default = "default_true")]
    pub members_can_send_messages: bool,
    #[serde(default = "default_true")]
    pub members_can_send_media: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupMembership {
    pub group_id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub rules: String,
    #[serde(default)]
    pub avatar: Option<ProfileImage>,
    #[serde(default)]
    pub background: Option<ProfileImage>,
    #[serde(default = "default_group_accent_color")]
    pub accent_color: String,
    #[serde(default = "default_true")]
    pub members_can_send_messages: bool,
    #[serde(default = "default_true")]
    pub members_can_send_media: bool,
    #[serde(default)]
    pub owner_public_key: String,
    #[serde(default)]
    pub authority_nonce_base64: String,
    pub secret_base64: String,
}

impl GroupMembership {
    pub fn create(name: impl Into<String>) -> Self {
        let random_id: [u8; 32] = random();
        let secret: [u8; 32] = random();
        Self {
            group_id: blake3::hash(&random_id).to_hex().to_string(),
            name: name.into(),
            description: String::new(),
            rules: String::new(),
            avatar: None,
            background: None,
            accent_color: default_group_accent_color(),
            members_can_send_messages: true,
            members_can_send_media: true,
            owner_public_key: String::new(),
            authority_nonce_base64: String::new(),
            secret_base64: STANDARD_NO_PAD.encode(secret),
        }
    }

    pub fn create_owned(name: impl Into<String>, owner_public_key: impl Into<String>) -> Self {
        let owner_public_key = owner_public_key.into();
        let authority_nonce: [u8; 32] = random();
        let secret: [u8; 32] = random();
        Self {
            group_id: authoritative_group_id(&owner_public_key, &authority_nonce),
            name: name.into(),
            description: String::new(),
            rules: String::new(),
            avatar: None,
            background: None,
            accent_color: default_group_accent_color(),
            members_can_send_messages: true,
            members_can_send_media: true,
            owner_public_key,
            authority_nonce_base64: STANDARD_NO_PAD.encode(authority_nonce),
            secret_base64: STANDARD_NO_PAD.encode(secret),
        }
    }

    pub fn profile(&self) -> GroupProfile {
        GroupProfile {
            name: self.name.clone(),
            description: self.description.clone(),
            rules: self.rules.clone(),
            avatar: self.avatar.clone(),
            background: self.background.clone(),
            accent_color: self.accent_color.clone(),
            members_can_send_messages: self.members_can_send_messages,
            members_can_send_media: self.members_can_send_media,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InvitePayload {
    pub group: GroupMembership,
    pub created_by: String,
    pub created_at_millis: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InviteRecord {
    pub locator: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
    pub salt_base64: String,
    pub nonce_base64: String,
    pub ciphertext_base64: String,
    pub issuer_public_key: String,
    pub signature_base64: String,
}

#[derive(Serialize)]
struct UnsignedInviteRecordV1<'a> {
    locator: &'a str,
    salt_base64: &'a str,
    nonce_base64: &'a str,
    ciphertext_base64: &'a str,
    issuer_public_key: &'a str,
}

#[derive(Serialize)]
struct UnsignedInviteRecordV2<'a> {
    locator: &'a str,
    group_id: &'a str,
    salt_base64: &'a str,
    nonce_base64: &'a str,
    ciphertext_base64: &'a str,
    issuer_public_key: &'a str,
}

impl InviteRecord {
    pub fn create(
        identity: &Identity,
        frequency: &str,
        group: GroupMembership,
    ) -> Result<Self, NoiseError> {
        let frequency = normalize_frequency(frequency)?;
        let locator = frequency_locator(&frequency);
        let salt: [u8; 16] = random();
        let nonce: [u8; 24] = random();
        let key = frequency_key(&frequency, &salt);
        let issuer_public_key = identity.public_key_base64();
        let payload = InvitePayload {
            group,
            created_by: issuer_public_key.clone(),
            created_at_millis: now_millis(),
        };
        let plaintext = serde_json::to_vec(&payload)?;
        let ciphertext = XChaCha20Poly1305::new((&key).into())
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: &plaintext,
                    aad: locator.as_bytes(),
                },
            )
            .map_err(|_| NoiseError::Crypto)?;

        let mut record = Self {
            locator,
            group_id: Some(payload.group.group_id.clone()),
            salt_base64: STANDARD_NO_PAD.encode(salt),
            nonce_base64: STANDARD_NO_PAD.encode(nonce),
            ciphertext_base64: STANDARD_NO_PAD.encode(ciphertext),
            issuer_public_key,
            signature_base64: String::new(),
        };
        record.signature_base64 = identity.sign(&record.signing_bytes()?);
        Ok(record)
    }

    pub fn verify(&self) -> Result<(), NoiseError> {
        verify_signature(
            &self.issuer_public_key,
            &self.signature_base64,
            &self.signing_bytes()?,
        )
    }

    pub fn open(&self, frequency: &str) -> Result<InvitePayload, NoiseError> {
        self.verify()?;
        let frequency = normalize_frequency(frequency)?;
        if frequency_locator(&frequency) != self.locator {
            return Err(NoiseError::FrequencyMismatch);
        }
        let salt = decode_array::<16>(&self.salt_base64, "invitation salt")?;
        let nonce = decode_array::<24>(&self.nonce_base64, "invitation nonce")?;
        let ciphertext = decode(&self.ciphertext_base64, "invitation ciphertext")?;
        let key = frequency_key(&frequency, &salt);
        let plaintext = XChaCha20Poly1305::new((&key).into())
            .decrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: &ciphertext,
                    aad: self.locator.as_bytes(),
                },
            )
            .map_err(|_| NoiseError::Crypto)?;
        let payload: InvitePayload = serde_json::from_slice(&plaintext)?;
        if payload.created_by != self.issuer_public_key {
            return Err(NoiseError::IdentityMismatch);
        }
        if self
            .group_id
            .as_deref()
            .is_some_and(|group_id| group_id != payload.group.group_id)
        {
            return Err(NoiseError::GroupMismatch);
        }
        Ok(payload)
    }

    fn signing_bytes(&self) -> Result<Vec<u8>, NoiseError> {
        if let Some(group_id) = self.group_id.as_deref() {
            Ok(serde_json::to_vec(&UnsignedInviteRecordV2 {
                locator: &self.locator,
                group_id,
                salt_base64: &self.salt_base64,
                nonce_base64: &self.nonce_base64,
                ciphertext_base64: &self.ciphertext_base64,
                issuer_public_key: &self.issuer_public_key,
            })?)
        } else {
            Ok(serde_json::to_vec(&UnsignedInviteRecordV1 {
                locator: &self.locator,
                salt_base64: &self.salt_base64,
                nonce_base64: &self.nonce_base64,
                ciphertext_base64: &self.ciphertext_base64,
                issuer_public_key: &self.issuer_public_key,
            })?)
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InviteRotation {
    pub group_id: String,
    pub owner_public_key: String,
    pub authority_nonce_base64: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_invite: Option<InviteRecord>,
    pub owner_sequence: u64,
    pub rotated_at_millis: u64,
    pub signature_base64: String,
}

#[derive(Serialize)]
struct UnsignedInviteRotation<'a> {
    operation: &'static str,
    group_id: &'a str,
    owner_public_key: &'a str,
    authority_nonce_base64: &'a str,
    new_invite: Option<&'a InviteRecord>,
    owner_sequence: u64,
    rotated_at_millis: u64,
}

impl InviteRotation {
    pub fn create(
        identity: &Identity,
        group: &GroupMembership,
        new_invite: Option<InviteRecord>,
        owner_sequence: u64,
    ) -> Result<Self, NoiseError> {
        if group.owner_public_key != identity.public_key_base64()
            || group.authority_nonce_base64.is_empty()
        {
            return Err(NoiseError::InvalidGroupAuthority);
        }
        let mut rotation = Self {
            group_id: group.group_id.clone(),
            owner_public_key: group.owner_public_key.clone(),
            authority_nonce_base64: group.authority_nonce_base64.clone(),
            new_invite,
            owner_sequence,
            rotated_at_millis: now_millis(),
            signature_base64: String::new(),
        };
        rotation.verify_authority()?;
        rotation.signature_base64 = identity.sign(&rotation.signing_bytes()?);
        Ok(rotation)
    }

    pub fn verify(&self) -> Result<(), NoiseError> {
        self.verify_authority()?;
        verify_signature(
            &self.owner_public_key,
            &self.signature_base64,
            &self.signing_bytes()?,
        )
    }

    fn verify_authority(&self) -> Result<(), NoiseError> {
        let authority_nonce =
            decode_array::<32>(&self.authority_nonce_base64, "group authority nonce")?;
        if authoritative_group_id(&self.owner_public_key, &authority_nonce) != self.group_id {
            return Err(NoiseError::InvalidGroupAuthority);
        }
        if let Some(invite) = self.new_invite.as_ref() {
            invite.verify()?;
            if invite.group_id.as_deref() != Some(self.group_id.as_str())
                || invite.issuer_public_key != self.owner_public_key
            {
                return Err(NoiseError::InvalidGroupAuthority);
            }
        }
        Ok(())
    }

    fn signing_bytes(&self) -> Result<Vec<u8>, NoiseError> {
        Ok(serde_json::to_vec(&UnsignedInviteRotation {
            operation: "rotate_invite_v1",
            group_id: &self.group_id,
            owner_public_key: &self.owner_public_key,
            authority_nonce_base64: &self.authority_nonce_base64,
            new_invite: self.new_invite.as_ref(),
            owner_sequence: self.owner_sequence,
            rotated_at_millis: self.rotated_at_millis,
        })?)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GroupDeletion {
    pub group_id: String,
    pub owner_public_key: String,
    pub authority_nonce_base64: String,
    pub deleted_at_millis: u64,
    pub signature_base64: String,
}

#[derive(Serialize)]
struct UnsignedGroupDeletion<'a> {
    operation: &'static str,
    group_id: &'a str,
    owner_public_key: &'a str,
    authority_nonce_base64: &'a str,
    deleted_at_millis: u64,
}

impl GroupDeletion {
    pub fn create(identity: &Identity, group: &GroupMembership) -> Result<Self, NoiseError> {
        let owner_public_key = identity.public_key_base64();
        if group.owner_public_key != owner_public_key {
            return Err(NoiseError::InvalidGroupAuthority);
        }
        let authority_nonce =
            decode_array::<32>(&group.authority_nonce_base64, "group authority nonce")?;
        if authoritative_group_id(&owner_public_key, &authority_nonce) != group.group_id {
            return Err(NoiseError::InvalidGroupAuthority);
        }
        let mut deletion = Self {
            group_id: group.group_id.clone(),
            owner_public_key,
            authority_nonce_base64: group.authority_nonce_base64.clone(),
            deleted_at_millis: now_millis(),
            signature_base64: String::new(),
        };
        deletion.signature_base64 = identity.sign(&deletion.signing_bytes()?);
        Ok(deletion)
    }

    pub fn verify(&self) -> Result<(), NoiseError> {
        let authority_nonce =
            decode_array::<32>(&self.authority_nonce_base64, "group authority nonce")?;
        if authoritative_group_id(&self.owner_public_key, &authority_nonce) != self.group_id {
            return Err(NoiseError::InvalidGroupAuthority);
        }
        verify_signature(
            &self.owner_public_key,
            &self.signature_base64,
            &self.signing_bytes()?,
        )
    }

    fn signing_bytes(&self) -> Result<Vec<u8>, NoiseError> {
        Ok(serde_json::to_vec(&UnsignedGroupDeletion {
            operation: "delete_group_v1",
            group_id: &self.group_id,
            owner_public_key: &self.owner_public_key,
            authority_nonce_base64: &self.authority_nonce_base64,
            deleted_at_millis: self.deleted_at_millis,
        })?)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GroupEventPayload {
    MemberJoined {
        username: String,
        #[serde(default)]
        bio: String,
        #[serde(default)]
        avatar: Option<ProfileImage>,
        #[serde(default = "default_true")]
        accepts_direct_messages: bool,
    },
    ProfileUpdated {
        profile: Profile,
    },
    GroupProfileUpdated {
        profile: GroupProfile,
    },
    ModeratorSet {
        member_public_key: String,
        enabled: bool,
    },
    MessageDeleted {
        message_event_id: String,
    },
    MessageReported {
        message_event_id: String,
        #[serde(default)]
        reason: String,
    },
    ReportResolved {
        report_event_id: String,
    },
    OwnMessagesDeleted,
    MemberBanned {
        member_public_key: String,
        delete_messages: bool,
    },
    MemberUnbanned {
        member_public_key: String,
    },
    DirectMessage {
        recipient_public_key: String,
        sender_profile: Profile,
        text: String,
        #[serde(default)]
        attachment: Option<MediaAttachment>,
        #[serde(default)]
        reply_to_message_id: Option<String>,
    },
    DirectThreadDeleted {
        recipient_public_key: String,
    },
    MemberLeft,
    Message {
        text: String,
        #[serde(default)]
        attachment: Option<MediaAttachment>,
        #[serde(default)]
        reply_to_message_id: Option<String>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SignedEvent {
    pub event_id: String,
    pub group_id: String,
    pub author_public_key: String,
    pub author_sequence: u64,
    pub created_at_millis: u64,
    pub nonce_base64: String,
    pub ciphertext_base64: String,
    pub signature_base64: String,
}

#[derive(Serialize)]
struct UnsignedEvent<'a> {
    group_id: &'a str,
    author_public_key: &'a str,
    author_sequence: u64,
    created_at_millis: u64,
    nonce_base64: &'a str,
    ciphertext_base64: &'a str,
}

impl SignedEvent {
    pub fn member_joined(
        identity: &Identity,
        group: &GroupMembership,
        profile: &Profile,
        author_sequence: u64,
    ) -> Result<Self, NoiseError> {
        Self::create(
            identity,
            group,
            GroupEventPayload::MemberJoined {
                username: profile.username.clone(),
                bio: profile.bio.clone(),
                avatar: profile.avatar.clone(),
                accepts_direct_messages: profile.accepts_direct_messages,
            },
            author_sequence,
        )
    }

    pub fn profile_updated(
        identity: &Identity,
        group: &GroupMembership,
        profile: &Profile,
        author_sequence: u64,
    ) -> Result<Self, NoiseError> {
        Self::create(
            identity,
            group,
            GroupEventPayload::ProfileUpdated {
                profile: profile.clone(),
            },
            author_sequence,
        )
    }

    pub fn group_profile_updated(
        identity: &Identity,
        group: &GroupMembership,
        profile: &GroupProfile,
        author_sequence: u64,
    ) -> Result<Self, NoiseError> {
        Self::create(
            identity,
            group,
            GroupEventPayload::GroupProfileUpdated {
                profile: profile.clone(),
            },
            author_sequence,
        )
    }

    pub fn member_left(
        identity: &Identity,
        group: &GroupMembership,
        author_sequence: u64,
    ) -> Result<Self, NoiseError> {
        Self::create(
            identity,
            group,
            GroupEventPayload::MemberLeft,
            author_sequence,
        )
    }

    pub fn moderator_set(
        identity: &Identity,
        group: &GroupMembership,
        member_public_key: impl Into<String>,
        enabled: bool,
        author_sequence: u64,
    ) -> Result<Self, NoiseError> {
        Self::create(
            identity,
            group,
            GroupEventPayload::ModeratorSet {
                member_public_key: member_public_key.into(),
                enabled,
            },
            author_sequence,
        )
    }

    pub fn message_deleted(
        identity: &Identity,
        group: &GroupMembership,
        message_event_id: impl Into<String>,
        author_sequence: u64,
    ) -> Result<Self, NoiseError> {
        Self::create(
            identity,
            group,
            GroupEventPayload::MessageDeleted {
                message_event_id: message_event_id.into(),
            },
            author_sequence,
        )
    }

    pub fn own_messages_deleted(
        identity: &Identity,
        group: &GroupMembership,
        author_sequence: u64,
    ) -> Result<Self, NoiseError> {
        Self::create(
            identity,
            group,
            GroupEventPayload::OwnMessagesDeleted,
            author_sequence,
        )
    }

    pub fn message_reported(
        identity: &Identity,
        group: &GroupMembership,
        message_event_id: impl Into<String>,
        reason: impl Into<String>,
        author_sequence: u64,
    ) -> Result<Self, NoiseError> {
        Self::create(
            identity,
            group,
            GroupEventPayload::MessageReported {
                message_event_id: message_event_id.into(),
                reason: reason.into(),
            },
            author_sequence,
        )
    }

    pub fn report_resolved(
        identity: &Identity,
        group: &GroupMembership,
        report_event_id: impl Into<String>,
        author_sequence: u64,
    ) -> Result<Self, NoiseError> {
        Self::create(
            identity,
            group,
            GroupEventPayload::ReportResolved {
                report_event_id: report_event_id.into(),
            },
            author_sequence,
        )
    }

    pub fn member_banned(
        identity: &Identity,
        group: &GroupMembership,
        member_public_key: impl Into<String>,
        delete_messages: bool,
        author_sequence: u64,
    ) -> Result<Self, NoiseError> {
        Self::create(
            identity,
            group,
            GroupEventPayload::MemberBanned {
                member_public_key: member_public_key.into(),
                delete_messages,
            },
            author_sequence,
        )
    }

    pub fn member_unbanned(
        identity: &Identity,
        group: &GroupMembership,
        member_public_key: impl Into<String>,
        author_sequence: u64,
    ) -> Result<Self, NoiseError> {
        Self::create(
            identity,
            group,
            GroupEventPayload::MemberUnbanned {
                member_public_key: member_public_key.into(),
            },
            author_sequence,
        )
    }

    pub fn direct_message(
        identity: &Identity,
        mailbox: &GroupMembership,
        recipient_public_key: impl Into<String>,
        sender_profile: &Profile,
        text: impl Into<String>,
        attachment: Option<MediaAttachment>,
        reply_to_message_id: Option<String>,
        author_sequence: u64,
    ) -> Result<Self, NoiseError> {
        Self::create(
            identity,
            mailbox,
            GroupEventPayload::DirectMessage {
                recipient_public_key: recipient_public_key.into(),
                sender_profile: sender_profile.clone(),
                text: text.into(),
                attachment,
                reply_to_message_id,
            },
            author_sequence,
        )
    }

    pub fn direct_thread_deleted(
        identity: &Identity,
        mailbox: &GroupMembership,
        recipient_public_key: impl Into<String>,
        author_sequence: u64,
    ) -> Result<Self, NoiseError> {
        Self::create(
            identity,
            mailbox,
            GroupEventPayload::DirectThreadDeleted {
                recipient_public_key: recipient_public_key.into(),
            },
            author_sequence,
        )
    }

    pub fn chat(
        identity: &Identity,
        group: &GroupMembership,
        text: impl Into<String>,
        author_sequence: u64,
    ) -> Result<Self, NoiseError> {
        Self::chat_reply(identity, group, text, None, author_sequence)
    }

    pub fn chat_reply(
        identity: &Identity,
        group: &GroupMembership,
        text: impl Into<String>,
        reply_to_message_id: Option<String>,
        author_sequence: u64,
    ) -> Result<Self, NoiseError> {
        Self::create(
            identity,
            group,
            GroupEventPayload::Message {
                text: text.into(),
                attachment: None,
                reply_to_message_id,
            },
            author_sequence,
        )
    }

    pub fn chat_with_attachment(
        identity: &Identity,
        group: &GroupMembership,
        text: impl Into<String>,
        attachment: MediaAttachment,
        author_sequence: u64,
    ) -> Result<Self, NoiseError> {
        Self::chat_with_attachment_reply(identity, group, text, attachment, None, author_sequence)
    }

    pub fn chat_with_attachment_reply(
        identity: &Identity,
        group: &GroupMembership,
        text: impl Into<String>,
        attachment: MediaAttachment,
        reply_to_message_id: Option<String>,
        author_sequence: u64,
    ) -> Result<Self, NoiseError> {
        Self::create(
            identity,
            group,
            GroupEventPayload::Message {
                text: text.into(),
                attachment: Some(attachment),
                reply_to_message_id,
            },
            author_sequence,
        )
    }

    fn create(
        identity: &Identity,
        group: &GroupMembership,
        payload: GroupEventPayload,
        author_sequence: u64,
    ) -> Result<Self, NoiseError> {
        let group_key = decode_array::<32>(&group.secret_base64, "group secret")?;
        let nonce: [u8; 24] = random();
        let author_public_key = identity.public_key_base64();
        let plaintext = serde_json::to_vec(&payload)?;
        let ciphertext = XChaCha20Poly1305::new((&group_key).into())
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: &plaintext,
                    aad: group.group_id.as_bytes(),
                },
            )
            .map_err(|_| NoiseError::Crypto)?;

        let mut event = Self {
            event_id: String::new(),
            group_id: group.group_id.clone(),
            author_public_key,
            author_sequence,
            created_at_millis: now_millis(),
            nonce_base64: STANDARD_NO_PAD.encode(nonce),
            ciphertext_base64: STANDARD_NO_PAD.encode(ciphertext),
            signature_base64: String::new(),
        };
        let signing_bytes = event.signing_bytes()?;
        event.signature_base64 = identity.sign(&signing_bytes);
        event.event_id = event.calculate_id(&signing_bytes)?;
        Ok(event)
    }

    pub fn verify(&self) -> Result<(), NoiseError> {
        let signing_bytes = self.signing_bytes()?;
        verify_signature(
            &self.author_public_key,
            &self.signature_base64,
            &signing_bytes,
        )?;
        if self.calculate_id(&signing_bytes)? != self.event_id {
            return Err(NoiseError::InvalidSignature);
        }
        Ok(())
    }

    pub fn decrypt(&self, group: &GroupMembership) -> Result<GroupEventPayload, NoiseError> {
        self.verify()?;
        if self.group_id != group.group_id {
            return Err(NoiseError::GroupMismatch);
        }
        let group_key = decode_array::<32>(&group.secret_base64, "group secret")?;
        let nonce = decode_array::<24>(&self.nonce_base64, "message nonce")?;
        let ciphertext = decode(&self.ciphertext_base64, "message ciphertext")?;
        let plaintext = XChaCha20Poly1305::new((&group_key).into())
            .decrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: &ciphertext,
                    aad: self.group_id.as_bytes(),
                },
            )
            .map_err(|_| NoiseError::Crypto)?;
        Ok(serde_json::from_slice(&plaintext)?)
    }

    fn signing_bytes(&self) -> Result<Vec<u8>, NoiseError> {
        Ok(serde_json::to_vec(&UnsignedEvent {
            group_id: &self.group_id,
            author_public_key: &self.author_public_key,
            author_sequence: self.author_sequence,
            created_at_millis: self.created_at_millis,
            nonce_base64: &self.nonce_base64,
            ciphertext_base64: &self.ciphertext_base64,
        })?)
    }

    fn calculate_id(&self, signing_bytes: &[u8]) -> Result<String, NoiseError> {
        let signature = decode(&self.signature_base64, "event signature")?;
        let mut hasher = blake3::Hasher::new();
        hasher.update(signing_bytes);
        hasher.update(&signature);
        Ok(hasher.finalize().to_hex().to_string())
    }
}

#[derive(Clone, Debug)]
pub struct MemberState {
    pub public_key: String,
    pub username: String,
    pub bio: String,
    pub avatar: Option<ProfileImage>,
    pub accepts_direct_messages: bool,
    pub joined_at_millis: u64,
}

#[derive(Clone, Debug)]
pub struct AcceptedMessage {
    pub event_id: String,
    pub message_id: String,
    pub author_public_key: String,
    pub username: String,
    pub bio: String,
    pub avatar: Option<ProfileImage>,
    pub accepts_direct_messages: bool,
    pub text: String,
    pub attachment: Option<MediaAttachment>,
    pub reply_to_message_id: Option<String>,
    pub created_at_millis: u64,
}

#[derive(Clone, Debug)]
pub struct AcceptedReport {
    pub event_id: String,
    pub message_event_id: String,
    pub reporter_public_key: String,
    pub reporter_username: String,
    pub reporter_avatar: Option<ProfileImage>,
    pub reason: String,
    pub created_at_millis: u64,
}

#[derive(Clone, Debug, Default)]
pub struct GroupState {
    pub profile: GroupProfile,
    pub owner_public_key: Option<String>,
    pub members: HashMap<String, MemberState>,
    pub moderators: HashSet<String>,
    pub banned_members: HashSet<String>,
    pub banned_profiles: HashMap<String, MemberState>,
    pub messages: Vec<AcceptedMessage>,
    pub reports: Vec<AcceptedReport>,
    pub rejected_events: usize,
}

impl GroupState {
    pub fn rebuild(group: &GroupMembership, events: &[SignedEvent]) -> Self {
        let mut ordered = events.iter().collect::<Vec<_>>();
        ordered.sort_by(|left, right| {
            left.created_at_millis
                .cmp(&right.created_at_millis)
                .then_with(|| left.author_public_key.cmp(&right.author_public_key))
                .then_with(|| left.author_sequence.cmp(&right.author_sequence))
                .then_with(|| left.event_id.cmp(&right.event_id))
        });

        let mut state = Self {
            profile: group.profile(),
            owner_public_key: (!group.owner_public_key.is_empty())
                .then(|| group.owner_public_key.clone()),
            ..Self::default()
        };
        let mut last_sequence = HashMap::<String, u64>::new();
        for event in ordered {
            if let Some(previous) = last_sequence.get(&event.author_public_key)
                && event.author_sequence <= *previous
            {
                state.rejected_events += 1;
                continue;
            }
            let Ok(payload) = event.decrypt(group) else {
                state.rejected_events += 1;
                continue;
            };
            last_sequence.insert(event.author_public_key.clone(), event.author_sequence);

            match payload {
                GroupEventPayload::MemberJoined {
                    username,
                    bio,
                    avatar,
                    accepts_direct_messages,
                } => {
                    if state.banned_members.contains(&event.author_public_key) {
                        state.rejected_events += 1;
                        continue;
                    }
                    if state.owner_public_key.is_none() {
                        state.owner_public_key = Some(event.author_public_key.clone());
                    }
                    state.members.insert(
                        event.author_public_key.clone(),
                        MemberState {
                            public_key: event.author_public_key.clone(),
                            username: username.clone(),
                            bio: bio.clone(),
                            avatar: avatar.clone(),
                            accepts_direct_messages,
                            joined_at_millis: event.created_at_millis,
                        },
                    );
                    update_message_profiles(
                        &mut state.messages,
                        &event.author_public_key,
                        &username,
                        &bio,
                        &avatar,
                        accepts_direct_messages,
                    );
                }
                GroupEventPayload::ProfileUpdated { profile } => {
                    let Some(member) = state.members.get_mut(&event.author_public_key) else {
                        state.rejected_events += 1;
                        continue;
                    };
                    member.username = profile.username.clone();
                    member.bio = profile.bio.clone();
                    member.avatar = profile.avatar.clone();
                    member.accepts_direct_messages = profile.accepts_direct_messages;
                    update_message_profiles(
                        &mut state.messages,
                        &event.author_public_key,
                        &profile.username,
                        &profile.bio,
                        &profile.avatar,
                        profile.accepts_direct_messages,
                    );
                }
                GroupEventPayload::GroupProfileUpdated { profile } => {
                    let is_owner =
                        state.owner_public_key.as_deref() == Some(event.author_public_key.as_str());
                    let is_active = state.members.contains_key(&event.author_public_key);
                    if !is_owner || !is_active || !valid_group_profile(&profile) {
                        state.rejected_events += 1;
                        continue;
                    }
                    state.profile = profile;
                }
                GroupEventPayload::ModeratorSet {
                    member_public_key,
                    enabled,
                } => {
                    let is_owner =
                        state.owner_public_key.as_deref() == Some(event.author_public_key.as_str());
                    let owner_is_active = state.members.contains_key(&event.author_public_key);
                    let target_is_active = state.members.contains_key(&member_public_key);
                    let target_is_owner =
                        state.owner_public_key.as_deref() == Some(member_public_key.as_str());
                    if !is_owner || !owner_is_active || !target_is_active || target_is_owner {
                        state.rejected_events += 1;
                        continue;
                    }
                    if enabled {
                        state.moderators.insert(member_public_key);
                    } else {
                        state.moderators.remove(&member_public_key);
                    }
                }
                GroupEventPayload::MessageDeleted { message_event_id } => {
                    let is_owner =
                        state.owner_public_key.as_deref() == Some(event.author_public_key.as_str());
                    let can_moderate =
                        is_owner || state.moderators.contains(&event.author_public_key);
                    let is_active = state.members.contains_key(&event.author_public_key);
                    let target_is_own = state.messages.iter().any(|message| {
                        message.event_id == message_event_id
                            && message.author_public_key == event.author_public_key
                    });
                    if (!can_moderate && !target_is_own) || !is_active {
                        state.rejected_events += 1;
                        continue;
                    }
                    let previous_length = state.messages.len();
                    state
                        .messages
                        .retain(|message| message.event_id != message_event_id);
                    if state.messages.len() == previous_length {
                        state.rejected_events += 1;
                    } else {
                        state
                            .reports
                            .retain(|report| report.message_event_id != message_event_id);
                    }
                }
                GroupEventPayload::MessageReported {
                    message_event_id,
                    reason,
                } => {
                    let Some(reporter) = state.members.get(&event.author_public_key) else {
                        state.rejected_events += 1;
                        continue;
                    };
                    let Some(message) = state
                        .messages
                        .iter()
                        .find(|message| message.event_id == message_event_id)
                    else {
                        state.rejected_events += 1;
                        continue;
                    };
                    let duplicate = state.reports.iter().any(|report| {
                        report.message_event_id == message_event_id
                            && report.reporter_public_key == event.author_public_key
                    });
                    if message.author_public_key == event.author_public_key
                        || reason.chars().count() > 280
                        || duplicate
                    {
                        state.rejected_events += 1;
                        continue;
                    }
                    state.reports.push(AcceptedReport {
                        event_id: event.event_id.clone(),
                        message_event_id,
                        reporter_public_key: event.author_public_key.clone(),
                        reporter_username: reporter.username.clone(),
                        reporter_avatar: reporter.avatar.clone(),
                        reason,
                        created_at_millis: event.created_at_millis,
                    });
                }
                GroupEventPayload::ReportResolved { report_event_id } => {
                    let is_owner =
                        state.owner_public_key.as_deref() == Some(event.author_public_key.as_str());
                    let can_moderate =
                        is_owner || state.moderators.contains(&event.author_public_key);
                    let is_active = state.members.contains_key(&event.author_public_key);
                    let previous_length = state.reports.len();
                    if !can_moderate || !is_active {
                        state.rejected_events += 1;
                        continue;
                    }
                    state
                        .reports
                        .retain(|report| report.event_id != report_event_id);
                    if state.reports.len() == previous_length {
                        state.rejected_events += 1;
                    }
                }
                GroupEventPayload::OwnMessagesDeleted => {
                    if !state.members.contains_key(&event.author_public_key) {
                        state.rejected_events += 1;
                        continue;
                    }
                    state
                        .messages
                        .retain(|message| message.author_public_key != event.author_public_key);
                    retain_reports_for_existing_messages(&mut state);
                }
                GroupEventPayload::MemberBanned {
                    member_public_key,
                    delete_messages,
                } => {
                    let is_owner =
                        state.owner_public_key.as_deref() == Some(event.author_public_key.as_str());
                    let can_moderate =
                        is_owner || state.moderators.contains(&event.author_public_key);
                    let actor_is_active = state.members.contains_key(&event.author_public_key);
                    let target_is_owner =
                        state.owner_public_key.as_deref() == Some(member_public_key.as_str());
                    let target_is_moderator = state.moderators.contains(&member_public_key);
                    if !can_moderate
                        || !actor_is_active
                        || target_is_owner
                        || (!is_owner && target_is_moderator)
                        || !state.members.contains_key(&member_public_key)
                    {
                        state.rejected_events += 1;
                        continue;
                    }
                    let banned_member = state
                        .members
                        .remove(&member_public_key)
                        .expect("active banned member was checked");
                    state.moderators.remove(&member_public_key);
                    state.banned_members.insert(member_public_key.clone());
                    state
                        .banned_profiles
                        .insert(member_public_key.clone(), banned_member);
                    if delete_messages {
                        state
                            .messages
                            .retain(|message| message.author_public_key != member_public_key);
                        retain_reports_for_existing_messages(&mut state);
                    }
                }
                GroupEventPayload::MemberUnbanned { member_public_key } => {
                    let is_owner =
                        state.owner_public_key.as_deref() == Some(event.author_public_key.as_str());
                    let owner_is_active = state.members.contains_key(&event.author_public_key);
                    if !is_owner
                        || !owner_is_active
                        || !state.banned_members.remove(&member_public_key)
                    {
                        state.rejected_events += 1;
                        continue;
                    }
                    state.banned_profiles.remove(&member_public_key);
                }
                GroupEventPayload::MemberLeft => {
                    if state.members.remove(&event.author_public_key).is_none() {
                        state.rejected_events += 1;
                    } else {
                        state.moderators.remove(&event.author_public_key);
                    }
                }
                GroupEventPayload::DirectMessage { .. }
                | GroupEventPayload::DirectThreadDeleted { .. } => {
                    state.rejected_events += 1;
                }
                GroupEventPayload::Message {
                    text,
                    attachment,
                    reply_to_message_id,
                } => {
                    let Some(member) = state.members.get(&event.author_public_key) else {
                        state.rejected_events += 1;
                        continue;
                    };
                    if text.is_empty() && attachment.is_none()
                        || attachment.as_ref().is_some_and(|media| !valid_media(media))
                        || reply_to_message_id.as_ref().is_some_and(|message_id| {
                            !valid_message_id(message_id)
                                || !state
                                    .messages
                                    .iter()
                                    .any(|message| message.message_id == *message_id)
                        })
                    {
                        state.rejected_events += 1;
                        continue;
                    }
                    let is_owner =
                        state.owner_public_key.as_deref() == Some(event.author_public_key.as_str());
                    let is_moderator = state.moderators.contains(&event.author_public_key);
                    if !is_owner
                        && !is_moderator
                        && ((!text.is_empty() && !state.profile.members_can_send_messages)
                            || (attachment.is_some() && !state.profile.members_can_send_media))
                    {
                        state.rejected_events += 1;
                        continue;
                    }
                    state.messages.push(AcceptedMessage {
                        event_id: event.event_id.clone(),
                        message_id: event.event_id.clone(),
                        author_public_key: event.author_public_key.clone(),
                        username: member.username.clone(),
                        bio: member.bio.clone(),
                        avatar: member.avatar.clone(),
                        accepts_direct_messages: member.accepts_direct_messages,
                        text,
                        attachment,
                        reply_to_message_id,
                        created_at_millis: event.created_at_millis,
                    });
                }
            }
        }
        state
    }
}

fn retain_reports_for_existing_messages(state: &mut GroupState) {
    let message_ids = state
        .messages
        .iter()
        .map(|message| message.event_id.as_str())
        .collect::<HashSet<_>>();
    state
        .reports
        .retain(|report| message_ids.contains(report.message_event_id.as_str()));
}

fn valid_media(media: &MediaAttachment) -> bool {
    const MAX_MEDIA_BYTES: u64 = 500 * 1024 * 1024;
    const MAX_CHUNK_BYTES: u32 = 1024 * 1024;
    let file_name_length = media.file_name.trim().chars().count();
    let supported_type = media.mime_type.starts_with("image/")
        || media.mime_type.starts_with("video/")
        || media.mime_type.starts_with("audio/");
    file_name_length > 0
        && file_name_length <= 255
        && supported_type
        && media.mime_type.len() <= 100
        && media.byte_length > 0
        && media.byte_length <= MAX_MEDIA_BYTES
        && !media.chunks.is_empty()
        && media.chunks.len() <= 500
        && media.chunks.iter().all(|chunk| {
            chunk.blob_id.len() == 64
                && chunk.blob_id.bytes().all(|byte| byte.is_ascii_hexdigit())
                && !chunk.key_base64.is_empty()
                && chunk.key_base64.len() <= 64
                && chunk.byte_length > 0
                && chunk.byte_length <= MAX_CHUNK_BYTES
        })
        && media
            .chunks
            .iter()
            .map(|chunk| u64::from(chunk.byte_length))
            .sum::<u64>()
            == media.byte_length
}

fn valid_message_id(message_id: &str) -> bool {
    message_id.len() == 64 && message_id.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_group_profile(profile: &GroupProfile) -> bool {
    let name_length = profile.name.trim().chars().count();
    name_length > 0
        && name_length <= 80
        && profile.description.chars().count() <= 200
        && valid_group_rules(&profile.rules)
        && profile
            .avatar
            .as_ref()
            .is_none_or(|avatar| avatar.byte_length > 0 && avatar.byte_length <= 256 * 1024)
        && profile.background.as_ref().is_none_or(|background| {
            background.byte_length > 0 && background.byte_length <= 1536 * 1024
        })
        && valid_group_accent_color(&profile.accent_color)
}

fn valid_group_accent_color(color: &str) -> bool {
    color.len() == 7
        && color.starts_with('#')
        && color[1..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_group_rules(rules: &str) -> bool {
    let rules = rules
        .lines()
        .map(str::trim)
        .filter(|rule| !rule.is_empty())
        .collect::<Vec<_>>();
    rules.len() <= 20
        && rules.iter().all(|rule| rule.chars().count() <= 200)
        && rules.join("\n").chars().count() <= 4000
}

fn update_message_profiles(
    messages: &mut [AcceptedMessage],
    public_key: &str,
    username: &str,
    bio: &str,
    avatar: &Option<ProfileImage>,
    accepts_direct_messages: bool,
) {
    for message in messages
        .iter_mut()
        .filter(|message| message.author_public_key == public_key)
    {
        message.username = username.to_owned();
        message.bio = bio.to_owned();
        message.avatar = avatar.clone();
        message.accepts_direct_messages = accepts_direct_messages;
    }
}

pub fn generate_frequency() -> String {
    format!("{:012}", random::<u64>() % FREQUENCY_SPACE)
}

pub fn normalize_frequency(value: &str) -> Result<String, NoiseError> {
    let digits: String = value
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    if digits.len() != 12 || !digits.chars().all(|character| character.is_ascii_digit()) {
        return Err(NoiseError::InvalidFrequency);
    }
    Ok(digits)
}

pub fn display_frequency(value: &str) -> Result<String, NoiseError> {
    let digits = normalize_frequency(value)?;
    Ok(format!(
        "{} {} {}",
        &digits[0..4],
        &digits[4..8],
        &digits[8..12]
    ))
}

pub fn frequency_locator(value: &str) -> String {
    let mut hasher = blake3::Hasher::new_derive_key(INVITE_KDF_CONTEXT);
    hasher.update(b"locator");
    hasher.update(value.as_bytes());
    hasher.finalize().to_hex().to_string()
}

fn frequency_key(frequency: &str, salt: &[u8; 16]) -> [u8; 32] {
    let mut input = Vec::with_capacity(salt.len() + frequency.len());
    input.extend_from_slice(salt);
    input.extend_from_slice(frequency.as_bytes());
    blake3::derive_key(INVITE_KDF_CONTEXT, &input)
}

fn blob_id(group_id: Option<&str>, nonce: &[u8; 24], ciphertext: &[u8]) -> String {
    let mut hasher = blake3::Hasher::new();
    if let Some(group_id) = group_id {
        hasher.update(b"noise-group-blob-v1");
        hasher.update(group_id.as_bytes());
    }
    hasher.update(nonce);
    hasher.update(ciphertext);
    hasher.finalize().to_hex().to_string()
}

fn authoritative_group_id(owner_public_key: &str, authority_nonce: &[u8; 32]) -> String {
    let mut hasher = blake3::Hasher::new_derive_key("xyz.gnosyslabs.noise.group-authority.v1");
    hasher.update(owner_public_key.as_bytes());
    hasher.update(authority_nonce);
    hasher.finalize().to_hex().to_string()
}

fn verify_signature(public_key: &str, signature: &str, bytes: &[u8]) -> Result<(), NoiseError> {
    let verifying_key = VerifyingKey::from_bytes(&decode_array(public_key, "public key")?)
        .map_err(|_| NoiseError::InvalidSignature)?;
    let signature = Signature::from_bytes(&decode_array(signature, "signature")?);
    verifying_key
        .verify(bytes, &signature)
        .map_err(|_| NoiseError::InvalidSignature)
}

fn decode(value: &str, field: &'static str) -> Result<Vec<u8>, NoiseError> {
    STANDARD_NO_PAD
        .decode(value)
        .map_err(|_| NoiseError::InvalidEncoding(field))
}

fn decode_array<const N: usize>(value: &str, field: &'static str) -> Result<[u8; N], NoiseError> {
    decode(value, field)?
        .try_into()
        .map_err(|_| NoiseError::InvalidLength(field))
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is before Unix epoch")
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invitation_and_message_round_trip() {
        let identity = Identity::generate();
        let group = GroupMembership::create_owned("afterhours", identity.public_key_base64());
        let frequency = generate_frequency();
        let record = InviteRecord::create(&identity, &frequency, group.clone()).unwrap();
        let opened = record.open(&frequency).unwrap();
        assert_eq!(opened.group.group_id, group.group_id);
        let rotation = InviteRotation::create(&identity, &group, Some(record.clone()), 1).unwrap();
        rotation.verify().unwrap();
        assert_eq!(rotation.new_invite.unwrap().locator, record.locator);

        let joined = SignedEvent::member_joined(
            &identity,
            &group,
            &Profile {
                username: "alice".into(),
                bio: String::new(),
                avatar: None,
                accepts_direct_messages: true,
            },
            0,
        )
        .unwrap();
        let message = SignedEvent::chat(&identity, &group, "hello", 1).unwrap();
        let updated = SignedEvent::profile_updated(
            &identity,
            &group,
            &Profile {
                username: "alice".into(),
                bio: "still listening".into(),
                avatar: None,
                accepts_direct_messages: true,
            },
            2,
        )
        .unwrap();
        let state = GroupState::rebuild(&group, &[joined, message, updated]);
        assert_eq!(state.members.len(), 1);
        assert_eq!(state.messages[0].text, "hello");
        assert_eq!(state.messages[0].username, "alice");
        assert_eq!(state.messages[0].bio, "still listening");

        let image = b"encrypted profile image";
        let (blob, key) = EncryptedBlob::create(image).unwrap();
        assert_eq!(blob.open(&key).unwrap(), image);
    }

    #[test]
    fn noise_id_credentials_seal_and_recover_an_account_vault() {
        let identity = Identity::generate();
        let credentials =
            derive_account_credentials("4821 0937 6144", "correct horse battery staple").unwrap();
        let same =
            derive_account_credentials("482109376144", "correct horse battery staple").unwrap();
        let wrong =
            derive_account_credentials("482109376144", "correct horse battery stapler").unwrap();
        assert_eq!(credentials.locator, same.locator);
        assert_ne!(credentials.locator, wrong.locator);

        let vault = AccountVault::seal(&identity, &credentials, 1, b"encrypted identity").unwrap();
        vault.verify().unwrap();
        assert_eq!(vault.open(&same).unwrap(), b"encrypted identity");
        assert!(vault.open(&wrong).is_err());

        let tombstone = AccountVault::tombstone(&identity, credentials.locator, 2).unwrap();
        tombstone.verify().unwrap();
        assert!(tombstone.deleted);
        assert!(tombstone.open(&same).is_err());
    }

    #[test]
    fn founder_designates_moderator_and_moderator_can_delete_and_ban() {
        let founder = Identity::generate();
        let moderator = Identity::generate();
        let member = Identity::generate();
        let group = GroupMembership::create_owned("afterhours", founder.public_key_base64());
        let profile = |username: &str| Profile {
            username: username.into(),
            bio: String::new(),
            avatar: None,
            accepts_direct_messages: true,
        };
        let mut events = Vec::new();
        events.push(SignedEvent::member_joined(&founder, &group, &profile("founder"), 0).unwrap());
        std::thread::sleep(std::time::Duration::from_millis(2));
        events.push(SignedEvent::member_joined(&moderator, &group, &profile("mod"), 0).unwrap());
        std::thread::sleep(std::time::Duration::from_millis(2));
        events.push(SignedEvent::member_joined(&member, &group, &profile("member"), 0).unwrap());
        std::thread::sleep(std::time::Duration::from_millis(2));
        let first_message = SignedEvent::chat(&member, &group, "remove me", 1).unwrap();
        events.push(first_message.clone());
        std::thread::sleep(std::time::Duration::from_millis(2));
        events.push(
            SignedEvent::moderator_set(&founder, &group, moderator.public_key_base64(), true, 1)
                .unwrap(),
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
        events.push(
            SignedEvent::message_deleted(&moderator, &group, first_message.event_id, 1).unwrap(),
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
        events.push(SignedEvent::chat(&member, &group, "remove all of me", 2).unwrap());
        std::thread::sleep(std::time::Duration::from_millis(2));
        events.push(
            SignedEvent::member_banned(&moderator, &group, member.public_key_base64(), true, 2)
                .unwrap(),
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
        events.push(SignedEvent::chat(&member, &group, "rejected", 3).unwrap());

        let state = GroupState::rebuild(&group, &events);
        assert!(state.moderators.contains(&moderator.public_key_base64()));
        assert!(state.banned_members.contains(&member.public_key_base64()));
        assert!(!state.members.contains_key(&member.public_key_base64()));
        assert!(state.messages.is_empty());
        assert_eq!(state.rejected_events, 1);
    }

    #[test]
    fn member_reports_a_message_and_only_moderation_can_resolve_it() {
        let founder = Identity::generate();
        let member = Identity::generate();
        let group = GroupMembership::create_owned("reports", founder.public_key_base64());
        let profile = |username: &str| Profile {
            username: username.into(),
            bio: String::new(),
            avatar: None,
            accepts_direct_messages: true,
        };
        let mut events =
            vec![SignedEvent::member_joined(&founder, &group, &profile("founder"), 0).unwrap()];
        std::thread::sleep(std::time::Duration::from_millis(2));
        events.push(SignedEvent::member_joined(&member, &group, &profile("member"), 0).unwrap());
        std::thread::sleep(std::time::Duration::from_millis(2));
        let message = SignedEvent::chat(&founder, &group, "reported message", 1).unwrap();
        events.push(message.clone());
        std::thread::sleep(std::time::Duration::from_millis(2));
        let report =
            SignedEvent::message_reported(&member, &group, message.event_id, "breaks the rules", 1)
                .unwrap();
        events.push(report.clone());

        let pending = GroupState::rebuild(&group, &events);
        assert_eq!(pending.reports.len(), 1);
        assert_eq!(pending.reports[0].reason, "breaks the rules");

        std::thread::sleep(std::time::Duration::from_millis(2));
        events.push(SignedEvent::report_resolved(&member, &group, &report.event_id, 2).unwrap());
        std::thread::sleep(std::time::Duration::from_millis(2));
        events.push(SignedEvent::report_resolved(&founder, &group, report.event_id, 2).unwrap());

        let resolved = GroupState::rebuild(&group, &events);
        assert!(resolved.reports.is_empty());
        assert_eq!(resolved.rejected_events, 1);
    }

    #[test]
    fn member_can_reply_and_delete_only_their_own_message() {
        let founder = Identity::generate();
        let member = Identity::generate();
        let other = Identity::generate();
        let group = GroupMembership::create_owned("replies", founder.public_key_base64());
        let profile = |username: &str| Profile {
            username: username.into(),
            bio: String::new(),
            avatar: None,
            accepts_direct_messages: true,
        };
        let mut events =
            vec![SignedEvent::member_joined(&founder, &group, &profile("founder"), 0).unwrap()];
        std::thread::sleep(std::time::Duration::from_millis(2));
        events.push(SignedEvent::member_joined(&member, &group, &profile("member"), 0).unwrap());
        std::thread::sleep(std::time::Duration::from_millis(2));
        events.push(SignedEvent::member_joined(&other, &group, &profile("other"), 0).unwrap());
        std::thread::sleep(std::time::Duration::from_millis(2));
        let original = SignedEvent::chat(&member, &group, "original", 1).unwrap();
        events.push(original.clone());
        std::thread::sleep(std::time::Duration::from_millis(2));
        let reply =
            SignedEvent::chat_reply(&other, &group, "reply", Some(original.event_id.clone()), 1)
                .unwrap();
        events.push(reply.clone());
        std::thread::sleep(std::time::Duration::from_millis(2));
        events.push(
            SignedEvent::message_deleted(&member, &group, reply.event_id.clone(), 2).unwrap(),
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
        events.push(
            SignedEvent::message_deleted(&member, &group, original.event_id.clone(), 3).unwrap(),
        );

        let state = GroupState::rebuild(&group, &events);
        assert_eq!(state.rejected_events, 1);
        assert_eq!(state.messages.len(), 1);
        assert_eq!(state.messages[0].message_id, reply.event_id);
        assert_eq!(
            state.messages[0].reply_to_message_id.as_deref(),
            Some(original.event_id.as_str())
        );
    }

    #[test]
    fn group_posting_policy_and_founder_unban_are_enforced() {
        let founder = Identity::generate();
        let member = Identity::generate();
        let group = GroupMembership::create_owned("locked", founder.public_key_base64());
        let profile = |username: &str| Profile {
            username: username.into(),
            bio: String::new(),
            avatar: None,
            accepts_direct_messages: true,
        };
        let mut events =
            vec![SignedEvent::member_joined(&founder, &group, &profile("founder"), 0).unwrap()];
        std::thread::sleep(std::time::Duration::from_millis(2));
        events.push(SignedEvent::member_joined(&member, &group, &profile("member"), 0).unwrap());
        std::thread::sleep(std::time::Duration::from_millis(2));
        events.push(
            SignedEvent::group_profile_updated(
                &founder,
                &group,
                &GroupProfile {
                    name: "locked".into(),
                    description: String::new(),
                    rules: String::new(),
                    avatar: None,
                    background: None,
                    accent_color: default_group_accent_color(),
                    members_can_send_messages: false,
                    members_can_send_media: true,
                },
                1,
            )
            .unwrap(),
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
        events.push(SignedEvent::chat(&member, &group, "blocked", 1).unwrap());
        std::thread::sleep(std::time::Duration::from_millis(2));
        events.push(
            SignedEvent::member_banned(&founder, &group, member.public_key_base64(), false, 2)
                .unwrap(),
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
        events.push(
            SignedEvent::member_unbanned(&founder, &group, member.public_key_base64(), 3).unwrap(),
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
        events.push(SignedEvent::member_joined(&member, &group, &profile("member"), 2).unwrap());

        let state = GroupState::rebuild(&group, &events);
        assert!(!state.banned_members.contains(&member.public_key_base64()));
        assert!(
            !state
                .banned_profiles
                .contains_key(&member.public_key_base64())
        );
        assert!(state.members.contains_key(&member.public_key_base64()));
        assert!(state.messages.is_empty());
        assert_eq!(state.rejected_events, 1);
    }

    #[test]
    fn direct_messages_are_pairwise_encrypted_for_both_mailboxes() {
        let alice = Identity::generate();
        let bob = Identity::generate();
        let alice_public_key = alice.public_key_base64();
        let bob_public_key = bob.public_key_base64();
        let alice_view_of_bob_mailbox = alice
            .direct_mailbox(&bob_public_key, &bob_public_key)
            .unwrap();
        let bob_view_of_bob_mailbox = bob
            .direct_mailbox(&alice_public_key, &bob_public_key)
            .unwrap();
        assert_eq!(
            alice_view_of_bob_mailbox.group_id,
            bob_view_of_bob_mailbox.group_id
        );
        assert_eq!(
            alice_view_of_bob_mailbox.secret_base64,
            bob_view_of_bob_mailbox.secret_base64
        );
        assert_eq!(
            alice.direct_scope_id(&bob_public_key).unwrap(),
            bob.direct_scope_id(&alice_public_key).unwrap()
        );

        let event = SignedEvent::direct_message(
            &alice,
            &alice_view_of_bob_mailbox,
            &bob_public_key,
            &Profile {
                username: "alice".into(),
                bio: String::new(),
                avatar: None,
                accepts_direct_messages: true,
            },
            "secret hello",
            None,
            None,
            0,
        )
        .unwrap();
        let GroupEventPayload::DirectMessage {
            recipient_public_key,
            text,
            ..
        } = event.decrypt(&bob_view_of_bob_mailbox).unwrap()
        else {
            panic!("expected a direct message");
        };
        assert_eq!(recipient_public_key, bob_public_key);
        assert_eq!(text, "secret hello");

        let deletion = SignedEvent::direct_thread_deleted(
            &alice,
            &alice_view_of_bob_mailbox,
            &bob_public_key,
            1,
        )
        .unwrap();
        assert!(matches!(
            deletion.decrypt(&bob_view_of_bob_mailbox).unwrap(),
            GroupEventPayload::DirectThreadDeleted { recipient_public_key } if recipient_public_key == bob_public_key
        ));
    }
}
