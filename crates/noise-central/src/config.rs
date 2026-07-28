use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use anyhow::{Context, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD_NO_PAD};
use clap::Parser;

#[derive(Parser)]
#[command(
    name = "noise-central",
    version,
    about = "The centrally operated, end-to-end encrypted noise service"
)]
pub struct CentralConfig {
    #[arg(long, env = "NOISE_CENTRAL_LISTEN", default_value = "127.0.0.1:4302")]
    pub listen: SocketAddr,

    #[arg(long, env = "NOISE_DATABASE_HOST", default_value = "127.0.0.1")]
    pub database_host: String,

    #[arg(long, env = "NOISE_DATABASE_PORT", default_value_t = 5432)]
    pub database_port: u16,

    #[arg(long, env = "NOISE_DATABASE_NAME", default_value = "noise")]
    pub database_name: String,

    #[arg(long, env = "NOISE_DATABASE_USER", default_value = "noise_app")]
    pub database_user: String,

    #[arg(long, env = "NOISE_DATABASE_PASSWORD", hide_env_values = true)]
    pub database_password: String,

    #[arg(long, env = "NOISE_DATABASE_POOL_SIZE", default_value_t = 8)]
    pub database_pool_size: usize,

    #[arg(long, env = "NOISE_TOKEN_HASH_KEY", hide_env_values = true)]
    token_hash_key_base64: String,

    #[arg(long, env = "NOISE_ALLOWED_ORIGIN")]
    pub allowed_origin: Option<String>,

    #[arg(long, env = "NOISE_R2_ENDPOINT")]
    pub(crate) r2_endpoint: Option<String>,

    #[arg(long, env = "NOISE_R2_BUCKET")]
    pub(crate) r2_bucket: Option<String>,

    #[arg(long, env = "NOISE_R2_ACCESS_KEY_ID", hide_env_values = true)]
    pub(crate) r2_access_key_id: Option<String>,

    #[arg(long, env = "NOISE_R2_SECRET_ACCESS_KEY", hide_env_values = true)]
    pub(crate) r2_secret_access_key: Option<String>,

    #[arg(long, env = "NOISE_COMPATIBILITY_READ_TOKEN", hide_env_values = true)]
    pub(crate) compatibility_read_token: Option<String>,
}

impl CentralConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.database_host.trim().is_empty()
            || self.database_name.trim().is_empty()
            || self.database_user.trim().is_empty()
            || self.database_password.is_empty()
        {
            bail!("database configuration is incomplete");
        }
        if !(1..=16).contains(&self.database_pool_size) {
            bail!("database pool size must be between 1 and 16");
        }
        if self.listen.ip() != IpAddr::V4(Ipv4Addr::LOCALHOST) && !self.listen.ip().is_loopback() {
            bail!("noise-central must listen on loopback behind the TLS reverse proxy");
        }
        if let Some(origin) = self.allowed_origin.as_deref() {
            let parsed =
                url::Url::parse(origin).context("allowed origin must be a valid HTTPS origin")?;
            if parsed.scheme() != "https"
                || parsed.host_str().is_none()
                || !parsed.username().is_empty()
                || parsed.password().is_some()
                || parsed.path() != "/"
                || parsed.query().is_some()
                || parsed.fragment().is_some()
                || origin.ends_with('/')
            {
                bail!("allowed origin must be an exact HTTPS origin without a trailing slash");
            }
        }
        self.token_hash_key()?;
        self.legacy_media_config()?;
        Ok(())
    }

    pub fn token_hash_key(&self) -> anyhow::Result<[u8; 32]> {
        let decoded = STANDARD_NO_PAD
            .decode(&self.token_hash_key_base64)
            .context("NOISE_TOKEN_HASH_KEY must be canonical unpadded base64")?;
        let key: [u8; 32] = decoded
            .try_into()
            .map_err(|_| anyhow::anyhow!("NOISE_TOKEN_HASH_KEY must contain exactly 32 bytes"))?;
        if STANDARD_NO_PAD.encode(key) != self.token_hash_key_base64 {
            bail!("NOISE_TOKEN_HASH_KEY must be canonical unpadded base64");
        }
        Ok(key)
    }

    pub(crate) fn legacy_media_config(&self) -> anyhow::Result<Option<LegacyMediaConfig>> {
        let configured = [
            self.r2_endpoint.as_deref(),
            self.r2_bucket.as_deref(),
            self.r2_access_key_id.as_deref(),
            self.r2_secret_access_key.as_deref(),
            self.compatibility_read_token.as_deref(),
        ];
        if configured.iter().all(|value| value.is_none()) {
            return Ok(None);
        }
        if configured
            .iter()
            .any(|value| value.is_none_or(|value| value.trim().is_empty()))
        {
            bail!(
                "R2 legacy-media reads require endpoint, bucket, access key, secret key, and compatibility token"
            );
        }

        let endpoint = self.r2_endpoint.as_deref().expect("checked above").trim();
        let parsed = url::Url::parse(endpoint).context("NOISE_R2_ENDPOINT is invalid")?;
        let host = parsed.host_str().unwrap_or_default();
        if parsed.scheme() != "https"
            || !host.ends_with(".r2.cloudflarestorage.com")
            || host == "r2.cloudflarestorage.com"
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.port().is_some()
            || parsed.path() != "/"
            || parsed.query().is_some()
            || parsed.fragment().is_some()
        {
            bail!("NOISE_R2_ENDPOINT must be an account-specific R2 HTTPS endpoint");
        }

        let bucket = self.r2_bucket.as_deref().expect("checked above").trim();
        if !(3..=63).contains(&bucket.len())
            || !bucket
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphanumeric)
            || !bucket
                .as_bytes()
                .last()
                .is_some_and(u8::is_ascii_alphanumeric)
            || !bucket
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            bail!("NOISE_R2_BUCKET is invalid");
        }

        let compatibility_token = self
            .compatibility_read_token
            .as_deref()
            .expect("checked above");
        decode_secret(compatibility_token, "NOISE_COMPATIBILITY_READ_TOKEN")?;
        Ok(Some(LegacyMediaConfig {
            endpoint: endpoint.to_owned(),
            bucket: bucket.to_owned(),
            access_key_id: self
                .r2_access_key_id
                .as_deref()
                .expect("checked above")
                .trim()
                .to_owned(),
            secret_access_key: self
                .r2_secret_access_key
                .as_deref()
                .expect("checked above")
                .trim()
                .to_owned(),
            compatibility_token_hash: *blake3::hash(compatibility_token.as_bytes()).as_bytes(),
        }))
    }
}

pub(crate) struct LegacyMediaConfig {
    pub endpoint: String,
    pub bucket: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub compatibility_token_hash: [u8; 32],
}

fn decode_secret(value: &str, name: &str) -> anyhow::Result<[u8; 32]> {
    let decoded = STANDARD_NO_PAD
        .decode(value)
        .with_context(|| format!("{name} must be canonical unpadded base64"))?;
    let secret: [u8; 32] = decoded
        .try_into()
        .map_err(|_| anyhow::anyhow!("{name} must contain exactly 32 bytes"))?;
    if STANDARD_NO_PAD.encode(secret) != value {
        bail!("{name} must be canonical unpadded base64");
    }
    Ok(secret)
}
