mod config;
mod discovery;
mod identity;
mod privacy;
mod shard_store;
mod store;
mod update;

use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, bail};
use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderMap, StatusCode, header::CONTENT_TYPE},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine as _, engine::general_purpose::STANDARD_NO_PAD};
use clap::{Args as ClapArgs, Parser, Subcommand};
use noise_core::{
    AccountVault, GroupDeletion, GroupPresence, InviteRecord, InviteRotation, MlsControlLog,
    MlsEpochRecord, MlsGroupGenesis, MlsJoinRequest, MlsRemovalRequest, ShardDeletion, SignedEvent,
    StorageShard,
};
use noise_transport::{
    GATEWAY_HEADER, OHTTP_GATEWAY_PATH, OHTTP_KEYS_MEDIA_TYPE, OHTTP_KEYS_PATH, OHTTP_RELAY_PATH,
    OHTTP_REQUEST_MEDIA_TYPE, OHTTP_RESPONSE_MEDIA_TYPE, PlainRequest, RELAY_DIRECTORY_PATH,
    RELAY_PROTOCOL_VERSION, RelayDescriptor, SIGNED_RELAY_DESCRIPTOR_PATH, SignedRelayDescriptor,
    encode_response,
};
use serde::{Deserialize, Serialize};
use tokio::{
    net::TcpListener,
    sync::{Mutex, RwLock, watch},
    time::{sleep, timeout},
};
use tower_http::cors::CorsLayer;

use config::RelayConfig;
use discovery::{
    AnnouncementLimiter, RelayDirectory, announce_relay, client_for_verified_relay,
    fetch_relay_directory, verify_relay_reachability,
};
use identity::RelayIdentity;
use privacy::PrivacyGateway;
use shard_store::ShardStore;
use store::{DurableStore, ShardMetadata};

const MAX_DISCOVERY_TARGETS_PER_ROUND: usize = 8;
const MAX_DISCOVERED_RELAYS_PER_TARGET: usize = 16;
const MAX_GROUP_PRESENCE_MILLIS: u64 = 60_000;
const RECENT_GROUP_PRESENCE_MILLIS: u64 = 5 * 60_000;
const MAX_GROUP_PRESENCES: usize = 100_000;

#[derive(Debug, Parser)]
#[command(
    name = "noise-relay",
    version,
    about = "An untrusted Noise protocol relay"
)]
struct Args {
    #[arg(long, global = true, env = "NOISE_RELAY_CONFIG")]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<RelayCommand>,
    #[command(flatten)]
    server: ServerOverrides,
}

#[derive(Debug, Default, ClapArgs)]
struct ServerOverrides {
    #[arg(long)]
    listen: Option<SocketAddr>,
    #[arg(long)]
    peer: Vec<String>,
    #[arg(long)]
    data: Option<PathBuf>,
    #[arg(long)]
    public_url: Option<String>,
    #[arg(long)]
    mask_target: Vec<String>,
    #[arg(long)]
    bootstrap_relay: Vec<String>,
    #[arg(long)]
    discovery_interval_seconds: Option<u64>,
    #[arg(long, env = "NOISE_STORAGE_LIMIT_BYTES")]
    storage_limit_bytes: Option<u64>,
}

#[derive(Debug, Subcommand)]
enum RelayCommand {
    Update {
        #[arg(long)]
        apply: bool,
        #[arg(long, default_value = update::DEFAULT_MANIFEST_URL)]
        manifest_url: String,
        #[arg(long)]
        signature_url: Option<String>,
        #[arg(long)]
        json: bool,
    },
    Status {
        #[arg(long)]
        json: bool,
    },
    Doctor {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Clone, Debug)]
struct ServerSettings {
    listen: SocketAddr,
    peer: Vec<String>,
    data: Option<PathBuf>,
    public_url: Option<String>,
    mask_target: Vec<String>,
    bootstrap_relay: Vec<String>,
    discovery_interval_seconds: u64,
    storage_limit_bytes: u64,
}

#[derive(Clone)]
struct AppState {
    accounts: Arc<RwLock<HashMap<String, AccountVault>>>,
    invites: Arc<RwLock<HashMap<String, InviteRecord>>>,
    invite_rotations: Arc<RwLock<HashMap<String, InviteRotation>>>,
    events: Arc<RwLock<HashMap<String, SignedEvent>>>,
    mls_join_requests: Arc<RwLock<HashMap<String, MlsJoinRequest>>>,
    mls_removal_requests: Arc<RwLock<HashMap<String, MlsRemovalRequest>>>,
    mls_geneses: Arc<RwLock<HashMap<String, MlsGroupGenesis>>>,
    mls_epochs: Arc<RwLock<HashMap<String, MlsEpochRecord>>>,
    shard_count: Arc<AtomicU64>,
    shard_bytes: Arc<AtomicU64>,
    storage_limit_bytes: u64,
    deletions: Arc<RwLock<HashMap<String, GroupDeletion>>>,
    group_changes: Arc<RwLock<HashMap<String, watch::Sender<u64>>>>,
    group_presences: Arc<RwLock<HashMap<String, HashMap<String, GroupPresence>>>>,
    account_changes: Arc<RwLock<HashMap<String, watch::Sender<u64>>>>,
    mutations: Arc<Mutex<()>>,
    peers: Arc<Vec<String>>,
    client: reqwest::Client,
    store: DurableStore,
    shard_store: ShardStore,
    privacy: PrivacyGateway,
    relay_identity: RelayIdentity,
    relay_directory: RelayDirectory,
    announcement_limiter: AnnouncementLimiter,
    bootstrap_relays: Arc<Vec<String>>,
    discovery_interval: Duration,
    allow_local_discovery: bool,
    mask_targets: Arc<HashSet<String>>,
    public_url: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Snapshot {
    #[serde(default)]
    accounts: Vec<AccountVault>,
    deletions: Vec<GroupDeletion>,
    #[serde(default)]
    invite_rotations: Vec<InviteRotation>,
    invites: Vec<InviteRecord>,
    events: Vec<SignedEvent>,
    #[serde(default)]
    mls_join_requests: Vec<MlsJoinRequest>,
    #[serde(default)]
    mls_removal_requests: Vec<MlsRemovalRequest>,
    #[serde(default)]
    mls_geneses: Vec<MlsGroupGenesis>,
    #[serde(default)]
    mls_epochs: Vec<MlsEpochRecord>,
}

#[derive(Serialize)]
struct Health {
    status: &'static str,
    software_version: &'static str,
    protocol_version: u16,
    accounts: usize,
    invitations: usize,
    events: usize,
    shards: u64,
    shard_bytes: u64,
    storage_limit_bytes: Option<u64>,
    shard_storage: String,
    deleted_groups: usize,
    peers: usize,
    privacy_gateway: bool,
    mask_targets: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct GroupWatchResponse {
    revision: u64,
    changed: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    presences: Vec<GroupPresence>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InsertResult {
    Inserted,
    Present,
    Deleted,
}

enum ShardInsertError {
    Full,
    Deleted,
    Storage(anyhow::Error),
}

fn resolve_server_settings(args: &Args) -> anyhow::Result<ServerSettings> {
    let mut config = RelayConfig::load(args.config.as_deref())?;
    if let Some(listen) = args.server.listen {
        config.listen = listen;
    }
    if !args.server.peer.is_empty() {
        config.peers = args.server.peer.clone();
    }
    if let Some(data) = args.server.data.clone() {
        config.data = Some(data);
    }
    if let Some(public_url) = args.server.public_url.clone() {
        config.public_url = Some(public_url);
    }
    if !args.server.mask_target.is_empty() {
        config.mask_targets = args.server.mask_target.clone();
    }
    if !args.server.bootstrap_relay.is_empty() {
        config.bootstrap_relays = args.server.bootstrap_relay.clone();
    }
    if let Some(interval) = args.server.discovery_interval_seconds {
        config.discovery_interval_seconds = interval;
    }
    if let Some(limit) = args.server.storage_limit_bytes {
        config.storage_limit_bytes = limit;
    }
    config.validate()?;
    Ok(ServerSettings {
        listen: config.listen,
        peer: config.peers,
        data: config.data,
        public_url: config.public_url,
        mask_target: config.mask_targets,
        bootstrap_relay: config.bootstrap_relays,
        discovery_interval_seconds: config.discovery_interval_seconds,
        storage_limit_bytes: config.storage_limit_bytes,
    })
}

fn settings_data_directory(settings: &ServerSettings) -> PathBuf {
    settings
        .data
        .clone()
        .unwrap_or_else(|| PathBuf::from("relay-data").join(settings.listen.port().to_string()))
}

fn settings_health_url(settings: &ServerSettings) -> String {
    let host = if settings.listen.ip().is_unspecified() {
        if settings.listen.is_ipv6() {
            "[::1]".to_owned()
        } else {
            "127.0.0.1".to_owned()
        }
    } else if settings.listen.is_ipv6() {
        format!("[{}]", settings.listen.ip())
    } else {
        settings.listen.ip().to_string()
    };
    format!("http://{host}:{}/health", settings.listen.port())
}

fn print_status(settings: &ServerSettings, json: bool) -> anyhow::Result<()> {
    let status = serde_json::json!({
        "software_version": env!("CARGO_PKG_VERSION"),
        "protocol_version": RELAY_PROTOCOL_VERSION,
        "listen": settings.listen.to_string(),
        "data": settings_data_directory(settings),
        "public_url": settings.public_url,
        "peers": settings.peer.len(),
        "mask_targets": settings.mask_target.len(),
        "bootstrap_relays": settings.bootstrap_relay.len(),
        "storage_limit_bytes": (settings.storage_limit_bytes != 0).then_some(settings.storage_limit_bytes),
        "storage_backend": std::env::var("NOISE_STORAGE_BACKEND").unwrap_or_else(|_| "local".into()),
    });
    if json {
        println!("{}", serde_json::to_string_pretty(&status)?);
    } else {
        println!(
            "noise relay {} (protocol {})",
            env!("CARGO_PKG_VERSION"),
            RELAY_PROTOCOL_VERSION
        );
        println!("listen: {}", settings.listen);
        println!("data: {}", settings_data_directory(settings).display());
        println!(
            "public URL: {}",
            settings.public_url.as_deref().unwrap_or("not configured")
        );
        println!(
            "network: {} bootstrap relay(s), {} mask target(s), {} replication peer(s)",
            settings.bootstrap_relay.len(),
            settings.mask_target.len(),
            settings.peer.len()
        );
    }
    Ok(())
}

async fn doctor(settings: &ServerSettings, json: bool) -> anyhow::Result<()> {
    let data_directory = settings_data_directory(settings);
    if !data_directory.exists() {
        bail!(
            "relay data directory {} does not exist",
            data_directory.display()
        )
    }
    let local_host = if settings.listen.ip().is_unspecified() {
        if settings.listen.is_ipv6() {
            "[::1]".to_owned()
        } else {
            "127.0.0.1".to_owned()
        }
    } else {
        settings.listen.ip().to_string()
    };
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(8))
        .build()
        .context("could not initialize relay diagnostics")?;
    let local_health_url = format!("http://{local_host}:{}/health", settings.listen.port());
    let local_health = client
        .get(&local_health_url)
        .send()
        .await
        .context("relay is not reachable on its configured listen address")?
        .error_for_status()
        .context("local relay health check failed")?
        .json::<serde_json::Value>()
        .await
        .context("local relay health response is invalid")?;
    if local_health.get("status").and_then(|value| value.as_str()) != Some("ok") {
        bail!("local relay is not healthy")
    }

    let mut public_reachable = false;
    if let Some(public_url) = settings.public_url.as_deref() {
        let descriptor = client
            .get(format!(
                "{}{}",
                public_url.trim_end_matches('/'),
                SIGNED_RELAY_DESCRIPTOR_PATH
            ))
            .send()
            .await
            .context("public relay URL is not reachable")?
            .error_for_status()
            .context("public relay descriptor request failed")?
            .json::<SignedRelayDescriptor>()
            .await
            .context("public relay descriptor is invalid")?;
        descriptor.verify_at(unix_seconds()?)?;
        if descriptor.base_url != public_url.trim_end_matches('/') {
            bail!("public relay descriptor advertises a different URL")
        }
        public_reachable = true;
    }

    let report = serde_json::json!({
        "ok": true,
        "software_version": env!("CARGO_PKG_VERSION"),
        "protocol_version": RELAY_PROTOCOL_VERSION,
        "local_health": local_health,
        "public_reachable": public_reachable,
        "data": data_directory,
    });
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "healthy: noise relay {} (protocol {})",
            env!("CARGO_PKG_VERSION"),
            RELAY_PROTOCOL_VERSION
        );
        println!("local health: {local_health_url}");
        println!(
            "public reachability: {}",
            if public_reachable {
                "verified"
            } else {
                "not configured"
            }
        );
        println!("data: {}", data_directory.display());
    }
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let parsed = Args::parse();
    match &parsed.command {
        Some(RelayCommand::Update {
            apply,
            manifest_url,
            signature_url,
            json,
        }) => {
            let settings = resolve_server_settings(&parsed)?;
            let status = update::run(update::UpdateOptions {
                manifest_url: manifest_url.clone(),
                signature_url: signature_url.clone(),
                apply: *apply,
                health_url: settings_health_url(&settings),
            })
            .await?;
            if *json {
                println!("{}", serde_json::to_string_pretty(&status)?);
            } else if status.update_available {
                println!(
                    "relay {} is available for {} (running {}){}",
                    status.latest_version,
                    status.target,
                    status.current_version,
                    if *apply { " and was installed" } else { "" }
                );
            } else {
                println!("noise relay {} is current", status.current_version);
            }
            return Ok(());
        }
        Some(RelayCommand::Status { json }) => {
            let settings = resolve_server_settings(&parsed)?;
            print_status(&settings, *json)?;
            return Ok(());
        }
        Some(RelayCommand::Doctor { json }) => {
            let settings = resolve_server_settings(&parsed)?;
            doctor(&settings, *json).await?;
            return Ok(());
        }
        None => {}
    }
    let args = resolve_server_settings(&parsed)?;
    let data_directory = settings_data_directory(&args);
    let shard_store = ShardStore::open(&data_directory)?;
    let (mut store, mut recovered) = DurableStore::open(&data_directory).await?;
    let inline_blob_count = recovered.legacy_blobs.len();
    let legacy_blob_schema = recovered.legacy_blob_schema;
    let mut legacy_blob_ids = recovered.blobs.clone();
    legacy_blob_ids.extend(
        recovered
            .legacy_blobs
            .iter()
            .map(|metadata| metadata.blob_id.clone()),
    );
    legacy_blob_ids.extend(recovered.pending_blob_deletions.iter().cloned());
    legacy_blob_ids.sort();
    legacy_blob_ids.dedup();
    if legacy_blob_schema || !legacy_blob_ids.is_empty() {
        println!(
            "discarding {} legacy full media object(s); shard storage is now mandatory",
            legacy_blob_ids.len()
        );
        for blob_id in &legacy_blob_ids {
            shard_store.delete_legacy_blob(blob_id).await?;
        }
        store.discard_all_legacy_blobs(&legacy_blob_ids).await?;
        store.reclaim_inline_blob_space().await?;
        drop(recovered);
        drop(store);
        (store, recovered) = DurableStore::open(&data_directory).await?;
        println!(
            "legacy full media removed{}",
            if inline_blob_count == 0 {
                ""
            } else {
                "; reclaimed inline Turso pages and cache"
            }
        );
    }
    shard_store.discard_legacy_local_store()?;
    recovered.blobs.clear();
    recovered.legacy_blobs.clear();
    for shard_id in std::mem::take(&mut recovered.pending_shard_deletions) {
        match shard_store.delete_shard(&shard_id).await {
            Ok(()) => store.complete_shard_deletion(&shard_id).await?,
            Err(error) => eprintln!("will retry deletion of storage shard {shard_id}: {error:#}"),
        }
    }
    let privacy = PrivacyGateway::open(&data_directory)?;
    let relay_identity = RelayIdentity::open(&data_directory)?;
    let public_url = args
        .public_url
        .map(|url| RelayDescriptor::parse(&url).map(|descriptor| descriptor.base_url))
        .transpose()?;
    let mask_targets = args
        .mask_target
        .into_iter()
        .map(|target| RelayDescriptor::parse(&target).map(|descriptor| descriptor.base_url))
        .collect::<anyhow::Result<HashSet<_>>>()?;
    if public_url
        .as_ref()
        .is_some_and(|public_url| mask_targets.contains(public_url))
    {
        bail!("a relay cannot use itself as a privacy mask target")
    }
    let peers = args
        .peer
        .into_iter()
        .map(|peer| peer.trim_end_matches('/').to_owned())
        .collect::<Vec<_>>();
    let mut bootstrap_relays = args
        .bootstrap_relay
        .iter()
        .map(|relay| RelayDescriptor::parse(relay).map(|descriptor| descriptor.base_url))
        .collect::<anyhow::Result<Vec<_>>>()?;
    bootstrap_relays.sort();
    bootstrap_relays.dedup();
    if let Some(public_url) = public_url.as_ref() {
        bootstrap_relays.retain(|relay| relay != public_url);
    }
    let allow_local_discovery = public_url
        .as_ref()
        .map(|url| RelayDescriptor::parse(url).map(|descriptor| descriptor.is_local()))
        .transpose()?
        .unwrap_or(false);
    let relay_directory = RelayDirectory::new(recovered.relay_descriptors, store.clone());
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(40))
        .build()
        .context("could not initialize relay HTTP")?;
    let state = AppState {
        accounts: Arc::new(RwLock::new(recovered.accounts)),
        invites: Arc::new(RwLock::new(recovered.invites)),
        invite_rotations: Arc::new(RwLock::new(recovered.invite_rotations)),
        events: Arc::new(RwLock::new(recovered.events)),
        mls_join_requests: Arc::new(RwLock::new(recovered.mls_join_requests)),
        mls_removal_requests: Arc::new(RwLock::new(recovered.mls_removal_requests)),
        mls_geneses: Arc::new(RwLock::new(recovered.mls_geneses)),
        mls_epochs: Arc::new(RwLock::new(recovered.mls_epochs)),
        shard_count: Arc::new(AtomicU64::new(recovered.shard_count)),
        shard_bytes: Arc::new(AtomicU64::new(recovered.shard_bytes)),
        storage_limit_bytes: args.storage_limit_bytes,
        deletions: Arc::new(RwLock::new(recovered.deletions)),
        group_changes: Arc::new(RwLock::new(HashMap::new())),
        group_presences: Arc::new(RwLock::new(HashMap::new())),
        account_changes: Arc::new(RwLock::new(HashMap::new())),
        mutations: Arc::new(Mutex::new(())),
        peers: Arc::new(peers),
        client,
        store,
        shard_store,
        privacy,
        relay_identity,
        relay_directory,
        announcement_limiter: AnnouncementLimiter::new(),
        bootstrap_relays: Arc::new(bootstrap_relays),
        discovery_interval: Duration::from_secs(args.discovery_interval_seconds.max(1)),
        allow_local_discovery,
        mask_targets: Arc::new(mask_targets),
        public_url: public_url.clone(),
    };

    tokio::spawn(anti_entropy_loop(state.clone()));
    tokio::spawn(shard_deletion_loop(state.clone()));

    let app = Router::new()
        .route("/health", get(health))
        .route("/v1/accounts", post(publish_account))
        .route("/v1/accounts/{locator}", get(get_account))
        .route("/v1/accounts/{locator}/watch/{since}", get(account_watch))
        .route("/v1/invites", post(publish_invite))
        .route("/v1/invites/{locator}", get(get_invite))
        .route("/v1/invite-rotations", post(publish_invite_rotation))
        .route("/v1/events", post(publish_event))
        .route("/v1/groups/{group_id}/events", get(group_events))
        .route("/v2/mls/join-requests", post(publish_mls_join_request))
        .route(
            "/v2/mls/groups/{group_id}/join-requests",
            get(group_mls_join_requests),
        )
        .route(
            "/v2/mls/removal-requests",
            post(publish_mls_removal_request),
        )
        .route(
            "/v2/mls/groups/{group_id}/removal-requests",
            get(group_mls_removal_requests),
        )
        .route("/v2/mls/genesis", post(publish_mls_genesis))
        .route("/v2/mls/epochs", post(publish_mls_epoch))
        .route("/v2/mls/groups/{group_id}", get(group_mls_control_log))
        .route("/v1/groups/{group_id}/watch/{since}", get(group_watch))
        .route("/v1/groups/{group_id}/presence", post(publish_presence))
        .route("/v3/shards", post(publish_shard))
        .route("/v3/shards/{shard_id}", get(get_shard).delete(delete_shard))
        .route("/v1/group-deletions", post(publish_group_deletion))
        .route("/v1/snapshot", get(snapshot))
        .route(OHTTP_KEYS_PATH, get(ohttp_keys))
        .route("/v1/relay-descriptor", get(relay_descriptor))
        .route(SIGNED_RELAY_DESCRIPTOR_PATH, get(signed_relay_descriptor))
        .route(
            RELAY_DIRECTORY_PATH,
            get(get_relay_directory).post(announce_relay_descriptor),
        )
        .route(OHTTP_GATEWAY_PATH, post(ohttp_gateway))
        .route(OHTTP_RELAY_PATH, post(ohttp_relay))
        // Relays only accept public invitations and signed, encrypted objects;
        // browser clients do not send cookies or relay-held credentials.
        .layer(DefaultBodyLimit::max(2_700_000))
        .layer(CorsLayer::permissive())
        .with_state(state.clone());

    let listener = TcpListener::bind(args.listen)
        .await
        .with_context(|| format!("could not bind relay to {}", args.listen))?;
    tokio::spawn(relay_discovery_loop(state.clone()));
    println!(
        "noise relay listening on http://{} with {} peer(s); durable data at {}",
        args.listen,
        state.peers.len(),
        state.store.path().display()
    );
    println!(
        "encrypted media storage: {}",
        state.shard_store.description()
    );
    println!("relay identity: {}", state.relay_identity.relay_id());
    println!(
        "relay discovery has {} bootstrap(s)",
        state.bootstrap_relays.len()
    );
    if let Some(public_url) = public_url {
        println!(
            "shareable private relay address: {}",
            RelayDescriptor::shareable(&public_url, state.privacy.public_config())
        );
    }
    axum::serve(listener, app).await?;
    Ok(())
}

async fn health(State(state): State<AppState>) -> Json<Health> {
    Json(Health {
        status: "ok",
        software_version: env!("CARGO_PKG_VERSION"),
        protocol_version: RELAY_PROTOCOL_VERSION,
        accounts: state
            .accounts
            .read()
            .await
            .values()
            .filter(|vault| !vault.deleted)
            .count(),
        invitations: state.invites.read().await.len(),
        events: state.events.read().await.len(),
        shards: state.shard_count.load(Ordering::Relaxed),
        shard_bytes: state.shard_bytes.load(Ordering::Relaxed),
        storage_limit_bytes: (state.storage_limit_bytes != 0).then_some(state.storage_limit_bytes),
        shard_storage: state.shard_store.description().to_owned(),
        deleted_groups: state.deletions.read().await.len(),
        peers: state.peers.len(),
        privacy_gateway: true,
        mask_targets: state.mask_targets.len(),
    })
}

fn storage_descriptor_values(state: &AppState) -> (u64, u64) {
    let capacity = state.storage_limit_bytes;
    let available = if capacity == 0 {
        u64::MAX
    } else {
        capacity.saturating_sub(state.shard_bytes.load(Ordering::Relaxed))
    };
    (capacity, available)
}

async fn publish_account(
    State(state): State<AppState>,
    Json(vault): Json<AccountVault>,
) -> Result<StatusCode, (StatusCode, String)> {
    vault
        .verify()
        .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?;
    match apply_account_vault(&state, vault.clone()).await {
        Ok(InsertResult::Inserted) => {
            tokio::spawn(gossip_account(state, vault));
        }
        Ok(InsertResult::Present) => {}
        Ok(InsertResult::Deleted) => unreachable!("account vaults use signed tombstones"),
        Err(error) => {
            eprintln!("account vault update rejected: {error:#}");
            return Err((
                StatusCode::CONFLICT,
                "account vault revision conflicts".into(),
            ));
        }
    }
    Ok(StatusCode::ACCEPTED)
}

async fn get_account(
    State(state): State<AppState>,
    Path(locator): Path<String>,
) -> Result<Json<AccountVault>, StatusCode> {
    match state.accounts.read().await.get(&locator).cloned() {
        Some(vault) if vault.deleted => Err(StatusCode::GONE),
        Some(vault) => Ok(Json(vault)),
        None => Err(StatusCode::NOT_FOUND),
    }
}

async fn publish_invite(
    State(state): State<AppState>,
    Json(record): Json<InviteRecord>,
) -> Result<StatusCode, (StatusCode, String)> {
    record
        .verify()
        .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?;
    match insert_invite(&state, record.clone())
        .await
        .map_err(storage_error)?
    {
        InsertResult::Inserted => {
            tokio::spawn(gossip_invite(state, record));
        }
        InsertResult::Deleted => {
            return Err((StatusCode::GONE, "group has been deleted".into()));
        }
        InsertResult::Present => {}
    }
    Ok(StatusCode::ACCEPTED)
}

async fn get_invite(
    State(state): State<AppState>,
    Path(locator): Path<String>,
) -> Result<Json<InviteRecord>, StatusCode> {
    state
        .invites
        .read()
        .await
        .get(&locator)
        .cloned()
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

async fn publish_invite_rotation(
    State(state): State<AppState>,
    Json(rotation): Json<InviteRotation>,
) -> Result<StatusCode, (StatusCode, String)> {
    rotation
        .verify()
        .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?;
    match apply_invite_rotation(&state, rotation.clone())
        .await
        .map_err(storage_error)?
    {
        InsertResult::Inserted => {
            tokio::spawn(gossip_invite_rotation(state, rotation));
        }
        InsertResult::Deleted => {
            return Err((StatusCode::GONE, "group has been deleted".into()));
        }
        InsertResult::Present => {}
    }
    Ok(StatusCode::ACCEPTED)
}

async fn publish_event(
    State(state): State<AppState>,
    Json(event): Json<SignedEvent>,
) -> Result<StatusCode, (StatusCode, String)> {
    event
        .verify()
        .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?;
    validate_mls_event(&state, &event)
        .await
        .map_err(|error| (StatusCode::CONFLICT, error.to_string()))?;
    match insert_event(&state, event.clone())
        .await
        .map_err(storage_error)?
    {
        InsertResult::Inserted => {
            tokio::spawn(gossip_event(state, event));
        }
        InsertResult::Deleted => {
            return Err((StatusCode::GONE, "group has been deleted".into()));
        }
        InsertResult::Present => {}
    }
    Ok(StatusCode::ACCEPTED)
}

async fn group_events(
    State(state): State<AppState>,
    Path(group_id): Path<String>,
) -> Result<Json<Vec<SignedEvent>>, StatusCode> {
    if state.deletions.read().await.contains_key(&group_id) {
        return Err(StatusCode::GONE);
    }
    let mut events = state
        .events
        .read()
        .await
        .values()
        .filter(|event| event.group_id == group_id)
        .cloned()
        .collect::<Vec<_>>();
    events.sort_by(|left, right| {
        left.created_at_millis
            .cmp(&right.created_at_millis)
            .then_with(|| left.event_id.cmp(&right.event_id))
    });
    Ok(Json(events))
}

async fn publish_mls_join_request(
    State(state): State<AppState>,
    Json(request): Json<MlsJoinRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    request
        .verify()
        .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?;
    match insert_mls_join_request(&state, request.clone())
        .await
        .map_err(storage_error)?
    {
        InsertResult::Inserted => {
            tokio::spawn(gossip_mls_join_request(state, request));
        }
        InsertResult::Deleted => {
            return Err((StatusCode::GONE, "group has been deleted".into()));
        }
        InsertResult::Present => {}
    }
    Ok(StatusCode::ACCEPTED)
}

async fn group_mls_join_requests(
    State(state): State<AppState>,
    Path(group_id): Path<String>,
) -> Result<Json<Vec<MlsJoinRequest>>, StatusCode> {
    if state.deletions.read().await.contains_key(&group_id) {
        return Err(StatusCode::GONE);
    }
    let mut requests = state
        .mls_join_requests
        .read()
        .await
        .values()
        .filter(|request| request.group_id == group_id)
        .cloned()
        .collect::<Vec<_>>();
    requests.sort_by(|left, right| {
        left.created_at_millis
            .cmp(&right.created_at_millis)
            .then_with(|| left.request_id.cmp(&right.request_id))
    });
    Ok(Json(requests))
}

async fn publish_mls_removal_request(
    State(state): State<AppState>,
    Json(request): Json<MlsRemovalRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    request
        .verify()
        .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?;
    match insert_mls_removal_request(&state, request.clone())
        .await
        .map_err(storage_error)?
    {
        InsertResult::Inserted => {
            tokio::spawn(gossip_mls_removal_request(state, request));
        }
        InsertResult::Deleted => {
            return Err((StatusCode::GONE, "group has been deleted".into()));
        }
        InsertResult::Present => {}
    }
    Ok(StatusCode::ACCEPTED)
}

async fn group_mls_removal_requests(
    State(state): State<AppState>,
    Path(group_id): Path<String>,
) -> Result<Json<Vec<MlsRemovalRequest>>, StatusCode> {
    if state.deletions.read().await.contains_key(&group_id) {
        return Err(StatusCode::GONE);
    }
    let mut requests = state
        .mls_removal_requests
        .read()
        .await
        .values()
        .filter(|request| request.group_id == group_id)
        .cloned()
        .collect::<Vec<_>>();
    requests.sort_by(|left, right| {
        left.created_at_millis
            .cmp(&right.created_at_millis)
            .then_with(|| left.request_id.cmp(&right.request_id))
    });
    Ok(Json(requests))
}

async fn publish_mls_genesis(
    State(state): State<AppState>,
    Json(genesis): Json<MlsGroupGenesis>,
) -> Result<StatusCode, (StatusCode, String)> {
    genesis
        .verify()
        .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?;
    match insert_mls_genesis(&state, genesis.clone())
        .await
        .map_err(control_storage_error)?
    {
        InsertResult::Inserted => {
            tokio::spawn(gossip_mls_genesis(state, genesis));
        }
        InsertResult::Deleted => {
            return Err((StatusCode::GONE, "group has been deleted".into()));
        }
        InsertResult::Present => {}
    }
    Ok(StatusCode::ACCEPTED)
}

async fn publish_mls_epoch(
    State(state): State<AppState>,
    Json(record): Json<MlsEpochRecord>,
) -> Result<StatusCode, (StatusCode, String)> {
    record
        .verify()
        .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?;
    match insert_mls_epoch(&state, record.clone())
        .await
        .map_err(control_storage_error)?
    {
        InsertResult::Inserted => {
            tokio::spawn(gossip_mls_epoch(state, record));
        }
        InsertResult::Deleted => {
            return Err((StatusCode::GONE, "group has been deleted".into()));
        }
        InsertResult::Present => {}
    }
    Ok(StatusCode::ACCEPTED)
}

async fn group_mls_control_log(
    State(state): State<AppState>,
    Path(group_id): Path<String>,
) -> Result<Json<MlsControlLog>, StatusCode> {
    if state.deletions.read().await.contains_key(&group_id) {
        return Err(StatusCode::GONE);
    }
    mls_control_log(&state, &group_id)
        .await
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

async fn group_watch(
    State(state): State<AppState>,
    Path((group_id, since)): Path<(String, String)>,
) -> Result<Json<GroupWatchResponse>, StatusCode> {
    let since = parse_watch_revision(&since).ok_or(StatusCode::BAD_REQUEST)?;
    wait_for_group_change(&state, &group_id, since)
        .await
        .map(Json)
}

async fn publish_presence(
    State(state): State<AppState>,
    Path(group_id): Path<String>,
    Json(presence): Json<GroupPresence>,
) -> Result<StatusCode, StatusCode> {
    store_group_presence(&state, &group_id, presence).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn account_watch(
    State(state): State<AppState>,
    Path((locator, since)): Path<(String, String)>,
) -> Result<Json<GroupWatchResponse>, StatusCode> {
    let since = parse_watch_revision(&since).ok_or(StatusCode::BAD_REQUEST)?;
    wait_for_account_change(&state, &locator, since)
        .await
        .map(Json)
}

async fn store_group_presence(
    state: &AppState,
    group_id: &str,
    presence: GroupPresence,
) -> Result<(), StatusCode> {
    if group_id.len() != 64
        || !group_id.bytes().all(|byte| byte.is_ascii_hexdigit())
        || presence.group_id != group_id
        || presence.member_tag_base64.len() != 43
        || presence.member_nonce_base64.len() != 32
        || presence.member_ciphertext_base64.len() > 128
        || presence.signature_base64.len() != 86
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    if state.deletions.read().await.contains_key(group_id) {
        return Err(StatusCode::GONE);
    }
    if !state
        .events
        .read()
        .await
        .values()
        .any(|event| event.group_id == group_id)
    {
        return Err(StatusCode::NOT_FOUND);
    }
    let now = unix_millis().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if presence.expires_at_millis <= now
        || presence.expires_at_millis > now.saturating_add(MAX_GROUP_PRESENCE_MILLIS)
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    let mut groups = state.group_presences.write().await;
    let group = groups.entry(group_id.to_owned()).or_default();
    group.retain(|_, current| {
        current
            .expires_at_millis
            .saturating_add(RECENT_GROUP_PRESENCE_MILLIS)
            > now
    });
    if group.len() >= MAX_GROUP_PRESENCES && !group.contains_key(&presence.member_tag_base64) {
        return Err(StatusCode::TOO_MANY_REQUESTS);
    }
    group.insert(presence.member_tag_base64.clone(), presence);
    Ok(())
}

async fn active_group_presences(state: &AppState, group_id: &str) -> Vec<GroupPresence> {
    let Ok(now) = unix_millis() else {
        return Vec::new();
    };
    let mut groups = state.group_presences.write().await;
    let Some(group) = groups.get_mut(group_id) else {
        return Vec::new();
    };
    group.retain(|_, presence| {
        presence
            .expires_at_millis
            .saturating_add(RECENT_GROUP_PRESENCE_MILLIS)
            > now
    });
    let mut active = group.values().cloned().collect::<Vec<_>>();
    active.sort_by(|left, right| left.member_tag_base64.cmp(&right.member_tag_base64));
    if group.is_empty() {
        groups.remove(group_id);
    }
    active
}

async fn publish_shard(
    State(state): State<AppState>,
    Json(shard): Json<StorageShard>,
) -> Result<StatusCode, (StatusCode, String)> {
    shard
        .verify()
        .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?;
    match insert_shard(&state, shard).await {
        Ok(true) => Ok(StatusCode::CREATED),
        Ok(false) => Ok(StatusCode::ACCEPTED),
        Err(ShardInsertError::Full) => Err((
            StatusCode::INSUFFICIENT_STORAGE,
            "relay storage allocation is full".into(),
        )),
        Err(ShardInsertError::Deleted) => {
            Err((StatusCode::GONE, "storage shard was deleted".into()))
        }
        Err(ShardInsertError::Storage(error)) => Err(storage_error(error)),
    }
}

async fn publish_group_deletion(
    State(state): State<AppState>,
    Json(deletion): Json<GroupDeletion>,
) -> Result<StatusCode, (StatusCode, String)> {
    deletion
        .verify()
        .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?;
    if apply_group_deletion(&state, deletion.clone())
        .await
        .map_err(storage_error)?
    {
        tokio::spawn(gossip_group_deletion(state, deletion));
    }
    Ok(StatusCode::ACCEPTED)
}

async fn get_shard(
    State(state): State<AppState>,
    Path(shard_id): Path<String>,
) -> Result<Json<StorageShard>, StatusCode> {
    let metadata = match state.store.shard_metadata(&shard_id).await {
        Ok(Some(metadata)) => metadata,
        Ok(None) => return Err(StatusCode::NOT_FOUND),
        Err(error) => {
            eprintln!("could not read storage shard metadata for {shard_id}: {error:#}");
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };
    match state
        .shard_store
        .get_shard(
            &shard_id,
            &metadata.payload_hash,
            &metadata.delete_token_hash,
            metadata.byte_length,
        )
        .await
    {
        Ok(Some(shard)) => Ok(Json(shard)),
        Ok(None) => {
            eprintln!("indexed storage shard {shard_id} is missing from object storage");
            Err(StatusCode::NOT_FOUND)
        }
        Err(error) => {
            eprintln!("could not read storage shard {shard_id}: {error:#}");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

async fn delete_shard(
    State(state): State<AppState>,
    Path(shard_id): Path<String>,
    Json(deletion): Json<ShardDeletion>,
) -> Result<StatusCode, (StatusCode, String)> {
    let metadata = state
        .store
        .shard_metadata(&shard_id)
        .await
        .map_err(storage_error)?
        .ok_or((StatusCode::NOT_FOUND, "storage shard is unavailable".into()))?;
    let token = STANDARD_NO_PAD
        .decode(&deletion.delete_token_base64)
        .map_err(|_| {
            (
                StatusCode::BAD_REQUEST,
                "invalid shard deletion token".into(),
            )
        })?;
    if token.len() != 32 || blake3::hash(&token).to_hex().as_str() != metadata.delete_token_hash {
        return Err((
            StatusCode::FORBIDDEN,
            "shard deletion token was rejected".into(),
        ));
    }
    erase_shard(&state, &shard_id, &metadata)
        .await
        .map_err(storage_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn ohttp_keys(State(state): State<AppState>) -> Response {
    (
        [(CONTENT_TYPE, OHTTP_KEYS_MEDIA_TYPE)],
        Bytes::copy_from_slice(state.privacy.public_config_list()),
    )
        .into_response()
}

async fn relay_descriptor(State(state): State<AppState>) -> Response {
    let Some(public_url) = state.public_url.as_deref() else {
        return (StatusCode::NOT_FOUND, "relay public URL is not configured").into_response();
    };
    (
        StatusCode::OK,
        [(CONTENT_TYPE, "text/plain; charset=utf-8")],
        RelayDescriptor::shareable(public_url, state.privacy.public_config()),
    )
        .into_response()
}

async fn signed_relay_descriptor(State(state): State<AppState>) -> Response {
    let Some(public_url) = state.public_url.as_deref() else {
        return (StatusCode::NOT_FOUND, "relay public URL is not configured").into_response();
    };
    let now = match unix_seconds() {
        Ok(now) => now,
        Err(error) => {
            eprintln!("could not read system clock: {error:#}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "system clock is before the Unix epoch",
            )
                .into_response();
        }
    };
    let (capacity, available) = storage_descriptor_values(&state);
    match state.relay_identity.signed_descriptor(
        public_url,
        state.privacy.public_config(),
        capacity,
        available,
        now,
    ) {
        Ok(descriptor) => Json(descriptor).into_response(),
        Err(error) => {
            eprintln!("could not create signed relay descriptor: {error:#}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "could not create signed relay descriptor",
            )
                .into_response()
        }
    }
}

async fn get_relay_directory(State(state): State<AppState>) -> Response {
    let Some(public_url) = state.public_url.as_deref() else {
        return (StatusCode::NOT_FOUND, "relay public URL is not configured").into_response();
    };
    let now = match unix_seconds() {
        Ok(now) => now,
        Err(error) => {
            eprintln!("could not read system clock: {error:#}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "system clock is before the Unix epoch",
            )
                .into_response();
        }
    };
    let (capacity, available) = storage_descriptor_values(&state);
    let own = match state.relay_identity.signed_descriptor(
        public_url,
        state.privacy.public_config(),
        capacity,
        available,
        now,
    ) {
        Ok(descriptor) => descriptor,
        Err(error) => {
            eprintln!("could not create signed relay descriptor: {error:#}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "could not create signed relay descriptor",
            )
                .into_response();
        }
    };
    let mut descriptors = state.relay_directory.list(now).await;
    descriptors.retain(|descriptor| descriptor.relay_id != own.relay_id);
    descriptors.push(own);
    descriptors.sort_by(|left, right| left.relay_id.cmp(&right.relay_id));
    Json(descriptors).into_response()
}

async fn announce_relay_descriptor(
    State(state): State<AppState>,
    Json(announced): Json<SignedRelayDescriptor>,
) -> Response {
    let now = match unix_seconds() {
        Ok(now) => now,
        Err(error) => {
            eprintln!("could not read system clock: {error:#}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "system clock is before the Unix epoch",
            )
                .into_response();
        }
    };
    if announced.relay_id == state.relay_identity.relay_id() {
        return StatusCode::NO_CONTENT.into_response();
    }
    let Some(_permit) = state.announcement_limiter.begin().await else {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            "relay announcement verification is busy",
        )
            .into_response();
    };
    let reachable =
        match verify_relay_reachability(&announced, now, state.allow_local_discovery).await {
            Ok(descriptor) => descriptor,
            Err(_) => {
                return (
                    StatusCode::BAD_GATEWAY,
                    "relay could not prove public reachability",
                )
                    .into_response();
            }
        };
    match state.relay_directory.insert(reachable, now).await {
        Ok(_) => StatusCode::ACCEPTED.into_response(),
        Err(error) => {
            eprintln!("could not store relay announcement: {error:#}");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "relay directory is unavailable",
            )
                .into_response()
        }
    }
}

async fn ohttp_gateway(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    if !has_media_type(&headers, OHTTP_REQUEST_MEDIA_TYPE) {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "expected an oblivious HTTP request",
        )
            .into_response();
    }
    let Ok((request, response_context)) = state.privacy.decapsulate(&body) else {
        return (StatusCode::BAD_REQUEST, "invalid oblivious request").into_response();
    };
    let (status, response_body) = dispatch_private_request(&state, request).await;
    let Ok(response) = encode_response(status.as_u16(), &response_body) else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "could not encode response",
        )
            .into_response();
    };
    let Ok(encrypted_response) = response_context.encapsulate(&response) else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "could not seal response").into_response();
    };
    (
        StatusCode::OK,
        [(CONTENT_TYPE, OHTTP_RESPONSE_MEDIA_TYPE)],
        encrypted_response,
    )
        .into_response()
}

async fn ohttp_relay(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    if !has_media_type(&headers, OHTTP_REQUEST_MEDIA_TYPE) {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "expected an oblivious HTTP request",
        )
            .into_response();
    }
    let Some(target) = headers
        .get(GATEWAY_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.trim_end_matches('/'))
    else {
        return (StatusCode::BAD_REQUEST, "privacy gateway is missing").into_response();
    };
    let configured_target = state.mask_targets.contains(target);
    let discovered_target = if configured_target {
        false
    } else {
        let Ok(now) = unix_seconds() else {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "system clock is before the Unix epoch",
            )
                .into_response();
        };
        state
            .relay_directory
            .descriptor_for_base_url(target, now)
            .await
            .is_some()
    };
    if !configured_target && !discovered_target {
        return (StatusCode::FORBIDDEN, "privacy gateway is not allowed").into_response();
    }
    let endpoint = format!("{target}{OHTTP_GATEWAY_PATH}");
    let client = if configured_target {
        state.client.clone()
    } else {
        let Ok(client) = client_for_verified_relay(target, state.allow_local_discovery).await
        else {
            return (StatusCode::BAD_GATEWAY, "privacy gateway is unavailable").into_response();
        };
        client
    };
    let Ok(response) = client
        .post(endpoint)
        .header(CONTENT_TYPE.as_str(), OHTTP_REQUEST_MEDIA_TYPE)
        .body(body)
        .send()
        .await
    else {
        return (StatusCode::BAD_GATEWAY, "privacy gateway is unavailable").into_response();
    };
    if response
        .content_length()
        .is_some_and(|length| length > 2_600_000)
    {
        return (
            StatusCode::BAD_GATEWAY,
            "privacy gateway response is too large",
        )
            .into_response();
    }
    let status =
        StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let Ok(response_body) = response.bytes().await else {
        return (StatusCode::BAD_GATEWAY, "privacy gateway response failed").into_response();
    };
    if response_body.len() > 2_600_000 {
        return (
            StatusCode::BAD_GATEWAY,
            "privacy gateway response is too large",
        )
            .into_response();
    }
    let media_type = if status.is_success() {
        OHTTP_RESPONSE_MEDIA_TYPE
    } else {
        "text/plain"
    };
    (status, [(CONTENT_TYPE, media_type)], response_body).into_response()
}

async fn dispatch_private_request(
    state: &AppState,
    request: PlainRequest,
) -> (StatusCode, Vec<u8>) {
    if request.method == "GET"
        && let Some((locator, revision)) = parse_private_account_watch_path(&request.path)
    {
        let Some(since) = parse_watch_revision(revision) else {
            return private_error(StatusCode::BAD_REQUEST, "invalid account revision");
        };
        return match wait_for_account_change(state, locator, since).await {
            Ok(response) => private_json(StatusCode::OK, &response),
            Err(StatusCode::GONE) => private_error(StatusCode::GONE, "account was deleted"),
            Err(StatusCode::NOT_FOUND) => private_error(StatusCode::NOT_FOUND, "nothing here"),
            Err(status) => private_error(status, "account watch failed"),
        };
    }
    if request.method == "GET"
        && let Some((group_id, revision)) = parse_private_watch_path(&request.path)
    {
        let Some(since) = parse_watch_revision(revision) else {
            return private_error(StatusCode::BAD_REQUEST, "invalid group revision");
        };
        return match wait_for_group_change(state, group_id, since).await {
            Ok(response) => private_json(StatusCode::OK, &response),
            Err(StatusCode::GONE) => private_error(StatusCode::GONE, "group has been deleted"),
            Err(StatusCode::NOT_FOUND) => private_error(StatusCode::NOT_FOUND, "nothing here"),
            Err(status) => private_error(status, "group watch failed"),
        };
    }
    if request.method == "POST"
        && let Some(group_id) = parse_private_presence_path(&request.path)
    {
        let Ok(presence) = serde_json::from_slice::<GroupPresence>(&request.body) else {
            return private_error(StatusCode::BAD_REQUEST, "invalid group presence");
        };
        return match store_group_presence(state, group_id, presence).await {
            Ok(()) => (StatusCode::NO_CONTENT, Vec::new()),
            Err(StatusCode::GONE) => private_error(StatusCode::GONE, "group has been deleted"),
            Err(StatusCode::NOT_FOUND) => private_error(StatusCode::NOT_FOUND, "nothing here"),
            Err(StatusCode::TOO_MANY_REQUESTS) => {
                private_error(StatusCode::TOO_MANY_REQUESTS, "group presence is full")
            }
            Err(status) => private_error(status, "group presence was rejected"),
        };
    }

    match (request.method.as_str(), request.path.as_str()) {
        ("POST", "/v1/accounts") => {
            let Ok(vault) = serde_json::from_slice::<AccountVault>(&request.body) else {
                return private_error(StatusCode::BAD_REQUEST, "invalid account vault");
            };
            if let Err(error) = vault.verify() {
                return private_error(StatusCode::BAD_REQUEST, &error.to_string());
            }
            match apply_account_vault(state, vault.clone()).await {
                Ok(InsertResult::Inserted) => {
                    tokio::spawn(gossip_account(state.clone(), vault));
                    (StatusCode::ACCEPTED, Vec::new())
                }
                Ok(InsertResult::Present) => (StatusCode::ACCEPTED, Vec::new()),
                Ok(InsertResult::Deleted) => unreachable!("account vaults use signed tombstones"),
                Err(error) => {
                    eprintln!("account vault update rejected: {error:#}");
                    private_error(StatusCode::CONFLICT, "account vault revision conflicts")
                }
            }
        }
        ("POST", "/v1/invites") => {
            let Ok(record) = serde_json::from_slice::<InviteRecord>(&request.body) else {
                return private_error(StatusCode::BAD_REQUEST, "invalid invitation");
            };
            if let Err(error) = record.verify() {
                return private_error(StatusCode::BAD_REQUEST, &error.to_string());
            }
            match insert_invite(state, record.clone()).await {
                Ok(InsertResult::Inserted) => {
                    tokio::spawn(gossip_invite(state.clone(), record));
                    (StatusCode::ACCEPTED, Vec::new())
                }
                Ok(InsertResult::Present) => (StatusCode::ACCEPTED, Vec::new()),
                Ok(InsertResult::Deleted) => {
                    private_error(StatusCode::GONE, "group has been deleted")
                }
                Err(error) => {
                    eprintln!("relay storage error: {error:#}");
                    private_error(StatusCode::INTERNAL_SERVER_ERROR, "storage failed")
                }
            }
        }
        ("POST", "/v1/invite-rotations") => {
            let Ok(rotation) = serde_json::from_slice::<InviteRotation>(&request.body) else {
                return private_error(StatusCode::BAD_REQUEST, "invalid invite rotation");
            };
            if let Err(error) = rotation.verify() {
                return private_error(StatusCode::BAD_REQUEST, &error.to_string());
            }
            match apply_invite_rotation(state, rotation.clone()).await {
                Ok(InsertResult::Inserted) => {
                    tokio::spawn(gossip_invite_rotation(state.clone(), rotation));
                    (StatusCode::ACCEPTED, Vec::new())
                }
                Ok(InsertResult::Present) => (StatusCode::ACCEPTED, Vec::new()),
                Ok(InsertResult::Deleted) => {
                    private_error(StatusCode::GONE, "group has been deleted")
                }
                Err(error) => {
                    eprintln!("relay storage error: {error:#}");
                    private_error(StatusCode::INTERNAL_SERVER_ERROR, "storage failed")
                }
            }
        }
        ("POST", "/v1/events") => {
            let Ok(event) = serde_json::from_slice::<SignedEvent>(&request.body) else {
                return private_error(StatusCode::BAD_REQUEST, "invalid event");
            };
            if let Err(error) = event.verify() {
                return private_error(StatusCode::BAD_REQUEST, &error.to_string());
            }
            if let Err(error) = validate_mls_event(state, &event).await {
                return private_error(StatusCode::CONFLICT, &error.to_string());
            }
            match insert_event(state, event.clone()).await {
                Ok(InsertResult::Inserted) => {
                    tokio::spawn(gossip_event(state.clone(), event));
                    (StatusCode::ACCEPTED, Vec::new())
                }
                Ok(InsertResult::Present) => (StatusCode::ACCEPTED, Vec::new()),
                Ok(InsertResult::Deleted) => {
                    private_error(StatusCode::GONE, "group has been deleted")
                }
                Err(error) => {
                    eprintln!("relay storage error: {error:#}");
                    private_error(StatusCode::INTERNAL_SERVER_ERROR, "storage failed")
                }
            }
        }
        ("POST", "/v2/mls/join-requests") => {
            let Ok(join_request) = serde_json::from_slice::<MlsJoinRequest>(&request.body) else {
                return private_error(StatusCode::BAD_REQUEST, "invalid MLS join request");
            };
            if let Err(error) = join_request.verify() {
                return private_error(StatusCode::BAD_REQUEST, &error.to_string());
            }
            match insert_mls_join_request(state, join_request.clone()).await {
                Ok(InsertResult::Inserted) => {
                    tokio::spawn(gossip_mls_join_request(state.clone(), join_request));
                    (StatusCode::ACCEPTED, Vec::new())
                }
                Ok(InsertResult::Present) => (StatusCode::ACCEPTED, Vec::new()),
                Ok(InsertResult::Deleted) => {
                    private_error(StatusCode::GONE, "group has been deleted")
                }
                Err(error) => {
                    eprintln!("MLS join request rejected: {error:#}");
                    private_error(StatusCode::CONFLICT, "MLS join request was rejected")
                }
            }
        }
        ("POST", "/v2/mls/removal-requests") => {
            let Ok(removal_request) = serde_json::from_slice::<MlsRemovalRequest>(&request.body)
            else {
                return private_error(StatusCode::BAD_REQUEST, "invalid MLS removal request");
            };
            if let Err(error) = removal_request.verify() {
                return private_error(StatusCode::BAD_REQUEST, &error.to_string());
            }
            match insert_mls_removal_request(state, removal_request.clone()).await {
                Ok(InsertResult::Inserted) => {
                    tokio::spawn(gossip_mls_removal_request(state.clone(), removal_request));
                    (StatusCode::ACCEPTED, Vec::new())
                }
                Ok(InsertResult::Present) => (StatusCode::ACCEPTED, Vec::new()),
                Ok(InsertResult::Deleted) => {
                    private_error(StatusCode::GONE, "group has been deleted")
                }
                Err(error) => {
                    eprintln!("MLS removal request rejected: {error:#}");
                    private_error(StatusCode::CONFLICT, "MLS removal request was rejected")
                }
            }
        }
        ("POST", "/v2/mls/genesis") => {
            let Ok(genesis) = serde_json::from_slice::<MlsGroupGenesis>(&request.body) else {
                return private_error(StatusCode::BAD_REQUEST, "invalid MLS genesis");
            };
            if let Err(error) = genesis.verify() {
                return private_error(StatusCode::BAD_REQUEST, &error.to_string());
            }
            match insert_mls_genesis(state, genesis.clone()).await {
                Ok(InsertResult::Inserted) => {
                    tokio::spawn(gossip_mls_genesis(state.clone(), genesis));
                    (StatusCode::ACCEPTED, Vec::new())
                }
                Ok(InsertResult::Present) => (StatusCode::ACCEPTED, Vec::new()),
                Ok(InsertResult::Deleted) => {
                    private_error(StatusCode::GONE, "group has been deleted")
                }
                Err(error) => {
                    eprintln!("MLS genesis rejected: {error:#}");
                    private_error(StatusCode::CONFLICT, "MLS genesis conflicts")
                }
            }
        }
        ("POST", "/v2/mls/epochs") => {
            let Ok(epoch) = serde_json::from_slice::<MlsEpochRecord>(&request.body) else {
                return private_error(StatusCode::BAD_REQUEST, "invalid MLS epoch");
            };
            if let Err(error) = epoch.verify() {
                return private_error(StatusCode::BAD_REQUEST, &error.to_string());
            }
            match insert_mls_epoch(state, epoch.clone()).await {
                Ok(InsertResult::Inserted) => {
                    tokio::spawn(gossip_mls_epoch(state.clone(), epoch));
                    (StatusCode::ACCEPTED, Vec::new())
                }
                Ok(InsertResult::Present) => (StatusCode::ACCEPTED, Vec::new()),
                Ok(InsertResult::Deleted) => {
                    private_error(StatusCode::GONE, "group has been deleted")
                }
                Err(error) => {
                    eprintln!("MLS epoch rejected: {error:#}");
                    private_error(
                        StatusCode::CONFLICT,
                        "MLS control head changed; fetch the latest epoch and retry",
                    )
                }
            }
        }
        ("POST", "/v3/shards") => {
            let Ok(shard) = serde_json::from_slice::<StorageShard>(&request.body) else {
                return private_error(StatusCode::BAD_REQUEST, "invalid storage shard");
            };
            if let Err(error) = shard.verify() {
                return private_error(StatusCode::BAD_REQUEST, &error.to_string());
            }
            match insert_shard(state, shard).await {
                Ok(true) => (StatusCode::CREATED, Vec::new()),
                Ok(false) => (StatusCode::ACCEPTED, Vec::new()),
                Err(ShardInsertError::Full) => {
                    private_error(StatusCode::INSUFFICIENT_STORAGE, "relay storage is full")
                }
                Err(ShardInsertError::Deleted) => {
                    private_error(StatusCode::GONE, "storage shard was deleted")
                }
                Err(ShardInsertError::Storage(error)) => {
                    eprintln!("relay storage error: {error:#}");
                    private_error(StatusCode::INTERNAL_SERVER_ERROR, "storage failed")
                }
            }
        }
        ("POST", "/v1/group-deletions") => {
            let Ok(deletion) = serde_json::from_slice::<GroupDeletion>(&request.body) else {
                return private_error(StatusCode::BAD_REQUEST, "invalid group deletion");
            };
            if let Err(error) = deletion.verify() {
                return private_error(StatusCode::BAD_REQUEST, &error.to_string());
            }
            match apply_group_deletion(state, deletion.clone()).await {
                Ok(true) => {
                    tokio::spawn(gossip_group_deletion(state.clone(), deletion));
                    (StatusCode::ACCEPTED, Vec::new())
                }
                Ok(false) => (StatusCode::ACCEPTED, Vec::new()),
                Err(error) => {
                    eprintln!("relay storage error: {error:#}");
                    private_error(StatusCode::INTERNAL_SERVER_ERROR, "storage failed")
                }
            }
        }
        ("GET", path) if path.starts_with("/v1/invites/") => {
            let locator = path.trim_start_matches("/v1/invites/");
            match state.invites.read().await.get(locator).cloned() {
                Some(record) => private_json(StatusCode::OK, &record),
                None => private_error(StatusCode::NOT_FOUND, "nothing here"),
            }
        }
        ("GET", path) if path.starts_with("/v1/accounts/") => {
            let locator = path.trim_start_matches("/v1/accounts/");
            match state.accounts.read().await.get(locator).cloned() {
                Some(vault) if vault.deleted => {
                    private_error(StatusCode::GONE, "account was deleted")
                }
                Some(vault) => private_json(StatusCode::OK, &vault),
                None => private_error(StatusCode::NOT_FOUND, "nothing here"),
            }
        }
        ("GET", path)
            if path.starts_with("/v2/mls/groups/") && path.ends_with("/join-requests") =>
        {
            let group_id = path
                .trim_start_matches("/v2/mls/groups/")
                .trim_end_matches("/join-requests");
            if state.deletions.read().await.contains_key(group_id) {
                return private_error(StatusCode::GONE, "group has been deleted");
            }
            let mut requests = state
                .mls_join_requests
                .read()
                .await
                .values()
                .filter(|request| request.group_id == group_id)
                .cloned()
                .collect::<Vec<_>>();
            requests.sort_by(|left, right| {
                left.created_at_millis
                    .cmp(&right.created_at_millis)
                    .then_with(|| left.request_id.cmp(&right.request_id))
            });
            private_json(StatusCode::OK, &requests)
        }
        ("GET", path)
            if path.starts_with("/v2/mls/groups/") && path.ends_with("/removal-requests") =>
        {
            let group_id = path
                .trim_start_matches("/v2/mls/groups/")
                .trim_end_matches("/removal-requests");
            if state.deletions.read().await.contains_key(group_id) {
                return private_error(StatusCode::GONE, "group has been deleted");
            }
            let mut requests = state
                .mls_removal_requests
                .read()
                .await
                .values()
                .filter(|request| request.group_id == group_id)
                .cloned()
                .collect::<Vec<_>>();
            requests.sort_by(|left, right| {
                left.created_at_millis
                    .cmp(&right.created_at_millis)
                    .then_with(|| left.request_id.cmp(&right.request_id))
            });
            private_json(StatusCode::OK, &requests)
        }
        ("GET", path) if path.starts_with("/v2/mls/groups/") => {
            let group_id = path.trim_start_matches("/v2/mls/groups/");
            if state.deletions.read().await.contains_key(group_id) {
                return private_error(StatusCode::GONE, "group has been deleted");
            }
            match mls_control_log(state, group_id).await {
                Some(log) => private_json(StatusCode::OK, &log),
                None => private_error(StatusCode::NOT_FOUND, "nothing here"),
            }
        }
        ("GET", path) if path.starts_with("/v3/shards/") => {
            let shard_id = path.trim_start_matches("/v3/shards/");
            let metadata = match state.store.shard_metadata(shard_id).await {
                Ok(Some(metadata)) => metadata,
                Ok(None) => {
                    return private_error(StatusCode::NOT_FOUND, "shard is unavailable");
                }
                Err(error) => {
                    eprintln!("could not read shard metadata: {error:#}");
                    return private_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "shard storage is unavailable",
                    );
                }
            };
            match state
                .shard_store
                .get_shard(
                    shard_id,
                    &metadata.payload_hash,
                    &metadata.delete_token_hash,
                    metadata.byte_length,
                )
                .await
            {
                Ok(Some(shard)) => private_json(StatusCode::OK, &shard),
                Ok(None) => {
                    eprintln!("indexed storage shard {shard_id} is missing from object storage");
                    private_error(StatusCode::NOT_FOUND, "shard is unavailable")
                }
                Err(error) => {
                    eprintln!("could not read storage shard {shard_id}: {error:#}");
                    private_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "shard storage is unavailable",
                    )
                }
            }
        }
        ("DELETE", path) if path.starts_with("/v3/shards/") => {
            let shard_id = path.trim_start_matches("/v3/shards/");
            let Ok(deletion) = serde_json::from_slice::<ShardDeletion>(&request.body) else {
                return private_error(StatusCode::BAD_REQUEST, "invalid shard deletion");
            };
            let metadata = match state.store.shard_metadata(shard_id).await {
                Ok(Some(metadata)) => metadata,
                Ok(None) => {
                    return private_error(StatusCode::NOT_FOUND, "shard is unavailable");
                }
                Err(error) => {
                    eprintln!("could not read shard metadata: {error:#}");
                    return private_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "shard storage is unavailable",
                    );
                }
            };
            let Ok(token) = STANDARD_NO_PAD.decode(&deletion.delete_token_base64) else {
                return private_error(StatusCode::BAD_REQUEST, "invalid shard deletion token");
            };
            if token.len() != 32
                || blake3::hash(&token).to_hex().as_str() != metadata.delete_token_hash
            {
                return private_error(StatusCode::FORBIDDEN, "shard deletion token was rejected");
            }
            match erase_shard(state, shard_id, &metadata).await {
                Ok(()) => (StatusCode::NO_CONTENT, Vec::new()),
                Err(error) => {
                    eprintln!("could not delete storage shard {shard_id}: {error:#}");
                    private_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "shard storage is unavailable",
                    )
                }
            }
        }
        ("GET", path) if path.starts_with("/v1/groups/") && path.ends_with("/events") => {
            let group_id = path
                .trim_start_matches("/v1/groups/")
                .trim_end_matches("/events");
            if state.deletions.read().await.contains_key(group_id) {
                return private_error(StatusCode::GONE, "group has been deleted");
            }
            let mut events = state
                .events
                .read()
                .await
                .values()
                .filter(|event| event.group_id == group_id)
                .cloned()
                .collect::<Vec<_>>();
            events.sort_by(|left, right| {
                left.created_at_millis
                    .cmp(&right.created_at_millis)
                    .then_with(|| left.event_id.cmp(&right.event_id))
            });
            private_json(StatusCode::OK, &events)
        }
        _ => private_error(StatusCode::NOT_FOUND, "unsupported private relay request"),
    }
}

fn private_json(value_status: StatusCode, value: &impl Serialize) -> (StatusCode, Vec<u8>) {
    serde_json::to_vec(value)
        .map(|body| (value_status, body))
        .unwrap_or_else(|_| private_error(StatusCode::INTERNAL_SERVER_ERROR, "encoding failed"))
}

fn private_error(status: StatusCode, message: &str) -> (StatusCode, Vec<u8>) {
    (status, message.as_bytes().to_vec())
}

fn parse_private_watch_path(path: &str) -> Option<(&str, &str)> {
    let rest = path.strip_prefix("/v1/groups/")?;
    let (group_id, revision) = rest.split_once("/watch/")?;
    if group_id.contains('/') || revision.contains('/') {
        return None;
    }
    Some((group_id, revision))
}

fn parse_private_presence_path(path: &str) -> Option<&str> {
    let group_id = path
        .strip_prefix("/v1/groups/")?
        .strip_suffix("/presence")?;
    if group_id.contains('/') {
        return None;
    }
    Some(group_id)
}

fn parse_private_account_watch_path(path: &str) -> Option<(&str, &str)> {
    let rest = path.strip_prefix("/v1/accounts/")?;
    let (locator, revision) = rest.split_once("/watch/")?;
    if locator.contains('/') || revision.contains('/') {
        return None;
    }
    Some((locator, revision))
}

fn parse_watch_revision(value: &str) -> Option<Option<u64>> {
    if value == "initial" {
        Some(None)
    } else {
        value.parse::<u64>().ok().map(Some)
    }
}

async fn wait_for_group_change(
    state: &AppState,
    group_id: &str,
    since: Option<u64>,
) -> Result<GroupWatchResponse, StatusCode> {
    if group_id.len() != 64 || !group_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(StatusCode::BAD_REQUEST);
    }

    let (current, mut changes) = {
        let _guard = state.mutations.lock().await;
        if state.deletions.read().await.contains_key(group_id) {
            return Err(StatusCode::GONE);
        }
        let current = state
            .events
            .read()
            .await
            .values()
            .filter(|event| event.group_id == group_id)
            .count() as u64;
        if current == 0 {
            return Err(StatusCode::NOT_FOUND);
        }
        let sender = state
            .group_changes
            .write()
            .await
            .entry(group_id.to_owned())
            .or_insert_with(|| watch::channel(current).0)
            .clone();
        (current, sender.subscribe())
    };

    let Some(since) = since else {
        return Ok(GroupWatchResponse {
            revision: current,
            changed: false,
            presences: active_group_presences(state, group_id).await,
        });
    };
    if current != since {
        return Ok(GroupWatchResponse {
            revision: current,
            changed: true,
            presences: active_group_presences(state, group_id).await,
        });
    }

    if timeout(Duration::from_secs(20), changes.changed())
        .await
        .is_ok_and(|result| result.is_ok())
    {
        if state.deletions.read().await.contains_key(group_id) {
            return Err(StatusCode::GONE);
        }
        let revision = *changes.borrow_and_update();
        return Ok(GroupWatchResponse {
            revision,
            changed: revision != since,
            presences: active_group_presences(state, group_id).await,
        });
    }

    Ok(GroupWatchResponse {
        revision: since,
        changed: false,
        presences: active_group_presences(state, group_id).await,
    })
}

async fn wait_for_account_change(
    state: &AppState,
    locator: &str,
    since: Option<u64>,
) -> Result<GroupWatchResponse, StatusCode> {
    if locator.len() != 64 || !locator.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(StatusCode::BAD_REQUEST);
    }

    let (current, mut changes) = {
        let _guard = state.mutations.lock().await;
        let current = match state.accounts.read().await.get(locator) {
            Some(vault) if vault.deleted => return Err(StatusCode::GONE),
            Some(vault) => vault.revision,
            None => return Err(StatusCode::NOT_FOUND),
        };
        let sender = state
            .account_changes
            .write()
            .await
            .entry(locator.to_owned())
            .or_insert_with(|| watch::channel(current).0)
            .clone();
        (current, sender.subscribe())
    };

    let Some(since) = since else {
        return Ok(GroupWatchResponse {
            revision: current,
            changed: false,
            presences: Vec::new(),
        });
    };
    if current != since {
        return Ok(GroupWatchResponse {
            revision: current,
            changed: true,
            presences: Vec::new(),
        });
    }

    if timeout(Duration::from_secs(20), changes.changed())
        .await
        .is_ok_and(|result| result.is_ok())
    {
        if state
            .accounts
            .read()
            .await
            .get(locator)
            .is_none_or(|vault| vault.deleted)
        {
            return Err(StatusCode::GONE);
        }
        let revision = *changes.borrow_and_update();
        return Ok(GroupWatchResponse {
            revision,
            changed: revision != since,
            presences: Vec::new(),
        });
    }

    Ok(GroupWatchResponse {
        revision: since,
        changed: false,
        presences: Vec::new(),
    })
}

fn has_media_type(headers: &HeaderMap, expected: &str) -> bool {
    headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case(expected))
}

async fn snapshot(State(state): State<AppState>) -> Json<Snapshot> {
    Json(current_snapshot(&state).await)
}

async fn current_snapshot(state: &AppState) -> Snapshot {
    Snapshot {
        accounts: state.accounts.read().await.values().cloned().collect(),
        deletions: state.deletions.read().await.values().cloned().collect(),
        invite_rotations: state
            .invite_rotations
            .read()
            .await
            .values()
            .cloned()
            .collect(),
        invites: state.invites.read().await.values().cloned().collect(),
        events: state.events.read().await.values().cloned().collect(),
        mls_join_requests: state
            .mls_join_requests
            .read()
            .await
            .values()
            .cloned()
            .collect(),
        mls_removal_requests: state
            .mls_removal_requests
            .read()
            .await
            .values()
            .cloned()
            .collect(),
        mls_geneses: state.mls_geneses.read().await.values().cloned().collect(),
        mls_epochs: state.mls_epochs.read().await.values().cloned().collect(),
    }
}

fn storage_error(error: anyhow::Error) -> (StatusCode, String) {
    eprintln!("relay storage error: {error:#}");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "relay could not persist the object".into(),
    )
}

fn control_storage_error(error: anyhow::Error) -> (StatusCode, String) {
    eprintln!("MLS control update rejected: {error:#}");
    (
        StatusCode::CONFLICT,
        "MLS control head changed; fetch the latest epoch and retry".into(),
    )
}

async fn apply_account_vault(
    state: &AppState,
    vault: AccountVault,
) -> anyhow::Result<InsertResult> {
    let _guard = state.mutations.lock().await;
    if let Some(current) = state.accounts.read().await.get(&vault.locator) {
        if current.identity_public_key != vault.identity_public_key {
            bail!("account identity does not match the existing vault")
        }
        if current.revision > vault.revision {
            bail!("account vault revision is stale")
        }
        if current.revision == vault.revision {
            if current.signature_base64 == vault.signature_base64 {
                return Ok(InsertResult::Present);
            }
            bail!("account vault revision is already occupied")
        }
        if current.deleted {
            bail!("deleted accounts cannot be restored")
        }
    }
    let locator = vault.locator.clone();
    let revision = vault.revision;
    state.store.upsert_account(&vault).await?;
    state.accounts.write().await.insert(locator.clone(), vault);
    let sender = state
        .account_changes
        .write()
        .await
        .entry(locator)
        .or_insert_with(|| watch::channel(revision).0)
        .clone();
    sender.send_replace(revision);
    Ok(InsertResult::Inserted)
}

async fn insert_invite(state: &AppState, record: InviteRecord) -> anyhow::Result<InsertResult> {
    let _guard = state.mutations.lock().await;
    if let Some(group_id) = record.group_id.as_ref()
        && state.deletions.read().await.contains_key(group_id)
    {
        return Ok(InsertResult::Deleted);
    }
    if let Some(group_id) = record.group_id.as_ref()
        && let Some(rotation) = state.invite_rotations.read().await.get(group_id)
        && rotation
            .new_invite
            .as_ref()
            .is_none_or(|current| current.locator != record.locator)
    {
        return Ok(InsertResult::Present);
    }
    let object_id = record.locator.clone();
    let inserted = state
        .store
        .insert("invite", object_id.clone(), &record)
        .await?;
    if inserted {
        state.invites.write().await.insert(object_id, record);
    }
    Ok(if inserted {
        InsertResult::Inserted
    } else {
        InsertResult::Present
    })
}

async fn apply_invite_rotation(
    state: &AppState,
    rotation: InviteRotation,
) -> anyhow::Result<InsertResult> {
    let _guard = state.mutations.lock().await;
    if state
        .deletions
        .read()
        .await
        .contains_key(&rotation.group_id)
    {
        return Ok(InsertResult::Deleted);
    }
    if let Some(current) = state.invite_rotations.read().await.get(&rotation.group_id) {
        if current.owner_sequence == rotation.owner_sequence
            && current.signature_base64 == rotation.signature_base64
        {
            return Ok(InsertResult::Present);
        }
        if current.owner_sequence >= rotation.owner_sequence {
            bail!("invite rotation sequence is stale")
        }
    }
    let invite_ids = state
        .invites
        .read()
        .await
        .iter()
        .filter(|(_, invite)| invite.group_id.as_deref() == Some(rotation.group_id.as_str()))
        .map(|(locator, _)| locator.clone())
        .collect::<Vec<_>>();
    state
        .store
        .apply_invite_rotation(&rotation, &invite_ids)
        .await?;
    state
        .invites
        .write()
        .await
        .retain(|_, invite| invite.group_id.as_deref() != Some(rotation.group_id.as_str()));
    if let Some(invite) = rotation.new_invite.as_ref() {
        state
            .invites
            .write()
            .await
            .insert(invite.locator.clone(), invite.clone());
    }
    state
        .invite_rotations
        .write()
        .await
        .insert(rotation.group_id.clone(), rotation);
    Ok(InsertResult::Inserted)
}

async fn insert_event(state: &AppState, event: SignedEvent) -> anyhow::Result<InsertResult> {
    validate_mls_event(state, &event).await?;
    let _guard = state.mutations.lock().await;
    if state.deletions.read().await.contains_key(&event.group_id) {
        return Ok(InsertResult::Deleted);
    }
    let group_id = event.group_id.clone();
    let object_id = event.event_id.clone();
    let inserted = state
        .store
        .insert("event", object_id.clone(), &event)
        .await?;
    if inserted {
        state.events.write().await.insert(object_id, event);
        if let Some(sender) = state.group_changes.read().await.get(&group_id) {
            let revision = sender.borrow().saturating_add(1);
            sender.send_replace(revision);
        }
    }
    Ok(if inserted {
        InsertResult::Inserted
    } else {
        InsertResult::Present
    })
}

async fn validate_mls_event(state: &AppState, event: &SignedEvent) -> anyhow::Result<()> {
    let has_genesis = state.mls_geneses.read().await.contains_key(&event.group_id);
    if !has_genesis {
        if event.encryption_version == 1 && event.epoch.is_none() {
            return Ok(());
        }
        bail!("group has not enabled MLS encryption")
    }
    if event.encryption_version != 2 {
        bail!("legacy events are closed after MLS cutover")
    }
    let epoch = event.epoch.context("MLS event has no epoch")?;
    let log = mls_control_log(state, &event.group_id)
        .await
        .context("group MLS control log is invalid")?;
    let members = log
        .member_accounts_at(epoch)
        .context("MLS event references an unknown epoch")?;
    if !members.contains(&event.author_public_key) {
        bail!("event author was not a member in that MLS epoch")
    }
    Ok(())
}

async fn insert_mls_join_request(
    state: &AppState,
    request: MlsJoinRequest,
) -> anyhow::Result<InsertResult> {
    let _guard = state.mutations.lock().await;
    if state.deletions.read().await.contains_key(&request.group_id) {
        return Ok(InsertResult::Deleted);
    }
    let group_exists = state
        .mls_geneses
        .read()
        .await
        .contains_key(&request.group_id)
        || state
            .events
            .read()
            .await
            .values()
            .any(|event| event.group_id == request.group_id)
        || state
            .invites
            .read()
            .await
            .values()
            .any(|invite| invite.group_id.as_deref() == Some(request.group_id.as_str()));
    if !group_exists {
        bail!("MLS join request targets an unknown group")
    }
    let object_id = request.request_id.clone();
    let inserted = state
        .store
        .insert("mls_join_request", object_id.clone(), &request)
        .await?;
    if inserted {
        let group_id = request.group_id.clone();
        state
            .mls_join_requests
            .write()
            .await
            .insert(object_id, request);
        notify_group_change(state, &group_id).await;
    }
    Ok(if inserted {
        InsertResult::Inserted
    } else {
        InsertResult::Present
    })
}

async fn insert_mls_removal_request(
    state: &AppState,
    request: MlsRemovalRequest,
) -> anyhow::Result<InsertResult> {
    let _guard = state.mutations.lock().await;
    if state.deletions.read().await.contains_key(&request.group_id) {
        return Ok(InsertResult::Deleted);
    }
    if !state
        .mls_geneses
        .read()
        .await
        .contains_key(&request.group_id)
    {
        bail!("MLS removal request targets a group without a control log")
    }
    let object_id = request.request_id.clone();
    let inserted = state
        .store
        .insert("mls_removal_request", object_id.clone(), &request)
        .await?;
    if inserted {
        let group_id = request.group_id.clone();
        state
            .mls_removal_requests
            .write()
            .await
            .insert(object_id, request);
        notify_group_change(state, &group_id).await;
    }
    Ok(if inserted {
        InsertResult::Inserted
    } else {
        InsertResult::Present
    })
}

async fn insert_mls_genesis(
    state: &AppState,
    genesis: MlsGroupGenesis,
) -> anyhow::Result<InsertResult> {
    let _guard = state.mutations.lock().await;
    if state.deletions.read().await.contains_key(&genesis.group_id) {
        return Ok(InsertResult::Deleted);
    }
    if let Some(current) = state.mls_geneses.read().await.get(&genesis.group_id) {
        if current.record_id == genesis.record_id {
            return Ok(InsertResult::Present);
        }
        bail!("group already has a different MLS genesis")
    }
    let group_id = genesis.group_id.clone();
    let inserted = state
        .store
        .insert("mls_genesis", group_id.clone(), &genesis)
        .await?;
    if !inserted {
        bail!("stored MLS genesis conflicts with memory")
    }
    state
        .mls_geneses
        .write()
        .await
        .insert(group_id.clone(), genesis);
    notify_group_change(state, &group_id).await;
    Ok(InsertResult::Inserted)
}

async fn insert_mls_epoch(
    state: &AppState,
    record: MlsEpochRecord,
) -> anyhow::Result<InsertResult> {
    let _guard = state.mutations.lock().await;
    let group_id = record.bundle.group_id.clone();
    if state.deletions.read().await.contains_key(&group_id) {
        return Ok(InsertResult::Deleted);
    }
    if state
        .mls_epochs
        .read()
        .await
        .contains_key(&record.record_id)
    {
        return Ok(InsertResult::Present);
    }
    let genesis = state
        .mls_geneses
        .read()
        .await
        .get(&group_id)
        .cloned()
        .context("group has no MLS genesis")?;
    if record.owner_public_key != genesis.owner_public_key {
        bail!("MLS epoch author is not the group founder")
    }
    let mut epochs = state
        .mls_epochs
        .read()
        .await
        .values()
        .filter(|epoch| epoch.bundle.group_id == group_id)
        .cloned()
        .collect::<Vec<_>>();
    epochs.sort_by_key(|epoch| epoch.bundle.epoch);
    let (head_epoch, head_record_id) = epochs
        .last()
        .map(|epoch| (epoch.bundle.epoch, epoch.record_id.as_str()))
        .unwrap_or((0, genesis.record_id.as_str()));
    if record.bundle.parent_epoch != head_epoch
        || record.bundle.epoch != head_epoch.saturating_add(1)
        || record.previous_record_id != head_record_id
    {
        bail!("MLS epoch does not extend the current control head")
    }
    let object_id = record.record_id.clone();
    let inserted = state
        .store
        .insert("mls_epoch", object_id.clone(), &record)
        .await?;
    if !inserted {
        bail!("stored MLS epoch conflicts with memory")
    }
    state.mls_epochs.write().await.insert(object_id, record);
    notify_group_change(state, &group_id).await;
    Ok(InsertResult::Inserted)
}

async fn mls_control_log(state: &AppState, group_id: &str) -> Option<MlsControlLog> {
    let genesis = state.mls_geneses.read().await.get(group_id).cloned()?;
    let mut epochs = state
        .mls_epochs
        .read()
        .await
        .values()
        .filter(|epoch| epoch.bundle.group_id == group_id)
        .cloned()
        .collect::<Vec<_>>();
    epochs.sort_by_key(|epoch| epoch.bundle.epoch);
    let log = MlsControlLog { genesis, epochs };
    log.verify().ok()?;
    Some(log)
}

async fn notify_group_change(state: &AppState, group_id: &str) {
    if let Some(sender) = state.group_changes.read().await.get(group_id) {
        let revision = sender.borrow().saturating_add(1);
        sender.send_replace(revision);
    }
}

async fn insert_shard(state: &AppState, shard: StorageShard) -> Result<bool, ShardInsertError> {
    let _guard = state.mutations.lock().await;
    if state
        .store
        .shard_was_deleted(&shard.shard_id)
        .await
        .map_err(ShardInsertError::Storage)?
    {
        return Err(ShardInsertError::Deleted);
    }
    if let Some(existing) = state
        .store
        .shard_metadata(&shard.shard_id)
        .await
        .map_err(ShardInsertError::Storage)?
    {
        if existing.payload_hash == shard.payload_hash
            && existing.delete_token_hash == shard.delete_token_hash
        {
            return Ok(false);
        }
        return Err(ShardInsertError::Storage(anyhow::anyhow!(
            "storage shard identifier conflicts with existing metadata"
        )));
    }
    let payload_length = STANDARD_NO_PAD
        .decode(&shard.payload_base64)
        .map_err(|error| ShardInsertError::Storage(error.into()))?
        .len() as u64;
    let current = state.shard_bytes.load(Ordering::Relaxed);
    if state.storage_limit_bytes != 0
        && current.saturating_add(payload_length) > state.storage_limit_bytes
    {
        return Err(ShardInsertError::Full);
    }
    let stored_length = state
        .shard_store
        .put_shard(&shard)
        .await
        .map_err(ShardInsertError::Storage)?;
    if let Err(error) = state.store.insert_shard(&shard, stored_length).await {
        let _ = state.shard_store.delete_shard(&shard.shard_id).await;
        return Err(ShardInsertError::Storage(error));
    }
    state.shard_count.fetch_add(1, Ordering::Relaxed);
    state
        .shard_bytes
        .fetch_add(stored_length, Ordering::Relaxed);
    Ok(true)
}

async fn erase_shard(
    state: &AppState,
    shard_id: &str,
    metadata: &ShardMetadata,
) -> anyhow::Result<()> {
    let _guard = state.mutations.lock().await;
    if state.store.queue_shard_deletion(shard_id).await? {
        state.shard_count.fetch_sub(1, Ordering::Relaxed);
        state
            .shard_bytes
            .fetch_sub(metadata.byte_length, Ordering::Relaxed);
    }
    state.shard_store.delete_shard(shard_id).await?;
    state.store.complete_shard_deletion(shard_id).await?;
    Ok(())
}

async fn shard_deletion_loop(state: AppState) {
    loop {
        match state.store.pending_shard_deletions().await {
            Ok(shard_ids) => {
                for shard_id in shard_ids {
                    match state.shard_store.delete_shard(&shard_id).await {
                        Ok(()) => {
                            if let Err(error) = state.store.complete_shard_deletion(&shard_id).await
                            {
                                eprintln!(
                                    "could not complete storage shard deletion {shard_id}: {error:#}"
                                );
                            }
                        }
                        Err(error) => eprintln!(
                            "could not retry storage shard deletion {shard_id}: {error:#}"
                        ),
                    }
                }
            }
            Err(error) => eprintln!("could not load queued shard deletions: {error:#}"),
        }
        sleep(Duration::from_secs(30)).await;
    }
}

async fn apply_group_deletion(state: &AppState, deletion: GroupDeletion) -> anyhow::Result<bool> {
    let guard = state.mutations.lock().await;
    if state
        .deletions
        .read()
        .await
        .contains_key(&deletion.group_id)
    {
        return Ok(false);
    }
    let invite_ids = state
        .invites
        .read()
        .await
        .iter()
        .filter(|(_, invite)| invite.group_id.as_deref() == Some(deletion.group_id.as_str()))
        .map(|(object_id, _)| object_id.clone())
        .collect::<Vec<_>>();
    let event_ids = state
        .events
        .read()
        .await
        .iter()
        .filter(|(_, event)| event.group_id == deletion.group_id)
        .map(|(object_id, _)| object_id.clone())
        .collect::<Vec<_>>();
    let mls_join_request_ids = state
        .mls_join_requests
        .read()
        .await
        .iter()
        .filter(|(_, request)| request.group_id == deletion.group_id)
        .map(|(object_id, _)| object_id.clone())
        .collect::<Vec<_>>();
    let mls_epoch_ids = state
        .mls_epochs
        .read()
        .await
        .iter()
        .filter(|(_, epoch)| epoch.bundle.group_id == deletion.group_id)
        .map(|(object_id, _)| object_id.clone())
        .collect::<Vec<_>>();
    let mls_removal_request_ids = state
        .mls_removal_requests
        .read()
        .await
        .iter()
        .filter(|(_, request)| request.group_id == deletion.group_id)
        .map(|(object_id, _)| object_id.clone())
        .collect::<Vec<_>>();
    state
        .store
        .apply_group_deletion(
            &deletion,
            &invite_ids,
            &event_ids,
            &mls_join_request_ids,
            &mls_removal_request_ids,
            &mls_epoch_ids,
        )
        .await?;
    state
        .invites
        .write()
        .await
        .retain(|_, invite| invite.group_id.as_deref() != Some(deletion.group_id.as_str()));
    state
        .invite_rotations
        .write()
        .await
        .remove(&deletion.group_id);
    state
        .events
        .write()
        .await
        .retain(|_, event| event.group_id != deletion.group_id);
    state
        .mls_join_requests
        .write()
        .await
        .retain(|_, request| request.group_id != deletion.group_id);
    state
        .mls_removal_requests
        .write()
        .await
        .retain(|_, request| request.group_id != deletion.group_id);
    state.mls_geneses.write().await.remove(&deletion.group_id);
    state
        .mls_epochs
        .write()
        .await
        .retain(|_, epoch| epoch.bundle.group_id != deletion.group_id);
    state
        .deletions
        .write()
        .await
        .insert(deletion.group_id.clone(), deletion.clone());
    state.group_changes.write().await.remove(&deletion.group_id);
    state
        .group_presences
        .write()
        .await
        .remove(&deletion.group_id);
    drop(guard);
    Ok(true)
}

async fn gossip_invite(state: AppState, record: InviteRecord) {
    for peer in state.peers.iter() {
        let _ = state
            .client
            .post(format!("{peer}/v1/invites"))
            .json(&record)
            .send()
            .await;
    }
}

async fn gossip_account(state: AppState, vault: AccountVault) {
    for peer in state.peers.iter() {
        let _ = state
            .client
            .post(format!("{peer}/v1/accounts"))
            .json(&vault)
            .send()
            .await;
    }
}

async fn gossip_invite_rotation(state: AppState, rotation: InviteRotation) {
    for peer in state.peers.iter() {
        let _ = state
            .client
            .post(format!("{peer}/v1/invite-rotations"))
            .json(&rotation)
            .send()
            .await;
    }
}

async fn gossip_event(state: AppState, event: SignedEvent) {
    for peer in state.peers.iter() {
        let _ = state
            .client
            .post(format!("{peer}/v1/events"))
            .json(&event)
            .send()
            .await;
    }
}

async fn gossip_mls_join_request(state: AppState, request: MlsJoinRequest) {
    for peer in state.peers.iter() {
        let _ = state
            .client
            .post(format!("{peer}/v2/mls/join-requests"))
            .json(&request)
            .send()
            .await;
    }
}

async fn gossip_mls_removal_request(state: AppState, request: MlsRemovalRequest) {
    for peer in state.peers.iter() {
        let _ = state
            .client
            .post(format!("{peer}/v2/mls/removal-requests"))
            .json(&request)
            .send()
            .await;
    }
}

async fn gossip_mls_genesis(state: AppState, genesis: MlsGroupGenesis) {
    for peer in state.peers.iter() {
        let _ = state
            .client
            .post(format!("{peer}/v2/mls/genesis"))
            .json(&genesis)
            .send()
            .await;
    }
}

async fn gossip_mls_epoch(state: AppState, epoch: MlsEpochRecord) {
    for peer in state.peers.iter() {
        let _ = state
            .client
            .post(format!("{peer}/v2/mls/epochs"))
            .json(&epoch)
            .send()
            .await;
    }
}

async fn gossip_group_deletion(state: AppState, deletion: GroupDeletion) {
    for peer in state.peers.iter() {
        let _ = state
            .client
            .post(format!("{peer}/v1/group-deletions"))
            .json(&deletion)
            .send()
            .await;
    }
}

async fn relay_discovery_loop(state: AppState) {
    let Some(public_url) = state.public_url.as_deref() else {
        return;
    };
    sleep(Duration::from_millis(250)).await;
    loop {
        let Ok(now) = unix_seconds() else {
            sleep(state.discovery_interval).await;
            continue;
        };
        let (capacity, available) = storage_descriptor_values(&state);
        let Ok(own) = state.relay_identity.signed_descriptor(
            public_url,
            state.privacy.public_config(),
            capacity,
            available,
            now,
        ) else {
            sleep(state.discovery_interval).await;
            continue;
        };
        let known = state.relay_directory.list(now).await;
        let mut seen = HashSet::new();
        let mut targets = Vec::new();
        for target in state.bootstrap_relays.iter() {
            if target != public_url && seen.insert(target.clone()) {
                targets.push(target.clone());
            }
        }
        if !known.is_empty() {
            let start = (now / state.discovery_interval.as_secs()) as usize % known.len();
            for offset in 0..known.len().min(MAX_DISCOVERY_TARGETS_PER_ROUND) {
                let target = &known[(start + offset) % known.len()].base_url;
                if target != public_url && seen.insert(target.clone()) {
                    targets.push(target.clone());
                }
            }
        }

        for target in targets {
            let _ = announce_relay(&target, &own, state.allow_local_discovery).await;
            let Ok(descriptors) =
                fetch_relay_directory(&target, now, state.allow_local_discovery).await
            else {
                continue;
            };
            if descriptors.is_empty() {
                continue;
            }
            let start = now as usize % descriptors.len();
            for offset in 0..descriptors.len().min(MAX_DISCOVERED_RELAYS_PER_TARGET) {
                let announced = &descriptors[(start + offset) % descriptors.len()];
                if announced.relay_id == own.relay_id {
                    continue;
                }
                let Ok(reachable) =
                    verify_relay_reachability(announced, now, state.allow_local_discovery).await
                else {
                    continue;
                };
                if let Err(error) = state.relay_directory.insert(reachable, now).await {
                    eprintln!("could not persist discovered relay: {error:#}");
                }
            }
        }
        sleep(state.discovery_interval).await;
    }
}

async fn anti_entropy_loop(state: AppState) {
    loop {
        for peer in state.peers.iter() {
            let Ok(response) = state.client.get(format!("{peer}/v1/snapshot")).send().await else {
                continue;
            };
            let Ok(snapshot) = response.error_for_status().and_then(|response| {
                // Parsing happens below because reqwest's JSON method is asynchronous.
                Ok(response)
            }) else {
                continue;
            };
            let Ok(snapshot) = snapshot.json::<Snapshot>().await else {
                continue;
            };
            for account in snapshot.accounts {
                if state
                    .accounts
                    .read()
                    .await
                    .get(&account.locator)
                    .is_some_and(|current| current.revision >= account.revision)
                {
                    continue;
                }
                if account.verify().is_ok()
                    && let Err(error) = apply_account_vault(&state, account).await
                {
                    eprintln!("could not persist gossiped account vault: {error:#}");
                }
            }
            for deletion in snapshot.deletions {
                if deletion.verify().is_ok()
                    && let Err(error) = apply_group_deletion(&state, deletion).await
                {
                    eprintln!("could not persist gossiped group deletion: {error:#}");
                }
            }
            for rotation in snapshot.invite_rotations {
                if rotation.verify().is_ok()
                    && let Err(error) = apply_invite_rotation(&state, rotation).await
                {
                    eprintln!("could not persist gossiped invite rotation: {error:#}");
                }
            }
            for invite in snapshot.invites {
                if invite.verify().is_ok() {
                    if let Err(error) = insert_invite(&state, invite).await {
                        eprintln!("could not persist gossiped invitation: {error:#}");
                    }
                }
            }
            let (legacy_events, epoch_events): (Vec<_>, Vec<_>) = snapshot
                .events
                .into_iter()
                .partition(|event| event.encryption_version == 1);
            for event in legacy_events {
                if event.verify().is_ok() {
                    if let Err(error) = insert_event(&state, event).await {
                        eprintln!("could not persist gossiped event: {error:#}");
                    }
                }
            }
            for genesis in snapshot.mls_geneses {
                if genesis.verify().is_ok()
                    && let Err(error) = insert_mls_genesis(&state, genesis).await
                {
                    eprintln!("could not persist gossiped MLS genesis: {error:#}");
                }
            }
            for request in snapshot.mls_join_requests {
                if request.verify().is_ok()
                    && let Err(error) = insert_mls_join_request(&state, request).await
                {
                    eprintln!("could not persist gossiped MLS join request: {error:#}");
                }
            }
            for request in snapshot.mls_removal_requests {
                if request.verify().is_ok()
                    && let Err(error) = insert_mls_removal_request(&state, request).await
                {
                    eprintln!("could not persist gossiped MLS removal request: {error:#}");
                }
            }
            let mut epochs = snapshot.mls_epochs;
            epochs.sort_by_key(|epoch| epoch.bundle.epoch);
            for epoch in epochs {
                if epoch.verify().is_ok()
                    && let Err(error) = insert_mls_epoch(&state, epoch).await
                {
                    eprintln!("could not persist gossiped MLS epoch: {error:#}");
                }
            }
            for event in epoch_events {
                if event.verify().is_ok()
                    && let Err(error) = insert_event(&state, event).await
                {
                    eprintln!("could not persist gossiped MLS event: {error:#}");
                }
            }
        }
        sleep(Duration::from_secs(2)).await;
    }
}

fn unix_seconds() -> anyhow::Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_secs())
}

fn unix_millis() -> anyhow::Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_millis() as u64)
}
