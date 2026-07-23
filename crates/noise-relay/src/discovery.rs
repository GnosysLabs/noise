use std::{
    collections::{HashMap, VecDeque},
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use noise_transport::{
    RELAY_DIRECTORY_PATH, RelayDescriptor, SIGNED_RELAY_DESCRIPTOR_PATH, SignedRelayDescriptor,
};
use reqwest::{Client, Response, redirect::Policy};
use serde::de::DeserializeOwned;
use tokio::{
    net::lookup_host,
    sync::{Mutex, OwnedSemaphorePermit, RwLock, Semaphore},
};
use url::{Host, Url};

use crate::store::DurableStore;

const MAX_DIRECTORY_ENTRIES: usize = 512;
const MAX_DESCRIPTOR_RESPONSE_BYTES: usize = 16 * 1024;
const MAX_DIRECTORY_RESPONSE_BYTES: usize = 1024 * 1024;
const RELAY_REQUEST_TIMEOUT: Duration = Duration::from_secs(8);
const ANNOUNCEMENT_WINDOW: Duration = Duration::from_secs(60);
const MAX_ANNOUNCEMENTS_PER_WINDOW: usize = 30;
const MAX_CONCURRENT_ANNOUNCEMENTS: usize = 4;

#[derive(Clone)]
pub struct AnnouncementLimiter {
    attempts: Arc<Mutex<VecDeque<Instant>>>,
    permits: Arc<Semaphore>,
}

impl AnnouncementLimiter {
    pub fn new() -> Self {
        Self {
            attempts: Arc::new(Mutex::new(VecDeque::new())),
            permits: Arc::new(Semaphore::new(MAX_CONCURRENT_ANNOUNCEMENTS)),
        }
    }

    pub async fn begin(&self) -> Option<OwnedSemaphorePermit> {
        let permit = self.permits.clone().try_acquire_owned().ok()?;
        let now = Instant::now();
        let mut attempts = self.attempts.lock().await;
        while attempts
            .front()
            .is_some_and(|attempt| now.duration_since(*attempt) >= ANNOUNCEMENT_WINDOW)
        {
            attempts.pop_front();
        }
        if attempts.len() >= MAX_ANNOUNCEMENTS_PER_WINDOW {
            return None;
        }
        attempts.push_back(now);
        Some(permit)
    }
}

#[derive(Clone)]
pub struct RelayDirectory {
    entries: Arc<RwLock<HashMap<String, SignedRelayDescriptor>>>,
    store: DurableStore,
}

impl RelayDirectory {
    pub fn new(entries: HashMap<String, SignedRelayDescriptor>, store: DurableStore) -> Self {
        Self {
            entries: Arc::new(RwLock::new(entries)),
            store,
        }
    }

    pub async fn insert(
        &self,
        descriptor: SignedRelayDescriptor,
        now: u64,
    ) -> anyhow::Result<bool> {
        descriptor.verify_at(now)?;
        let mut entries = self.entries.write().await;
        entries.retain(|_, current| current.verify_at(now).is_ok());
        if let Some(current) = entries.get(&descriptor.relay_id) {
            let current_order = (
                current.issued_at_unix_seconds,
                current.signature_base64.as_str(),
            );
            let incoming_order = (
                descriptor.issued_at_unix_seconds,
                descriptor.signature_base64.as_str(),
            );
            if current_order >= incoming_order {
                return Ok(false);
            }
        } else if entries.len() >= MAX_DIRECTORY_ENTRIES {
            bail!("relay directory is full")
        }
        self.store.upsert_relay_descriptor(&descriptor).await?;
        entries.insert(descriptor.relay_id.clone(), descriptor);
        Ok(true)
    }

    pub async fn list(&self, now: u64) -> Vec<SignedRelayDescriptor> {
        let mut entries = self.entries.write().await;
        entries.retain(|_, descriptor| descriptor.verify_at(now).is_ok());
        let mut descriptors = entries.values().cloned().collect::<Vec<_>>();
        descriptors.sort_by(|left, right| left.relay_id.cmp(&right.relay_id));
        descriptors
    }

    pub async fn descriptor_for_base_url(
        &self,
        base_url: &str,
        now: u64,
    ) -> Option<SignedRelayDescriptor> {
        let mut entries = self.entries.write().await;
        entries.retain(|_, descriptor| descriptor.verify_at(now).is_ok());
        entries
            .values()
            .find(|descriptor| descriptor.base_url == base_url)
            .cloned()
    }
}

pub async fn verify_relay_reachability(
    announced: &SignedRelayDescriptor,
    now: u64,
    allow_local: bool,
) -> anyhow::Result<SignedRelayDescriptor> {
    announced.verify_at(now)?;
    let client = client_for_relay(&announced.base_url, allow_local).await?;
    let response = client
        .get(format!(
            "{}{}",
            announced.base_url, SIGNED_RELAY_DESCRIPTOR_PATH
        ))
        .send()
        .await
        .context("could not reach the announced relay")?
        .error_for_status()
        .context("announced relay rejected its descriptor request")?;
    let fetched =
        read_json_limited::<SignedRelayDescriptor>(response, MAX_DESCRIPTOR_RESPONSE_BYTES).await?;
    fetched.verify_at(now)?;
    if fetched.relay_id != announced.relay_id
        || fetched.public_key_base64 != announced.public_key_base64
        || fetched.base_url != announced.base_url
        || fetched.ohttp_config_base64 != announced.ohttp_config_base64
    {
        bail!("announced relay does not serve the same identity and endpoint")
    }
    Ok(fetched)
}

pub async fn fetch_relay_directory(
    base_url: &str,
    now: u64,
    allow_local: bool,
) -> anyhow::Result<Vec<SignedRelayDescriptor>> {
    let client = client_for_relay(base_url, allow_local).await?;
    let response = client
        .get(format!("{base_url}{RELAY_DIRECTORY_PATH}"))
        .send()
        .await
        .context("could not fetch the relay directory")?
        .error_for_status()
        .context("relay directory request was rejected")?;
    let descriptors =
        read_json_limited::<Vec<SignedRelayDescriptor>>(response, MAX_DIRECTORY_RESPONSE_BYTES)
            .await?;
    if descriptors.len() > MAX_DIRECTORY_ENTRIES {
        bail!("remote relay directory is too large")
    }
    for descriptor in &descriptors {
        descriptor.verify_at(now)?;
    }
    Ok(descriptors)
}

pub async fn announce_relay(
    base_url: &str,
    descriptor: &SignedRelayDescriptor,
    allow_local: bool,
) -> anyhow::Result<()> {
    client_for_relay(base_url, allow_local)
        .await?
        .post(format!("{base_url}{RELAY_DIRECTORY_PATH}"))
        .json(descriptor)
        .send()
        .await
        .context("could not announce the relay")?
        .error_for_status()
        .context("relay announcement was rejected")?;
    Ok(())
}

pub async fn client_for_verified_relay(
    base_url: &str,
    allow_local: bool,
) -> anyhow::Result<Client> {
    client_for_relay(base_url, allow_local).await
}

async fn client_for_relay(base_url: &str, allow_local: bool) -> anyhow::Result<Client> {
    let relay = RelayDescriptor::parse(base_url)?;
    if relay.base_url != base_url || relay.ohttp_config.is_some() {
        bail!("relay discovery URL is not canonical")
    }
    if relay.is_local() && !allow_local {
        bail!("local relay discovery targets are disabled")
    }
    let url = Url::parse(base_url).context("relay discovery URL is invalid")?;
    let host = url.host().context("relay discovery URL has no host")?;
    let port = url
        .port_or_known_default()
        .context("relay discovery URL has no port")?;
    let mut builder = Client::builder()
        .redirect(Policy::none())
        .timeout(RELAY_REQUEST_TIMEOUT)
        .user_agent("noise-relay/2");
    match host {
        Host::Domain(domain) => {
            let addresses = lookup_host((domain, port))
                .await
                .context("could not resolve relay discovery URL")?
                .collect::<Vec<_>>();
            if addresses.is_empty() {
                bail!("relay discovery URL did not resolve")
            }
            if !allow_local && addresses.iter().any(|address| !is_public_ip(address.ip())) {
                bail!("relay discovery URL resolves to a non-public address")
            }
            builder = builder.resolve_to_addrs(domain, &addresses);
        }
        Host::Ipv4(address) => {
            if !allow_local && !is_public_ipv4(address) {
                bail!("relay discovery URL uses a non-public address")
            }
        }
        Host::Ipv6(address) => {
            if !allow_local && !is_public_ipv6(address) {
                bail!("relay discovery URL uses a non-public address")
            }
        }
    }
    builder.build().context("could not create relay client")
}

async fn read_json_limited<T: DeserializeOwned>(
    mut response: Response,
    maximum_bytes: usize,
) -> anyhow::Result<T> {
    if response
        .content_length()
        .is_some_and(|length| length > maximum_bytes as u64)
    {
        bail!("relay response is too large")
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .context("could not read relay response")?
    {
        if body.len().saturating_add(chunk.len()) > maximum_bytes {
            bail!("relay response is too large")
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).context("relay response is invalid JSON")
}

fn is_public_ip(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => is_public_ipv4(address),
        IpAddr::V6(address) => is_public_ipv6(address),
    }
}

fn is_public_ipv4(address: Ipv4Addr) -> bool {
    let [first, second, third, _] = address.octets();
    !(address.is_private()
        || address.is_loopback()
        || address.is_link_local()
        || address.is_broadcast()
        || address.is_documentation()
        || address.is_multicast()
        || address.is_unspecified()
        || first == 0
        || first >= 240
        || (first == 100 && (64..=127).contains(&second))
        || (first == 192 && second == 0 && third == 0)
        || (first == 192 && second == 88 && third == 99)
        || (first == 198 && matches!(second, 18 | 19)))
}

fn is_public_ipv6(address: Ipv6Addr) -> bool {
    if let Some(mapped) = address.to_ipv4_mapped() {
        return is_public_ipv4(mapped);
    }
    let segments = address.segments();
    !(address.is_loopback()
        || address.is_unspecified()
        || address.is_multicast()
        || segments[0] & 0xfe00 == 0xfc00
        || segments[0] & 0xffc0 == 0xfe80
        || segments[0] & 0xffc0 == 0xfec0
        || (segments[0] == 0x2001 && segments[1] == 0x0db8))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_public_discovery_addresses() {
        for address in [
            Ipv4Addr::LOCALHOST,
            Ipv4Addr::new(10, 0, 0, 1),
            Ipv4Addr::new(100, 64, 0, 1),
            Ipv4Addr::new(169, 254, 0, 1),
            Ipv4Addr::new(192, 0, 2, 1),
            Ipv4Addr::new(198, 18, 0, 1),
        ] {
            assert!(!is_public_ipv4(address));
        }
        assert!(is_public_ipv4(Ipv4Addr::new(1, 1, 1, 1)));
        assert!(!is_public_ipv6(Ipv6Addr::LOCALHOST));
        assert!(is_public_ipv6("2606:4700:4700::1111".parse().unwrap()));
    }
}
