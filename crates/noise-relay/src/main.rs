use std::{collections::HashMap, net::SocketAddr, sync::Arc, time::Duration};

use anyhow::Context;
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
};
use clap::Parser;
use noise_core::{EncryptedBlob, InviteRecord, SignedEvent};
use serde::{Deserialize, Serialize};
use tokio::{net::TcpListener, sync::RwLock, time::sleep};
use tower_http::cors::CorsLayer;

#[derive(Debug, Parser)]
#[command(name = "noise-relay", about = "An untrusted Noise protocol relay")]
struct Args {
    #[arg(long, default_value = "127.0.0.1:4301")]
    listen: SocketAddr,
    #[arg(long)]
    peer: Vec<String>,
}

#[derive(Clone)]
struct AppState {
    invites: Arc<RwLock<HashMap<String, InviteRecord>>>,
    events: Arc<RwLock<HashMap<String, SignedEvent>>>,
    blobs: Arc<RwLock<HashMap<String, EncryptedBlob>>>,
    peers: Arc<Vec<String>>,
    client: reqwest::Client,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Snapshot {
    invites: Vec<InviteRecord>,
    events: Vec<SignedEvent>,
    blob_ids: Vec<String>,
}

#[derive(Serialize)]
struct Health {
    status: &'static str,
    invitations: usize,
    events: usize,
    blobs: usize,
    peers: usize,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let peers = args
        .peer
        .into_iter()
        .map(|peer| peer.trim_end_matches('/').to_owned())
        .collect::<Vec<_>>();
    let state = AppState {
        invites: Arc::new(RwLock::new(HashMap::new())),
        events: Arc::new(RwLock::new(HashMap::new())),
        blobs: Arc::new(RwLock::new(HashMap::new())),
        peers: Arc::new(peers),
        client: reqwest::Client::new(),
    };

    tokio::spawn(anti_entropy_loop(state.clone()));

    let app = Router::new()
        .route("/health", get(health))
        .route("/v1/invites", post(publish_invite))
        .route("/v1/invites/{locator}", get(get_invite))
        .route("/v1/events", post(publish_event))
        .route("/v1/groups/{group_id}/events", get(group_events))
        .route("/v1/blobs", post(publish_blob))
        .route("/v1/blobs/{blob_id}", get(get_blob))
        .route("/v1/snapshot", get(snapshot))
        // Relays only accept public invitations and signed, encrypted objects;
        // browser clients do not send cookies or relay-held credentials.
        .layer(CorsLayer::permissive())
        .with_state(state.clone());

    let listener = TcpListener::bind(args.listen)
        .await
        .with_context(|| format!("could not bind relay to {}", args.listen))?;
    println!(
        "noise relay listening on http://{} with {} peer(s)",
        args.listen,
        state.peers.len()
    );
    axum::serve(listener, app).await?;
    Ok(())
}

async fn health(State(state): State<AppState>) -> Json<Health> {
    Json(Health {
        status: "ok",
        invitations: state.invites.read().await.len(),
        events: state.events.read().await.len(),
        blobs: state.blobs.read().await.len(),
        peers: state.peers.len(),
    })
}

async fn publish_invite(
    State(state): State<AppState>,
    Json(record): Json<InviteRecord>,
) -> Result<StatusCode, (StatusCode, String)> {
    record
        .verify()
        .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?;
    if insert_invite(&state, record.clone()).await {
        tokio::spawn(gossip_invite(state, record));
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

async fn publish_event(
    State(state): State<AppState>,
    Json(event): Json<SignedEvent>,
) -> Result<StatusCode, (StatusCode, String)> {
    event
        .verify()
        .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?;
    if insert_event(&state, event.clone()).await {
        tokio::spawn(gossip_event(state, event));
    }
    Ok(StatusCode::ACCEPTED)
}

async fn group_events(
    State(state): State<AppState>,
    Path(group_id): Path<String>,
) -> Json<Vec<SignedEvent>> {
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
    Json(events)
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
    if insert_blob(&state, blob.clone()).await {
        tokio::spawn(gossip_blob(state, blob));
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

async fn snapshot(State(state): State<AppState>) -> Json<Snapshot> {
    Json(current_snapshot(&state).await)
}

async fn current_snapshot(state: &AppState) -> Snapshot {
    Snapshot {
        invites: state.invites.read().await.values().cloned().collect(),
        events: state.events.read().await.values().cloned().collect(),
        blob_ids: state.blobs.read().await.keys().cloned().collect(),
    }
}

async fn insert_invite(state: &AppState, record: InviteRecord) -> bool {
    let mut invites = state.invites.write().await;
    if invites.contains_key(&record.locator) {
        false
    } else {
        invites.insert(record.locator.clone(), record);
        true
    }
}

async fn insert_event(state: &AppState, event: SignedEvent) -> bool {
    let mut events = state.events.write().await;
    if events.contains_key(&event.event_id) {
        false
    } else {
        events.insert(event.event_id.clone(), event);
        true
    }
}

async fn insert_blob(state: &AppState, blob: EncryptedBlob) -> bool {
    let mut blobs = state.blobs.write().await;
    if blobs.contains_key(&blob.blob_id) {
        false
    } else {
        blobs.insert(blob.blob_id.clone(), blob);
        true
    }
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
            for invite in snapshot.invites {
                if invite.verify().is_ok() {
                    insert_invite(&state, invite).await;
                }
            }
            for event in snapshot.events {
                if event.verify().is_ok() {
                    insert_event(&state, event).await;
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
                    insert_blob(&state, blob).await;
                }
            }
        }
        sleep(Duration::from_secs(2)).await;
    }
}
