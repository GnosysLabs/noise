use std::{
    collections::HashMap,
    io::ErrorKind,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use axum::{
    Json, Router,
    extract::{ConnectInfo, DefaultBodyLimit, State},
    http::{HeaderValue, Method, StatusCode, header},
    response::IntoResponse,
    routing::{get, post},
};
use base64::{Engine as _, engine::general_purpose::STANDARD_NO_PAD};
use clap::{Parser, Subcommand};
use noise_core::{
    SafetyEncryptionKeyPair, SealedSafetyReportV1, noise_signature_for_public_key,
    safety_recipient_key_id,
};
use serde::{Deserialize, Serialize};
use tokio::{fs, io::AsyncWriteExt, net::TcpListener};
use tower_http::cors::CorsLayer;

mod review;

const DEFAULT_BIND: &str = "127.0.0.1:4310";
const DEFAULT_REVIEW_BIND: &str = "127.0.0.1:4311";
const MAX_REPORT_BODY_BYTES: usize = 384 * 1024;
const MAX_CIPHERTEXT_BYTES: usize = 256 * 1024;
const RATE_LIMIT_REPORTS: u32 = 30;
const RATE_LIMIT_WINDOW: Duration = Duration::from_secs(60);

#[derive(Parser)]
#[command(name = "noise-safety-intake")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Generate separate public and private recipient-key files.
    Keygen {
        #[arg(long)]
        public_output: PathBuf,
        #[arg(long)]
        secret_output: PathBuf,
    },
    /// Run the write-only public intake with no recipient secret.
    Serve {
        #[arg(long)]
        public_key_file: PathBuf,
        #[arg(long)]
        spool_dir: PathBuf,
        #[arg(long, default_value = DEFAULT_BIND)]
        bind: SocketAddr,
    },
    /// Review and mark reports from a private, localhost-only interface.
    Review {
        #[arg(long)]
        secret_key_file: PathBuf,
        #[arg(long)]
        spool_dir: PathBuf,
        #[arg(long)]
        state_dir: Option<PathBuf>,
        #[arg(long, default_value = DEFAULT_REVIEW_BIND)]
        bind: SocketAddr,
    },
}

#[derive(Clone, Deserialize, Serialize)]
struct PublicRecipientKey {
    version: u32,
    recipient_key_id: String,
    public_key_base64: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    directive_signing_public_key_base64: Option<String>,
}

#[derive(Deserialize, Serialize)]
struct PrivateRecipientKey {
    version: u32,
    recipient_key_id: String,
    secret_base64: String,
}

#[derive(Serialize)]
struct ReadyResponse {
    status: &'static str,
    recipient_key_id: String,
}

#[derive(Serialize)]
struct ReceiptResponse {
    receipt_id: String,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: &'static str,
}

#[derive(Clone)]
struct AppState {
    recipient: PublicRecipientKey,
    spool_dir: PathBuf,
    rate_limits: Arc<Mutex<HashMap<IpAddr, RateWindow>>>,
}

struct RateWindow {
    started_at: Instant,
    count: u32,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    match args.command {
        Command::Keygen {
            public_output,
            secret_output,
        } => generate_keys(&public_output, &secret_output).await,
        Command::Serve {
            public_key_file,
            spool_dir,
            bind,
        } => serve(&public_key_file, &spool_dir, bind).await,
        Command::Review {
            secret_key_file,
            spool_dir,
            state_dir,
            bind,
        } => review::serve(&secret_key_file, &spool_dir, state_dir.as_deref(), bind).await,
    }
}

async fn generate_keys(public_output: &Path, secret_output: &Path) -> anyhow::Result<()> {
    if public_output == secret_output {
        bail!("public and private key files must use different paths")
    }
    let pair = SafetyEncryptionKeyPair::generate().context("could not generate recipient key")?;
    let recipient_key_id = pair.key_id();
    let public = PublicRecipientKey {
        version: 1,
        recipient_key_id: recipient_key_id.clone(),
        public_key_base64: pair.public_key_base64(),
        directive_signing_public_key_base64: Some(
            pair.directive_signing_key_pair()?.public_key_base64(),
        ),
    };
    let private = PrivateRecipientKey {
        version: 1,
        recipient_key_id,
        secret_base64: pair.secret_base64(),
    };
    write_new_json(public_output, &public, false).await?;
    if let Err(error) = write_new_json(secret_output, &private, true).await {
        let _ = fs::remove_file(public_output).await;
        return Err(error);
    }
    println!("generated separate noise safety public and private key files");
    println!("public key: {}", public_output.display());
    println!("private key: {}", secret_output.display());
    Ok(())
}

async fn serve(public_key_file: &Path, spool_dir: &Path, bind: SocketAddr) -> anyhow::Result<()> {
    let recipient = read_public_key(public_key_file).await?;
    fs::create_dir_all(spool_dir)
        .await
        .with_context(|| format!("could not create {}", spool_dir.display()))?;
    secure_directory(spool_dir).await?;
    let state = AppState {
        recipient,
        spool_dir: spool_dir.to_owned(),
        rate_limits: Arc::new(Mutex::new(HashMap::new())),
    };
    let cors = CorsLayer::new()
        .allow_origin([
            HeaderValue::from_static("http://127.0.0.1:1420"),
            HeaderValue::from_static("http://localhost:1420"),
        ])
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([header::CONTENT_TYPE]);
    let router = Router::new()
        .route("/health/ready", get(ready))
        .route("/v1/key", get(recipient_key))
        .route("/v1/reports", post(submit_report))
        .layer(DefaultBodyLimit::max(MAX_REPORT_BODY_BYTES))
        .layer(cors)
        .with_state(state.clone());
    let listener = TcpListener::bind(bind)
        .await
        .with_context(|| format!("could not bind noise safety intake to {bind}"))?;
    println!("noise safety intake listening on http://{bind}");
    println!("recipient key id: {}", state.recipient.recipient_key_id);
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .context("noise safety intake stopped")
}

async fn ready(State(state): State<AppState>) -> Json<ReadyResponse> {
    Json(ReadyResponse {
        status: "ready",
        recipient_key_id: state.recipient.recipient_key_id,
    })
}

async fn recipient_key(State(state): State<AppState>) -> Json<PublicRecipientKey> {
    Json(state.recipient)
}

async fn submit_report(
    ConnectInfo(address): ConnectInfo<SocketAddr>,
    State(state): State<AppState>,
    Json(envelope): Json<SealedSafetyReportV1>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    enforce_rate_limit(&state, address.ip())?;
    validate_envelope(&state.recipient, &envelope)?;
    let bytes = serde_json::to_vec(&envelope).map_err(|_| internal_error())?;
    let receipt_id = blake3::hash(&bytes).to_hex().to_string();
    let path = state.spool_dir.join(format!("{receipt_id}.json"));
    match write_new_bytes(&path, &bytes, true).await {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
        Err(_) => return Err(internal_error()),
    }
    Ok((StatusCode::ACCEPTED, Json(ReceiptResponse { receipt_id })))
}

fn enforce_rate_limit(
    state: &AppState,
    address: IpAddr,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    let now = Instant::now();
    let mut limits = state.rate_limits.lock().map_err(|_| internal_error())?;
    if limits.len() > 2_048 {
        limits.retain(|_, window| now.duration_since(window.started_at) < RATE_LIMIT_WINDOW);
    }
    let window = limits.entry(address).or_insert(RateWindow {
        started_at: now,
        count: 0,
    });
    if now.duration_since(window.started_at) >= RATE_LIMIT_WINDOW {
        window.started_at = now;
        window.count = 0;
    }
    if window.count >= RATE_LIMIT_REPORTS {
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            Json(ErrorResponse {
                error: "report rate limit exceeded",
            }),
        ));
    }
    window.count += 1;
    Ok(())
}

fn validate_envelope(
    recipient: &PublicRecipientKey,
    envelope: &SealedSafetyReportV1,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    let kem_output = STANDARD_NO_PAD
        .decode(&envelope.kem_output_base64)
        .map_err(|_| invalid_report())?;
    let ciphertext = STANDARD_NO_PAD
        .decode(&envelope.ciphertext_base64)
        .map_err(|_| invalid_report())?;
    if envelope.version != 1
        || envelope.recipient_key_id != recipient.recipient_key_id
        || kem_output.len() != 32
        || ciphertext.is_empty()
        || ciphertext.len() > MAX_CIPHERTEXT_BYTES
    {
        return Err(invalid_report());
    }
    Ok(())
}

async fn read_public_key(path: &Path) -> anyhow::Result<PublicRecipientKey> {
    let bytes = fs::read(path)
        .await
        .with_context(|| format!("could not read {}", path.display()))?;
    if bytes.len() > 8_192 {
        bail!("recipient public-key file is too large")
    }
    let recipient: PublicRecipientKey =
        serde_json::from_slice(&bytes).context("recipient public-key file is invalid")?;
    let expected = safety_recipient_key_id(&recipient.public_key_base64)
        .context("recipient public key is invalid")?;
    if recipient.version != 1
        || recipient.recipient_key_id != expected
        || recipient
            .directive_signing_public_key_base64
            .as_deref()
            .is_some_and(|public_key| noise_signature_for_public_key(public_key).is_err())
    {
        bail!("recipient public-key file does not match its identifier")
    }
    Ok(recipient)
}

async fn write_new_json(path: &Path, value: &impl Serialize, private: bool) -> anyhow::Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    write_new_bytes(path, &bytes, private)
        .await
        .with_context(|| format!("could not create {}", path.display()))
}

async fn write_new_bytes(path: &Path, bytes: &[u8], private: bool) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await?;
    }
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(if private { 0o600 } else { 0o644 });
    let mut file = options.open(path).await?;
    if private && let Err(error) = secure_file(path).await {
        let _ = fs::remove_file(path).await;
        return Err(error);
    }
    if let Err(error) = file.write_all(bytes).await {
        let _ = fs::remove_file(path).await;
        return Err(error);
    }
    file.sync_all().await
}

#[cfg(unix)]
async fn secure_file(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).await
}

#[cfg(not(unix))]
async fn secure_file(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
async fn secure_directory(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).await
}

#[cfg(not(unix))]
async fn secure_directory(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

fn invalid_report() -> (StatusCode, Json<ErrorResponse>) {
    (
        StatusCode::BAD_REQUEST,
        Json(ErrorResponse {
            error: "invalid sealed report",
        }),
    )
}

fn internal_error() -> (StatusCode, Json<ErrorResponse>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse {
            error: "report intake failed",
        }),
    )
}

#[cfg(test)]
mod tests {
    use noise_core::{
        DirectMessagePolicy, GroupMembership, Identity, Profile, SafetyReportCategoryV1,
        SafetyReportV1, SignedEvent,
    };

    use super::*;

    #[test]
    fn intake_accepts_a_current_sealed_report_for_its_public_key() {
        let recipient = SafetyEncryptionKeyPair::generate().unwrap();
        let reporter = Identity::generate();
        let author = Identity::generate();
        let group = GroupMembership::create("safety test");
        let reported_event = SignedEvent::chat(&author, &group, "synthetic threat", 1).unwrap();
        let context = SignedEvent::member_joined(
            &reporter,
            &group,
            &Profile {
                username: "reporter".to_owned(),
                bio: String::new(),
                avatar: None,
                album: None,
                accepts_direct_messages: true,
                direct_message_policy: DirectMessagePolicy::Everyone,
            },
            1,
        )
        .unwrap();
        let report = SafetyReportV1::create(
            &reporter,
            SafetyReportCategoryV1::ThreatsOrImmediateDanger,
            reported_event,
            Some(context),
            None,
            Vec::new(),
            Some("synthetic threat".to_owned()),
            Some("synthetic test report".to_owned()),
        )
        .unwrap();
        let envelope = report.seal(&recipient.public_key_base64()).unwrap();
        let public = PublicRecipientKey {
            version: 1,
            recipient_key_id: recipient.key_id(),
            public_key_base64: recipient.public_key_base64(),
            directive_signing_public_key_base64: Some(
                recipient
                    .directive_signing_key_pair()
                    .unwrap()
                    .public_key_base64(),
            ),
        };

        assert!(validate_envelope(&public, &envelope).is_ok());
        assert_eq!(recipient.open(&envelope).unwrap(), report);
    }
}
