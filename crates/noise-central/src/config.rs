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
}
