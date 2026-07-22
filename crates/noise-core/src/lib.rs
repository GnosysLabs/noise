use std::{
    collections::HashMap,
    time::{SystemTime, UNIX_EPOCH},
};

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
const INVITE_KDF_CONTEXT: &str = "xyz.gnosyslabs.noise.frequency-invite.v1";

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
    #[error("the invitation belongs to a different frequency")]
    FrequencyMismatch,
    #[error("the invitation issuer does not match its payload")]
    IdentityMismatch,
    #[error("the event belongs to a different group")]
    GroupMismatch,
    #[error("the encrypted blob does not match its identifier")]
    BlobMismatch,
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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Profile {
    pub username: String,
    #[serde(default)]
    pub bio: String,
    #[serde(default)]
    pub avatar: Option<ProfileImage>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileImage {
    pub blob_id: String,
    pub key_base64: String,
    pub mime_type: String,
    pub byte_length: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EncryptedBlob {
    pub blob_id: String,
    pub nonce_base64: String,
    pub ciphertext_base64: String,
}

impl EncryptedBlob {
    pub fn create(plaintext: &[u8]) -> Result<(Self, String), NoiseError> {
        let key: [u8; 32] = random();
        let nonce: [u8; 24] = random();
        let ciphertext = XChaCha20Poly1305::new((&key).into())
            .encrypt(XNonce::from_slice(&nonce), plaintext)
            .map_err(|_| NoiseError::Crypto)?;
        let blob_id = blob_id(&nonce, &ciphertext);
        Ok((
            Self {
                blob_id,
                nonce_base64: STANDARD_NO_PAD.encode(nonce),
                ciphertext_base64: STANDARD_NO_PAD.encode(ciphertext),
            },
            STANDARD_NO_PAD.encode(key),
        ))
    }

    pub fn verify(&self) -> Result<(), NoiseError> {
        let nonce = decode_array::<24>(&self.nonce_base64, "blob nonce")?;
        let ciphertext = decode(&self.ciphertext_base64, "blob ciphertext")?;
        if blob_id(&nonce, &ciphertext) != self.blob_id {
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
    pub avatar: Option<ProfileImage>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupMembership {
    pub group_id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub avatar: Option<ProfileImage>,
    #[serde(default)]
    pub owner_public_key: String,
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
            avatar: None,
            owner_public_key: String::new(),
            secret_base64: STANDARD_NO_PAD.encode(secret),
        }
    }

    pub fn create_owned(name: impl Into<String>, owner_public_key: impl Into<String>) -> Self {
        let mut group = Self::create(name);
        group.owner_public_key = owner_public_key.into();
        group
    }

    pub fn profile(&self) -> GroupProfile {
        GroupProfile {
            name: self.name.clone(),
            description: self.description.clone(),
            avatar: self.avatar.clone(),
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
    pub salt_base64: String,
    pub nonce_base64: String,
    pub ciphertext_base64: String,
    pub issuer_public_key: String,
    pub signature_base64: String,
}

#[derive(Serialize)]
struct UnsignedInviteRecord<'a> {
    locator: &'a str,
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
        Ok(payload)
    }

    fn signing_bytes(&self) -> Result<Vec<u8>, NoiseError> {
        Ok(serde_json::to_vec(&UnsignedInviteRecord {
            locator: &self.locator,
            salt_base64: &self.salt_base64,
            nonce_base64: &self.nonce_base64,
            ciphertext_base64: &self.ciphertext_base64,
            issuer_public_key: &self.issuer_public_key,
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
    },
    ProfileUpdated {
        profile: Profile,
    },
    GroupProfileUpdated {
        profile: GroupProfile,
    },
    MemberLeft,
    Message {
        text: String,
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

    pub fn chat(
        identity: &Identity,
        group: &GroupMembership,
        text: impl Into<String>,
        author_sequence: u64,
    ) -> Result<Self, NoiseError> {
        Self::create(
            identity,
            group,
            GroupEventPayload::Message { text: text.into() },
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
    pub joined_at_millis: u64,
}

#[derive(Clone, Debug)]
pub struct AcceptedMessage {
    pub event_id: String,
    pub author_public_key: String,
    pub username: String,
    pub bio: String,
    pub avatar: Option<ProfileImage>,
    pub text: String,
    pub created_at_millis: u64,
}

#[derive(Clone, Debug, Default)]
pub struct GroupState {
    pub profile: GroupProfile,
    pub owner_public_key: Option<String>,
    pub members: HashMap<String, MemberState>,
    pub messages: Vec<AcceptedMessage>,
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
                } => {
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
                            joined_at_millis: event.created_at_millis,
                        },
                    );
                    update_message_profiles(
                        &mut state.messages,
                        &event.author_public_key,
                        &username,
                        &bio,
                        &avatar,
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
                    update_message_profiles(
                        &mut state.messages,
                        &event.author_public_key,
                        &profile.username,
                        &profile.bio,
                        &profile.avatar,
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
                GroupEventPayload::MemberLeft => {
                    if state.members.remove(&event.author_public_key).is_none() {
                        state.rejected_events += 1;
                    }
                }
                GroupEventPayload::Message { text } => {
                    let Some(member) = state.members.get(&event.author_public_key) else {
                        state.rejected_events += 1;
                        continue;
                    };
                    state.messages.push(AcceptedMessage {
                        event_id: event.event_id.clone(),
                        author_public_key: event.author_public_key.clone(),
                        username: member.username.clone(),
                        bio: member.bio.clone(),
                        avatar: member.avatar.clone(),
                        text,
                        created_at_millis: event.created_at_millis,
                    });
                }
            }
        }
        state
    }
}

fn valid_group_profile(profile: &GroupProfile) -> bool {
    let name_length = profile.name.trim().chars().count();
    name_length > 0
        && name_length <= 80
        && profile.description.chars().count() <= 200
        && profile
            .avatar
            .as_ref()
            .is_none_or(|avatar| avatar.byte_length > 0 && avatar.byte_length <= 256 * 1024)
}

fn update_message_profiles(
    messages: &mut [AcceptedMessage],
    public_key: &str,
    username: &str,
    bio: &str,
    avatar: &Option<ProfileImage>,
) {
    for message in messages
        .iter_mut()
        .filter(|message| message.author_public_key == public_key)
    {
        message.username = username.to_owned();
        message.bio = bio.to_owned();
        message.avatar = avatar.clone();
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

fn blob_id(nonce: &[u8; 24], ciphertext: &[u8]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(nonce);
    hasher.update(ciphertext);
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
        let group = GroupMembership::create("afterhours");
        let frequency = generate_frequency();
        let record = InviteRecord::create(&identity, &frequency, group.clone()).unwrap();
        let opened = record.open(&frequency).unwrap();
        assert_eq!(opened.group.group_id, group.group_id);

        let joined = SignedEvent::member_joined(
            &identity,
            &group,
            &Profile {
                username: "alice".into(),
                bio: String::new(),
                avatar: None,
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
}
