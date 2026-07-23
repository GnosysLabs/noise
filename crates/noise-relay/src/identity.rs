use std::{
    fs::{self, OpenOptions},
    io::{ErrorKind, Write},
    path::Path,
};

use anyhow::{Context, bail};
use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD_NO_PAD, URL_SAFE_NO_PAD},
};
use ed25519_dalek::{Signer, SigningKey};
use noise_transport::{RELAY_PROTOCOL_VERSION, SignedRelayDescriptor, relay_id_from_public_key};

const KEY_BYTES: usize = 32;
const DESCRIPTOR_REFRESH_SECONDS: u64 = 60 * 60;
const DESCRIPTOR_LIFETIME_SECONDS: u64 = 48 * 60 * 60;

#[derive(Clone)]
pub struct RelayIdentity {
    signing_key: SigningKey,
}

impl RelayIdentity {
    pub fn open(data_directory: &Path) -> anyhow::Result<Self> {
        let key_path = data_directory.join("relay-identity.key");
        let key = load_or_create_key(&key_path)?;
        Ok(Self {
            signing_key: SigningKey::from_bytes(&key),
        })
    }

    #[must_use]
    pub fn relay_id(&self) -> String {
        relay_id_from_public_key(&self.signing_key.verifying_key().to_bytes())
    }

    pub fn signed_descriptor(
        &self,
        base_url: &str,
        ohttp_config: &[u8],
        now: u64,
    ) -> anyhow::Result<SignedRelayDescriptor> {
        let issued_at_unix_seconds = now - (now % DESCRIPTOR_REFRESH_SECONDS);
        let expires_at_unix_seconds = issued_at_unix_seconds
            .checked_add(DESCRIPTOR_LIFETIME_SECONDS)
            .context("relay descriptor expiration overflowed")?;
        let mut descriptor = SignedRelayDescriptor {
            protocol_version: RELAY_PROTOCOL_VERSION,
            relay_id: self.relay_id(),
            public_key_base64: STANDARD_NO_PAD.encode(self.signing_key.verifying_key().to_bytes()),
            base_url: base_url.to_owned(),
            ohttp_config_base64: URL_SAFE_NO_PAD.encode(ohttp_config),
            issued_at_unix_seconds,
            expires_at_unix_seconds,
            signature_base64: String::new(),
        };
        descriptor.signature_base64 = STANDARD_NO_PAD.encode(
            self.signing_key
                .sign(&descriptor.signing_bytes()?)
                .to_bytes(),
        );
        descriptor.verify_at(now)?;
        Ok(descriptor)
    }
}

fn load_or_create_key(path: &Path) -> anyhow::Result<[u8; KEY_BYTES]> {
    match fs::read(path) {
        Ok(bytes) => decode_key(path, bytes),
        Err(error) if error.kind() == ErrorKind::NotFound => create_key(path),
        Err(error) => Err(error).with_context(|| format!("could not read {}", path.display())),
    }
}

fn create_key(path: &Path) -> anyhow::Result<[u8; KEY_BYTES]> {
    let key: [u8; KEY_BYTES] = rand::random();
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(mut file) => {
            file.write_all(&key)
                .with_context(|| format!("could not write {}", path.display()))?;
            file.sync_all()
                .with_context(|| format!("could not sync {}", path.display()))?;
            Ok(key)
        }
        Err(error) if error.kind() == ErrorKind::AlreadyExists => decode_key(path, fs::read(path)?),
        Err(error) => Err(error).with_context(|| format!("could not create {}", path.display())),
    }
}

fn decode_key(path: &Path, bytes: Vec<u8>) -> anyhow::Result<[u8; KEY_BYTES]> {
    if bytes.len() != KEY_BYTES {
        bail!(
            "{} has an invalid relay identity key length",
            path.display()
        )
    }
    Ok(bytes
        .try_into()
        .expect("relay identity key length was checked"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_persists_and_signed_descriptors_reject_tampering() {
        let directory = std::env::temp_dir().join(format!(
            "noise-relay-identity-test-{}",
            rand::random::<u64>()
        ));
        fs::create_dir_all(&directory).unwrap();

        let first = RelayIdentity::open(&directory).unwrap();
        let first_id = first.relay_id();
        let descriptor = first
            .signed_descriptor("https://relay.example", &[1, 2, 3, 4], 1_800_000_000)
            .unwrap();
        descriptor.verify_at(1_800_000_000).unwrap();

        let second = RelayIdentity::open(&directory).unwrap();
        assert_eq!(second.relay_id(), first_id);

        let mut tampered = descriptor;
        tampered.base_url = "https://attacker.example".into();
        assert!(tampered.verify_at(1_800_000_000).is_err());

        fs::remove_dir_all(directory).unwrap();
    }
}
