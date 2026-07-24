use std::{
    collections::{HashMap, HashSet},
    sync::RwLock,
};

use base64::{Engine as _, engine::general_purpose::STANDARD_NO_PAD};
use chacha20poly1305::{
    KeyInit, XChaCha20Poly1305, XNonce,
    aead::{Aead, Payload},
};
use openmls::prelude::{
    BasicCredential, Ciphersuite, CredentialWithKey, GroupId, KeyPackage, KeyPackageIn,
    LeafNodeIndex, Member, MlsGroup, MlsGroupCreateConfig, MlsGroupJoinConfig, MlsMessageBodyIn,
    MlsMessageIn, ProcessedMessageContent, ProtocolMessage, ProtocolVersion, StagedWelcome,
    tls_codec::{Deserialize as _, Serialize as _},
};
use openmls_basic_credential::SignatureKeyPair;
use openmls_memory_storage::MemoryStorage;
use openmls_rust_crypto::RustCrypto;
use openmls_traits::OpenMlsProvider;
use rand::random;
use serde::{Deserialize, Serialize};

use crate::{
    GroupMembership, Identity, NoiseError, SignedEvent, authoritative_group_id, decode,
    decode_array, now_millis, verify_signature,
};

const MLS_STATE_VERSION: u32 = 1;
const MLS_DEVICE_CREDENTIAL_VERSION: u32 = 1;
const HISTORY_LINK_VERSION: u32 = 1;
const LEGACY_HISTORY_BRIDGE_VERSION: u32 = 1;
const MLS_CONTROL_VERSION: u32 = 1;
const ARCHIVE_EXPORT_LABEL: &str = "xyz.gnosyslabs.noise.archive-root.v1";
const ARCHIVE_LINK_CONTEXT: &str = "xyz.gnosyslabs.noise.archive-link.v1";
const LEGACY_BRIDGE_CONTEXT: &str = "xyz.gnosyslabs.noise.legacy-history-bridge.v1";
const DEVICE_CREDENTIAL_CONTEXT: &str = "xyz.gnosyslabs.noise.mls-device-credential.v1";
const JOIN_REQUEST_CONTEXT: &str = "xyz.gnosyslabs.noise.mls-join-request.v1";
const REMOVAL_REQUEST_CONTEXT: &str = "xyz.gnosyslabs.noise.mls-removal-request.v1";
const GENESIS_CONTEXT: &str = "xyz.gnosyslabs.noise.mls-genesis.v1";
const EPOCH_RECORD_CONTEXT: &str = "xyz.gnosyslabs.noise.mls-epoch-record.v1";
const CIPHERSUITE: Ciphersuite = Ciphersuite::MLS_128_DHKEMX25519_CHACHA20POLY1305_SHA256_Ed25519;

#[derive(Debug)]
struct NoiseMlsProvider {
    crypto: RustCrypto,
    storage: MemoryStorage,
}

impl Default for NoiseMlsProvider {
    fn default() -> Self {
        Self {
            crypto: RustCrypto::default(),
            storage: MemoryStorage::default(),
        }
    }
}

impl OpenMlsProvider for NoiseMlsProvider {
    type CryptoProvider = RustCrypto;
    type RandProvider = RustCrypto;
    type StorageProvider = MemoryStorage;

    fn storage(&self) -> &Self::StorageProvider {
        &self.storage
    }

    fn crypto(&self) -> &Self::CryptoProvider {
        &self.crypto
    }

    fn rand(&self) -> &Self::RandProvider {
        &self.crypto
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct MlsStorageSnapshot {
    entries: Vec<(String, String)>,
}

impl MlsStorageSnapshot {
    fn from_provider(provider: &NoiseMlsProvider) -> Result<Self, NoiseError> {
        let values = provider
            .storage
            .values
            .read()
            .map_err(|_| NoiseError::InvalidMlsState)?;
        let mut entries = values
            .iter()
            .map(|(key, value)| (STANDARD_NO_PAD.encode(key), STANDARD_NO_PAD.encode(value)))
            .collect::<Vec<_>>();
        entries.sort();
        Ok(Self { entries })
    }

    fn into_provider(self) -> Result<NoiseMlsProvider, NoiseError> {
        let mut values = HashMap::with_capacity(self.entries.len());
        for (key, value) in self.entries {
            let key = STANDARD_NO_PAD
                .decode(key)
                .map_err(|_| NoiseError::InvalidMlsState)?;
            let value = STANDARD_NO_PAD
                .decode(value)
                .map_err(|_| NoiseError::InvalidMlsState)?;
            if values.insert(key, value).is_some() {
                return Err(NoiseError::InvalidMlsState);
            }
        }
        Ok(NoiseMlsProvider {
            crypto: RustCrypto::default(),
            storage: MemoryStorage {
                values: RwLock::new(values),
            },
        })
    }
}

/// Persisted MLS material for one Noise identity.
///
/// Despite the account-scoped name, this is deliberately unique to one device.
/// It contains signature keys, HPKE private keys, and MLS ratchet state and
/// must stay inside that device's encrypted local vault. The synchronized
/// account vault may contain its public credential, but never this value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MlsAccountState {
    version: u32,
    account_public_key: String,
    signer_public_key_base64: String,
    device_credential: MlsDeviceCredential,
    storage: MlsStorageSnapshot,
}

/// An MLS device leaf authorized by the long-lived Noise account identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MlsDeviceCredential {
    pub version: u32,
    pub account_public_key: String,
    pub device_id_base64: String,
    pub mls_signature_public_key_base64: String,
    pub account_signature_base64: String,
}

impl MlsDeviceCredential {
    fn create(identity: &Identity, signer_public_key_base64: String) -> Result<Self, NoiseError> {
        decode_array::<32>(&signer_public_key_base64, "MLS signing key")?;
        let mut credential = Self {
            version: MLS_DEVICE_CREDENTIAL_VERSION,
            account_public_key: identity.public_key_base64(),
            device_id_base64: STANDARD_NO_PAD.encode(random::<[u8; 32]>()),
            mls_signature_public_key_base64: signer_public_key_base64,
            account_signature_base64: String::new(),
        };
        credential.account_signature_base64 = identity.sign(&credential.signing_bytes());
        credential.validate()?;
        Ok(credential)
    }

    pub fn validate(&self) -> Result<(), NoiseError> {
        if self.version != MLS_DEVICE_CREDENTIAL_VERSION {
            return Err(NoiseError::InvalidMlsState);
        }
        decode_array::<32>(&self.account_public_key, "identity public key")?;
        decode_array::<32>(&self.device_id_base64, "MLS device id")?;
        decode_array::<32>(&self.mls_signature_public_key_base64, "MLS signing key")?;
        verify_signature(
            &self.account_public_key,
            &self.account_signature_base64,
            &self.signing_bytes(),
        )
    }

    fn signing_bytes(&self) -> Vec<u8> {
        format!(
            "{DEVICE_CREDENTIAL_CONTEXT}:{}:{}:{}:{}",
            self.version,
            self.account_public_key,
            self.device_id_base64,
            self.mls_signature_public_key_base64
        )
        .into_bytes()
    }
}

/// Public result of a successful MLS epoch transition.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MlsEpochSummary {
    pub group_id: String,
    pub epoch: u64,
    pub archive_key_base64: String,
}

/// The control records produced by an MLS add or remove commit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MlsCommitBundle {
    pub group_id: String,
    pub parent_epoch: u64,
    pub epoch: u64,
    pub commit_base64: String,
    pub welcome_base64: Option<String>,
    pub history_link: HistoryKeyLink,
}

/// The one-time bridge that keeps pre-MLS history readable after cutover.
///
/// It encrypts the former group secret under MLS epoch zero's archive root.
/// The legacy secret is never used to encrypt events created after cutover.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MlsLegacyHistoryBridge {
    pub version: u32,
    pub group_id: String,
    pub epoch: u64,
    pub nonce_base64: String,
    pub ciphertext_base64: String,
}

impl MlsLegacyHistoryBridge {
    pub fn create(group: &GroupMembership, epoch: &MlsEpochSummary) -> Result<Self, NoiseError> {
        if epoch.group_id != group.group_id || epoch.epoch != 0 {
            return Err(NoiseError::InvalidMlsState);
        }
        let archive_key = decode_array::<32>(&epoch.archive_key_base64, "archive key")?;
        let legacy_key = decode_array::<32>(&group.secret_base64, "group secret")?;
        let nonce: [u8; 24] = random();
        let ciphertext = XChaCha20Poly1305::new((&archive_key).into())
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: &legacy_key,
                    aad: &legacy_bridge_aad(&group.group_id),
                },
            )
            .map_err(|_| NoiseError::Crypto)?;
        Ok(Self {
            version: LEGACY_HISTORY_BRIDGE_VERSION,
            group_id: group.group_id.clone(),
            epoch: 0,
            nonce_base64: STANDARD_NO_PAD.encode(nonce),
            ciphertext_base64: STANDARD_NO_PAD.encode(ciphertext),
        })
    }

    pub fn open(&self, archive_key_base64: &str) -> Result<String, NoiseError> {
        if self.version != LEGACY_HISTORY_BRIDGE_VERSION
            || self.epoch != 0
            || self.group_id.is_empty()
        {
            return Err(NoiseError::InvalidMlsState);
        }
        let archive_key = decode_array::<32>(archive_key_base64, "archive key")?;
        let nonce = decode_array::<24>(&self.nonce_base64, "legacy bridge nonce")?;
        let ciphertext = decode(&self.ciphertext_base64, "legacy bridge ciphertext")?;
        let plaintext = XChaCha20Poly1305::new((&archive_key).into())
            .decrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: &ciphertext,
                    aad: &legacy_bridge_aad(&self.group_id),
                },
            )
            .map_err(|_| NoiseError::Crypto)?;
        if plaintext.len() != 32 {
            return Err(NoiseError::InvalidMlsState);
        }
        Ok(STANDARD_NO_PAD.encode(plaintext))
    }
}

/// A signed, group-scoped KeyPackage request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MlsJoinRequest {
    pub version: u32,
    pub request_id: String,
    pub group_id: String,
    pub account_public_key: String,
    pub device_credential: MlsDeviceCredential,
    pub key_package_base64: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub membership_proof: Option<SignedEvent>,
    pub created_at_millis: u64,
    pub signature_base64: String,
}

#[derive(Serialize)]
struct UnsignedMlsJoinRequest<'a> {
    version: u32,
    group_id: &'a str,
    account_public_key: &'a str,
    device_credential: &'a MlsDeviceCredential,
    key_package_base64: &'a str,
    membership_proof: Option<&'a SignedEvent>,
    created_at_millis: u64,
}

impl MlsJoinRequest {
    pub fn create(
        identity: &Identity,
        mls: &mut MlsAccountState,
        group_id: impl Into<String>,
    ) -> Result<Self, NoiseError> {
        Self::create_internal(identity, mls, group_id.into(), None)
    }

    pub fn create_with_membership_proof(
        identity: &Identity,
        mls: &mut MlsAccountState,
        group_id: impl Into<String>,
        membership_proof: SignedEvent,
    ) -> Result<Self, NoiseError> {
        Self::create_internal(identity, mls, group_id.into(), Some(membership_proof))
    }

    fn create_internal(
        identity: &Identity,
        mls: &mut MlsAccountState,
        group_id: String,
        membership_proof: Option<SignedEvent>,
    ) -> Result<Self, NoiseError> {
        let mut request = Self {
            version: MLS_CONTROL_VERSION,
            request_id: String::new(),
            group_id,
            account_public_key: identity.public_key_base64(),
            device_credential: mls.device_credential().clone(),
            key_package_base64: mls.key_package()?,
            membership_proof,
            created_at_millis: now_millis(),
            signature_base64: String::new(),
        };
        let unsigned = request.unsigned_bytes()?;
        request.request_id = blake3::hash(&unsigned).to_hex().to_string();
        request.signature_base64 =
            identity.sign(&join_request_signing_bytes(&request.request_id, &unsigned));
        request.verify()?;
        Ok(request)
    }

    pub fn verify(&self) -> Result<(), NoiseError> {
        if self.version != MLS_CONTROL_VERSION
            || !valid_group_id(&self.group_id)
            || !valid_record_id(&self.request_id)
            || self.key_package_base64.is_empty()
            || self.key_package_base64.len() > 131_072
            || self.device_credential.account_public_key != self.account_public_key
        {
            return Err(NoiseError::InvalidMlsState);
        }
        decode_array::<32>(&self.account_public_key, "identity public key")?;
        self.device_credential.validate()?;
        if let Some(proof) = &self.membership_proof {
            proof.verify()?;
            if proof.group_id != self.group_id
                || proof.author_public_key != self.account_public_key
                || proof.encryption_version != 1
                || proof.epoch.is_some()
            {
                return Err(NoiseError::InvalidMlsState);
            }
        }
        let unsigned = self.unsigned_bytes()?;
        if blake3::hash(&unsigned).to_hex().as_str() != self.request_id {
            return Err(NoiseError::InvalidMlsState);
        }
        verify_signature(
            &self.account_public_key,
            &self.signature_base64,
            &join_request_signing_bytes(&self.request_id, &unsigned),
        )
    }

    fn unsigned_bytes(&self) -> Result<Vec<u8>, NoiseError> {
        Ok(serde_json::to_vec(&UnsignedMlsJoinRequest {
            version: self.version,
            group_id: &self.group_id,
            account_public_key: &self.account_public_key,
            device_credential: &self.device_credential,
            key_package_base64: &self.key_package_base64,
            membership_proof: self.membership_proof.as_ref(),
            created_at_millis: self.created_at_millis,
        })?)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MlsRemovalReason {
    SelfLeft,
    Banned,
}

/// A signed request for the founder to remove every MLS leaf belonging to an
/// account. Self-leave requests are authorized directly by the departing
/// account; ban requests are validated against the encrypted moderator state
/// by the founder before a removal commit is created.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MlsRemovalRequest {
    pub version: u32,
    pub request_id: String,
    pub group_id: String,
    pub requester_public_key: String,
    pub target_public_key: String,
    pub reason: MlsRemovalReason,
    pub delete_messages: bool,
    pub created_at_millis: u64,
    pub signature_base64: String,
}

#[derive(Serialize)]
struct UnsignedMlsRemovalRequest<'a> {
    version: u32,
    group_id: &'a str,
    requester_public_key: &'a str,
    target_public_key: &'a str,
    reason: MlsRemovalReason,
    delete_messages: bool,
    created_at_millis: u64,
}

impl MlsRemovalRequest {
    pub fn self_left(identity: &Identity, group_id: impl Into<String>) -> Result<Self, NoiseError> {
        let public_key = identity.public_key_base64();
        Self::create(
            identity,
            group_id.into(),
            public_key.clone(),
            MlsRemovalReason::SelfLeft,
            false,
        )
    }

    pub fn member_banned(
        identity: &Identity,
        group_id: impl Into<String>,
        target_public_key: impl Into<String>,
        delete_messages: bool,
    ) -> Result<Self, NoiseError> {
        Self::create(
            identity,
            group_id.into(),
            target_public_key.into(),
            MlsRemovalReason::Banned,
            delete_messages,
        )
    }

    fn create(
        identity: &Identity,
        group_id: String,
        target_public_key: String,
        reason: MlsRemovalReason,
        delete_messages: bool,
    ) -> Result<Self, NoiseError> {
        let mut request = Self {
            version: MLS_CONTROL_VERSION,
            request_id: String::new(),
            group_id,
            requester_public_key: identity.public_key_base64(),
            target_public_key,
            reason,
            delete_messages,
            created_at_millis: now_millis(),
            signature_base64: String::new(),
        };
        let unsigned = request.unsigned_bytes()?;
        request.request_id = blake3::hash(&unsigned).to_hex().to_string();
        request.signature_base64 = identity.sign(&removal_request_signing_bytes(
            &request.request_id,
            &unsigned,
        ));
        request.verify()?;
        Ok(request)
    }

    pub fn verify(&self) -> Result<(), NoiseError> {
        if self.version != MLS_CONTROL_VERSION
            || !valid_group_id(&self.group_id)
            || !valid_record_id(&self.request_id)
        {
            return Err(NoiseError::InvalidMlsState);
        }
        decode_array::<32>(&self.requester_public_key, "identity public key")?;
        decode_array::<32>(&self.target_public_key, "identity public key")?;
        match self.reason {
            MlsRemovalReason::SelfLeft
                if self.requester_public_key == self.target_public_key && !self.delete_messages => {
            }
            MlsRemovalReason::Banned if self.requester_public_key != self.target_public_key => {}
            _ => return Err(NoiseError::InvalidMlsState),
        }
        let unsigned = self.unsigned_bytes()?;
        if blake3::hash(&unsigned).to_hex().as_str() != self.request_id {
            return Err(NoiseError::InvalidMlsState);
        }
        verify_signature(
            &self.requester_public_key,
            &self.signature_base64,
            &removal_request_signing_bytes(&self.request_id, &unsigned),
        )
    }

    fn unsigned_bytes(&self) -> Result<Vec<u8>, NoiseError> {
        Ok(serde_json::to_vec(&UnsignedMlsRemovalRequest {
            version: self.version,
            group_id: &self.group_id,
            requester_public_key: &self.requester_public_key,
            target_public_key: &self.target_public_key,
            reason: self.reason,
            delete_messages: self.delete_messages,
            created_at_millis: self.created_at_millis,
        })?)
    }
}

/// Epoch-zero control record for either a new group or a legacy-group cutover.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MlsGroupGenesis {
    pub version: u32,
    pub record_id: String,
    pub group_id: String,
    pub owner_public_key: String,
    pub authority_nonce_base64: String,
    pub founder_device_credential: MlsDeviceCredential,
    pub member_accounts: Vec<String>,
    pub legacy_history_bridge: MlsLegacyHistoryBridge,
    pub created_at_millis: u64,
    pub signature_base64: String,
}

#[derive(Serialize)]
struct UnsignedMlsGroupGenesis<'a> {
    version: u32,
    group_id: &'a str,
    owner_public_key: &'a str,
    authority_nonce_base64: &'a str,
    founder_device_credential: &'a MlsDeviceCredential,
    member_accounts: &'a [String],
    legacy_history_bridge: &'a MlsLegacyHistoryBridge,
    created_at_millis: u64,
}

impl MlsGroupGenesis {
    fn create(
        identity: &Identity,
        group: &GroupMembership,
        founder_device_credential: MlsDeviceCredential,
        legacy_history_bridge: MlsLegacyHistoryBridge,
    ) -> Result<Self, NoiseError> {
        let mut genesis = Self {
            version: MLS_CONTROL_VERSION,
            record_id: String::new(),
            group_id: group.group_id.clone(),
            owner_public_key: identity.public_key_base64(),
            authority_nonce_base64: group.authority_nonce_base64.clone(),
            founder_device_credential,
            member_accounts: vec![identity.public_key_base64()],
            legacy_history_bridge,
            created_at_millis: now_millis(),
            signature_base64: String::new(),
        };
        let unsigned = genesis.unsigned_bytes()?;
        genesis.record_id = blake3::hash(&unsigned).to_hex().to_string();
        genesis.signature_base64 = identity.sign(&control_signing_bytes(
            GENESIS_CONTEXT,
            &genesis.record_id,
            &unsigned,
        ));
        genesis.verify()?;
        Ok(genesis)
    }

    pub fn verify(&self) -> Result<(), NoiseError> {
        if self.version != MLS_CONTROL_VERSION
            || !valid_record_id(&self.record_id)
            || self.legacy_history_bridge.group_id != self.group_id
            || self.legacy_history_bridge.epoch != 0
            || self.founder_device_credential.account_public_key != self.owner_public_key
            || self.member_accounts != [self.owner_public_key.clone()]
        {
            return Err(NoiseError::InvalidMlsState);
        }
        let authority_nonce =
            decode_array::<32>(&self.authority_nonce_base64, "group authority nonce")?;
        decode_array::<32>(&self.owner_public_key, "identity public key")?;
        if authoritative_group_id(&self.owner_public_key, &authority_nonce) != self.group_id {
            return Err(NoiseError::InvalidGroupAuthority);
        }
        self.founder_device_credential.validate()?;
        let unsigned = self.unsigned_bytes()?;
        if blake3::hash(&unsigned).to_hex().as_str() != self.record_id {
            return Err(NoiseError::InvalidMlsState);
        }
        verify_signature(
            &self.owner_public_key,
            &self.signature_base64,
            &control_signing_bytes(GENESIS_CONTEXT, &self.record_id, &unsigned),
        )
    }

    fn unsigned_bytes(&self) -> Result<Vec<u8>, NoiseError> {
        Ok(serde_json::to_vec(&UnsignedMlsGroupGenesis {
            version: self.version,
            group_id: &self.group_id,
            owner_public_key: &self.owner_public_key,
            authority_nonce_base64: &self.authority_nonce_base64,
            founder_device_credential: &self.founder_device_credential,
            member_accounts: &self.member_accounts,
            legacy_history_bridge: &self.legacy_history_bridge,
            created_at_millis: self.created_at_millis,
        })?)
    }
}

/// One strictly ordered MLS epoch transition in the relay control log.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MlsEpochRecord {
    pub version: u32,
    pub record_id: String,
    pub previous_record_id: String,
    pub owner_public_key: String,
    pub member_accounts: Vec<String>,
    pub bundle: MlsCommitBundle,
    pub created_at_millis: u64,
    pub signature_base64: String,
}

#[derive(Serialize)]
struct UnsignedMlsEpochRecord<'a> {
    version: u32,
    previous_record_id: &'a str,
    owner_public_key: &'a str,
    member_accounts: &'a [String],
    bundle: &'a MlsCommitBundle,
    created_at_millis: u64,
}

impl MlsEpochRecord {
    pub fn create(
        identity: &Identity,
        previous_record_id: impl Into<String>,
        bundle: MlsCommitBundle,
        mut member_accounts: Vec<String>,
    ) -> Result<Self, NoiseError> {
        member_accounts.sort();
        member_accounts.dedup();
        let mut record = Self {
            version: MLS_CONTROL_VERSION,
            record_id: String::new(),
            previous_record_id: previous_record_id.into(),
            owner_public_key: identity.public_key_base64(),
            member_accounts,
            bundle,
            created_at_millis: now_millis(),
            signature_base64: String::new(),
        };
        let unsigned = record.unsigned_bytes()?;
        record.record_id = blake3::hash(&unsigned).to_hex().to_string();
        record.signature_base64 = identity.sign(&control_signing_bytes(
            EPOCH_RECORD_CONTEXT,
            &record.record_id,
            &unsigned,
        ));
        record.verify()?;
        Ok(record)
    }

    pub fn verify(&self) -> Result<(), NoiseError> {
        if self.version != MLS_CONTROL_VERSION
            || !valid_record_id(&self.record_id)
            || !valid_record_id(&self.previous_record_id)
            || !valid_group_id(&self.bundle.group_id)
            || self.bundle.epoch != self.bundle.parent_epoch.saturating_add(1)
            || self.bundle.history_link.group_id != self.bundle.group_id
            || self.bundle.history_link.epoch != self.bundle.epoch
            || self.bundle.history_link.previous_epoch != self.bundle.parent_epoch
            || self.member_accounts.is_empty()
            || !self
                .member_accounts
                .windows(2)
                .all(|members| members[0] < members[1])
            || !self.member_accounts.contains(&self.owner_public_key)
        {
            return Err(NoiseError::InvalidMlsState);
        }
        decode_array::<32>(&self.owner_public_key, "identity public key")?;
        let unsigned = self.unsigned_bytes()?;
        if blake3::hash(&unsigned).to_hex().as_str() != self.record_id {
            return Err(NoiseError::InvalidMlsState);
        }
        verify_signature(
            &self.owner_public_key,
            &self.signature_base64,
            &control_signing_bytes(EPOCH_RECORD_CONTEXT, &self.record_id, &unsigned),
        )
    }

    fn unsigned_bytes(&self) -> Result<Vec<u8>, NoiseError> {
        Ok(serde_json::to_vec(&UnsignedMlsEpochRecord {
            version: self.version,
            previous_record_id: &self.previous_record_id,
            owner_public_key: &self.owner_public_key,
            member_accounts: &self.member_accounts,
            bundle: &self.bundle,
            created_at_millis: self.created_at_millis,
        })?)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MlsControlLog {
    pub genesis: MlsGroupGenesis,
    pub epochs: Vec<MlsEpochRecord>,
}

impl MlsControlLog {
    pub fn verify(&self) -> Result<(), NoiseError> {
        self.genesis.verify()?;
        let mut parent_epoch = 0;
        let mut previous_record_id = self.genesis.record_id.as_str();
        for record in &self.epochs {
            record.verify()?;
            if record.bundle.group_id != self.genesis.group_id
                || record.owner_public_key != self.genesis.owner_public_key
                || record.bundle.parent_epoch != parent_epoch
                || record.previous_record_id != previous_record_id
            {
                return Err(NoiseError::InvalidMlsState);
            }
            parent_epoch = record.bundle.epoch;
            previous_record_id = &record.record_id;
        }
        Ok(())
    }

    pub fn head(&self) -> (u64, &str) {
        self.epochs
            .last()
            .map(|record| (record.bundle.epoch, record.record_id.as_str()))
            .unwrap_or((0, self.genesis.record_id.as_str()))
    }

    pub fn member_accounts_at(&self, epoch: u64) -> Option<&[String]> {
        if epoch == 0 {
            return Some(&self.genesis.member_accounts);
        }
        self.epochs
            .iter()
            .find(|record| record.bundle.epoch == epoch)
            .map(|record| record.member_accounts.as_slice())
    }
}

/// A one-way link from a new archive epoch to the preceding archive key.
///
/// Possessing the new key opens the old key. Possessing only the old key gives
/// no information about the new key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryKeyLink {
    pub version: u32,
    pub group_id: String,
    pub epoch: u64,
    pub previous_epoch: u64,
    pub nonce_base64: String,
    pub ciphertext_base64: String,
}

#[derive(Serialize, Deserialize)]
struct HistoryKeyLinkPayload {
    previous_epoch: u64,
    previous_key_base64: String,
}

impl HistoryKeyLink {
    pub fn create(
        group_id: &str,
        epoch: u64,
        current_key_base64: &str,
        previous_epoch: u64,
        previous_key_base64: &str,
    ) -> Result<Self, NoiseError> {
        if epoch != previous_epoch.saturating_add(1) || group_id.is_empty() {
            return Err(NoiseError::InvalidMlsState);
        }
        let current_key = decode_array::<32>(current_key_base64, "archive key")?;
        decode_array::<32>(previous_key_base64, "previous archive key")?;
        let nonce: [u8; 24] = random();
        let plaintext = serde_json::to_vec(&HistoryKeyLinkPayload {
            previous_epoch,
            previous_key_base64: previous_key_base64.to_owned(),
        })?;
        let aad = history_link_aad(group_id, epoch, previous_epoch);
        let ciphertext = XChaCha20Poly1305::new((&current_key).into())
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: &plaintext,
                    aad: &aad,
                },
            )
            .map_err(|_| NoiseError::Crypto)?;
        Ok(Self {
            version: HISTORY_LINK_VERSION,
            group_id: group_id.to_owned(),
            epoch,
            previous_epoch,
            nonce_base64: STANDARD_NO_PAD.encode(nonce),
            ciphertext_base64: STANDARD_NO_PAD.encode(ciphertext),
        })
    }

    pub fn open(&self, current_key_base64: &str) -> Result<String, NoiseError> {
        if self.version != HISTORY_LINK_VERSION
            || self.epoch != self.previous_epoch.saturating_add(1)
            || self.group_id.is_empty()
        {
            return Err(NoiseError::InvalidMlsState);
        }
        let key = decode_array::<32>(current_key_base64, "archive key")?;
        let nonce = decode_array::<24>(&self.nonce_base64, "archive link nonce")?;
        let ciphertext = decode(&self.ciphertext_base64, "archive link ciphertext")?;
        let aad = history_link_aad(&self.group_id, self.epoch, self.previous_epoch);
        let plaintext = XChaCha20Poly1305::new((&key).into())
            .decrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: &ciphertext,
                    aad: &aad,
                },
            )
            .map_err(|_| NoiseError::Crypto)?;
        let payload: HistoryKeyLinkPayload = serde_json::from_slice(&plaintext)?;
        if payload.previous_epoch != self.previous_epoch {
            return Err(NoiseError::InvalidMlsState);
        }
        decode_array::<32>(&payload.previous_key_base64, "previous archive key")?;
        Ok(payload.previous_key_base64)
    }

    pub fn unlock_history(
        group_id: &str,
        current_epoch: u64,
        current_key_base64: &str,
        links: &[Self],
    ) -> Result<HashMap<u64, String>, NoiseError> {
        decode_array::<32>(current_key_base64, "archive key")?;
        let mut by_epoch = HashMap::with_capacity(links.len());
        for link in links {
            if link.group_id != group_id || by_epoch.insert(link.epoch, link).is_some() {
                return Err(NoiseError::InvalidMlsState);
            }
        }
        let mut unlocked = HashMap::new();
        let mut visited = HashSet::new();
        let mut epoch = current_epoch;
        let mut key = current_key_base64.to_owned();
        unlocked.insert(epoch, key.clone());
        while let Some(link) = by_epoch.get(&epoch) {
            if !visited.insert(epoch) || link.previous_epoch >= epoch {
                return Err(NoiseError::InvalidMlsState);
            }
            key = link.open(&key)?;
            epoch = link.previous_epoch;
            unlocked.insert(epoch, key.clone());
        }
        Ok(unlocked)
    }
}

impl MlsAccountState {
    pub fn create(identity: &Identity) -> Result<Self, NoiseError> {
        let account_public_key = identity.public_key_base64();
        let provider = NoiseMlsProvider::default();
        let signer = SignatureKeyPair::new(CIPHERSUITE.signature_algorithm())
            .map_err(|_| NoiseError::Mls)?;
        signer
            .store(provider.storage())
            .map_err(|_| NoiseError::Mls)?;
        let signer_public_key_base64 = STANDARD_NO_PAD.encode(signer.public());
        let device_credential =
            MlsDeviceCredential::create(identity, signer_public_key_base64.clone())?;
        Ok(Self {
            version: MLS_STATE_VERSION,
            account_public_key,
            signer_public_key_base64,
            device_credential,
            storage: MlsStorageSnapshot::from_provider(&provider)?,
        })
    }

    pub fn device_credential(&self) -> &MlsDeviceCredential {
        &self.device_credential
    }

    pub fn key_package(&mut self) -> Result<String, NoiseError> {
        self.validate()?;
        let provider = self.take_provider()?;
        let signer = self.signer(&provider)?;
        let bundle = KeyPackage::builder()
            .build(CIPHERSUITE, &provider, &signer, self.credential_with_key()?)
            .map_err(|_| NoiseError::Mls)?;
        let bytes = bundle
            .key_package()
            .tls_serialize_detached()
            .map_err(|_| NoiseError::Mls)?;
        self.storage = MlsStorageSnapshot::from_provider(&provider)?;
        Ok(STANDARD_NO_PAD.encode(bytes))
    }

    pub fn create_group(&mut self, group_id: &str) -> Result<MlsEpochSummary, NoiseError> {
        self.validate()?;
        let provider = self.take_provider()?;
        let signer = self.signer(&provider)?;
        let config = MlsGroupCreateConfig::builder()
            .ciphersuite(CIPHERSUITE)
            .use_ratchet_tree_extension(true)
            .max_past_epochs(0)
            .build();
        let group = MlsGroup::new_with_group_id(
            &provider,
            &signer,
            &config,
            GroupId::from_slice(group_id.as_bytes()),
            self.credential_with_key()?,
        )
        .map_err(|_| NoiseError::Mls)?;
        let summary = epoch_summary(&provider, &group, group_id)?;
        self.storage = MlsStorageSnapshot::from_provider(&provider)?;
        Ok(summary)
    }

    pub fn create_group_genesis(
        &mut self,
        identity: &Identity,
        group: &GroupMembership,
    ) -> Result<MlsGroupGenesis, NoiseError> {
        self.validate()?;
        if identity.public_key_base64() != self.account_public_key
            || group.owner_public_key != self.account_public_key
        {
            return Err(NoiseError::InvalidGroupAuthority);
        }
        let epoch = self.create_group(&group.group_id)?;
        let bridge = MlsLegacyHistoryBridge::create(group, &epoch)?;
        MlsGroupGenesis::create(identity, group, self.device_credential.clone(), bridge)
    }

    pub fn epoch(&self, group_id: &str) -> Result<MlsEpochSummary, NoiseError> {
        self.validate()?;
        let provider = self.storage.clone().into_provider()?;
        let group = load_group(&provider, group_id)?;
        epoch_summary(&provider, &group, group_id)
    }

    pub fn add_member(
        &mut self,
        group_id: &str,
        key_package_base64: &str,
    ) -> Result<MlsCommitBundle, NoiseError> {
        self.add_members(group_id, &[key_package_base64.to_owned()])
    }

    pub fn add_members(
        &mut self,
        group_id: &str,
        key_packages_base64: &[String],
    ) -> Result<MlsCommitBundle, NoiseError> {
        self.validate()?;
        if key_packages_base64.is_empty() {
            return Err(NoiseError::InvalidMlsState);
        }
        let provider = self.take_provider()?;
        let signer = self.signer(&provider)?;
        let mut group = load_group(&provider, group_id)?;
        let parent = epoch_summary(&provider, &group, group_id)?;
        let key_packages = key_packages_base64
            .iter()
            .map(|key_package_base64| {
                let key_package_bytes = STANDARD_NO_PAD
                    .decode(key_package_base64)
                    .map_err(|_| NoiseError::InvalidMlsState)?;
                KeyPackageIn::tls_deserialize_exact(&key_package_bytes)
                    .map_err(|_| NoiseError::InvalidMlsState)?
                    .validate(provider.crypto(), ProtocolVersion::Mls10)
                    .map_err(|_| NoiseError::InvalidMlsState)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let (commit, welcome, _) = group
            .add_members(&provider, &signer, &key_packages)
            .map_err(|_| NoiseError::Mls)?;
        let commit_base64 = STANDARD_NO_PAD.encode(
            commit
                .tls_serialize_detached()
                .map_err(|_| NoiseError::Mls)?,
        );
        let welcome_base64 = Some(
            STANDARD_NO_PAD.encode(
                welcome
                    .tls_serialize_detached()
                    .map_err(|_| NoiseError::Mls)?,
            ),
        );
        group
            .merge_pending_commit(&provider)
            .map_err(|_| NoiseError::Mls)?;
        let current = epoch_summary(&provider, &group, group_id)?;
        let history_link = HistoryKeyLink::create(
            group_id,
            current.epoch,
            &current.archive_key_base64,
            parent.epoch,
            &parent.archive_key_base64,
        )?;
        self.storage = MlsStorageSnapshot::from_provider(&provider)?;
        Ok(MlsCommitBundle {
            group_id: group_id.to_owned(),
            parent_epoch: parent.epoch,
            epoch: current.epoch,
            commit_base64,
            welcome_base64,
            history_link,
        })
    }

    pub fn join_group(
        &mut self,
        expected_group_id: &str,
        welcome_base64: &str,
    ) -> Result<MlsEpochSummary, NoiseError> {
        self.validate()?;
        let provider = self.take_provider()?;
        let welcome_bytes = STANDARD_NO_PAD
            .decode(welcome_base64)
            .map_err(|_| NoiseError::InvalidMlsState)?;
        let message = MlsMessageIn::tls_deserialize_exact(&welcome_bytes)
            .map_err(|_| NoiseError::InvalidMlsState)?;
        let welcome = match message.extract() {
            MlsMessageBodyIn::Welcome(welcome) => welcome,
            _ => return Err(NoiseError::InvalidMlsState),
        };
        let join_config = MlsGroupJoinConfig::builder()
            .use_ratchet_tree_extension(true)
            .max_past_epochs(0)
            .build();
        let staged = StagedWelcome::new_from_welcome(&provider, &join_config, welcome, None)
            .map_err(|_| NoiseError::Mls)?;
        let group = staged.into_group(&provider).map_err(|_| NoiseError::Mls)?;
        if group.group_id().as_slice() != expected_group_id.as_bytes() {
            return Err(NoiseError::InvalidMlsState);
        }
        let summary = epoch_summary(&provider, &group, expected_group_id)?;
        self.storage = MlsStorageSnapshot::from_provider(&provider)?;
        Ok(summary)
    }

    pub fn remove_member(
        &mut self,
        group_id: &str,
        member_account_public_key: &str,
    ) -> Result<MlsCommitBundle, NoiseError> {
        self.remove_members(group_id, &[member_account_public_key.to_owned()])
    }

    pub fn remove_members(
        &mut self,
        group_id: &str,
        member_account_public_keys: &[String],
    ) -> Result<MlsCommitBundle, NoiseError> {
        self.validate()?;
        if member_account_public_keys.is_empty() {
            return Err(NoiseError::InvalidMlsState);
        }
        let provider = self.take_provider()?;
        let signer = self.signer(&provider)?;
        let mut group = load_group(&provider, group_id)?;
        let parent = epoch_summary(&provider, &group, group_id)?;
        let unique_accounts = member_account_public_keys
            .iter()
            .map(String::as_str)
            .collect::<HashSet<_>>();
        let targets = unique_accounts
            .into_iter()
            .flat_map(|account| member_leaves(&group, account))
            .collect::<Vec<_>>();
        if targets.is_empty() {
            return Err(NoiseError::InvalidMlsState);
        }
        let (commit, welcome, _) = group
            .remove_members(&provider, &signer, &targets)
            .map_err(|_| NoiseError::Mls)?;
        let commit_base64 = STANDARD_NO_PAD.encode(
            commit
                .tls_serialize_detached()
                .map_err(|_| NoiseError::Mls)?,
        );
        let welcome_base64 = welcome
            .map(|message| {
                message
                    .tls_serialize_detached()
                    .map(|bytes| STANDARD_NO_PAD.encode(bytes))
                    .map_err(|_| NoiseError::Mls)
            })
            .transpose()?;
        group
            .merge_pending_commit(&provider)
            .map_err(|_| NoiseError::Mls)?;
        let current = epoch_summary(&provider, &group, group_id)?;
        let history_link = HistoryKeyLink::create(
            group_id,
            current.epoch,
            &current.archive_key_base64,
            parent.epoch,
            &parent.archive_key_base64,
        )?;
        self.storage = MlsStorageSnapshot::from_provider(&provider)?;
        Ok(MlsCommitBundle {
            group_id: group_id.to_owned(),
            parent_epoch: parent.epoch,
            epoch: current.epoch,
            commit_base64,
            welcome_base64,
            history_link,
        })
    }

    pub fn process_commit(
        &mut self,
        bundle: &MlsCommitBundle,
    ) -> Result<MlsEpochSummary, NoiseError> {
        self.validate()?;
        if bundle.epoch != bundle.parent_epoch.saturating_add(1)
            || bundle.history_link.group_id != bundle.group_id
            || bundle.history_link.epoch != bundle.epoch
            || bundle.history_link.previous_epoch != bundle.parent_epoch
        {
            return Err(NoiseError::InvalidMlsState);
        }
        let provider = self.take_provider()?;
        let mut group = load_group(&provider, &bundle.group_id)?;
        let parent = epoch_summary(&provider, &group, &bundle.group_id)?;
        if parent.epoch != bundle.parent_epoch {
            return Err(NoiseError::InvalidMlsState);
        }
        let commit_bytes = STANDARD_NO_PAD
            .decode(&bundle.commit_base64)
            .map_err(|_| NoiseError::InvalidMlsState)?;
        let message = MlsMessageIn::tls_deserialize_exact(&commit_bytes)
            .map_err(|_| NoiseError::InvalidMlsState)?;
        let protocol: ProtocolMessage = message
            .try_into_protocol_message()
            .map_err(|_| NoiseError::InvalidMlsState)?;
        let processed = group
            .process_message(&provider, protocol)
            .map_err(|_| NoiseError::Mls)?;
        let staged = match processed.into_content() {
            ProcessedMessageContent::StagedCommitMessage(staged) => staged,
            _ => return Err(NoiseError::InvalidMlsState),
        };
        group
            .merge_staged_commit(&provider, *staged)
            .map_err(|_| NoiseError::Mls)?;
        let current = epoch_summary(&provider, &group, &bundle.group_id)?;
        if current.epoch != bundle.epoch {
            return Err(NoiseError::InvalidMlsState);
        }
        let linked_previous = bundle.history_link.open(&current.archive_key_base64)?;
        if linked_previous != parent.archive_key_base64 {
            return Err(NoiseError::InvalidMlsState);
        }
        self.storage = MlsStorageSnapshot::from_provider(&provider)?;
        Ok(current)
    }

    pub fn members(&self, group_id: &str) -> Result<Vec<String>, NoiseError> {
        self.validate()?;
        let provider = self.storage.clone().into_provider()?;
        let group = load_group(&provider, group_id)?;
        group
            .members()
            .map(|member| account_identity(&member))
            .collect()
    }

    pub fn member_devices(&self, group_id: &str) -> Result<Vec<MlsDeviceCredential>, NoiseError> {
        self.validate()?;
        let provider = self.storage.clone().into_provider()?;
        let group = load_group(&provider, group_id)?;
        group
            .members()
            .map(|member| device_credential(&member))
            .collect()
    }

    pub fn forget_group(&mut self, group_id: &str) -> Result<(), NoiseError> {
        self.validate()?;
        let provider = self.take_provider()?;
        if let Some(mut group) = MlsGroup::load(
            provider.storage(),
            &GroupId::from_slice(group_id.as_bytes()),
        )
        .map_err(|_| NoiseError::Mls)?
        {
            group
                .delete(provider.storage())
                .map_err(|_| NoiseError::Mls)?;
        }
        self.storage = MlsStorageSnapshot::from_provider(&provider)?;
        Ok(())
    }

    pub fn create_epoch_record(
        &self,
        identity: &Identity,
        previous_record_id: impl Into<String>,
        bundle: MlsCommitBundle,
    ) -> Result<MlsEpochRecord, NoiseError> {
        let current = self.epoch(&bundle.group_id)?;
        if current.epoch != bundle.epoch {
            return Err(NoiseError::InvalidMlsState);
        }
        MlsEpochRecord::create(
            identity,
            previous_record_id,
            bundle.clone(),
            self.members(&bundle.group_id)?,
        )
    }

    fn validate(&self) -> Result<(), NoiseError> {
        if self.version != MLS_STATE_VERSION
            || self.account_public_key.is_empty()
            || decode_array::<32>(&self.signer_public_key_base64, "MLS signing key").is_err()
            || self.device_credential.account_public_key != self.account_public_key
            || self.device_credential.mls_signature_public_key_base64
                != self.signer_public_key_base64
        {
            return Err(NoiseError::InvalidMlsState);
        }
        decode_array::<32>(&self.account_public_key, "identity public key")?;
        self.device_credential.validate()?;
        Ok(())
    }

    fn take_provider(&self) -> Result<NoiseMlsProvider, NoiseError> {
        self.storage.clone().into_provider()
    }

    fn signer(&self, provider: &NoiseMlsProvider) -> Result<SignatureKeyPair, NoiseError> {
        let public = STANDARD_NO_PAD
            .decode(&self.signer_public_key_base64)
            .map_err(|_| NoiseError::InvalidMlsState)?;
        SignatureKeyPair::read(
            provider.storage(),
            &public,
            CIPHERSUITE.signature_algorithm(),
        )
        .ok_or(NoiseError::InvalidMlsState)
    }

    fn credential_with_key(&self) -> Result<CredentialWithKey, NoiseError> {
        let public = STANDARD_NO_PAD
            .decode(&self.signer_public_key_base64)
            .map_err(|_| NoiseError::InvalidMlsState)?;
        Ok(CredentialWithKey {
            credential: BasicCredential::new(serde_json::to_vec(&self.device_credential)?).into(),
            signature_key: public.into(),
        })
    }
}

fn history_link_aad(group_id: &str, epoch: u64, previous_epoch: u64) -> Vec<u8> {
    format!("{ARCHIVE_LINK_CONTEXT}:{group_id}:{epoch}:{previous_epoch}").into_bytes()
}

fn legacy_bridge_aad(group_id: &str) -> Vec<u8> {
    format!("{LEGACY_BRIDGE_CONTEXT}:{group_id}:0").into_bytes()
}

fn join_request_signing_bytes(request_id: &str, unsigned: &[u8]) -> Vec<u8> {
    let mut bytes = format!("{JOIN_REQUEST_CONTEXT}:{request_id}:").into_bytes();
    bytes.extend_from_slice(unsigned);
    bytes
}

fn removal_request_signing_bytes(request_id: &str, unsigned: &[u8]) -> Vec<u8> {
    let mut bytes = format!("{REMOVAL_REQUEST_CONTEXT}:{request_id}:").into_bytes();
    bytes.extend_from_slice(unsigned);
    bytes
}

fn control_signing_bytes(context: &str, record_id: &str, unsigned: &[u8]) -> Vec<u8> {
    let mut bytes = format!("{context}:{record_id}:").into_bytes();
    bytes.extend_from_slice(unsigned);
    bytes
}

fn valid_group_id(value: &str) -> bool {
    valid_record_id(value)
}

fn valid_record_id(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn load_group(provider: &NoiseMlsProvider, group_id: &str) -> Result<MlsGroup, NoiseError> {
    MlsGroup::load(
        provider.storage(),
        &GroupId::from_slice(group_id.as_bytes()),
    )
    .map_err(|_| NoiseError::Mls)?
    .ok_or(NoiseError::InvalidMlsState)
}

fn epoch_summary(
    provider: &NoiseMlsProvider,
    group: &MlsGroup,
    expected_group_id: &str,
) -> Result<MlsEpochSummary, NoiseError> {
    if group.group_id().as_slice() != expected_group_id.as_bytes() || !group.is_active() {
        return Err(NoiseError::InvalidMlsState);
    }
    let archive_key = group
        .export_secret(
            provider.crypto(),
            ARCHIVE_EXPORT_LABEL,
            expected_group_id.as_bytes(),
            32,
        )
        .map_err(|_| NoiseError::Mls)?;
    Ok(MlsEpochSummary {
        group_id: expected_group_id.to_owned(),
        epoch: group.epoch().as_u64(),
        archive_key_base64: STANDARD_NO_PAD.encode(archive_key),
    })
}

fn device_credential(member: &Member) -> Result<MlsDeviceCredential, NoiseError> {
    let basic = BasicCredential::try_from(member.credential.clone())
        .map_err(|_| NoiseError::InvalidMlsState)?;
    let credential: MlsDeviceCredential =
        serde_json::from_slice(basic.identity()).map_err(|_| NoiseError::InvalidMlsState)?;
    credential.validate()?;
    if STANDARD_NO_PAD.encode(&member.signature_key) != credential.mls_signature_public_key_base64 {
        return Err(NoiseError::InvalidMlsState);
    }
    Ok(credential)
}

fn account_identity(member: &Member) -> Result<String, NoiseError> {
    Ok(device_credential(member)?.account_public_key)
}

fn member_leaves(group: &MlsGroup, account_public_key: &str) -> Vec<LeafNodeIndex> {
    group
        .members()
        .filter_map(|member| {
            (account_identity(&member).ok().as_deref() == Some(account_public_key))
                .then_some(member.index)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Identity, Profile, SignedEvent};

    #[test]
    fn mls_epochs_preserve_history_for_new_members_and_cut_off_removed_members() {
        let alice_identity = Identity::generate();
        let bob_identity = Identity::generate();
        let charlie_identity = Identity::generate();
        let group = GroupMembership::create_owned(
            "production noise group",
            alice_identity.public_key_base64(),
        );
        let group_id = group.group_id.as_str();
        let mut alice = MlsAccountState::create(&alice_identity).unwrap();
        let mut bob = MlsAccountState::create(&bob_identity).unwrap();
        let mut charlie = MlsAccountState::create(&charlie_identity).unwrap();

        let bob_profile = Profile {
            username: "bob".into(),
            bio: String::new(),
            avatar: None,
            album: None,
            accepts_direct_messages: true,
        };
        let membership_proof =
            SignedEvent::member_joined(&bob_identity, &group, &bob_profile, 1).unwrap();
        let bob_request = MlsJoinRequest::create_with_membership_proof(
            &bob_identity,
            &mut bob,
            group_id,
            membership_proof,
        )
        .unwrap();
        bob_request.verify().unwrap();
        let charlie_request =
            MlsJoinRequest::create(&charlie_identity, &mut charlie, group_id).unwrap();
        let genesis = alice.create_group_genesis(&alice_identity, &group).unwrap();
        let epoch_zero = alice.epoch(group_id).unwrap();
        assert_eq!(
            genesis
                .legacy_history_bridge
                .open(&epoch_zero.archive_key_base64)
                .unwrap(),
            group.secret_base64
        );

        let add_bob = alice
            .add_member(group_id, &bob_request.key_package_base64)
            .unwrap();
        let add_bob_record = alice
            .create_epoch_record(&alice_identity, &genesis.record_id, add_bob.clone())
            .unwrap();
        let bob_epoch = bob
            .join_group(group_id, add_bob.welcome_base64.as_deref().unwrap())
            .unwrap();
        assert_eq!(
            bob_epoch.archive_key_base64,
            alice.epoch(group_id).unwrap().archive_key_base64
        );

        let add_charlie = alice
            .add_member(group_id, &charlie_request.key_package_base64)
            .unwrap();
        let add_charlie_record = alice
            .create_epoch_record(
                &alice_identity,
                &add_bob_record.record_id,
                add_charlie.clone(),
            )
            .unwrap();
        bob.process_commit(&add_charlie).unwrap();
        let charlie_epoch = charlie
            .join_group(group_id, add_charlie.welcome_base64.as_deref().unwrap())
            .unwrap();
        let history = HistoryKeyLink::unlock_history(
            group_id,
            charlie_epoch.epoch,
            &charlie_epoch.archive_key_base64,
            &[
                add_bob.history_link.clone(),
                add_charlie.history_link.clone(),
            ],
        )
        .unwrap();
        assert_eq!(
            history.get(&epoch_zero.epoch),
            Some(&epoch_zero.archive_key_base64)
        );

        let remove_bob = alice
            .remove_member(group_id, &bob_identity.public_key_base64())
            .unwrap();
        let remove_bob_record = alice
            .create_epoch_record(
                &alice_identity,
                &add_charlie_record.record_id,
                remove_bob.clone(),
            )
            .unwrap();
        let mut charlie: MlsAccountState =
            serde_json::from_slice(&serde_json::to_vec(&charlie).unwrap()).unwrap();
        MlsControlLog {
            genesis,
            epochs: vec![add_bob_record, add_charlie_record, remove_bob_record],
        }
        .verify()
        .unwrap();
        charlie.process_commit(&remove_bob).unwrap();
        assert!(bob.process_commit(&remove_bob).is_err());
        let alice_after = alice.epoch(group_id).unwrap();
        let charlie_after = charlie.epoch(group_id).unwrap();
        assert_eq!(
            alice_after.archive_key_base64,
            charlie_after.archive_key_base64
        );
        assert_ne!(bob_epoch.archive_key_base64, alice_after.archive_key_base64);
    }
}
