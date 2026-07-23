use std::{
    fs,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
};

use anyhow::{Context, bail};
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RelayConfig {
    pub listen: SocketAddr,
    pub data: Option<PathBuf>,
    pub public_url: Option<String>,
    pub peers: Vec<String>,
    pub mask_targets: Vec<String>,
    pub bootstrap_relays: Vec<String>,
    pub discovery_interval_seconds: u64,
    pub storage_limit_bytes: u64,
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            listen: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4301),
            data: None,
            public_url: None,
            peers: Vec::new(),
            mask_targets: Vec::new(),
            bootstrap_relays: Vec::new(),
            discovery_interval_seconds: 30,
            storage_limit_bytes: 0,
        }
    }
}

impl RelayConfig {
    pub fn load(path: Option<&Path>) -> anyhow::Result<Self> {
        let Some(path) = path else {
            return Ok(Self::default());
        };
        let source = fs::read_to_string(path)
            .with_context(|| format!("could not read relay config {}", path.display()))?;
        let config = toml::from_str::<Self>(&source)
            .with_context(|| format!("could not parse relay config {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        if self.discovery_interval_seconds == 0 {
            bail!("discovery_interval_seconds must be greater than zero")
        }
        if self
            .public_url
            .as_deref()
            .is_some_and(|url| url.trim().is_empty())
        {
            bail!("public_url cannot be empty")
        }
        Ok(())
    }
}
