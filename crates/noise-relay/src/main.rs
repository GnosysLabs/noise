mod privacy;
mod store;

use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    path::PathBuf,
    sync::Arc,
    time::Duration,
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
use clap::Parser;
use noise_core::{
    AccountVault, EncryptedBlob, GroupDeletion, InviteRecord, InviteRotation, SignedEvent,
};
use noise_transport::{
    GATEWAY_HEADER, OHTTP_GATEWAY_PATH, OHTTP_KEYS_MEDIA_TYPE, OHTTP_KEYS_PATH, OHTTP_RELAY_PATH,
    OHTTP_REQUEST_MEDIA_TYPE, OHTTP_RESPONSE_MEDIA_TYPE, PlainRequest, RelayDescriptor,
    encode_response,
};
use serde::{Deserialize, Serialize};
use tokio::{
    net::TcpListener,
    sync::{Mutex, RwLock, watch},
    time::{sleep, timeout},
};
use tower_http::cors::CorsLayer;

use privacy::PrivacyGateway;
use store::DurableStore;

#[derive(Debug, Parser)]
#[command(name = "noise-relay", about = "An untrusted Noise protocol relay")]
struct Args {
    #[arg(long, default_value = "127.0.0.1:4301")]
    listen: SocketAddr,
    #[arg(long)]
    peer: Vec<String>,
    #[arg(long)]
    data: Option<PathBuf>,
    #[arg(long)]
    public_url: Option<String>,
    #[arg(long)]
    mask_target: Vec<String>,
}

#[derive(Clone)]
struct AppState {
    accounts: Arc<RwLock<HashMap<String, AccountVault>>>,
    invites: Arc<RwLock<HashMap<String, InviteRecord>>>,
    invite_rotations: Arc<RwLock<HashMap<String, InviteRotation>>>,
    events: Arc<RwLock<HashMap<String, SignedEvent>>>,
    blobs: Arc<RwLock<HashMap<String, EncryptedBlob>>>,
    deletions: Arc<RwLock<HashMap<String, GroupDeletion>>>,
    group_changes: Arc<RwLock<HashMap<String, watch::Sender<u64>>>>,
    account_changes: Arc<RwLock<HashMap<String, watch::Sender<u64>>>>,
    mutations: Arc<Mutex<()>>,
    peers: Arc<Vec<String>>,
    client: reqwest::Client,
    store: DurableStore,
    privacy: PrivacyGateway,
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
    blob_ids: Vec<String>,
}

#[derive(Serialize)]
struct Health {
    status: &'static str,
    accounts: usize,
    invitations: usize,
    events: usize,
    blobs: usize,
    deleted_groups: usize,
    peers: usize,
    privacy_gateway: bool,
    mask_targets: usize,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
struct GroupWatchResponse {
    revision: u64,
    changed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InsertResult {
    Inserted,
    Present,
    Deleted,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let data_directory = args
        .data
        .unwrap_or_else(|| PathBuf::from("relay-data").join(args.listen.port().to_string()));
    let (store, recovered) = DurableStore::open(&data_directory).await?;
    let privacy = PrivacyGateway::open(&data_directory)?;
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
        blobs: Arc::new(RwLock::new(recovered.blobs)),
        deletions: Arc::new(RwLock::new(recovered.deletions)),
        group_changes: Arc::new(RwLock::new(HashMap::new())),
        account_changes: Arc::new(RwLock::new(HashMap::new())),
        mutations: Arc::new(Mutex::new(())),
        peers: Arc::new(peers),
        client,
        store,
        privacy,
        mask_targets: Arc::new(mask_targets),
        public_url: public_url.clone(),
    };

    tokio::spawn(anti_entropy_loop(state.clone()));

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
        .route("/v1/groups/{group_id}/watch/{since}", get(group_watch))
        .route("/v1/blobs", post(publish_blob))
        .route("/v1/blobs/{blob_id}", get(get_blob))
        .route("/v1/group-deletions", post(publish_group_deletion))
        .route("/v1/snapshot", get(snapshot))
        .route(OHTTP_KEYS_PATH, get(ohttp_keys))
        .route("/v1/relay-descriptor", get(relay_descriptor))
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
    println!(
        "noise relay listening on http://{} with {} peer(s); durable data at {}",
        args.listen,
        state.peers.len(),
        state.store.path().display()
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
        accounts: state
            .accounts
            .read()
            .await
            .values()
            .filter(|vault| !vault.deleted)
            .count(),
        invitations: state.invites.read().await.len(),
        events: state.events.read().await.len(),
        blobs: state.blobs.read().await.len(),
        deleted_groups: state.deletions.read().await.len(),
        peers: state.peers.len(),
        privacy_gateway: true,
        mask_targets: state.mask_targets.len(),
    })
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

async fn group_watch(
    State(state): State<AppState>,
    Path((group_id, since)): Path<(String, String)>,
) -> Result<Json<GroupWatchResponse>, StatusCode> {
    let since = parse_watch_revision(&since).ok_or(StatusCode::BAD_REQUEST)?;
    wait_for_group_change(&state, &group_id, since)
        .await
        .map(Json)
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

async fn publish_blob(
    State(state): State<AppState>,
    Json(blob): Json<EncryptedBlob>,
) -> Result<StatusCode, (StatusCode, String)> {
    if blob.ciphertext_base64.len() > 2_000_000 {
        return Err((StatusCode::PAYLOAD_TOO_LARGE, "blob is too large".into()));
    }
    blob.verify()
        .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?;
    match insert_blob(&state, blob.clone())
        .await
        .map_err(storage_error)?
    {
        InsertResult::Inserted => {
            tokio::spawn(gossip_blob(state, blob));
        }
        InsertResult::Deleted => {
            return Err((StatusCode::GONE, "group has been deleted".into()));
        }
        InsertResult::Present => {}
    }
    Ok(StatusCode::ACCEPTED)
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

async fn get_blob(
    State(state): State<AppState>,
    Path(blob_id): Path<String>,
) -> Result<Json<EncryptedBlob>, StatusCode> {
    state
        .blobs
        .read()
        .await
        .get(&blob_id)
        .cloned()
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
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
    if !state.mask_targets.contains(target) {
        return (StatusCode::FORBIDDEN, "privacy gateway is not allowed").into_response();
    }
    let endpoint = format!("{target}{OHTTP_GATEWAY_PATH}");
    let Ok(response) = state
        .client
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
        ("POST", "/v1/blobs") => {
            let Ok(blob) = serde_json::from_slice::<EncryptedBlob>(&request.body) else {
                return private_error(StatusCode::BAD_REQUEST, "invalid encrypted blob");
            };
            if blob.ciphertext_base64.len() > 2_000_000 {
                return private_error(StatusCode::PAYLOAD_TOO_LARGE, "blob is too large");
            }
            if let Err(error) = blob.verify() {
                return private_error(StatusCode::BAD_REQUEST, &error.to_string());
            }
            match insert_blob(state, blob.clone()).await {
                Ok(InsertResult::Inserted) => {
                    tokio::spawn(gossip_blob(state.clone(), blob));
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
        ("GET", path) if path.starts_with("/v1/blobs/") => {
            let blob_id = path.trim_start_matches("/v1/blobs/");
            match state.blobs.read().await.get(blob_id).cloned() {
                Some(blob) => private_json(StatusCode::OK, &blob),
                None => private_error(StatusCode::NOT_FOUND, "blob is unavailable"),
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
        });
    };
    if current != since {
        return Ok(GroupWatchResponse {
            revision: current,
            changed: true,
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
        });
    }

    Ok(GroupWatchResponse {
        revision: since,
        changed: false,
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
        });
    };
    if current != since {
        return Ok(GroupWatchResponse {
            revision: current,
            changed: true,
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
        });
    }

    Ok(GroupWatchResponse {
        revision: since,
        changed: false,
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
        blob_ids: state.blobs.read().await.keys().cloned().collect(),
    }
}

fn storage_error(error: anyhow::Error) -> (StatusCode, String) {
    eprintln!("relay storage error: {error:#}");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "relay could not persist the object".into(),
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

async fn insert_blob(state: &AppState, blob: EncryptedBlob) -> anyhow::Result<InsertResult> {
    let _guard = state.mutations.lock().await;
    if let Some(group_id) = blob.group_id.as_ref()
        && state.deletions.read().await.contains_key(group_id)
    {
        return Ok(InsertResult::Deleted);
    }
    let object_id = blob.blob_id.clone();
    let inserted = state.store.insert("blob", object_id.clone(), &blob).await?;
    if inserted {
        state.blobs.write().await.insert(object_id, blob);
    }
    Ok(if inserted {
        InsertResult::Inserted
    } else {
        InsertResult::Present
    })
}

async fn apply_group_deletion(state: &AppState, deletion: GroupDeletion) -> anyhow::Result<bool> {
    let _guard = state.mutations.lock().await;
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
    let blob_ids = state
        .blobs
        .read()
        .await
        .iter()
        .filter(|(_, blob)| blob.group_id.as_deref() == Some(deletion.group_id.as_str()))
        .map(|(object_id, _)| object_id.clone())
        .collect::<Vec<_>>();
    state
        .store
        .apply_group_deletion(&deletion, &invite_ids, &event_ids, &blob_ids)
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
        .blobs
        .write()
        .await
        .retain(|_, blob| blob.group_id.as_deref() != Some(deletion.group_id.as_str()));
    state
        .deletions
        .write()
        .await
        .insert(deletion.group_id.clone(), deletion.clone());
    state.group_changes.write().await.remove(&deletion.group_id);
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

async fn gossip_blob(state: AppState, blob: EncryptedBlob) {
    for peer in state.peers.iter() {
        let _ = state
            .client
            .post(format!("{peer}/v1/blobs"))
            .json(&blob)
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
            for event in snapshot.events {
                if event.verify().is_ok() {
                    if let Err(error) = insert_event(&state, event).await {
                        eprintln!("could not persist gossiped event: {error:#}");
                    }
                }
            }
            for blob_id in snapshot.blob_ids {
                if state.blobs.read().await.contains_key(&blob_id) {
                    continue;
                }
                let Ok(response) = state
                    .client
                    .get(format!("{peer}/v1/blobs/{blob_id}"))
                    .send()
                    .await
                else {
                    continue;
                };
                let Ok(blob) = response.json::<EncryptedBlob>().await else {
                    continue;
                };
                if blob.verify().is_ok() {
                    if let Err(error) = insert_blob(&state, blob).await {
                        eprintln!("could not persist gossiped blob: {error:#}");
                    }
                }
            }
        }
        sleep(Duration::from_secs(2)).await;
    }
}
