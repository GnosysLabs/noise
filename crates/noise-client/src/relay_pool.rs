use std::{
    collections::HashMap,
    fs,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, bail};
use noise_transport::{
    RELAY_DIRECTORY_PATH, RelayDescriptor, SIGNED_RELAY_DESCRIPTOR_PATH, SignedRelayDescriptor,
};
use reqwest::{Client, Response, redirect::Policy};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::{net::lookup_host, task::JoinSet};
use url::{Host, Url};

const CACHE_VERSION: u32 = 1;
const MAX_DIRECTORY_ENTRIES: usize = 512;
const MAX_DISCOVERY_SOURCES: usize = 10;
const MAX_CANDIDATES_TO_VERIFY: usize = 24;
const MAX_MASK_RELAYS: usize = 12;
const MAX_DESCRIPTOR_RESPONSE_BYTES: usize = 16 * 1024;
const MAX_DIRECTORY_RESPONSE_BYTES: usize = 1024 * 1024;
const MIN_STORAGE_AVAILABLE_BYTES: u64 = 2_000_000;
const RELAY_REQUEST_TIMEOUT: Duration = Duration::from_secs(8);

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RelayPoolCache {
    version: u32,
    descriptors: Vec<SignedRelayDescriptor>,
}

pub async fn discover(cache_path: &Path, seeds: Vec<String>) -> anyhow::Result<Vec<String>> {
    let cache_path = prepare_cache_file(cache_path)?;
    let now = unix_seconds()?;
    let seeds = seeds
        .iter()
        .map(|seed| RelayDescriptor::parse(seed))
        .collect::<anyhow::Result<Vec<_>>>()?;
    if seeds.is_empty() {
        bail!("relay discovery needs at least one seed")
    }
    let allow_local = seeds.iter().all(RelayDescriptor::is_local);
    let seed_urls = seeds
        .iter()
        .map(|seed| seed.base_url.clone())
        .collect::<Vec<_>>();
    let cached = load_cache(&cache_path)
        .descriptors
        .into_iter()
        .filter(|descriptor| descriptor.verify_at(now).is_ok())
        .collect::<Vec<_>>();

    let mut sources = seed_urls.clone();
    for descriptor in &cached {
        if sources.len() >= MAX_DISCOVERY_SOURCES {
            break;
        }
        if !sources.contains(&descriptor.base_url) {
            sources.push(descriptor.base_url.clone());
        }
    }

    let mut directory_tasks = JoinSet::new();
    for source in sources {
        directory_tasks.spawn(async move { fetch_directory(&source, now, allow_local).await });
    }

    let mut candidates = HashMap::<String, SignedRelayDescriptor>::new();
    for descriptor in cached {
        consider_descriptor(&mut candidates, descriptor, now);
    }
    while let Some(result) = directory_tasks.join_next().await {
        let Ok(Ok(descriptors)) = result else {
            continue;
        };
        for descriptor in descriptors {
            consider_descriptor(&mut candidates, descriptor, now);
        }
    }

    let mut candidates = candidates.into_values().collect::<Vec<_>>();
    candidates.sort_by(|left, right| left.relay_id.cmp(&right.relay_id));
    if !candidates.is_empty() {
        let start = (now / (24 * 60 * 60)) as usize % candidates.len();
        candidates.rotate_left(start);
    }
    candidates.truncate(MAX_CANDIDATES_TO_VERIFY);

    let mut verification_tasks = JoinSet::new();
    for descriptor in candidates {
        verification_tasks.spawn(async move { verify_relay(&descriptor, now, allow_local).await });
    }
    let mut verified = Vec::new();
    while let Some(result) = verification_tasks.join_next().await {
        let Ok(Ok(descriptor)) = result else {
            continue;
        };
        if !seed_urls.contains(&descriptor.base_url) {
            verified.push(descriptor);
        }
    }
    verified.sort_by(|left, right| left.relay_id.cmp(&right.relay_id));
    verified.dedup_by(|left, right| left.base_url == right.base_url);
    verified.truncate(MAX_MASK_RELAYS);

    let _ = save_cache(
        &cache_path,
        &RelayPoolCache {
            version: CACHE_VERSION,
            descriptors: verified.clone(),
        },
    );
    Ok(verified
        .into_iter()
        // Keep the discovered relay's pinned OHTTP key. These relays are masks
        // and eligible shard stores; dropping the key here would force direct,
        // IP-revealing storage requests.
        .map(|descriptor| {
            format!(
                "{}#ohttp={}",
                descriptor.base_url, descriptor.ohttp_config_base64
            )
        })
        .collect())
}

fn prepare_cache_file(cache_root: &Path) -> anyhow::Result<std::path::PathBuf> {
    const FILE_NAME: &str = "relay-pool.json";
    if cache_root.is_file() {
        // Older desktop builds mistakenly wrote the relay-pool JSON directly
        // at Tauri's cache-directory path, preventing every other cache from
        // creating a subdirectory there. Preserve that cache while repairing
        // the path into the directory it was always meant to be.
        let migration = cache_root.with_extension("relay-pool-migration");
        if migration.exists() {
            fs::remove_file(&migration)
                .with_context(|| format!("could not clear {}", migration.display()))?;
        }
        fs::rename(cache_root, &migration)
            .with_context(|| format!("could not move {}", cache_root.display()))?;
        if let Err(error) = fs::create_dir_all(cache_root) {
            let _ = fs::rename(&migration, cache_root);
            return Err(error)
                .with_context(|| format!("could not create {}", cache_root.display()));
        }
        let destination = cache_root.join(FILE_NAME);
        if let Err(error) = fs::rename(&migration, &destination) {
            let _ = fs::remove_dir(cache_root);
            let _ = fs::rename(&migration, cache_root);
            return Err(error)
                .with_context(|| format!("could not migrate {}", destination.display()));
        }
        return Ok(destination);
    }
    fs::create_dir_all(cache_root)
        .with_context(|| format!("could not create {}", cache_root.display()))?;
    Ok(cache_root.join(FILE_NAME))
}

fn consider_descriptor(
    candidates: &mut HashMap<String, SignedRelayDescriptor>,
    descriptor: SignedRelayDescriptor,
    now: u64,
) {
    if descriptor.verify_at(now).is_err() {
        return;
    }
    if descriptor.storage_available_bytes < MIN_STORAGE_AVAILABLE_BYTES {
        return;
    }
    let replace = candidates.get(&descriptor.relay_id).is_none_or(|current| {
        (
            descriptor.issued_at_unix_seconds,
            descriptor.signature_base64.as_str(),
        ) > (
            current.issued_at_unix_seconds,
            current.signature_base64.as_str(),
        )
    });
    if replace
        && (candidates.contains_key(&descriptor.relay_id)
            || candidates.len() < MAX_DIRECTORY_ENTRIES)
    {
        candidates.insert(descriptor.relay_id.clone(), descriptor);
    }
}

pub(super) async fn client_for_mask(base_url: &str) -> anyhow::Result<Client> {
    let relay = RelayDescriptor::parse(base_url)?;
    client_for_relay(&relay.base_url, relay.is_local()).await
}

async fn fetch_directory(
    base_url: &str,
    now: u64,
    allow_local: bool,
) -> anyhow::Result<Vec<SignedRelayDescriptor>> {
    let response = client_for_relay(base_url, allow_local)
        .await?
        .get(format!("{base_url}{RELAY_DIRECTORY_PATH}"))
        .send()
        .await
        .context("could not fetch relay directory")?
        .error_for_status()
        .context("relay directory request was rejected")?;
    let descriptors =
        read_json_limited::<Vec<SignedRelayDescriptor>>(response, MAX_DIRECTORY_RESPONSE_BYTES)
            .await?;
    if descriptors.len() > MAX_DIRECTORY_ENTRIES {
        bail!("relay directory is too large")
    }
    for descriptor in &descriptors {
        descriptor.verify_at(now)?;
    }
    Ok(descriptors)
}

async fn verify_relay(
    announced: &SignedRelayDescriptor,
    now: u64,
    allow_local: bool,
) -> anyhow::Result<SignedRelayDescriptor> {
    announced.verify_at(now)?;
    let response = client_for_relay(&announced.base_url, allow_local)
        .await?
        .get(format!(
            "{}{}",
            announced.base_url, SIGNED_RELAY_DESCRIPTOR_PATH
        ))
        .send()
        .await
        .context("could not reach discovered relay")?
        .error_for_status()
        .context("discovered relay rejected its descriptor request")?;
    let fetched =
        read_json_limited::<SignedRelayDescriptor>(response, MAX_DESCRIPTOR_RESPONSE_BYTES).await?;
    fetched.verify_at(now)?;
    if fetched.relay_id != announced.relay_id
        || fetched.public_key_base64 != announced.public_key_base64
        || fetched.base_url != announced.base_url
        || fetched.ohttp_config_base64 != announced.ohttp_config_base64
    {
        bail!("discovered relay does not serve the announced identity")
    }
    Ok(fetched)
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
        .user_agent("noise-client/3");
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

fn load_cache(path: &Path) -> RelayPoolCache {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<RelayPoolCache>(&bytes).ok())
        .filter(|cache| cache.version == CACHE_VERSION)
        .unwrap_or_default()
}

fn save_cache(path: &Path, cache: &RelayPoolCache) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, serde_json::to_vec(cache)?)
        .with_context(|| format!("could not write {}", temporary.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("could not secure {}", temporary.display()))?;
    }
    fs::rename(&temporary, path).with_context(|| format!("could not replace {}", path.display()))
}

fn unix_seconds() -> anyhow::Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_secs())
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
