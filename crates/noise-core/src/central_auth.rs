use base64::{Engine as _, engine::general_purpose::STANDARD_NO_PAD};
use ed25519_dalek::{Signer, SigningKey};
use rand::random;
use serde::{Deserialize, Serialize};

use super::{Identity, NoiseError, decode_array, verify_signature};

const INSTALLATION_REGISTRATION_CONTEXT: &str = "noise.central.device-registration.v1";
const SESSION_PROOF_CONTEXT: &str = "noise.central.session-proof.v1";
const INSTALLATION_REVOCATION_CONTEXT: &str = "noise.central.device-revocation.v1";
const CENTRAL_AUTH_VERSION: u32 = 1;

/// A transport-authentication key owned by one noise installation.
///
/// This is not the account identity key, the synchronized UI `DeviceRecord`
/// identifier, or an MLS credential. The private key stays in platform-secure
/// local storage and silently proves possession when this same installation
/// opens or renews a central-service session.
#[derive(Clone)]
pub struct CentralInstallationAuthKey {
    signing_key: SigningKey,
}

impl CentralInstallationAuthKey {
    #[must_use]
    pub fn generate() -> Self {
        Self {
            signing_key: SigningKey::from_bytes(&random()),
        }
    }

    pub fn from_secret_base64(encoded: &str) -> Result<Self, NoiseError> {
        Ok(Self {
            signing_key: SigningKey::from_bytes(&decode_canonical_array(
                encoded,
                "central installation authentication secret",
            )?),
        })
    }

    #[must_use]
    pub fn secret_base64(&self) -> String {
        STANDARD_NO_PAD.encode(self.signing_key.to_bytes())
    }

    #[must_use]
    pub fn public_key_base64(&self) -> String {
        STANDARD_NO_PAD.encode(self.signing_key.verifying_key().to_bytes())
    }

    pub fn session_proof(
        &self,
        account_public_key: impl Into<String>,
        installation_id_base64: impl Into<String>,
        challenge_id_base64: impl Into<String>,
        challenge_nonce_base64: impl Into<String>,
        issued_at_millis: u64,
    ) -> Result<CentralSessionProofV1, NoiseError> {
        let mut proof = CentralSessionProofV1 {
            version: CENTRAL_AUTH_VERSION,
            account_public_key: account_public_key.into(),
            installation_id_base64: installation_id_base64.into(),
            installation_auth_public_key_base64: self.public_key_base64(),
            challenge_id_base64: challenge_id_base64.into(),
            challenge_nonce_base64: challenge_nonce_base64.into(),
            issued_at_millis,
            signature_base64: String::new(),
        };
        proof.signature_base64 =
            STANDARD_NO_PAD.encode(self.signing_key.sign(&proof.signing_bytes()?).to_bytes());
        proof.verify()?;
        Ok(proof)
    }
}

/// An account-authorized binding between one installation ID and its local
/// central-service authentication public key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CentralInstallationRegistrationV1 {
    pub version: u32,
    pub account_public_key: String,
    pub installation_id_base64: String,
    pub installation_auth_public_key_base64: String,
    pub challenge_id_base64: String,
    pub challenge_nonce_base64: String,
    pub issued_at_millis: u64,
    pub registration_version: u64,
    pub signature_base64: String,
}

impl CentralInstallationRegistrationV1 {
    pub fn verify(&self) -> Result<(), NoiseError> {
        if self.version != CENTRAL_AUTH_VERSION
            || self.issued_at_millis == 0
            || self.registration_version == 0
        {
            return Err(NoiseError::Crypto);
        }
        let account_public_key =
            decode_canonical_array::<32>(&self.account_public_key, "identity public key")?;
        ed25519_dalek::VerifyingKey::from_bytes(&account_public_key)
            .map_err(|_| NoiseError::InvalidSignature)?;
        decode_canonical_array::<32>(&self.installation_id_base64, "installation ID")?;
        let installation_auth_public_key = decode_canonical_array::<32>(
            &self.installation_auth_public_key_base64,
            "central installation authentication public key",
        )?;
        ed25519_dalek::VerifyingKey::from_bytes(&installation_auth_public_key)
            .map_err(|_| NoiseError::InvalidSignature)?;
        decode_canonical_array::<32>(
            &self.challenge_id_base64,
            "central registration challenge ID",
        )?;
        decode_canonical_array::<32>(
            &self.challenge_nonce_base64,
            "central registration challenge nonce",
        )?;
        decode_canonical_array::<64>(
            &self.signature_base64,
            "central installation registration signature",
        )?;
        verify_signature(
            &self.account_public_key,
            &self.signature_base64,
            &self.signing_bytes()?,
        )
    }

    fn signing_bytes(&self) -> Result<Vec<u8>, NoiseError> {
        let mut encoder = SigningEncoder::new(INSTALLATION_REGISTRATION_CONTEXT, self.version);
        encoder.field(&decode_canonical_array::<32>(
            &self.account_public_key,
            "identity public key",
        )?);
        encoder.field(&decode_canonical_array::<32>(
            &self.installation_id_base64,
            "installation ID",
        )?);
        encoder.field(&decode_canonical_array::<32>(
            &self.installation_auth_public_key_base64,
            "central installation authentication public key",
        )?);
        encoder.field(&decode_canonical_array::<32>(
            &self.challenge_id_base64,
            "central registration challenge ID",
        )?);
        encoder.field(&decode_canonical_array::<32>(
            &self.challenge_nonce_base64,
            "central registration challenge nonce",
        )?);
        encoder.u64(self.issued_at_millis);
        encoder.u64(self.registration_version);
        Ok(encoder.finish())
    }
}

/// A one-time challenge proof signed silently by the same installation that is
/// opening or renewing its central-service session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CentralSessionProofV1 {
    pub version: u32,
    pub account_public_key: String,
    pub installation_id_base64: String,
    pub installation_auth_public_key_base64: String,
    pub challenge_id_base64: String,
    pub challenge_nonce_base64: String,
    pub issued_at_millis: u64,
    pub signature_base64: String,
}

impl CentralSessionProofV1 {
    pub fn verify(&self) -> Result<(), NoiseError> {
        if self.version != CENTRAL_AUTH_VERSION || self.issued_at_millis == 0 {
            return Err(NoiseError::Crypto);
        }
        let account_public_key =
            decode_canonical_array::<32>(&self.account_public_key, "identity public key")?;
        ed25519_dalek::VerifyingKey::from_bytes(&account_public_key)
            .map_err(|_| NoiseError::InvalidSignature)?;
        decode_canonical_array::<32>(&self.installation_id_base64, "installation ID")?;
        let installation_auth_public_key = decode_canonical_array::<32>(
            &self.installation_auth_public_key_base64,
            "central installation authentication public key",
        )?;
        ed25519_dalek::VerifyingKey::from_bytes(&installation_auth_public_key)
            .map_err(|_| NoiseError::InvalidSignature)?;
        decode_canonical_array::<32>(&self.challenge_id_base64, "central session challenge ID")?;
        decode_canonical_array::<32>(
            &self.challenge_nonce_base64,
            "central session challenge nonce",
        )?;
        decode_canonical_array::<64>(&self.signature_base64, "central session proof signature")?;
        verify_signature(
            &self.installation_auth_public_key_base64,
            &self.signature_base64,
            &self.signing_bytes()?,
        )
    }

    fn signing_bytes(&self) -> Result<Vec<u8>, NoiseError> {
        let mut encoder = SigningEncoder::new(SESSION_PROOF_CONTEXT, self.version);
        encoder.field(&decode_canonical_array::<32>(
            &self.account_public_key,
            "identity public key",
        )?);
        encoder.field(&decode_canonical_array::<32>(
            &self.installation_id_base64,
            "installation ID",
        )?);
        encoder.field(&decode_canonical_array::<32>(
            &self.installation_auth_public_key_base64,
            "central installation authentication public key",
        )?);
        encoder.field(&decode_canonical_array::<32>(
            &self.challenge_id_base64,
            "central session challenge ID",
        )?);
        encoder.field(&decode_canonical_array::<32>(
            &self.challenge_nonce_base64,
            "central session challenge nonce",
        )?);
        encoder.u64(self.issued_at_millis);
        Ok(encoder.finish())
    }
}

/// An account-signed instruction that revokes one installation binding.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CentralInstallationRevocationV1 {
    pub version: u32,
    pub account_public_key: String,
    pub installation_id_base64: String,
    pub installation_auth_public_key_base64: String,
    pub revocation_sequence: u64,
    pub issued_at_millis: u64,
    pub signature_base64: String,
}

impl CentralInstallationRevocationV1 {
    pub fn verify(&self) -> Result<(), NoiseError> {
        if self.version != CENTRAL_AUTH_VERSION
            || self.revocation_sequence == 0
            || self.issued_at_millis == 0
        {
            return Err(NoiseError::Crypto);
        }
        let account_public_key =
            decode_canonical_array::<32>(&self.account_public_key, "identity public key")?;
        ed25519_dalek::VerifyingKey::from_bytes(&account_public_key)
            .map_err(|_| NoiseError::InvalidSignature)?;
        decode_canonical_array::<32>(&self.installation_id_base64, "installation ID")?;
        let installation_auth_public_key = decode_canonical_array::<32>(
            &self.installation_auth_public_key_base64,
            "central installation authentication public key",
        )?;
        ed25519_dalek::VerifyingKey::from_bytes(&installation_auth_public_key)
            .map_err(|_| NoiseError::InvalidSignature)?;
        decode_canonical_array::<64>(
            &self.signature_base64,
            "central installation revocation signature",
        )?;
        verify_signature(
            &self.account_public_key,
            &self.signature_base64,
            &self.signing_bytes()?,
        )
    }

    fn signing_bytes(&self) -> Result<Vec<u8>, NoiseError> {
        let mut encoder = SigningEncoder::new(INSTALLATION_REVOCATION_CONTEXT, self.version);
        encoder.field(&decode_canonical_array::<32>(
            &self.account_public_key,
            "identity public key",
        )?);
        encoder.field(&decode_canonical_array::<32>(
            &self.installation_id_base64,
            "installation ID",
        )?);
        encoder.field(&decode_canonical_array::<32>(
            &self.installation_auth_public_key_base64,
            "central installation authentication public key",
        )?);
        encoder.u64(self.revocation_sequence);
        encoder.u64(self.issued_at_millis);
        Ok(encoder.finish())
    }
}

impl Identity {
    pub fn central_installation_registration(
        &self,
        installation_id_base64: impl Into<String>,
        installation_auth_public_key_base64: impl Into<String>,
        challenge_id_base64: impl Into<String>,
        challenge_nonce_base64: impl Into<String>,
        issued_at_millis: u64,
        registration_version: u64,
    ) -> Result<CentralInstallationRegistrationV1, NoiseError> {
        let mut registration = CentralInstallationRegistrationV1 {
            version: CENTRAL_AUTH_VERSION,
            account_public_key: self.public_key_base64(),
            installation_id_base64: installation_id_base64.into(),
            installation_auth_public_key_base64: installation_auth_public_key_base64.into(),
            challenge_id_base64: challenge_id_base64.into(),
            challenge_nonce_base64: challenge_nonce_base64.into(),
            issued_at_millis,
            registration_version,
            signature_base64: String::new(),
        };
        registration.signature_base64 = self.sign(&registration.signing_bytes()?);
        registration.verify()?;
        Ok(registration)
    }

    pub fn central_installation_revocation(
        &self,
        installation_id_base64: impl Into<String>,
        installation_auth_public_key_base64: impl Into<String>,
        revocation_sequence: u64,
        issued_at_millis: u64,
    ) -> Result<CentralInstallationRevocationV1, NoiseError> {
        let mut revocation = CentralInstallationRevocationV1 {
            version: CENTRAL_AUTH_VERSION,
            account_public_key: self.public_key_base64(),
            installation_id_base64: installation_id_base64.into(),
            installation_auth_public_key_base64: installation_auth_public_key_base64.into(),
            revocation_sequence,
            issued_at_millis,
            signature_base64: String::new(),
        };
        revocation.signature_base64 = self.sign(&revocation.signing_bytes()?);
        revocation.verify()?;
        Ok(revocation)
    }
}

struct SigningEncoder {
    bytes: Vec<u8>,
}

impl SigningEncoder {
    fn new(context: &str, version: u32) -> Self {
        let mut encoder = Self { bytes: Vec::new() };
        encoder.field(context.as_bytes());
        encoder.bytes.extend_from_slice(&version.to_be_bytes());
        encoder
    }

    fn field(&mut self, value: &[u8]) {
        self.bytes
            .extend_from_slice(&(value.len() as u32).to_be_bytes());
        self.bytes.extend_from_slice(value);
    }

    fn u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

fn decode_canonical_array<const N: usize>(
    encoded: &str,
    label: &'static str,
) -> Result<[u8; N], NoiseError> {
    let bytes = decode_array::<N>(encoded, label)?;
    if STANDARD_NO_PAD.encode(bytes) != encoded {
        return Err(NoiseError::InvalidEncoding(label));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encoded(byte: u8) -> String {
        STANDARD_NO_PAD.encode([byte; 32])
    }

    #[test]
    fn central_auth_fixed_vectors_and_round_trip() {
        let identity = Identity::from_secret_base64(&encoded(0x11)).unwrap();
        let installation_key =
            CentralInstallationAuthKey::from_secret_base64(&encoded(0x22)).unwrap();
        let installation_id = encoded(0x33);
        let challenge_id = encoded(0x44);
        let challenge_nonce = encoded(0x55);

        let registration = identity
            .central_installation_registration(
                &installation_id,
                installation_key.public_key_base64(),
                &challenge_id,
                &challenge_nonce,
                1_722_000_000_123,
                7,
            )
            .unwrap();
        registration.verify().unwrap();

        let session = installation_key
            .session_proof(
                identity.public_key_base64(),
                &installation_id,
                &challenge_id,
                &challenge_nonce,
                1_722_000_000_456,
            )
            .unwrap();
        session.verify().unwrap();

        let revocation = identity
            .central_installation_revocation(
                &installation_id,
                installation_key.public_key_base64(),
                8,
                1_722_000_000_789,
            )
            .unwrap();
        revocation.verify().unwrap();

        assert_eq!(
            blake3::hash(&registration.signing_bytes().unwrap())
                .to_hex()
                .as_str(),
            "4f06d990b71faa511e4abc4d062a10a323148f685dab923ee943aeefde7d08b1"
        );
        assert_eq!(
            registration.signature_base64,
            "xspjZYyfF4MM6vjXQA33cSL8Amb+piGSX+PTfd26fURgtR20VzVxWSR0+JR+SBxczcAZOfUUvGZqbFvf2It+AA"
        );
        assert_eq!(
            blake3::hash(&session.signing_bytes().unwrap())
                .to_hex()
                .as_str(),
            "15ad48701d159c6e3007ab18f2dabb811fa3eb2484d5ad238bf25cbb1595ba09"
        );
        assert_eq!(
            session.signature_base64,
            "9O0HYuRX5O9fzD1lAAXu96Aa0dHNQ59tr8Lv1iRdisIJ6LfnrHDY9P16h4xxxpbnspT6vU4B9pwmc5+hizFgCw"
        );
        assert_eq!(
            blake3::hash(&revocation.signing_bytes().unwrap())
                .to_hex()
                .as_str(),
            "f9587a59f9c520838912c6604494ad28d83ac87ece27a14bc2759de1c8ce774a"
        );
        assert_eq!(
            revocation.signature_base64,
            "FICi1Ej84OpSd/IjQcwM8qIZRzKqTF2srXUTD55shQFyoHErzIBxSwbIA5ntr1i8N9SoiLx5X7e+GXt0dmCUAw"
        );
    }

    #[test]
    fn central_auth_rejects_tampering_and_key_substitution() {
        let identity = Identity::generate();
        let installation_key = CentralInstallationAuthKey::generate();
        let other_installation_key = CentralInstallationAuthKey::generate();
        let installation_id = encoded(0x66);
        let challenge_id = encoded(0x77);
        let challenge_nonce = encoded(0x88);

        let mut registration = identity
            .central_installation_registration(
                &installation_id,
                installation_key.public_key_base64(),
                &challenge_id,
                &challenge_nonce,
                1,
                1,
            )
            .unwrap();
        registration.installation_id_base64 = encoded(0x67);
        assert!(registration.verify().is_err());

        let mut proof = installation_key
            .session_proof(
                identity.public_key_base64(),
                &installation_id,
                &challenge_id,
                &challenge_nonce,
                1,
            )
            .unwrap();
        proof.installation_auth_public_key_base64 = other_installation_key.public_key_base64();
        assert!(proof.verify().is_err());

        let mut revocation = identity
            .central_installation_revocation(
                &installation_id,
                installation_key.public_key_base64(),
                1,
                1,
            )
            .unwrap();
        revocation.revocation_sequence = 2;
        assert!(revocation.verify().is_err());
    }

    #[test]
    fn central_auth_requires_canonical_base64_and_nonzero_sequences() {
        let identity = Identity::generate();
        let installation_key = CentralInstallationAuthKey::generate();

        assert!(
            identity
                .central_installation_registration(
                    format!("{}=", encoded(0x33)),
                    installation_key.public_key_base64(),
                    encoded(0x44),
                    encoded(0x55),
                    1,
                    1,
                )
                .is_err()
        );
        assert!(
            identity
                .central_installation_registration(
                    encoded(0x33),
                    installation_key.public_key_base64(),
                    encoded(0x44),
                    encoded(0x55),
                    1,
                    0,
                )
                .is_err()
        );
        assert!(
            identity
                .central_installation_revocation(
                    encoded(0x33),
                    installation_key.public_key_base64(),
                    0,
                    1,
                )
                .is_err()
        );
    }
}
