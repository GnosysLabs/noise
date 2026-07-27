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
    DirectMessagePolicy, Identity, MAX_STORAGE_SHARDS, NoiseError, SignedEvent,
    authoritative_group_id, decode, decode_array, now_millis, valid_hex_id, verify_signature,
};

const SAFETY_REPORT_VERSION: u32 = 1;
const SAFETY_ENVELOPE_VERSION: u32 = 1;
const SAFETY_REPORT_CONTEXT: &str = "xyz.gnosyslabs.noise.safety-report.v1";
const SAFETY_HPKE_INFO: &[u8] = b"xyz.gnosyslabs.noise.safety-envelope.v1";
const SAFETY_KEY_ID_CONTEXT: &str = "xyz.gnosyslabs.noise.safety-key-id.v1";
const SAFETY_DIRECTIVE_VERSION: u32 = 1;
const SAFETY_DIRECTIVE_CONTEXT: &str = "xyz.gnosyslabs.noise.safety-directive.v1";
const SAFETY_DIRECTIVE_SIGNING_KEY_CONTEXT: &str =
    "xyz.gnosyslabs.noise.safety-directive-signing-key.v1";
const MAX_REPORT_DETAILS_CHARS: usize = 1_000;
const MAX_REPORTED_TEXT_CHARS: usize = 10_000;
const MAX_ENCRYPTED_OBJECTS: usize = 256;
const MAX_RELAY_URL_BYTES: usize = 2_048;
const MAX_GROUP_QUARANTINE_MILLIS: u64 = 7 * 24 * 60 * 60 * 1_000;
const MAX_SAFETY_GROUP_STAFF: usize = 64;

/// The user-facing reason selected for a report or safety directive.
///
/// Official clients route routine community moderation categories to group
/// staff and reserve noise safety intake for app-level safety categories.
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

/// A human-readable profile snapshot carried inside the encrypted report.
///
/// The public key remains the cryptographic identity. A snapshot for the
/// reporter is self-attested by the report signature; other snapshots describe
/// what the reporting client displayed at report time.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SafetyProfileSnapshotV1 {
    pub public_key: String,
    pub username: String,
    pub direct_message_policy: DirectMessagePolicy,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SafetyReporterContextV1 {
    pub profile: SafetyProfileSnapshotV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_up_allowed: Option<bool>,
}

/// Human-readable group context encrypted to noise safety.
///
/// The founder relationship is verified from the authoritative group id.
/// Moderator entries are a reporter-signed snapshot of the group state and are
/// deliberately named `reported_moderators` so they are not mistaken for
/// independently published role attestations.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SafetyGroupContextV1 {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub founder: Option<SafetyProfileSnapshotV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority_nonce_base64: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reported_moderators: Vec<SafetyProfileSnapshotV1>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SafetyReportHumanContextV1 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reporter: Option<SafetyReporterContextV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reported_author: Option<SafetyProfileSnapshotV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<SafetyGroupContextV1>,
}

pub fn noise_signature_for_public_key(public_key: &str) -> Result<String, NoiseError> {
    const ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let bytes = decode_array::<32>(public_key, "noise signature public key")?;
    let mut signature = String::with_capacity(13);
    for character_index in 0..12 {
        let mut value = 0usize;
        for bit_index in 0..5 {
            let source_bit = character_index * 5 + bit_index;
            value =
                (value << 1) | usize::from((bytes[source_bit / 8] >> (7 - (source_bit % 8))) & 1);
        }
        signature.push(ALPHABET[value] as char);
        if character_index == 5 {
            signature.push('-');
        }
    }
    Ok(signature)
}

/// A content-free action that official noise clients can enforce after
/// verifying the pinned safety signing key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SafetyDirectiveActionV1 {
    SuppressEvent,
    RestrictGroup,
    RestrictIdentity,
    RestoreGroup,
    RestoreIdentity,
}

/// A signed, content-free safety decision intended for a future public feed.
///
/// It contains identifiers and policy metadata only. Report text, profile
/// snapshots, media bytes, fingerprints, and object locations stay private.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SafetyDirectiveV1 {
    pub version: u32,
    pub directive_id: String,
    pub action: SafetyDirectiveActionV1,
    pub reason: SafetyReportCategoryV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_public_key: Option<String>,
    pub issued_at_millis: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_millis: Option<u64>,
    pub signing_public_key: String,
    pub signature_base64: String,
}

#[derive(Serialize)]
struct UnsignedSafetyDirectiveV1<'a> {
    context: &'static str,
    version: u32,
    directive_id: &'a str,
    action: SafetyDirectiveActionV1,
    reason: SafetyReportCategoryV1,
    group_id: Option<&'a str>,
    event_id: Option<&'a str>,
    identity_public_key: Option<&'a str>,
    issued_at_millis: u64,
    expires_at_millis: Option<u64>,
    signing_public_key: &'a str,
}

/// The private Ed25519 signer derived from the safety recipient secret with a
/// role-specific KDF context.
#[derive(Clone)]
pub struct SafetyDirectiveSigningKeyPair {
    identity: Identity,
}

impl SafetyDirectiveSigningKeyPair {
    pub fn public_key_base64(&self) -> String {
        self.identity.public_key_base64()
    }
}

impl SafetyDirectiveV1 {
    pub fn suppress_event(
        signer: &SafetyDirectiveSigningKeyPair,
        reason: SafetyReportCategoryV1,
        group_id: String,
        event_id: String,
    ) -> Result<Self, NoiseError> {
        Self::create(
            signer,
            SafetyDirectiveActionV1::SuppressEvent,
            reason,
            Some(group_id),
            Some(event_id),
            None,
            None,
        )
    }

    pub fn restrict_group(
        signer: &SafetyDirectiveSigningKeyPair,
        reason: SafetyReportCategoryV1,
        group_id: String,
        expires_at_millis: Option<u64>,
    ) -> Result<Self, NoiseError> {
        Self::create(
            signer,
            SafetyDirectiveActionV1::RestrictGroup,
            reason,
            Some(group_id),
            None,
            None,
            expires_at_millis,
        )
    }

    pub fn restrict_identity(
        signer: &SafetyDirectiveSigningKeyPair,
        reason: SafetyReportCategoryV1,
        identity_public_key: String,
        expires_at_millis: Option<u64>,
    ) -> Result<Self, NoiseError> {
        Self::create(
            signer,
            SafetyDirectiveActionV1::RestrictIdentity,
            reason,
            None,
            None,
            Some(identity_public_key),
            expires_at_millis,
        )
    }

    pub fn restore_group(
        signer: &SafetyDirectiveSigningKeyPair,
        reason: SafetyReportCategoryV1,
        group_id: String,
    ) -> Result<Self, NoiseError> {
        Self::create(
            signer,
            SafetyDirectiveActionV1::RestoreGroup,
            reason,
            Some(group_id),
            None,
            None,
            None,
        )
    }

    pub fn restore_identity(
        signer: &SafetyDirectiveSigningKeyPair,
        reason: SafetyReportCategoryV1,
        identity_public_key: String,
    ) -> Result<Self, NoiseError> {
        Self::create(
            signer,
            SafetyDirectiveActionV1::RestoreIdentity,
            reason,
            None,
            None,
            Some(identity_public_key),
            None,
        )
    }

    pub fn verify_with_signing_public_key(
        &self,
        expected_public_key: &str,
    ) -> Result<(), NoiseError> {
        if self.version != SAFETY_DIRECTIVE_VERSION
            || !valid_hex_id(&self.directive_id)
            || self.issued_at_millis == 0
            || self.signing_public_key != expected_public_key
        {
            return Err(NoiseError::InvalidEncoding("safety directive"));
        }
        decode_array::<32>(
            &self.signing_public_key,
            "safety directive signing public key",
        )?;
        match self.action {
            SafetyDirectiveActionV1::SuppressEvent => {
                if !self.group_id.as_deref().is_some_and(valid_hex_id)
                    || !self.event_id.as_deref().is_some_and(valid_hex_id)
                    || self.identity_public_key.is_some()
                    || self.expires_at_millis.is_some()
                {
                    return Err(NoiseError::InvalidEncoding("safety directive"));
                }
            }
            SafetyDirectiveActionV1::RestrictGroup => {
                if !self.group_id.as_deref().is_some_and(valid_hex_id)
                    || self.event_id.is_some()
                    || self.identity_public_key.is_some()
                    || !valid_optional_restriction_expiry(
                        self.issued_at_millis,
                        self.expires_at_millis,
                    )
                {
                    return Err(NoiseError::InvalidEncoding("safety directive"));
                }
            }
            SafetyDirectiveActionV1::RestrictIdentity => {
                if self.group_id.is_some()
                    || self.event_id.is_some()
                    || self
                        .identity_public_key
                        .as_deref()
                        .is_none_or(|public_key| {
                            decode_array::<32>(public_key, "restricted identity public key")
                                .is_err()
                        })
                    || !valid_optional_restriction_expiry(
                        self.issued_at_millis,
                        self.expires_at_millis,
                    )
                {
                    return Err(NoiseError::InvalidEncoding("safety directive"));
                }
            }
            SafetyDirectiveActionV1::RestoreGroup => {
                if !self.group_id.as_deref().is_some_and(valid_hex_id)
                    || self.event_id.is_some()
                    || self.identity_public_key.is_some()
                    || self.expires_at_millis.is_some()
                {
                    return Err(NoiseError::InvalidEncoding("safety directive"));
                }
            }
            SafetyDirectiveActionV1::RestoreIdentity => {
                if self.group_id.is_some()
                    || self.event_id.is_some()
                    || self
                        .identity_public_key
                        .as_deref()
                        .is_none_or(|public_key| {
                            decode_array::<32>(public_key, "restored identity public key").is_err()
                        })
                    || self.expires_at_millis.is_some()
                {
                    return Err(NoiseError::InvalidEncoding("safety directive"));
                }
            }
        }
        verify_signature(
            &self.signing_public_key,
            &self.signature_base64,
            &self.signing_bytes()?,
        )
    }

    fn create(
        signer: &SafetyDirectiveSigningKeyPair,
        action: SafetyDirectiveActionV1,
        reason: SafetyReportCategoryV1,
        group_id: Option<String>,
        event_id: Option<String>,
        identity_public_key: Option<String>,
        expires_at_millis: Option<u64>,
    ) -> Result<Self, NoiseError> {
        let random_id: [u8; 32] = random();
        let mut directive = Self {
            version: SAFETY_DIRECTIVE_VERSION,
            directive_id: blake3::hash(&random_id).to_hex().to_string(),
            action,
            reason,
            group_id,
            event_id,
            identity_public_key,
            issued_at_millis: now_millis(),
            expires_at_millis,
            signing_public_key: signer.public_key_base64(),
            signature_base64: String::new(),
        };
        directive.signature_base64 = signer.identity.sign(&directive.signing_bytes()?);
        directive.verify_with_signing_public_key(&signer.public_key_base64())?;
        Ok(directive)
    }

    fn signing_bytes(&self) -> Result<Vec<u8>, NoiseError> {
        Ok(serde_json::to_vec(&UnsignedSafetyDirectiveV1 {
            context: SAFETY_DIRECTIVE_CONTEXT,
            version: self.version,
            directive_id: &self.directive_id,
            action: self.action,
            reason: self.reason,
            group_id: self.group_id.as_deref(),
            event_id: self.event_id.as_deref(),
            identity_public_key: self.identity_public_key.as_deref(),
            issued_at_millis: self.issued_at_millis,
            expires_at_millis: self.expires_at_millis,
            signing_public_key: &self.signing_public_key,
        })?)
    }
}

fn valid_optional_restriction_expiry(
    issued_at_millis: u64,
    expires_at_millis: Option<u64>,
) -> bool {
    let Some(expires_at_millis) = expires_at_millis else {
        return true;
    };
    expires_at_millis
        .checked_sub(issued_at_millis)
        .is_some_and(|duration| duration > 0 && duration <= MAX_GROUP_QUARANTINE_MILLIS)
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
/// without receiving media bytes. The exact text of the single reported
/// message may be included so staff can assess it without receiving surrounding
/// history.
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub human_context: Option<SafetyReportHumanContextV1>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    human_context: Option<&'a SafetyReportHumanContextV1>,
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
        Self::create_with_human_context(
            reporter,
            category,
            reported_event,
            group_context_proof,
            media_fingerprint,
            encrypted_objects,
            reported_text_excerpt,
            details,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_with_human_context(
        reporter: &Identity,
        category: SafetyReportCategoryV1,
        reported_event: SignedEvent,
        group_context_proof: Option<SignedEvent>,
        media_fingerprint: Option<SafetyMediaFingerprintV1>,
        encrypted_objects: Vec<SafetyEncryptedObjectV1>,
        reported_text_excerpt: Option<String>,
        details: Option<String>,
        human_context: Option<SafetyReportHumanContextV1>,
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
            human_context,
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
                text.trim().is_empty() || text.chars().count() > MAX_REPORTED_TEXT_CHARS
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

        if let Some(context) = &self.human_context {
            self.verify_human_context(context)?;
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
            human_context: self.human_context.as_ref(),
            created_at_millis: self.created_at_millis,
        })?)
    }

    fn verify_human_context(&self, context: &SafetyReportHumanContextV1) -> Result<(), NoiseError> {
        if let Some(reporter) = &context.reporter {
            verify_profile_snapshot(&reporter.profile)?;
            if reporter.profile.public_key != self.reporter_public_key {
                return Err(NoiseError::IdentityMismatch);
            }
        }
        if let Some(author) = &context.reported_author {
            verify_profile_snapshot(author)?;
            if author.public_key != self.reported_event.author_public_key {
                return Err(NoiseError::IdentityMismatch);
            }
        }
        if let Some(group) = &context.group {
            let name_length = group.name.trim().chars().count();
            if !(1..=80).contains(&name_length)
                || group.name.chars().any(char::is_control)
                || group.reported_moderators.len() > MAX_SAFETY_GROUP_STAFF
                || group.founder.is_some() != group.authority_nonce_base64.is_some()
            {
                return Err(NoiseError::InvalidEncoding("safety group context"));
            }
            if let (Some(founder), Some(authority_nonce_base64)) =
                (&group.founder, &group.authority_nonce_base64)
            {
                verify_profile_snapshot(founder)?;
                let authority_nonce =
                    decode_array::<32>(authority_nonce_base64, "group authority nonce")?;
                if authoritative_group_id(&founder.public_key, &authority_nonce)
                    != self.reported_event.group_id
                {
                    return Err(NoiseError::InvalidGroupAuthority);
                }
            }
            let mut moderator_keys = HashSet::with_capacity(group.reported_moderators.len());
            for moderator in &group.reported_moderators {
                verify_profile_snapshot(moderator)?;
                if !moderator_keys.insert(moderator.public_key.as_str())
                    || group
                        .founder
                        .as_ref()
                        .is_some_and(|founder| founder.public_key == moderator.public_key)
                {
                    return Err(NoiseError::InvalidEncoding("safety group context"));
                }
            }
        }
        Ok(())
    }
}

fn verify_profile_snapshot(profile: &SafetyProfileSnapshotV1) -> Result<(), NoiseError> {
    decode_array::<32>(&profile.public_key, "safety profile public key")?;
    let username_length = profile.username.trim().chars().count();
    if !(1..=32).contains(&username_length) || profile.username.chars().any(char::is_control) {
        return Err(NoiseError::InvalidEncoding("safety profile snapshot"));
    }
    Ok(())
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

    pub fn directive_signing_key_pair(&self) -> Result<SafetyDirectiveSigningKeyPair, NoiseError> {
        let signing_secret = blake3::derive_key(SAFETY_DIRECTIVE_SIGNING_KEY_CONTEXT, &self.secret);
        Ok(SafetyDirectiveSigningKeyPair {
            identity: Identity::from_secret_base64(&STANDARD_NO_PAD.encode(signing_secret))?,
        })
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
        let report = SafetyReportV1::create_with_human_context(
            &reporter,
            SafetyReportCategoryV1::HarassmentOrHatefulBehavior,
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
            Some("The exact text of the reported message.".into()),
            Some("Group staff may be involved.".into()),
            Some(SafetyReportHumanContextV1 {
                reporter: Some(SafetyReporterContextV1 {
                    profile: SafetyProfileSnapshotV1 {
                        public_key: reporter.public_key_base64(),
                        username: "reporter".into(),
                        direct_message_policy: DirectMessagePolicy::Everyone,
                    },
                    follow_up_allowed: Some(true),
                }),
                reported_author: Some(SafetyProfileSnapshotV1 {
                    public_key: author.public_key_base64(),
                    username: "reported author".into(),
                    direct_message_policy: DirectMessagePolicy::Nobody,
                }),
                group: Some(SafetyGroupContextV1 {
                    name: group.name.clone(),
                    founder: Some(SafetyProfileSnapshotV1 {
                        public_key: author.public_key_base64(),
                        username: "reported author".into(),
                        direct_message_policy: DirectMessagePolicy::Nobody,
                    }),
                    authority_nonce_base64: Some(group.authority_nonce_base64.clone()),
                    reported_moderators: Vec::new(),
                }),
            }),
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
            Some("The exact text of the reported message.")
        );
        assert_eq!(
            opened
                .human_context
                .as_ref()
                .and_then(|context| context.reported_author.as_ref())
                .map(|profile| profile.username.as_str()),
            Some("reported author")
        );
        assert_eq!(decoded.recipient_key_id, restored_key.key_id());
        assert!(
            SafetyEncryptionKeyPair::generate()
                .unwrap()
                .open(&decoded)
                .is_err()
        );
    }

    #[test]
    fn signed_safety_directives_cover_restrictions_and_restores() {
        let safety_key = SafetyEncryptionKeyPair::generate().unwrap();
        let signer = safety_key.directive_signing_key_pair().unwrap();
        let signing_public_key = signer.public_key_base64();
        let identity = Identity::generate();
        let group_id = "a".repeat(64);

        let pause = SafetyDirectiveV1::restrict_group(
            &signer,
            SafetyReportCategoryV1::ChildSafety,
            group_id.clone(),
            Some(now_millis() + 24 * 60 * 60 * 1_000),
        )
        .unwrap();
        let block = SafetyDirectiveV1::restrict_identity(
            &signer,
            SafetyReportCategoryV1::ChildSafety,
            identity.public_key_base64(),
            None,
        )
        .unwrap();
        let restore =
            SafetyDirectiveV1::restore_group(&signer, SafetyReportCategoryV1::Other, group_id)
                .unwrap();

        pause
            .verify_with_signing_public_key(&signing_public_key)
            .unwrap();
        block
            .verify_with_signing_public_key(&signing_public_key)
            .unwrap();
        restore
            .verify_with_signing_public_key(&signing_public_key)
            .unwrap();
        assert_eq!(
            noise_signature_for_public_key(&identity.public_key_base64())
                .unwrap()
                .len(),
            13
        );

        let mut tampered = block;
        tampered.identity_public_key = Some(Identity::generate().public_key_base64());
        assert!(
            tampered
                .verify_with_signing_public_key(&signing_public_key)
                .is_err()
        );
    }
}
