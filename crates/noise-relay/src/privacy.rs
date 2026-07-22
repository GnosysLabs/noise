use std::{
    fs::{self, OpenOptions},
    io::{ErrorKind, Write},
    path::Path,
    sync::Arc,
};

use anyhow::{Context, bail};
use noise_transport::{PlainRequest, decode_request};
use ohttp::hpke::{Aead, Kdf, Kem};
use ohttp::{KeyConfig, Server, ServerResponse, SymmetricSuite};

const KEY_ID: u8 = 1;
const KEY_BYTES: usize = 32;

#[derive(Clone)]
pub struct PrivacyGateway {
    server: Arc<Server>,
    public_config: Arc<Vec<u8>>,
    public_config_list: Arc<Vec<u8>>,
}

impl PrivacyGateway {
    pub fn open(data_directory: &Path) -> anyhow::Result<Self> {
        ohttp::init();
        let key_path = data_directory.join("ohttp.key");
        let key = load_or_create_key(&key_path)?;
        let config = KeyConfig::derive(
            KEY_ID,
            Kem::X25519Sha256,
            vec![SymmetricSuite::new(Kdf::HkdfSha256, Aead::ChaCha20Poly1305)],
            &key,
        )
        .context("could not derive the relay OHTTP key")?;
        let public_config = config
            .encode()
            .context("could not encode the relay OHTTP key")?;
        let public_config_list = KeyConfig::encode_list(&[&config])
            .context("could not encode the relay OHTTP key list")?;
        let server = Server::new(config).context("could not initialize the OHTTP gateway")?;
        Ok(Self {
            server: Arc::new(server),
            public_config: Arc::new(public_config),
            public_config_list: Arc::new(public_config_list),
        })
    }

    #[must_use]
    pub fn public_config(&self) -> &[u8] {
        &self.public_config
    }

    #[must_use]
    pub fn public_config_list(&self) -> &[u8] {
        &self.public_config_list
    }

    pub fn decapsulate(
        &self,
        encrypted_request: &[u8],
    ) -> anyhow::Result<(PlainRequest, ServerResponse)> {
        let (request, response) = self
            .server
            .decapsulate(encrypted_request)
            .context("could not open the OHTTP request")?;
        Ok((decode_request(&request)?, response))
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
        bail!("{} has an invalid OHTTP key length", path.display())
    }
    Ok(bytes.try_into().expect("key length was checked"))
}
