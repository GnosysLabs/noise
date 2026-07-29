use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
};

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

    #[arg(long, env = "NOISE_KLIPY_API_KEY", hide_env_values = true)]
    pub(crate) klipy_api_key: Option<String>,

    #[arg(long, env = "NOISE_APNS_KEY_FILE")]
    pub(crate) apns_key_file: Option<PathBuf>,

    #[arg(long, env = "NOISE_APNS_KEY_ID")]
    pub(crate) apns_key_id: Option<String>,

    #[arg(long, env = "NOISE_APNS_TEAM_ID")]
    pub(crate) apns_team_id: Option<String>,

    #[arg(long, env = "NOISE_APNS_TOPIC")]
    pub(crate) apns_topic: Option<String>,
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
        for origin in self.allowed_origins()? {
            let parsed =
                url::Url::parse(&origin).context("allowed origin must be a valid origin")?;
            let local_http = parsed.scheme() == "http"
                && parsed
                    .host_str()
                    .is_some_and(|host| matches!(host, "127.0.0.1" | "localhost" | "::1"));
            if (parsed.scheme() != "https" && !local_http)
                || parsed.host_str().is_none()
                || !parsed.username().is_empty()
                || parsed.password().is_some()
                || parsed.path() != "/"
                || parsed.query().is_some()
                || parsed.fragment().is_some()
                || origin.ends_with('/')
            {
                bail!("allowed origins must be exact HTTPS origins or local development origins");
            }
        }
        self.token_hash_key()?;
        self.media_config()?;
        self.push_config()?;
        if self
            .klipy_api_key
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            bail!("NOISE_KLIPY_API_KEY cannot be empty");
        }
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

    pub(crate) fn allowed_origins(&self) -> anyhow::Result<Vec<String>> {
        let Some(configured) = self.allowed_origin.as_deref() else {
            return Ok(Vec::new());
        };
        let origins = configured
            .split(',')
            .map(str::trim)
            .filter(|origin| !origin.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if origins.is_empty() || origins.len() > 8 {
            bail!("NOISE_ALLOWED_ORIGIN must contain between one and eight origins");
        }
        Ok(origins)
    }

    pub(crate) fn media_config(&self) -> anyhow::Result<Option<MediaConfig>> {
        let configured = [
            self.r2_endpoint.as_deref(),
            self.r2_bucket.as_deref(),
            self.r2_access_key_id.as_deref(),
            self.r2_secret_access_key.as_deref(),
        ];
        if configured.iter().all(|value| value.is_none()) {
            return Ok(None);
        }
        if configured
            .iter()
            .any(|value| value.is_none_or(|value| value.trim().is_empty()))
        {
            bail!("central media requires the R2 endpoint, bucket, access key, and secret key");
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
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            bail!("NOISE_R2_BUCKET is invalid");
        }
        Ok(Some(MediaConfig {
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
        }))
    }

    pub(crate) fn push_config(&self) -> anyhow::Result<Option<PushConfig>> {
        let configured = [
            self.apns_key_file.is_some(),
            self.apns_key_id.is_some(),
            self.apns_team_id.is_some(),
            self.apns_topic.is_some(),
        ];
        if configured.iter().all(|configured| !configured) {
            return Ok(None);
        }
        if configured.iter().any(|configured| !configured) {
            bail!("central APNs requires the key file, key ID, team ID, and application topic");
        }
        let key_file = self.apns_key_file.clone().expect("checked above");
        if !key_file.is_absolute() {
            bail!("NOISE_APNS_KEY_FILE must be an absolute path");
        }
        let key_id = self.apns_key_id.as_deref().expect("checked above").trim();
        let team_id = self.apns_team_id.as_deref().expect("checked above").trim();
        if !valid_apple_identifier(key_id) {
            bail!("NOISE_APNS_KEY_ID is invalid");
        }
        if !valid_apple_identifier(team_id) {
            bail!("NOISE_APNS_TEAM_ID is invalid");
        }
        let topic = self.apns_topic.as_deref().expect("checked above").trim();
        if topic.len() > 255
            || !topic.contains('.')
            || !topic
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
        {
            bail!("NOISE_APNS_TOPIC is invalid");
        }
        Ok(Some(PushConfig {
            key_file,
            key_id: key_id.to_owned(),
            team_id: team_id.to_owned(),
            topic: topic.to_owned(),
        }))
    }
}

fn valid_apple_identifier(value: &str) -> bool {
    value.len() == 10
        && value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
}

pub(crate) struct MediaConfig {
    pub endpoint: String,
    pub bucket: String,
    pub access_key_id: String,
    pub secret_access_key: String,
}

pub(crate) struct PushConfig {
    pub key_file: PathBuf,
    pub key_id: String,
    pub team_id: String,
    pub topic: String,
}
