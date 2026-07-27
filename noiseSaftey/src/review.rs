use std::{
    fmt::Write as _,
    io::ErrorKind,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, bail};
use axum::{
    Router,
    body::Body,
    extract::{DefaultBodyLimit, Path as RoutePath, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use noise_core::{
    SafetyEncryptionKeyPair, SafetyReportCategoryV1, SafetyReportV1, SealedSafetyReportV1,
};
use rand::random;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::{fs, io::AsyncWriteExt, net::TcpListener};
use zeroize::Zeroize;

use super::{PrivateRecipientKey, secure_directory, secure_file};

const MAX_ENVELOPE_FILE_BYTES: usize = 384 * 1024;
const MAX_SECRET_FILE_BYTES: usize = 8 * 1024;

#[derive(Clone)]
struct ReviewState {
    recipient: Arc<SafetyEncryptionKeyPair>,
    recipient_key_id: String,
    spool_dir: PathBuf,
    state_dir: PathBuf,
    token: String,
    expected_host: String,
}

struct ReviewItem {
    receipt_id: String,
    reviewed: bool,
    report: Result<SafetyReportV1, &'static str>,
}

impl ReviewItem {
    fn created_at_millis(&self) -> u64 {
        self.report
            .as_ref()
            .map_or(0, |report| report.created_at_millis)
    }
}

pub(crate) async fn serve(
    secret_key_file: &Path,
    spool_dir: &Path,
    configured_state_dir: Option<&Path>,
    bind: SocketAddr,
) -> anyhow::Result<()> {
    if !bind.ip().is_loopback() {
        bail!("the private reviewer can bind only to a loopback address")
    }
    let recipient = read_private_key(secret_key_file).await?;
    let spool_metadata = fs::metadata(spool_dir)
        .await
        .with_context(|| format!("could not read report inbox {}", spool_dir.display()))?;
    if !spool_metadata.is_dir() {
        bail!("the report inbox is not a directory")
    }
    let state_dir = configured_state_dir
        .map(Path::to_owned)
        .unwrap_or_else(|| spool_dir.join(".review-state"));
    fs::create_dir_all(&state_dir)
        .await
        .with_context(|| format!("could not create {}", state_dir.display()))?;
    secure_directory(&state_dir).await?;

    let token = URL_SAFE_NO_PAD.encode(random::<[u8; 32]>());
    let state = ReviewState {
        recipient_key_id: recipient.key_id(),
        recipient: Arc::new(recipient),
        spool_dir: spool_dir.to_owned(),
        state_dir,
        token,
        expected_host: bind.to_string(),
    };
    let router = Router::new()
        .route("/{token}", get(review_index))
        .route(
            "/{token}/reports/{receipt_id}/reviewed",
            post(mark_reviewed),
        )
        .layer(DefaultBodyLimit::max(1_024))
        .with_state(state.clone());
    let listener = TcpListener::bind(bind)
        .await
        .with_context(|| format!("could not bind private reviewer to {bind}"))?;
    let review_url = format!("http://{bind}/{}", state.token);
    println!("noise safety reviewer listening locally");
    println!("open this private URL: {review_url}");
    println!("recipient key id: {}", state.recipient_key_id);
    std::io::Write::flush(&mut std::io::stdout())?;
    axum::serve(listener, router)
        .await
        .context("noise safety reviewer stopped")
}

async fn review_index(
    RoutePath(token): RoutePath<String>,
    State(state): State<ReviewState>,
    headers: HeaderMap,
) -> Response {
    if !authorized(&state, &token, &headers) {
        return reviewer_error(StatusCode::NOT_FOUND, "reviewer not found");
    }
    match load_reports(&state).await {
        Ok(reports) => secure_html(StatusCode::OK, render_index(&state, &reports)),
        Err(_) => reviewer_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "the private report inbox could not be read",
        ),
    }
}

async fn mark_reviewed(
    RoutePath((token, receipt_id)): RoutePath<(String, String)>,
    State(state): State<ReviewState>,
    headers: HeaderMap,
) -> Response {
    if !authorized(&state, &token, &headers) || !valid_receipt_id(&receipt_id) {
        return reviewer_error(StatusCode::NOT_FOUND, "reviewer not found");
    }
    if verify_report_file(&state, &receipt_id).await.is_err() {
        return reviewer_error(StatusCode::BAD_REQUEST, "this report could not be verified");
    }
    let marker_path = reviewed_marker_path(&state.state_dir, &receipt_id);
    let reviewed_at_millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis());
    let marker = format!("reviewed_at_unix_millis={reviewed_at_millis}\n");
    match write_private_marker(&marker_path, marker.as_bytes()).await {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
        Err(_) => {
            return reviewer_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "the reviewed state could not be saved",
            );
        }
    }
    secure_redirect(&format!("/{token}"))
}

async fn load_reports(state: &ReviewState) -> anyhow::Result<Vec<ReviewItem>> {
    let mut directory = fs::read_dir(&state.spool_dir).await?;
    let mut reports = Vec::new();
    while let Some(entry) = directory.next_entry().await? {
        if !entry.file_type().await?.is_file() {
            continue;
        }
        let Some(file_name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some(receipt_id) = file_name.strip_suffix(".json").map(str::to_owned) else {
            continue;
        };
        if !valid_receipt_id(&receipt_id) {
            continue;
        }
        let reviewed = fs::metadata(reviewed_marker_path(&state.state_dir, &receipt_id))
            .await
            .is_ok();
        let report = match read_and_verify_report(
            state.recipient.as_ref(),
            &entry.path(),
            &receipt_id,
        )
        .await
        {
            Ok(report) => Ok(report),
            Err(_) => Err("could not decrypt or cryptographically verify this envelope"),
        };
        reports.push(ReviewItem {
            receipt_id,
            reviewed,
            report,
        });
    }
    reports.sort_by(|left, right| {
        left.reviewed
            .cmp(&right.reviewed)
            .then_with(|| right.created_at_millis().cmp(&left.created_at_millis()))
    });
    Ok(reports)
}

async fn verify_report_file(
    state: &ReviewState,
    receipt_id: &str,
) -> anyhow::Result<SafetyReportV1> {
    read_and_verify_report(
        state.recipient.as_ref(),
        &state.spool_dir.join(format!("{receipt_id}.json")),
        receipt_id,
    )
    .await
}

async fn read_and_verify_report(
    recipient: &SafetyEncryptionKeyPair,
    path: &Path,
    receipt_id: &str,
) -> anyhow::Result<SafetyReportV1> {
    let metadata = fs::symlink_metadata(path).await?;
    if !metadata.file_type().is_file() || metadata.len() as usize > MAX_ENVELOPE_FILE_BYTES {
        bail!("invalid report envelope file")
    }
    let bytes = fs::read(path).await?;
    if blake3::hash(&bytes).to_hex().as_str() != receipt_id {
        bail!("report receipt does not match the stored envelope")
    }
    let envelope: SealedSafetyReportV1 =
        serde_json::from_slice(&bytes).context("invalid sealed report")?;
    recipient
        .open(&envelope)
        .context("report could not be opened")
}

async fn read_private_key(path: &Path) -> anyhow::Result<SafetyEncryptionKeyPair> {
    let metadata = fs::symlink_metadata(path)
        .await
        .with_context(|| format!("could not read {}", path.display()))?;
    if !metadata.file_type().is_file() || metadata.len() as usize > MAX_SECRET_FILE_BYTES {
        bail!("recipient private-key file is invalid")
    }
    ensure_private_file_permissions(&metadata)?;
    let mut bytes = fs::read(path)
        .await
        .with_context(|| format!("could not read {}", path.display()))?;
    let private_result =
        serde_json::from_slice(&bytes).context("recipient private-key file is invalid");
    bytes.zeroize();
    let mut private: PrivateRecipientKey = private_result?;
    let recipient_result = SafetyEncryptionKeyPair::from_secret_base64(&private.secret_base64)
        .context("recipient private key is invalid");
    private.secret_base64.zeroize();
    let recipient = recipient_result?;
    if private.version != 1 || private.recipient_key_id != recipient.key_id() {
        bail!("recipient private-key file does not match its identifier")
    }
    Ok(recipient)
}

#[cfg(unix)]
fn ensure_private_file_permissions(metadata: &std::fs::Metadata) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if metadata.permissions().mode() & 0o077 != 0 {
        bail!("recipient private-key file must not be accessible by group or other users")
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_private_file_permissions(_metadata: &std::fs::Metadata) -> anyhow::Result<()> {
    Ok(())
}

async fn write_private_marker(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(path).await?;
    if let Err(error) = secure_file(path).await {
        let _ = fs::remove_file(path).await;
        return Err(error);
    }
    if let Err(error) = file.write_all(bytes).await {
        let _ = fs::remove_file(path).await;
        return Err(error);
    }
    file.sync_all().await
}

fn authorized(state: &ReviewState, token: &str, headers: &HeaderMap) -> bool {
    let valid_host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|host| host == state.expected_host);
    valid_host && constant_time_equal(token.as_bytes(), state.token.as_bytes())
}

fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

fn valid_receipt_id(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn reviewed_marker_path(state_dir: &Path, receipt_id: &str) -> PathBuf {
    state_dir.join(format!("{receipt_id}.reviewed"))
}

fn render_index(state: &ReviewState, reports: &[ReviewItem]) -> String {
    let new_count = reports.iter().filter(|report| !report.reviewed).count();
    let mut html = String::with_capacity(16_384);
    html.push_str(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>noise safety</title><style>
:root{color-scheme:dark;font-family:Inter,ui-sans-serif,-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif;background:#171619;color:#f4f0f5}
*{box-sizing:border-box}body{margin:0;background:radial-gradient(circle at 20% 0,#302538 0,transparent 34rem),#171619}
main{width:min(980px,calc(100% - 32px));margin:0 auto;padding:52px 0 80px}
header{display:flex;align-items:flex-end;justify-content:space-between;gap:24px;margin-bottom:28px}
.eyebrow{margin:0 0 7px;color:#b8ff38;font-size:11px;font-weight:800;letter-spacing:.16em}
h1{margin:0;font-size:clamp(34px,6vw,62px);line-height:.95;letter-spacing:-.055em}
.lede{max-width:660px;margin:13px 0 0;color:#aaa2ad;font-size:14px;line-height:1.55}
.refresh{flex:none;padding:10px 14px;border:1px solid #ffffff18;border-radius:10px;background:#ffffff0a;color:#e8e1ea;text-decoration:none;font-size:12px;font-weight:700}
.summary{display:flex;gap:8px;margin:0 0 18px}.summary span,.badge{padding:5px 8px;border-radius:999px;background:#ffffff0b;color:#bfb6c2;font-size:10px;font-weight:750;text-transform:uppercase;letter-spacing:.06em}
.summary .new,.badge.verified{background:#b8ff3818;color:#c8ff6b}.empty{padding:46px;border:1px dashed #ffffff20;border-radius:18px;color:#9e95a1;text-align:center}
.reports{display:grid;gap:14px}.report{overflow:hidden;border:1px solid #ffffff12;border-radius:18px;background:#201e22cc;box-shadow:0 18px 55px #0000002b}
.report.reviewed{opacity:.7}.report.invalid{border-color:#ff718555}.report-head{display:flex;align-items:flex-start;justify-content:space-between;gap:18px;padding:18px 20px;border-bottom:1px solid #ffffff0d}
.report-head h2{margin:4px 0 0;font-size:17px;letter-spacing:-.02em}.report-head p{margin:6px 0 0;color:#958c98;font:11px ui-monospace,SFMono-Regular,Menlo,monospace}
.report-body{display:grid;gap:14px;padding:20px}.content{padding:15px;border-radius:12px;background:#121114}.content h3,.facts dt{margin:0 0 7px;color:#8f8692;font-size:9px;font-weight:800;letter-spacing:.13em;text-transform:uppercase}
pre{margin:0;color:#f5eff7;font:13px/1.58 ui-monospace,SFMono-Regular,Menlo,monospace;white-space:pre-wrap;overflow-wrap:anywhere}
.muted{color:#938a96}.facts{display:grid;grid-template-columns:repeat(2,minmax(0,1fr));gap:12px;margin:0}.facts div{min-width:0;padding:12px;border:1px solid #ffffff0c;border-radius:10px}
.facts dd{margin:0;color:#d5ced7;font-size:11px;line-height:1.5;overflow-wrap:anywhere}.facts code{font:10px/1.45 ui-monospace,SFMono-Regular,Menlo,monospace}
.report-foot{display:flex;align-items:center;justify-content:space-between;gap:14px;padding:14px 20px;border-top:1px solid #ffffff0d;background:#17161980}
.report-foot span{color:#958c98;font-size:10px}.report-foot button{padding:9px 12px;border:0;border-radius:9px;background:#b8ff38;color:#192000;font-size:11px;font-weight:850;cursor:pointer}
@media(max-width:680px){main{width:min(100% - 20px,980px);padding-top:30px}header{align-items:flex-start;flex-direction:column}.facts{grid-template-columns:1fr}.report-head{flex-direction:column}}
</style></head><body><main><header><div><p class="eyebrow">LOCAL / PRIVATE / VERIFIED</p><h1>noise safety</h1>
<p class="lede">This reviewer reads encrypted envelope files from the local inbox. It never accepts internet connections and never renders or downloads reported media.</p></div>"#,
    );
    let _ = write!(
        html,
        r#"<a class="refresh" href="/{}">refresh inbox</a></header><div class="summary"><span class="new">{} new</span><span>{} total</span><span>key {}</span></div>"#,
        state.token,
        new_count,
        reports.len(),
        short_identifier(&state.recipient_key_id)
    );
    if reports.is_empty() {
        html.push_str(
            r#"<div class="empty">No encrypted reports are waiting in this local inbox.</div>"#,
        );
    } else {
        html.push_str(r#"<section class="reports">"#);
        for item in reports {
            render_report(&mut html, state, item);
        }
        html.push_str("</section>");
    }
    html.push_str("</main></body></html>");
    html
}

fn render_report(html: &mut String, state: &ReviewState, item: &ReviewItem) {
    let reviewed_class = if item.reviewed { " reviewed" } else { "" };
    let Ok(report) = &item.report else {
        let _ = write!(
            html,
            r#"<article class="report invalid"><div class="report-head"><div><span class="badge">invalid envelope</span><h2>report could not be verified</h2><p>{}</p></div></div><div class="report-body"><div class="content"><pre>{}</pre></div></div></article>"#,
            escape_html(&item.receipt_id),
            item.report.as_ref().unwrap_err()
        );
        return;
    };
    let text = report.reported_text_excerpt.as_deref();
    let details = report.details.as_deref();
    let shard_count = report
        .encrypted_objects
        .iter()
        .map(|object| object.shards.len())
        .sum::<usize>();
    let _ = write!(
        html,
        r#"<article class="report{}"><div class="report-head"><div><span class="badge verified">signatures verified</span><h2>{}</h2><p>{}</p></div><span class="badge">{}</span></div><div class="report-body">"#,
        reviewed_class,
        category_label(report.category),
        escape_html(&item.receipt_id),
        if item.reviewed { "reviewed" } else { "new" }
    );
    if let Some(text) = text {
        let _ = write!(
            html,
            r#"<div class="content"><h3>reported message text</h3><pre>{}</pre></div>"#,
            escape_html(text)
        );
    } else {
        html.push_str(
            r#"<div class="content"><h3>reported message text</h3><pre class="muted">No text was included. This may be a media-only message or an envelope created before text inclusion was enabled.</pre></div>"#,
        );
    }
    if let Some(details) = details {
        let _ = write!(
            html,
            r#"<div class="content"><h3>reporter statement</h3><pre>{}</pre></div>"#,
            escape_html(details)
        );
    }
    let media_summary = if report.encrypted_objects.is_empty() {
        "No encrypted media objects were referenced.".to_owned()
    } else {
        format!(
            "{} encrypted object location(s), {} shard location(s). No media bytes, previews, or keys were loaded.",
            report.encrypted_objects.len(),
            shard_count
        )
    };
    let _ = write!(
        html,
        r#"<dl class="facts"><div><dt>reported author</dt><dd><code>{}</code></dd></div><div><dt>reporter</dt><dd><code>{}</code></dd></div><div><dt>group / event</dt><dd><code>{}<br>{}</code></dd></div><div><dt>time</dt><dd>reported {}<br>event signed {}</dd></div><div><dt>group context</dt><dd>{}</dd></div><div><dt>media boundary</dt><dd>{}</dd></div></dl></div><div class="report-foot"><span>Cryptographic validity does not itself determine whether content violates policy.</span>"#,
        escape_html(&report.reported_event.author_public_key),
        escape_html(&report.reporter_public_key),
        escape_html(&report.reported_event.group_id),
        escape_html(&report.reported_event.event_id),
        format_timestamp(report.created_at_millis),
        format_timestamp(report.reported_event.created_at_millis),
        if report.group_context_proof.is_some() {
            "verified reporter-authored event for this group"
        } else {
            "no additional reporter-authored group event included"
        },
        escape_html(&media_summary)
    );
    if item.reviewed {
        html.push_str(r#"<span class="badge verified">reviewed</span>"#);
    } else {
        let _ = write!(
            html,
            r#"<form method="post" action="/{}/reports/{}/reviewed"><button type="submit">mark reviewed</button></form>"#,
            state.token, item.receipt_id
        );
    }
    html.push_str("</div></article>");
}

fn category_label(category: SafetyReportCategoryV1) -> &'static str {
    match category {
        SafetyReportCategoryV1::GroupRules => "group rules",
        SafetyReportCategoryV1::HarassmentOrHatefulBehavior => "harassment or hateful behavior",
        SafetyReportCategoryV1::SpamScamOrImpersonation => "spam, scam, or impersonation",
        SafetyReportCategoryV1::ThreatsOrImmediateDanger => "threats or immediate danger",
        SafetyReportCategoryV1::SexualExploitationOrNonConsensualSexualContent => {
            "sexual exploitation or non-consensual content"
        }
        SafetyReportCategoryV1::ChildSafety => "child safety",
        SafetyReportCategoryV1::ExplicitContentNotProperlyLabeled => {
            "unlabeled sexually explicit content"
        }
        SafetyReportCategoryV1::Other => "something else",
    }
}

fn format_timestamp(millis: u64) -> String {
    let nanos = i128::from(millis) * 1_000_000;
    OffsetDateTime::from_unix_timestamp_nanos(nanos)
        .ok()
        .and_then(|timestamp| timestamp.format(&Rfc3339).ok())
        .unwrap_or_else(|| format!("{millis} ms since Unix epoch"))
}

fn short_identifier(value: &str) -> String {
    if value.len() <= 20 {
        return escape_html(value);
    }
    format!(
        "{}…{}",
        escape_html(&value[..12]),
        escape_html(&value[value.len() - 8..])
    )
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn secure_html(status: StatusCode, body: String) -> Response {
    let mut response = (status, Html(body)).into_response();
    apply_security_headers(&mut response);
    response
}

fn reviewer_error(status: StatusCode, message: &str) -> Response {
    secure_html(
        status,
        format!(
            "<!doctype html><title>noise safety</title><p>{}</p>",
            escape_html(message)
        ),
    )
}

fn secure_redirect(location: &str) -> Response {
    let mut response = Redirect::to(location).into_response();
    apply_security_headers(&mut response);
    response
}

fn apply_security_headers(response: &mut Response<Body>) {
    let headers = response.headers_mut();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, max-age=0"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'none'; style-src 'unsafe-inline'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'",
        ),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reviewer_escapes_untrusted_report_text() {
        assert_eq!(
            escape_html("<script>alert('report')</script>"),
            "&lt;script&gt;alert(&#39;report&#39;)&lt;/script&gt;"
        );
    }

    #[test]
    fn reviewer_tokens_require_an_exact_match() {
        assert!(constant_time_equal(b"private-token", b"private-token"));
        assert!(!constant_time_equal(b"private-token", b"public-token"));
        assert!(!constant_time_equal(b"short", b"longer"));
    }
}
