use std::collections::HashSet;

use base64::{Engine as _, engine::general_purpose::STANDARD_NO_PAD};
use openmls_rust_crypto::RustCrypto;
use openmls_traits::{
    crypto::OpenMlsCrypto,
    types::{HpkeAeadType, HpkeCiphertext, HpkeConfig, HpkeKdfType, HpkeKemType, HpkeKeyPair},
};
use rand::random;
use serde::{Deserialize, Serialize};

use crate::{
    Identity, MAX_STORAGE_SHARDS, NoiseError, SignedEvent, decode, decode_array, now_millis,
    valid_hex_id, verify_signature,
};

const SAFETY_REPORT_VERSION: u32 = 1;
const SAFETY_ENVELOPE_VERSION: u32 = 1;
const SAFETY_REPORT_CONTEXT: &str = "xyz.gnosyslabs.noise.safety-report.v1";
const SAFETY_HPKE_INFO: &[u8] = b"xyz.gnosyslabs.noise.safety-envelope.v1";
const SAFETY_KEY_ID_CONTEXT: &str = "xyz.gnosyslabs.noise.safety-key-id.v1";
const MAX_REPORT_DETAILS_CHARS: usize = 1_000;
const MAX_REPORTED_TEXT_CHARS: usize = 4_000;
const MAX_ENCRYPTED_OBJECTS: usize = 256;
const MAX_RELAY_URL_BYTES: usize = 2_048;

/// The user-facing reason selected before a report is routed to noise safety.
///
/// Categories that normally belong to group staff remain valid here because a
/// user may escalate when group staff are involved or have not acted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SafetyReportCategoryV1 {
    GroupRules,
    HarassmentOrHatefulBehavior,
    SpamScamOrImpersonation,
    ThreatsOrImmediateDanger,
    SexualExploitationOrNonConsensualSexualContent,
    ChildSafety,
    ExplicitContentNotProperlyLabeled,
    Other,
}

/// A hash computed by the reporting client without saving or retransmitting
/// the underlying media.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SafetyMediaFingerprintV1 {
    pub algorithm: SafetyMediaHashAlgorithmV1,
    pub digest_base64: String,
    pub byte_length: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SafetyMediaHashAlgorithmV1 {
    Sha256,
}

/// An opaque encrypted storage location associated with the reported event.
///
/// It deliberately omits media keys, deletion capabilities, and payload bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SafetyEncryptedObjectV1 {
    pub object_id: String,
    pub shards: Vec<SafetyEncryptedShardV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SafetyEncryptedShardV1 {
    pub relay: String,
    pub shard_id: String,
}

/// A content-minimized report intended for noise safety rather than group staff.
///
/// `reported_event` remains encrypted. Including the complete signed event
/// lets the safety service verify its author, group, event id, and timestamp
/// without receiving the decrypted message or any media bytes. Threat reports
/// may explicitly include one bounded text excerpt so staff can assess the
/// danger without receiving surrounding history.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SafetyReportV1 {
    pub version: u32,
    pub report_id: String,
    pub category: SafetyReportCategoryV1,
    pub reported_event: SignedEvent,
    pub reporter_public_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_context_proof: Option<SignedEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_fingerprint: Option<SafetyMediaFingerprintV1>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub encrypted_objects: Vec<SafetyEncryptedObjectV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reported_text_excerpt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
    pub created_at_millis: u64,
    pub signature_base64: String,
}

#[derive(Serialize)]
struct UnsignedSafetyReportV1<'a> {
    context: &'static str,
    version: u32,
    report_id: &'a str,
    category: SafetyReportCategoryV1,
    reported_event: &'a SignedEvent,
    reporter_public_key: &'a str,
    group_context_proof: Option<&'a SignedEvent>,
    media_fingerprint: Option<&'a SafetyMediaFingerprintV1>,
    encrypted_objects: &'a [SafetyEncryptedObjectV1],
    #[serde(skip_serializing_if = "Option::is_none")]
    reported_text_excerpt: Option<&'a str>,
    details: Option<&'a str>,
    created_at_millis: u64,
}

impl SafetyReportV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn create(
        reporter: &Identity,
        category: SafetyReportCategoryV1,
        reported_event: SignedEvent,
        group_context_proof: Option<SignedEvent>,
        media_fingerprint: Option<SafetyMediaFingerprintV1>,
        encrypted_objects: Vec<SafetyEncryptedObjectV1>,
        reported_text_excerpt: Option<String>,
        details: Option<String>,
    ) -> Result<Self, NoiseError> {
        let reported_text_excerpt = reported_text_excerpt.filter(|value| !value.trim().is_empty());
        let details = details
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        let random_id: [u8; 32] = random();
        let mut report = Self {
            version: SAFETY_REPORT_VERSION,
            report_id: blake3::hash(&random_id).to_hex().to_string(),
            category,
            reported_event,
            reporter_public_key: reporter.public_key_base64(),
            group_context_proof,
            media_fingerprint,
            encrypted_objects,
            reported_text_excerpt,
            details,
            created_at_millis: now_millis(),
            signature_base64: String::new(),
        };
        report.signature_base64 = reporter.sign(&report.signing_bytes()?);
        report.verify()?;
        Ok(report)
    }

    pub fn verify(&self) -> Result<(), NoiseError> {
        if self.version != SAFETY_REPORT_VERSION
            || !valid_hex_id(&self.report_id)
            || !valid_hex_id(&self.reported_event.group_id)
            || self
                .details
                .as_deref()
                .is_some_and(|details| details.chars().count() > MAX_REPORT_DETAILS_CHARS)
            || self.reported_text_excerpt.as_deref().is_some_and(|text| {
                text.trim().is_empty()
                    || text.chars().count() > MAX_REPORTED_TEXT_CHARS
                    || self.category != SafetyReportCategoryV1::ThreatsOrImmediateDanger
            })
            || self.encrypted_objects.len() > MAX_ENCRYPTED_OBJECTS
        {
            return Err(NoiseError::InvalidEncoding("safety report"));
        }

        decode_array::<32>(&self.reporter_public_key, "safety reporter public key")?;
        self.reported_event.verify()?;

        if let Some(proof) = &self.group_context_proof {
            proof.verify()?;
            if proof.group_id != self.reported_event.group_id
                || proof.author_public_key != self.reporter_public_key
            {
                return Err(NoiseError::GroupMismatch);
            }
        }

        if let Some(fingerprint) = &self.media_fingerprint {
            match fingerprint.algorithm {
                SafetyMediaHashAlgorithmV1::Sha256 => {
                    decode_array::<32>(&fingerprint.digest_base64, "safety media fingerprint")?;
                }
            }
        }

        let mut object_ids = HashSet::with_capacity(self.encrypted_objects.len());
        for object in &self.encrypted_objects {
            if !valid_hex_id(&object.object_id)
                || !object_ids.insert(object.object_id.as_str())
                || object.shards.is_empty()
                || object.shards.len() > MAX_STORAGE_SHARDS
            {
                return Err(NoiseError::InvalidEncoding(
                    "safety encrypted object location",
                ));
            }
            let mut shard_ids = HashSet::with_capacity(object.shards.len());
            for shard in &object.shards {
                if shard.relay.is_empty()
                    || shard.relay.len() > MAX_RELAY_URL_BYTES
                    || !valid_hex_id(&shard.shard_id)
                    || !shard_ids.insert(shard.shard_id.as_str())
                {
                    return Err(NoiseError::InvalidEncoding(
                        "safety encrypted shard location",
                    ));
                }
            }
        }

        verify_signature(
            &self.reporter_public_key,
            &self.signature_base64,
            &self.signing_bytes()?,
        )
    }

    /// Encrypt the complete signed report to the current noise safety key.
    pub fn seal(
        &self,
        recipient_public_key_base64: &str,
    ) -> Result<SealedSafetyReportV1, NoiseError> {
        self.verify()?;
        let recipient_public_key =
            decode_array::<32>(recipient_public_key_base64, "safety recipient public key")?;
        let recipient_key_id = safety_key_id(&recipient_public_key);
        let aad = safety_envelope_aad(SAFETY_ENVELOPE_VERSION, &recipient_key_id);
        let plaintext = serde_json::to_vec(self)?;
        let ciphertext = RustCrypto::default()
            .hpke_seal(
                safety_hpke_config(),
                &recipient_public_key,
                SAFETY_HPKE_INFO,
                &aad,
                &plaintext,
            )
            .map_err(|_| NoiseError::Crypto)?;
        Ok(SealedSafetyReportV1 {
            version: SAFETY_ENVELOPE_VERSION,
            recipient_key_id,
            kem_output_base64: STANDARD_NO_PAD.encode(ciphertext.kem_output.as_slice()),
            ciphertext_base64: STANDARD_NO_PAD.encode(ciphertext.ciphertext.as_slice()),
        })
    }

    fn signing_bytes(&self) -> Result<Vec<u8>, NoiseError> {
        Ok(serde_json::to_vec(&UnsignedSafetyReportV1 {
            context: SAFETY_REPORT_CONTEXT,
            version: self.version,
            report_id: &self.report_id,
            category: self.category,
            reported_event: &self.reported_event,
            reporter_public_key: &self.reporter_public_key,
            group_context_proof: self.group_context_proof.as_ref(),
            media_fingerprint: self.media_fingerprint.as_ref(),
            encrypted_objects: &self.encrypted_objects,
            reported_text_excerpt: self.reported_text_excerpt.as_deref(),
            details: self.details.as_deref(),
            created_at_millis: self.created_at_millis,
        })?)
    }
}

/// The only report shape accepted by the future public intake endpoint.
///
/// Everything after `recipient_key_id` is HPKE ciphertext; web infrastructure
/// does not receive readable report metadata.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealedSafetyReportV1 {
    pub version: u32,
    pub recipient_key_id: String,
    pub kem_output_base64: String,
    pub ciphertext_base64: String,
}

/// A rotatable X25519 HPKE recipient key for the private safety service.
///
/// The exported secret is input key material, not the derived HPKE private key.
/// Re-deriving the pair keeps persistence to one 32-byte secret.
#[derive(Clone)]
pub struct SafetyEncryptionKeyPair {
    secret: [u8; 32],
    key_pair: HpkeKeyPair,
}

impl SafetyEncryptionKeyPair {
    pub fn generate() -> Result<Self, NoiseError> {
        Self::from_secret(random())
    }

    pub fn from_secret_base64(secret_base64: &str) -> Result<Self, NoiseError> {
        Self::from_secret(decode_array(secret_base64, "safety encryption secret")?)
    }

    pub fn secret_base64(&self) -> String {
        STANDARD_NO_PAD.encode(self.secret)
    }

    pub fn public_key_base64(&self) -> String {
        STANDARD_NO_PAD.encode(&self.key_pair.public)
    }

    pub fn key_id(&self) -> String {
        safety_key_id(&self.key_pair.public)
    }

    pub fn open(&self, envelope: &SealedSafetyReportV1) -> Result<SafetyReportV1, NoiseError> {
        if envelope.version != SAFETY_ENVELOPE_VERSION
            || envelope.recipient_key_id != self.key_id()
            || !valid_hex_id(&envelope.recipient_key_id)
        {
            return Err(NoiseError::InvalidEncoding("sealed safety report"));
        }
        let aad = safety_envelope_aad(envelope.version, &envelope.recipient_key_id);
        let ciphertext = HpkeCiphertext {
            kem_output: decode(&envelope.kem_output_base64, "safety HPKE encapsulated key")?.into(),
            ciphertext: decode(&envelope.ciphertext_base64, "safety HPKE ciphertext")?.into(),
        };
        let plaintext = RustCrypto::default()
            .hpke_open(
                safety_hpke_config(),
                &ciphertext,
                &self.key_pair.private,
                SAFETY_HPKE_INFO,
                &aad,
            )
            .map_err(|_| NoiseError::Crypto)?;
        let report: SafetyReportV1 = serde_json::from_slice(&plaintext)?;
        report.verify()?;
        Ok(report)
    }

    fn from_secret(secret: [u8; 32]) -> Result<Self, NoiseError> {
        let key_pair = RustCrypto::default()
            .derive_hpke_keypair(safety_hpke_config(), &secret)
            .map_err(|_| NoiseError::Crypto)?;
        if key_pair.public.len() != 32 || key_pair.private.len() != 32 {
            return Err(NoiseError::Crypto);
        }
        Ok(Self { secret, key_pair })
    }
}

fn safety_hpke_config() -> HpkeConfig {
    HpkeConfig(
        HpkeKemType::DhKem25519,
        HpkeKdfType::HkdfSha256,
        HpkeAeadType::ChaCha20Poly1305,
    )
}

fn safety_key_id(public_key: &[u8]) -> String {
    let mut hasher = blake3::Hasher::new_derive_key(SAFETY_KEY_ID_CONTEXT);
    hasher.update(public_key);
    hasher.finalize().to_hex().to_string()
}

pub fn safety_recipient_key_id(recipient_public_key_base64: &str) -> Result<String, NoiseError> {
    let recipient_public_key =
        decode_array::<32>(recipient_public_key_base64, "safety recipient public key")?;
    Ok(safety_key_id(&recipient_public_key))
}

fn safety_envelope_aad(version: u32, recipient_key_id: &str) -> Vec<u8> {
    format!("xyz.gnosyslabs.noise.safety-envelope:{version}:{recipient_key_id}").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DirectMessagePolicy, GroupMembership, Profile};

    #[test]
    fn signed_content_minimized_safety_report_seals_and_opens() {
        let author = Identity::generate();
        let reporter = Identity::generate();
        let group = GroupMembership::create_owned("reported group", author.public_key_base64());
        let membership_proof = SignedEvent::member_joined(
            &reporter,
            &group,
            &Profile {
                username: "reporter".into(),
                bio: String::new(),
                avatar: None,
                album: None,
                accepts_direct_messages: true,
                direct_message_policy: DirectMessagePolicy::Everyone,
            },
            0,
        )
        .unwrap();
        let reported_event =
            SignedEvent::chat(&author, &group, "encrypted report target", 1).unwrap();
        let report = SafetyReportV1::create(
            &reporter,
            SafetyReportCategoryV1::ThreatsOrImmediateDanger,
            reported_event,
            Some(membership_proof),
            Some(SafetyMediaFingerprintV1 {
                algorithm: SafetyMediaHashAlgorithmV1::Sha256,
                digest_base64: STANDARD_NO_PAD.encode([7_u8; 32]),
                byte_length: 128,
            }),
            vec![SafetyEncryptedObjectV1 {
                object_id: "a".repeat(64),
                shards: vec![SafetyEncryptedShardV1 {
                    relay: "https://relay.example".into(),
                    shard_id: "b".repeat(64),
                }],
            }],
            Some("A direct threat from the reported message.".into()),
            Some("Group staff may be involved.".into()),
        )
        .unwrap();

        let safety_key = SafetyEncryptionKeyPair::generate().unwrap();
        let restored_key =
            SafetyEncryptionKeyPair::from_secret_base64(&safety_key.secret_base64()).unwrap();
        let envelope = report.seal(&restored_key.public_key_base64()).unwrap();
        let serialized = serde_json::to_vec(&envelope).unwrap();
        let decoded: SealedSafetyReportV1 = serde_json::from_slice(&serialized).unwrap();
        let opened = restored_key.open(&decoded).unwrap();

        assert_eq!(opened, report);
        assert_eq!(
            opened.reported_text_excerpt.as_deref(),
            Some("A direct threat from the reported message.")
        );
        assert_eq!(decoded.recipient_key_id, restored_key.key_id());
        assert!(
            SafetyEncryptionKeyPair::generate()
                .unwrap()
                .open(&decoded)
                .is_err()
        );
    }
}
