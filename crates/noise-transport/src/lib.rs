use std::io::Cursor;

use anyhow::{Context, bail};
use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD_NO_PAD, URL_SAFE_NO_PAD},
};
use bhttp::{Message, Mode, StatusCode};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use url::{Host, Url};

pub const OHTTP_KEYS_MEDIA_TYPE: &str = "application/ohttp-keys";
pub const OHTTP_REQUEST_MEDIA_TYPE: &str = "message/ohttp-req";
pub const OHTTP_RESPONSE_MEDIA_TYPE: &str = "message/ohttp-res";
pub const GATEWAY_HEADER: &str = "noise-gateway";
pub const OHTTP_GATEWAY_PATH: &str = "/v1/ohttp/gateway";
pub const OHTTP_KEYS_PATH: &str = "/v1/ohttp-keys";
pub const OHTTP_RELAY_PATH: &str = "/v1/ohttp/relay";
pub const RELAY_DIRECTORY_PATH: &str = "/v3/relays";
pub const SIGNED_RELAY_DESCRIPTOR_PATH: &str = "/v3/relay-descriptor";
pub const RELAY_PROTOCOL_VERSION: u16 = 4;

const PAD_BUCKETS: &[usize] = &[
    1024,
    4096,
    16 * 1024,
    64 * 1024,
    256 * 1024,
    512 * 1024,
    1024 * 1024,
    2_500_000,
];
const RELAY_ID_CONTEXT: &str = "xyz.gnosyslabs.noise.relay-id.v1";
const RELAY_DESCRIPTOR_CONTEXT: &[u8] = b"xyz.gnosyslabs.noise.relay-descriptor.v3\0";
const RELAY_DESCRIPTOR_CLOCK_SKEW_SECONDS: u64 = 5 * 60;
const RELAY_DESCRIPTOR_MAX_LIFETIME_SECONDS: u64 = 48 * 60 * 60;
const RELAY_DESCRIPTOR_MAX_URL_BYTES: usize = 2_048;
const RELAY_DESCRIPTOR_MAX_OHTTP_CONFIG_BYTES: usize = 4_096;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelayDescriptor {
    pub base_url: String,
    pub ohttp_config: Option<Vec<u8>>,
}

impl RelayDescriptor {
    pub fn parse(value: &str) -> anyhow::Result<Self> {
        let value = value.trim();
        if value.is_empty() {
            bail!("relay address is empty")
        }
        let mut url = Url::parse(value).context("relay address is invalid")?;
        if !url.username().is_empty() || url.password().is_some() {
            bail!("relay addresses cannot contain credentials")
        }
        if url.query().is_some() || !matches!(url.path(), "" | "/") {
            bail!("relay addresses cannot contain a path or query")
        }
        let local = match url.host() {
            Some(Host::Domain("localhost")) => true,
            Some(Host::Ipv4(address)) => address.is_loopback(),
            Some(Host::Ipv6(address)) => address.is_loopback(),
            _ => false,
        };
        if url.scheme() != "https" && !(url.scheme() == "http" && local) {
            bail!("relay addresses must use HTTPS outside local development")
        }
        let ohttp_config = url
            .fragment()
            .map(|fragment| {
                let encoded = fragment
                    .strip_prefix("ohttp=")
                    .context("relay fragment is not an OHTTP key")?;
                let decoded = URL_SAFE_NO_PAD
                    .decode(encoded)
                    .context("relay OHTTP key is invalid")?;
                if decoded.is_empty() {
                    bail!("relay OHTTP key is empty")
                }
                Ok(decoded)
            })
            .transpose()?;
        url.set_fragment(None);
        let base_url = url.as_str().trim_end_matches('/').to_owned();
        Ok(Self {
            base_url,
            ohttp_config,
        })
    }

    #[must_use]
    pub fn shareable(base_url: &str, ohttp_config: &[u8]) -> String {
        format!(
            "{}#ohttp={}",
            base_url.trim_end_matches('/'),
            URL_SAFE_NO_PAD.encode(ohttp_config)
        )
    }

    #[must_use]
    pub fn is_local(&self) -> bool {
        Url::parse(&self.base_url)
            .ok()
            .and_then(|url| url.host().map(host_is_local))
            .unwrap_or(false)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedRelayDescriptor {
    pub protocol_version: u16,
    pub relay_id: String,
    pub public_key_base64: String,
    pub base_url: String,
    pub ohttp_config_base64: String,
    pub storage_capacity_bytes: u64,
    pub storage_available_bytes: u64,
    pub issued_at_unix_seconds: u64,
    pub expires_at_unix_seconds: u64,
    pub signature_base64: String,
}

impl SignedRelayDescriptor {
    pub fn signing_bytes(&self) -> anyhow::Result<Vec<u8>> {
        self.validate_fields()?;
        let mut bytes = Vec::with_capacity(
            RELAY_DESCRIPTOR_CONTEXT.len()
                + self.relay_id.len()
                + self.public_key_base64.len()
                + self.base_url.len()
                + self.ohttp_config_base64.len()
                + 64,
        );
        bytes.extend_from_slice(RELAY_DESCRIPTOR_CONTEXT);
        bytes.extend_from_slice(&self.protocol_version.to_be_bytes());
        append_field(&mut bytes, &self.relay_id)?;
        append_field(&mut bytes, &self.public_key_base64)?;
        append_field(&mut bytes, &self.base_url)?;
        append_field(&mut bytes, &self.ohttp_config_base64)?;
        bytes.extend_from_slice(&self.storage_capacity_bytes.to_be_bytes());
        bytes.extend_from_slice(&self.storage_available_bytes.to_be_bytes());
        bytes.extend_from_slice(&self.issued_at_unix_seconds.to_be_bytes());
        bytes.extend_from_slice(&self.expires_at_unix_seconds.to_be_bytes());
        Ok(bytes)
    }

    pub fn verify_at(&self, now: u64) -> anyhow::Result<()> {
        if self.issued_at_unix_seconds > now.saturating_add(RELAY_DESCRIPTOR_CLOCK_SKEW_SECONDS) {
            bail!("relay descriptor was issued in the future")
        }
        if self.expires_at_unix_seconds <= now {
            bail!("relay descriptor has expired")
        }
        if self.expires_at_unix_seconds <= self.issued_at_unix_seconds
            || self.expires_at_unix_seconds - self.issued_at_unix_seconds
                > RELAY_DESCRIPTOR_MAX_LIFETIME_SECONDS
        {
            bail!("relay descriptor has an invalid lifetime")
        }
        let public_key = decode_exact::<32>(
            &self.public_key_base64,
            &STANDARD_NO_PAD,
            "relay public key",
        )?;
        let signature_bytes = decode_exact::<64>(
            &self.signature_base64,
            &STANDARD_NO_PAD,
            "relay descriptor signature",
        )?;
        if STANDARD_NO_PAD.encode(signature_bytes) != self.signature_base64 {
            bail!("relay descriptor signature is not canonically encoded")
        }
        let signature = Signature::from_bytes(&signature_bytes);
        let verifying_key = VerifyingKey::from_bytes(&public_key)
            .context("relay descriptor public key is invalid")?;
        verifying_key
            .verify(&self.signing_bytes()?, &signature)
            .context("relay descriptor signature is invalid")
    }

    fn validate_fields(&self) -> anyhow::Result<()> {
        if self.protocol_version != RELAY_PROTOCOL_VERSION {
            bail!("unsupported relay protocol version")
        }
        let public_key = decode_exact::<32>(
            &self.public_key_base64,
            &STANDARD_NO_PAD,
            "relay public key",
        )?;
        if STANDARD_NO_PAD.encode(public_key) != self.public_key_base64 {
            bail!("relay public key is not canonically encoded")
        }
        if relay_id_from_public_key(&public_key) != self.relay_id {
            bail!("relay ID does not match its public key")
        }
        if self.base_url.len() > RELAY_DESCRIPTOR_MAX_URL_BYTES {
            bail!("relay base URL is too large")
        }
        let parsed = RelayDescriptor::parse(&self.base_url)?;
        if parsed.base_url != self.base_url || parsed.ohttp_config.is_some() {
            bail!("relay base URL is not canonical")
        }
        if self.ohttp_config_base64.len() > RELAY_DESCRIPTOR_MAX_OHTTP_CONFIG_BYTES {
            bail!("relay OHTTP key is too large")
        }
        if self.storage_capacity_bytes != 0
            && self.storage_available_bytes > self.storage_capacity_bytes
        {
            bail!("relay storage availability exceeds its capacity")
        }
        let ohttp_config = URL_SAFE_NO_PAD
            .decode(&self.ohttp_config_base64)
            .context("relay OHTTP key is invalid")?;
        if ohttp_config.is_empty()
            || URL_SAFE_NO_PAD.encode(&ohttp_config) != self.ohttp_config_base64
        {
            bail!("relay OHTTP key is not canonically encoded")
        }
        Ok(())
    }
}

#[must_use]
pub fn relay_id_from_public_key(public_key: &[u8; 32]) -> String {
    blake3::derive_key(RELAY_ID_CONTEXT, public_key)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn append_field(bytes: &mut Vec<u8>, value: &str) -> anyhow::Result<()> {
    let length = u32::try_from(value.len()).context("relay descriptor field is too large")?;
    bytes.extend_from_slice(&length.to_be_bytes());
    bytes.extend_from_slice(value.as_bytes());
    Ok(())
}

fn decode_exact<const LENGTH: usize>(
    encoded: &str,
    engine: &impl base64::Engine,
    label: &'static str,
) -> anyhow::Result<[u8; LENGTH]> {
    engine
        .decode(encoded)
        .with_context(|| format!("{label} encoding is invalid"))?
        .try_into()
        .map_err(|_| anyhow::anyhow!("{label} length is invalid"))
}

fn host_is_local(host: Host<&str>) -> bool {
    match host {
        Host::Domain(domain) => domain.eq_ignore_ascii_case("localhost"),
        Host::Ipv4(address) => address.is_loopback(),
        Host::Ipv6(address) => address.is_loopback(),
    }
}

#[derive(Debug)]
pub struct PlainRequest {
    pub method: String,
    pub path: String,
    pub body: Vec<u8>,
}

#[derive(Debug)]
pub struct PlainResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

pub fn encode_request(
    method: &str,
    scheme: &str,
    authority: &str,
    path: &str,
    body: &[u8],
) -> anyhow::Result<Vec<u8>> {
    let mut message = Message::request(
        method.as_bytes().to_vec(),
        scheme.as_bytes().to_vec(),
        authority.as_bytes().to_vec(),
        path.as_bytes().to_vec(),
    );
    if !body.is_empty() {
        message.put_header("content-type", "application/json");
        message.write_content(body);
    }
    encode_padded(&message)
}

pub fn decode_request(bytes: &[u8]) -> anyhow::Result<PlainRequest> {
    let message = Message::read_bhttp(&mut Cursor::new(bytes)).context("invalid binary request")?;
    let method = message
        .control()
        .method()
        .context("binary message is not a request")?;
    let path = message
        .control()
        .path()
        .context("binary request has no path")?;
    Ok(PlainRequest {
        method: String::from_utf8(method.to_vec()).context("request method is not UTF-8")?,
        path: String::from_utf8(path.to_vec()).context("request path is not UTF-8")?,
        body: message.content().to_vec(),
    })
}

pub fn encode_response(status: u16, body: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut message =
        Message::response(StatusCode::try_from(status).context("invalid response status")?);
    if !body.is_empty() {
        message.put_header("content-type", "application/json");
        message.write_content(body);
    }
    encode_padded(&message)
}

pub fn decode_response(bytes: &[u8]) -> anyhow::Result<PlainResponse> {
    let message =
        Message::read_bhttp(&mut Cursor::new(bytes)).context("invalid binary response")?;
    let status = message
        .control()
        .status()
        .context("binary message is not a response")?
        .code();
    Ok(PlainResponse {
        status,
        body: message.content().to_vec(),
    })
}

fn encode_padded(message: &Message) -> anyhow::Result<Vec<u8>> {
    let mut encoded = Vec::new();
    message
        .write_bhttp(Mode::KnownLength, &mut encoded)
        .context("could not encode binary HTTP")?;
    let Some(bucket) = PAD_BUCKETS
        .iter()
        .copied()
        .find(|bucket| *bucket >= encoded.len())
    else {
        bail!("oblivious message exceeds the maximum padded size")
    };
    encoded.resize(bucket, 0);
    Ok(encoded)
}
