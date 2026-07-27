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
    SafetyDirectiveV1, SafetyEncryptionKeyPair, SealedSafetyReportV1,
    noise_signature_for_public_key, safety_recipient_key_id,
};
use serde::{Deserialize, Serialize};
use tokio::{
    fs,
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use tower_http::cors::CorsLayer;

mod review;

const DEFAULT_BIND: &str = "127.0.0.1:4310";
const DEFAULT_REVIEW_BIND: &str = "127.0.0.1:4311";
const MAX_REPORT_BODY_BYTES: usize = 384 * 1024;
const MAX_CIPHERTEXT_BYTES: usize = 256 * 1024;
const MAX_DIRECTIVE_BYTES: usize = 64 * 1024;
const MAX_DIRECTIVES: usize = 10_000;
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
        /// Publish verified, content-free decisions from this read-only directory.
        #[arg(long)]
        directive_dir: Option<PathBuf>,
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
    /// List verified encrypted-report receipt identifiers for restricted sync.
    #[command(hide = true)]
    SyncListReports {
        #[arg(long)]
        spool_dir: PathBuf,
    },
    /// Write one verified encrypted report envelope to stdout for restricted sync.
    #[command(hide = true)]
    SyncReadReport {
        #[arg(long)]
        public_key_file: PathBuf,
        #[arg(long)]
        spool_dir: PathBuf,
        #[arg(long)]
        receipt_id: String,
    },
    /// Install one correctly signed, content-free directive from stdin.
    #[command(hide = true)]
    SyncInstallDirective {
        #[arg(long)]
        public_key_file: PathBuf,
        #[arg(long)]
        directive_dir: PathBuf,
        #[arg(long)]
        directive_id: String,
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
struct DirectiveFeedResponse {
    version: u32,
    directives: Vec<SafetyDirectiveV1>,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: &'static str,
}

#[derive(Clone)]
struct AppState {
    recipient: PublicRecipientKey,
    spool_dir: PathBuf,
    directive_dir: Option<PathBuf>,
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
            directive_dir,
            bind,
        } => serve(&public_key_file, &spool_dir, directive_dir.as_deref(), bind).await,
        Command::Review {
            secret_key_file,
            spool_dir,
            state_dir,
            bind,
        } => review::serve(&secret_key_file, &spool_dir, state_dir.as_deref(), bind).await,
        Command::SyncListReports { spool_dir } => sync_list_reports(&spool_dir).await,
        Command::SyncReadReport {
            public_key_file,
            spool_dir,
            receipt_id,
        } => sync_read_report(&public_key_file, &spool_dir, &receipt_id).await,
        Command::SyncInstallDirective {
            public_key_file,
            directive_dir,
            directive_id,
        } => sync_install_directive(&public_key_file, &directive_dir, &directive_id).await,
    }
}

async fn sync_list_reports(spool_dir: &Path) -> anyhow::Result<()> {
    let mut entries = fs::read_dir(spool_dir)
        .await
        .with_context(|| format!("could not read {}", spool_dir.display()))?;
    let mut receipt_ids = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        if !entry.file_type().await?.is_file() {
            continue;
        }
        let Some(receipt_id) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.strip_suffix(".json"))
            .map(str::to_owned)
        else {
            continue;
        };
        if !valid_hex_identifier(&receipt_id) {
            continue;
        }
        let metadata = entry.metadata().await?;
        if metadata.len() == 0 || metadata.len() as usize > MAX_REPORT_BODY_BYTES {
            bail!("invalid encrypted report file")
        }
        receipt_ids.push(receipt_id);
        if receipt_ids.len() > MAX_DIRECTIVES {
            bail!("the encrypted report inbox is too large")
        }
    }
    receipt_ids.sort();
    let mut stdout = tokio::io::stdout();
    for receipt_id in receipt_ids {
        stdout.write_all(receipt_id.as_bytes()).await?;
        stdout.write_all(b"\n").await?;
    }
    stdout.flush().await?;
    Ok(())
}

async fn sync_read_report(
    public_key_file: &Path,
    spool_dir: &Path,
    receipt_id: &str,
) -> anyhow::Result<()> {
    if !valid_hex_identifier(receipt_id) {
        bail!("invalid report receipt identifier")
    }
    let recipient = read_public_key(public_key_file).await?;
    let bytes = read_verified_envelope(&recipient, spool_dir, receipt_id).await?;
    let mut stdout = tokio::io::stdout();
    stdout.write_all(&bytes).await?;
    stdout.flush().await?;
    Ok(())
}

async fn read_verified_envelope(
    recipient: &PublicRecipientKey,
    spool_dir: &Path,
    receipt_id: &str,
) -> anyhow::Result<Vec<u8>> {
    let path = spool_dir.join(format!("{receipt_id}.json"));
    let metadata = fs::symlink_metadata(&path)
        .await
        .with_context(|| format!("could not read report {receipt_id}"))?;
    if !metadata.file_type().is_file() || metadata.len() as usize > MAX_REPORT_BODY_BYTES {
        bail!("invalid encrypted report file")
    }
    let bytes = fs::read(&path).await?;
    if blake3::hash(&bytes).to_hex().as_str() != receipt_id {
        bail!("encrypted report receipt does not match its contents")
    }
    let envelope: SealedSafetyReportV1 =
        serde_json::from_slice(&bytes).context("invalid encrypted report")?;
    validate_envelope(recipient, &envelope)
        .map_err(|_| anyhow::anyhow!("invalid encrypted report"))?;
    Ok(bytes)
}

async fn sync_install_directive(
    public_key_file: &Path,
    directive_dir: &Path,
    directive_id: &str,
) -> anyhow::Result<()> {
    if !valid_hex_identifier(directive_id) {
        bail!("invalid safety directive identifier")
    }
    let recipient = read_public_key(public_key_file).await?;
    let signing_public_key = recipient
        .directive_signing_public_key_base64
        .as_deref()
        .context("the public key file is missing its directive signing public key")?;
    let mut bytes = Vec::new();
    tokio::io::stdin()
        .take((MAX_DIRECTIVE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .await?;
    if bytes.is_empty() || bytes.len() > MAX_DIRECTIVE_BYTES {
        bail!("invalid safety directive file")
    }
    let directive = verified_sync_directive(&bytes, directive_id, signing_public_key)?;

    let path = directive_dir.join(format!("{directive_id}.json"));
    match write_new_bytes(&path, &bytes, true).await {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            let existing_bytes = fs::read(&path).await?;
            let existing: SafetyDirectiveV1 = serde_json::from_slice(&existing_bytes)
                .context("existing safety directive is invalid")?;
            if existing != directive {
                bail!("a different safety directive already uses this identifier")
            }
        }
        Err(error) => return Err(error.into()),
    }
    println!("installed");
    Ok(())
}

fn verified_sync_directive(
    bytes: &[u8],
    directive_id: &str,
    signing_public_key: &str,
) -> anyhow::Result<SafetyDirectiveV1> {
    let directive: SafetyDirectiveV1 =
        serde_json::from_slice(bytes).context("invalid safety directive")?;
    if directive.directive_id != directive_id {
        bail!("safety directive identifier does not match its contents")
    }
    directive
        .verify_with_signing_public_key(signing_public_key)
        .context("invalid safety directive signature")?;
    Ok(directive)
}

fn valid_hex_identifier(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
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

async fn serve(
    public_key_file: &Path,
    spool_dir: &Path,
    directive_dir: Option<&Path>,
    bind: SocketAddr,
) -> anyhow::Result<()> {
    let recipient = read_public_key(public_key_file).await?;
    if directive_dir.is_some() && recipient.directive_signing_public_key_base64.is_none() {
        bail!("the public key file is missing its directive signing public key")
    }
    fs::create_dir_all(spool_dir)
        .await
        .with_context(|| format!("could not create {}", spool_dir.display()))?;
    secure_directory(spool_dir).await?;
    if let Some(directive_dir) = directive_dir {
        fs::create_dir_all(directive_dir)
            .await
            .with_context(|| format!("could not create {}", directive_dir.display()))?;
    }
    let state = AppState {
        recipient,
        spool_dir: spool_dir.to_owned(),
        directive_dir: directive_dir.map(Path::to_owned),
        rate_limits: Arc::new(Mutex::new(HashMap::new())),
    };
    let cors = CorsLayer::new()
        .allow_origin([
            HeaderValue::from_static("http://127.0.0.1:1420"),
            HeaderValue::from_static("http://localhost:1420"),
            HeaderValue::from_static("https://app.makenoise.chat"),
        ])
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([header::CONTENT_TYPE]);
    let router = Router::new()
        .route("/health/ready", get(ready))
        .route("/v1/key", get(recipient_key))
        .route("/v1/directives", get(directive_feed))
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

async fn directive_feed(
    State(state): State<AppState>,
) -> Result<Json<DirectiveFeedResponse>, (StatusCode, Json<ErrorResponse>)> {
    let Some(directory) = state.directive_dir.as_deref() else {
        return Ok(Json(DirectiveFeedResponse {
            version: 1,
            directives: Vec::new(),
        }));
    };
    let signing_public_key = state
        .recipient
        .directive_signing_public_key_base64
        .as_deref()
        .ok_or_else(internal_error)?;
    let mut entries = fs::read_dir(directory)
        .await
        .map_err(|_| internal_error())?;
    let mut directives = Vec::new();
    while let Some(entry) = entries.next_entry().await.map_err(|_| internal_error())? {
        if directives.len() >= MAX_DIRECTIVES {
            return Err(internal_error());
        }
        let file_type = entry.file_type().await.map_err(|_| internal_error())?;
        if !file_type.is_file()
            || entry.path().extension().and_then(|value| value.to_str()) != Some("json")
        {
            continue;
        }
        let bytes = fs::read(entry.path()).await.map_err(|_| internal_error())?;
        if bytes.is_empty() || bytes.len() > MAX_DIRECTIVE_BYTES {
            return Err(internal_error());
        }
        let directive: SafetyDirectiveV1 =
            serde_json::from_slice(&bytes).map_err(|_| internal_error())?;
        directive
            .verify_with_signing_public_key(signing_public_key)
            .map_err(|_| internal_error())?;
        directives.push(directive);
    }
    directives.sort_by(|left, right| {
        left.issued_at_millis
            .cmp(&right.issued_at_millis)
            .then_with(|| left.directive_id.cmp(&right.directive_id))
    });
    Ok(Json(DirectiveFeedResponse {
        version: 1,
        directives,
    }))
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

    #[test]
    fn restricted_sync_accepts_only_the_pinned_directive_signature_and_id() {
        let recipient = SafetyEncryptionKeyPair::generate().unwrap();
        let signer = recipient.directive_signing_key_pair().unwrap();
        let directive = SafetyDirectiveV1::suppress_event(
            &signer,
            SafetyReportCategoryV1::ChildSafety,
            "ab".repeat(32),
            "cd".repeat(32),
        )
        .unwrap();
        let bytes = serde_json::to_vec(&directive).unwrap();

        assert!(
            verified_sync_directive(&bytes, &directive.directive_id, &signer.public_key_base64(),)
                .is_ok()
        );
        assert!(
            verified_sync_directive(&bytes, &"ef".repeat(32), &signer.public_key_base64()).is_err()
        );
        let other_signer = SafetyEncryptionKeyPair::generate()
            .unwrap()
            .directive_signing_key_pair()
            .unwrap();
        assert!(
            verified_sync_directive(
                &bytes,
                &directive.directive_id,
                &other_signer.public_key_base64(),
            )
            .is_err()
        );
    }
}
