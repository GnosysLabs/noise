use std::{fmt::Write as _, net::SocketAddr, path::Path, sync::Arc};

use anyhow::{Context, bail};
use axum::{
    Router,
    body::Body,
    extract::State,
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::get,
};
use deadpool_postgres::{Manager, ManagerConfig, Pool, RecyclingMethod};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::net::{TcpListener, UnixListener};
use tokio_postgres::NoTls;

const TAILSCALE_LOGIN_HEADER: HeaderName = HeaderName::from_static("tailscale-user-login");
const FORWARDED_HOST_HEADER: HeaderName = HeaderName::from_static("x-forwarded-host");

#[derive(Clone)]
struct AdminState {
    database: Pool,
    expected_host: String,
    tailscale_logins: Arc<Vec<String>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AdminPage {
    Overview,
    Usage,
    Infrastructure,
    Audit,
}

impl AdminPage {
    fn title(self) -> &'static str {
        match self {
            Self::Overview => "today",
            Self::Usage => "people",
            Self::Infrastructure => "service",
            Self::Audit => "audit log",
        }
    }

    fn path(self) -> &'static str {
        match self {
            Self::Overview => "/",
            Self::Usage => "/usage",
            Self::Infrastructure => "/infrastructure",
            Self::Audit => "/audit",
        }
    }
}

struct AdminSnapshot {
    captured_at: String,
    #[allow(dead_code)]
    schema_version: i32,
    database_bytes: i64,
    total_accounts: i64,
    active_accounts: i64,
    new_accounts_24h: i64,
    new_accounts_7d: i64,
    active_accounts_24h: i64,
    active_accounts_7d: i64,
    active_accounts_30d: i64,
    active_devices: i64,
    active_groups: i64,
    new_groups_7d: i64,
    memberships: i64,
    total_events: i64,
    events_24h: i64,
    events_7d: i64,
    group_events_24h: i64,
    direct_events_24h: i64,
    authors_24h: i64,
    available_media: i64,
    media_bytes: i64,
    media_7d: i64,
    active_sessions: i64,
    active_push_subscriptions: i64,
    active_restrictions: i64,
    daily: Vec<DailyUsage>,
    audit: Vec<AuditEvent>,
}

struct DailyUsage {
    label: String,
    events: i64,
    authors: i64,
}

struct AuditEvent {
    action: String,
    target_kind: String,
    reason: String,
    issued_at: String,
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn serve(
    database_host: &str,
    database_port: u16,
    database_name: &str,
    database_user: &str,
    database_password: &str,
    configured_tailscale_logins: &[String],
    configured_external_host: Option<&str>,
    unix_socket: Option<&Path>,
    bind: SocketAddr,
) -> anyhow::Result<()> {
    if database_host.trim().is_empty()
        || database_name.trim().is_empty()
        || database_user.trim().is_empty()
        || database_password.is_empty()
    {
        bail!("admin database configuration is incomplete")
    }
    if unix_socket.is_none() && !bind.ip().is_loopback() {
        bail!("the private admin dashboard can bind only to a loopback address")
    }
    if unix_socket.is_some() && configured_external_host.is_none() {
        bail!("an admin Unix socket requires Tailscale dashboard access")
    }
    let tailscale_logins =
        validate_tailscale_access(configured_tailscale_logins, configured_external_host)?;
    let expected_host = configured_external_host
        .map(str::to_owned)
        .unwrap_or_else(|| bind.to_string());

    let mut postgres = tokio_postgres::Config::new();
    postgres
        .host(database_host)
        .port(database_port)
        .dbname(database_name)
        .user(database_user)
        .password(database_password);
    let manager = Manager::from_config(
        postgres,
        NoTls,
        ManagerConfig {
            recycling_method: RecyclingMethod::Verified,
        },
    );
    let database = Pool::builder(manager)
        .max_size(2)
        .build()
        .context("could not create the admin database pool")?;
    let state = AdminState {
        database,
        expected_host,
        tailscale_logins: Arc::new(tailscale_logins),
    };
    load_snapshot(&state)
        .await
        .context("could not read the first admin snapshot")?;

    let router = Router::new()
        .route("/", get(overview))
        .route("/overview", get(overview))
        .route("/usage", get(usage))
        .route("/infrastructure", get(infrastructure))
        .route("/audit", get(audit))
        .with_state(state.clone());

    println!("noise admin dashboard listening on its private transport");
    if state.tailscale_logins.is_empty() {
        println!("open this local URL: http://{bind}/");
    } else {
        println!("open this private URL: https://{}/", state.expected_host);
    }
    std::io::Write::flush(&mut std::io::stdout())?;

    if let Some(socket_path) = unix_socket {
        prepare_unix_socket(socket_path).await?;
        let listener = UnixListener::bind(socket_path)
            .with_context(|| format!("could not bind {}", socket_path.display()))?;
        secure_unix_socket(socket_path).await?;
        axum::serve(listener, router)
            .await
            .context("noise admin dashboard stopped")
    } else {
        let listener = TcpListener::bind(bind)
            .await
            .with_context(|| format!("could not bind private admin dashboard to {bind}"))?;
        axum::serve(listener, router)
            .await
            .context("noise admin dashboard stopped")
    }
}

async fn overview(State(state): State<AdminState>, headers: HeaderMap) -> Response {
    render_page(&state, &headers, AdminPage::Overview).await
}

async fn usage(State(state): State<AdminState>, headers: HeaderMap) -> Response {
    render_page(&state, &headers, AdminPage::Usage).await
}

async fn infrastructure(State(state): State<AdminState>, headers: HeaderMap) -> Response {
    render_page(&state, &headers, AdminPage::Infrastructure).await
}

async fn audit(State(state): State<AdminState>, headers: HeaderMap) -> Response {
    render_page(&state, &headers, AdminPage::Audit).await
}

async fn render_page(state: &AdminState, headers: &HeaderMap, page: AdminPage) -> Response {
    let Some(identity) = authorized_identity(state, headers) else {
        return admin_error(StatusCode::NOT_FOUND, "dashboard not found");
    };
    match load_snapshot(state).await {
        Ok(snapshot) => secure_html(StatusCode::OK, render_dashboard(&snapshot, page, &identity)),
        Err(_) => admin_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "the operational snapshot is temporarily unavailable",
        ),
    }
}

async fn load_snapshot(state: &AdminState) -> anyhow::Result<AdminSnapshot> {
    let client = state
        .database
        .get()
        .await
        .context("could not connect to the admin database")?;
    let totals = client
        .query_one("SELECT * FROM noise_admin.operational_totals", &[])
        .await
        .context("could not query aggregate operational totals")?;

    let daily_rows = client
        .query(
            "SELECT label, events, authors FROM noise_admin.daily_usage ORDER BY day",
            &[],
        )
        .await
        .context("could not query daily usage")?;
    let daily = daily_rows
        .into_iter()
        .map(|row| DailyUsage {
            label: row.get("label"),
            events: row.get("events"),
            authors: row.get("authors"),
        })
        .collect();

    let audit_rows = client
        .query(
            r#"
SELECT
    action,
    target_kind,
    reason_code,
    to_char(issued_at AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI:SS "UTC"') AS issued_at
FROM noise_admin.enforcement_audit
ORDER BY issued_at DESC
LIMIT 50
"#,
            &[],
        )
        .await
        .context("could not query the content-free enforcement audit")?;
    let audit = audit_rows
        .into_iter()
        .map(|row| AuditEvent {
            action: row.get("action"),
            target_kind: row.get("target_kind"),
            reason: row.get("reason_code"),
            issued_at: row.get("issued_at"),
        })
        .collect();

    Ok(AdminSnapshot {
        captured_at: OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .unwrap_or_else(|_| "now".to_owned()),
        schema_version: totals.get("schema_version"),
        database_bytes: totals.get("database_bytes"),
        total_accounts: totals.get("total_accounts"),
        active_accounts: totals.get("active_accounts"),
        new_accounts_24h: totals.get("new_accounts_24h"),
        new_accounts_7d: totals.get("new_accounts_7d"),
        active_accounts_24h: totals.get("active_accounts_24h"),
        active_accounts_7d: totals.get("active_accounts_7d"),
        active_accounts_30d: totals.get("active_accounts_30d"),
        active_devices: totals.get("active_devices"),
        active_groups: totals.get("active_groups"),
        new_groups_7d: totals.get("new_groups_7d"),
        memberships: totals.get("memberships"),
        total_events: totals.get("total_events"),
        events_24h: totals.get("events_24h"),
        events_7d: totals.get("events_7d"),
        group_events_24h: totals.get("group_events_24h"),
        direct_events_24h: totals.get("direct_events_24h"),
        authors_24h: totals.get("authors_24h"),
        available_media: totals.get("available_media"),
        media_bytes: totals.get("media_bytes"),
        media_7d: totals.get("media_7d"),
        active_sessions: totals.get("active_sessions"),
        active_push_subscriptions: totals.get("active_push_subscriptions"),
        active_restrictions: totals.get("active_restrictions"),
        daily,
        audit,
    })
}

fn render_dashboard(snapshot: &AdminSnapshot, page: AdminPage, identity: &str) -> String {
    let mut html = String::with_capacity(24_000);
    html.push_str(DOCUMENT_START);
    html.push_str(crate::brand::BRAND_CSS);
    html.push_str(DOCUMENT_STYLE_END);
    render_navigation(&mut html, page);
    let _ = write!(
        html,
        r#"<main><header class="page-head"><div><p class="eyebrow">private · live</p><h1>{}</h1><p class="lede">{}</p></div><a class="refresh" href="{}">refresh</a></header>"#,
        page.title(),
        page_description(page),
        page.path(),
    );
    match page {
        AdminPage::Overview => render_overview(&mut html, snapshot),
        AdminPage::Usage => render_usage(&mut html, snapshot),
        AdminPage::Infrastructure => render_infrastructure(&mut html, snapshot),
        AdminPage::Audit => render_audit(&mut html, snapshot),
    }
    let _ = write!(
        html,
        r#"<footer><span>signed in as {}</span><span>updated {}</span></footer></main></body></html>"#,
        escape_html(identity),
        escape_html(&snapshot.captured_at),
    );
    html
}

fn render_navigation(html: &mut String, page: AdminPage) {
    html.push_str(r#"<div class="topbar">"#);
    html.push_str(crate::brand::BRAND_MARKUP);
    html.push_str(r#"<nav aria-label="admin sections">"#);
    for candidate in [
        AdminPage::Overview,
        AdminPage::Usage,
        AdminPage::Infrastructure,
        AdminPage::Audit,
    ] {
        let _ = write!(
            html,
            r#"<a class="{}" href="{}">{}</a>"#,
            if page == candidate { "active" } else { "" },
            candidate.path(),
            candidate.title(),
        );
    }
    html.push_str(
        r#"<a href="/safety">safety</a></nav><span class="private-pill">tailnet only</span></div>"#,
    );
}

struct ServiceStatus {
    title: &'static str,
    detail: &'static str,
    badge: &'static str,
    tone: &'static str,
}

fn service_status() -> ServiceStatus {
    ServiceStatus {
        title: "All good",
        detail: "These numbers just loaded from the live database. People can send and receive.",
        badge: "all good",
        tone: "good",
    }
}

fn render_overview(html: &mut String, snapshot: &AdminSnapshot) {
    html.push_str(r#"<section class="metric-grid">"#);
    metric_card(
        html,
        "people using it today",
        snapshot.active_accounts_24h,
        &format!(
            "{} people sent a message",
            format_number(snapshot.authors_24h)
        ),
        "lime",
    );
    metric_card(
        html,
        "messages today",
        snapshot.events_24h,
        &format!(
            "{} in groups · {} in DMs",
            format_number(snapshot.group_events_24h),
            format_number(snapshot.direct_events_24h)
        ),
        "violet",
    );
    metric_card(
        html,
        "groups",
        snapshot.active_groups,
        &format!("{} new this week", format_number(snapshot.new_groups_7d)),
        "orange",
    );
    metric_card(
        html,
        "photos & videos",
        snapshot.available_media,
        &format!("{} stored", format_bytes(snapshot.media_bytes)),
        "blue",
    );
    html.push_str("</section>");
    render_activity_chart(html, snapshot);
    let health = service_status();
    html.push_str(r#"<section class="split"><article class="panel"><div class="panel-head"><div><p class="kicker">quick check</p><h2>"#);
    html.push_str(escape_html(health.title).as_str());
    html.push_str(r#"</h2></div><span class="status "#);
    html.push_str(health.tone);
    html.push_str(r#"">"#);
    html.push_str(escape_html(health.badge).as_str());
    html.push_str(r#"</span></div><div class="rows">"#);
    status_row(
        html,
        "Signed in right now",
        &format_number(snapshot.active_sessions),
        "phones and computers with an open session",
    );
    status_row(
        html,
        "Safety holds",
        &format_number(snapshot.active_restrictions),
        "hidden messages, paused groups, or blocked people",
    );
    html.push_str(r#"</div></article><article class="panel safety-panel"><div><p class="kicker">reports</p><h2>safety</h2><p>Open the report queue when someone flags a serious problem. That page is separate so report details stay off this dashboard.</p></div><a class="primary" href="/safety">open reports</a></article></section>"#);
}

fn render_usage(html: &mut String, snapshot: &AdminSnapshot) {
    html.push_str(r#"<section class="metric-grid compact">"#);
    metric_card(
        html,
        "signed up",
        snapshot.total_accounts,
        &format!(
            "{} still active · {} new today",
            format_number(snapshot.active_accounts),
            format_number(snapshot.new_accounts_24h)
        ),
        "lime",
    );
    metric_card(
        html,
        "new this week",
        snapshot.new_accounts_7d,
        "people who created an account",
        "orange",
    );
    metric_card(
        html,
        "used it this week",
        snapshot.active_accounts_7d,
        "opened noise on a phone or computer",
        "violet",
    );
    metric_card(
        html,
        "used it this month",
        snapshot.active_accounts_30d,
        "opened noise in the last 30 days",
        "blue",
    );
    html.push_str("</section>");
    render_activity_chart(html, snapshot);
    html.push_str(r#"<section class="split"><article class="panel"><div class="panel-head"><div><p class="kicker">groups</p><h2>rooms people made</h2></div></div><div class="rows">"#);
    status_row(
        html,
        "Groups",
        &format_number(snapshot.active_groups),
        "not deleted",
    );
    status_row(
        html,
        "People in groups",
        &format_number(snapshot.memberships),
        "seats across every group",
    );
    status_row(
        html,
        "New groups this week",
        &format_number(snapshot.new_groups_7d),
        "created in the last 7 days",
    );
    status_row(
        html,
        "Phones & computers",
        &format_number(snapshot.active_devices),
        "signed-in devices that still work",
    );
    html.push_str(r#"</div></article><article class="panel"><div class="panel-head"><div><p class="kicker">activity</p><h2>what they sent</h2></div></div><div class="rows">"#);
    status_row(
        html,
        "Group messages today",
        &format_number(snapshot.group_events_24h),
        "in rooms",
    );
    status_row(
        html,
        "DMs today",
        &format_number(snapshot.direct_events_24h),
        "private chats",
    );
    status_row(
        html,
        "Photos & videos this week",
        &format_number(snapshot.media_7d),
        "uploads in the last 7 days",
    );
    status_row(
        html,
        "Messages all time",
        &format_number(snapshot.total_events),
        "everything noise has accepted",
    );
    html.push_str("</div></article></section>");
}

fn render_infrastructure(html: &mut String, snapshot: &AdminSnapshot) {
    let health = service_status();
    html.push_str(r#"<section class="health-banner "#);
    html.push_str(health.tone);
    html.push_str(r#""><strong>"#);
    html.push_str(escape_html(health.title).as_str());
    html.push_str(r#"</strong><span>"#);
    html.push_str(escape_html(health.detail).as_str());
    html.push_str(r#"</span></section><section class="metric-grid compact">"#);
    metric_card(
        html,
        "signed in right now",
        snapshot.active_sessions,
        "phones and computers with an open session",
        "lime",
    );
    metric_card(
        html,
        "can get notifications",
        snapshot.active_push_subscriptions,
        "devices noise can ping",
        "violet",
    );
    text_metric_card(
        html,
        "photo & video storage",
        &format_bytes(snapshot.media_bytes),
        &format!("{} files", format_number(snapshot.available_media)),
        "orange",
    );
    text_metric_card(
        html,
        "database size",
        &format_bytes(snapshot.database_bytes),
        "how much the main database takes",
        "blue",
    );
    html.push_str("</section>");
    html.push_str(r#"<section class="panel"><div class="panel-head"><div><p class="kicker">safety</p><h2>holds in effect</h2></div></div><div class="rows">"#);
    status_row(
        html,
        "Safety holds",
        &format_number(snapshot.active_restrictions),
        "hidden messages, paused groups, or blocked people",
    );
    html.push_str("</div></section>");
}

fn render_audit(html: &mut String, snapshot: &AdminSnapshot) {
    html.push_str(r#"<section class="panel"><div class="panel-head"><div><p class="kicker">content-free enforcement</p><h2>recent safety directives</h2><p class="panel-copy">This view intentionally omits target identifiers, report plaintext, reporter details, signatures, and relationship data.</p></div><span class="status">latest 50</span></div>"#);
    if snapshot.audit.is_empty() {
        html.push_str(r#"<div class="empty">No enforcement directives have been accepted.</div>"#);
    } else {
        html.push_str(r#"<div class="audit-table"><div class="audit-row audit-head"><span>action</span><span>target</span><span>reason</span><span>issued</span></div>"#);
        for event in &snapshot.audit {
            let _ = write!(
                html,
                r#"<div class="audit-row"><strong>{}</strong><span>{}</span><span>{}</span><time>{}</time></div>"#,
                escape_html(&humanize(&event.action)),
                escape_html(&humanize(&event.target_kind)),
                escape_html(&humanize(&event.reason)),
                escape_html(&event.issued_at),
            );
        }
        html.push_str("</div>");
    }
    html.push_str(r#"</section><div class="privacy-note"><strong>Decision details remain isolated</strong><span>The complete reviewer identity, report, and immutable decision record stay inside Noise Safety. Use the Safety section when case-level context is required.</span></div>"#);
}

fn render_activity_chart(html: &mut String, snapshot: &AdminSnapshot) {
    let max_events = snapshot
        .daily
        .iter()
        .map(|point| point.events)
        .max()
        .unwrap_or(0)
        .max(1);
    html.push_str(r#"<section class="panel chart-panel"><div class="panel-head"><div><p class="kicker">last two weeks</p><h2>messages each day</h2><p class="panel-copy">How many messages people sent. Hover a bar to see how many people were talking.</p></div><div class="chart-total"><strong>"#);
    html.push_str(&format_number(snapshot.events_7d));
    html.push_str(r#"</strong><span>this week</span></div></div><div class="chart" role="img" aria-label="Messages sent during the last fourteen days">"#);
    for point in &snapshot.daily {
        let percent = ((point.events * 100) / max_events).max(if point.events > 0 { 4 } else { 1 });
        let _ = write!(
            html,
            r#"<div class="bar-column" title="{} messages from {} people"><div class="bar-wrap"><div class="bar" style="height:{}%"></div></div><span>{}</span><small>{}</small></div>"#,
            format_number(point.events),
            format_number(point.authors),
            percent,
            escape_html(&point.label),
            format_number(point.events),
        );
    }
    html.push_str("</div></section>");
}

fn metric_card(html: &mut String, label: &str, value: i64, detail: &str, accent: &str) {
    text_metric_card(html, label, &format_number(value), detail, accent);
}

fn text_metric_card(html: &mut String, label: &str, value: &str, detail: &str, accent: &str) {
    let _ = write!(
        html,
        r#"<article class="metric {}"><span>{}</span><strong>{}</strong><small>{}</small></article>"#,
        accent,
        escape_html(label),
        escape_html(value),
        escape_html(detail),
    );
}

fn status_row(html: &mut String, label: &str, value: &str, detail: &str) {
    status_row_with_state(html, label, value, detail, "");
}

fn status_row_with_state(html: &mut String, label: &str, value: &str, detail: &str, state: &str) {
    let _ = write!(
        html,
        r#"<div class="data-row"><div><strong>{}</strong><span>{}</span></div><b class="{}">{}</b></div>"#,
        escape_html(label),
        escape_html(detail),
        escape_html(state),
        escape_html(value),
    );
}

fn page_description(page: AdminPage) -> &'static str {
    match page {
        AdminPage::Overview => "How noise is doing right now. No message text, just counts.",
        AdminPage::Usage => "Who signed up, who came back, and what they sent.",
        AdminPage::Infrastructure => {
            "Whether the service is keeping up, and how much space it uses."
        }
        AdminPage::Audit => "Safety actions that were taken, without names or message text.",
    }
}

fn format_number(value: i64) -> String {
    let digits = value.max(0).to_string();
    let mut result = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, character) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            result.push(',');
        }
        result.push(character);
    }
    result
}

fn format_bytes(value: i64) -> String {
    let value = value.max(0) as f64;
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut scaled = value;
    let mut unit = 0;
    while scaled >= 1024.0 && unit < UNITS.len() - 1 {
        scaled /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", scaled as i64, UNITS[unit])
    } else {
        format!("{scaled:.1} {}", UNITS[unit])
    }
}

fn humanize(value: &str) -> String {
    value.replace(['_', '-'], " ")
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn validate_tailscale_access(
    configured_logins: &[String],
    configured_external_host: Option<&str>,
) -> anyhow::Result<Vec<String>> {
    if configured_logins.is_empty() != configured_external_host.is_none() {
        bail!("Tailscale admin access requires both --external-host and --tailscale-login")
    }
    let Some(external_host) = configured_external_host else {
        return Ok(Vec::new());
    };
    if external_host.is_empty()
        || external_host.contains('/')
        || external_host.trim() != external_host
        || !valid_external_host(external_host)
    {
        bail!("the external admin host must be a hostname with an optional port")
    }
    let mut logins = Vec::with_capacity(configured_logins.len());
    for login in configured_logins {
        let normalized = login.trim().to_ascii_lowercase();
        if normalized.is_empty()
            || normalized != login.as_str()
            || !normalized.is_ascii()
            || normalized.contains(['\r', '\n'])
        {
            bail!("invalid Tailscale admin login")
        }
        if !logins.contains(&normalized) {
            logins.push(normalized);
        }
    }
    Ok(logins)
}

fn authorized_identity(state: &AdminState, headers: &HeaderMap) -> Option<String> {
    if !valid_admin_host(state, headers) {
        return None;
    }
    if state.tailscale_logins.is_empty() {
        return Some("local operator".to_owned());
    }
    let login = headers
        .get(&TAILSCALE_LOGIN_HEADER)?
        .to_str()
        .ok()?
        .to_ascii_lowercase();
    state
        .tailscale_logins
        .iter()
        .any(|allowed| constant_time_equal(allowed.as_bytes(), login.as_bytes()))
        .then_some(login)
}

fn valid_admin_host(state: &AdminState, headers: &HeaderMap) -> bool {
    let host_header = if state.tailscale_logins.is_empty() {
        header::HOST
    } else {
        FORWARDED_HOST_HEADER
    };
    headers
        .get(host_header)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|host| host_matches(&state.expected_host, host))
}

fn host_matches(expected: &str, presented: &str) -> bool {
    if expected.contains(':') {
        presented.eq_ignore_ascii_case(expected)
    } else {
        presented
            .strip_suffix(":443")
            .unwrap_or(presented)
            .eq_ignore_ascii_case(expected)
    }
}

fn valid_external_host(value: &str) -> bool {
    if !value.is_ascii() || value.contains('@') {
        return false;
    }
    let (hostname, port) = match value.rsplit_once(':') {
        Some((hostname, port)) => {
            let Ok(port) = port.parse::<u16>() else {
                return false;
            };
            if port == 0 {
                return false;
            }
            (hostname, Some(port))
        }
        None => (value, None),
    };
    !hostname.is_empty()
        && hostname
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
        && port.is_none_or(|port| port > 0)
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

fn secure_html(status: StatusCode, body: String) -> Response {
    let mut response = (status, Html(body)).into_response();
    apply_security_headers(&mut response);
    response
}

fn admin_error(status: StatusCode, message: &str) -> Response {
    secure_html(
        status,
        format!(
            "<!doctype html><title>noise control</title><p>{}</p>",
            escape_html(message)
        ),
    )
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

#[cfg(unix)]
async fn prepare_unix_socket(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::FileTypeExt;

    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.file_type().is_socket() => {
            tokio::fs::remove_file(path)
                .await
                .with_context(|| format!("could not remove stale {}", path.display()))?;
        }
        Ok(_) => bail!("refusing to replace non-socket path {}", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

#[cfg(not(unix))]
async fn prepare_unix_socket(_path: &Path) -> anyhow::Result<()> {
    bail!("Unix sockets are not supported on this platform")
}

#[cfg(unix)]
async fn secure_unix_socket(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .await
        .with_context(|| format!("could not secure {}", path.display()))
}

#[cfg(not(unix))]
async fn secure_unix_socket(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

const DOCUMENT_START: &str = r#"<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>noise control</title><style>
:root{color-scheme:dark;font-family:Inter,ui-sans-serif,-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif;background:#111013;color:#f6f1f7}
*{box-sizing:border-box}body{margin:0;background:radial-gradient(circle at 12% -10%,#b8ff3810,transparent 28%),radial-gradient(circle at 84% 4%,#8d6bff12,transparent 24%),#111013;min-height:100vh}
a{color:inherit}.topbar{position:sticky;top:0;z-index:10;display:flex;align-items:center;gap:24px;min-height:68px;padding:0 max(24px,calc((100vw - 1180px)/2));border-bottom:1px solid #ffffff10;background:#111013e8;backdrop-filter:blur(18px)}
.topbar nav{display:flex;align-items:center;gap:3px}.topbar nav a{padding:8px 10px;border-radius:8px;color:#978f9b;text-decoration:none;font-size:11px;font-weight:800}.topbar nav a:hover,.topbar nav a.active{background:#ffffff0b;color:#f3edf5}.topbar nav a:last-child{color:#c8ff6b}.private-pill{padding:6px 8px;border:1px solid #b8ff3825;border-radius:999px;color:#b8ff38;font-size:9px;font-weight:850;letter-spacing:.09em;text-transform:uppercase}
main{width:min(1180px,calc(100% - 36px));margin:0 auto;padding:48px 0 72px}.page-head{display:flex;align-items:flex-end;justify-content:space-between;gap:24px;margin-bottom:26px}.eyebrow,.kicker{margin:0 0 8px;color:#b8ff38;font-size:10px;font-weight:850;letter-spacing:.15em;text-transform:uppercase}h1{margin:0;font-size:clamp(38px,5vw,64px);line-height:.95;letter-spacing:-.055em}h2{margin:0;font-size:19px;letter-spacing:-.025em}.lede{max-width:700px;margin:13px 0 0;color:#aaa1ae;font-size:14px;line-height:1.55}.refresh,.primary{flex:none;padding:10px 13px;border:1px solid #ffffff18;border-radius:9px;background:#ffffff08;color:#e8e1ea;text-decoration:none;font-size:11px;font-weight:800}.primary{border-color:#b8ff3840;background:#b8ff38;color:#1a210d}
.metric-grid{display:grid;grid-template-columns:repeat(4,minmax(0,1fr));gap:12px;margin-bottom:14px}.metric{position:relative;overflow:hidden;min-height:150px;padding:18px;border:1px solid #ffffff10;border-radius:16px;background:#1a181ccc}.metric:before{content:"";position:absolute;inset:0 0 auto;height:2px;background:var(--accent)}.metric.lime{--accent:#b8ff38}.metric.violet{--accent:#9c7cff}.metric.orange{--accent:#ff9a5b}.metric.blue{--accent:#59b7ff}.metric span{display:block;color:#8f8793;font-size:10px;font-weight:800;letter-spacing:.08em;text-transform:uppercase}.metric strong{display:block;margin:24px 0 8px;font-size:34px;letter-spacing:-.05em}.metric small{color:#9f97a3;font-size:11px;line-height:1.45}
.health-banner{display:flex;flex-direction:column;gap:6px;margin-bottom:14px;padding:18px 20px;border:1px solid #ffffff10;border-radius:16px;background:#19171bcc}.health-banner.good{border-color:#b8ff3828;background:#b8ff380c}.health-banner.attention{border-color:#ff9a5b40;background:#ff9a5b10}.health-banner strong{font-size:22px;letter-spacing:-.03em}.health-banner span{color:#9f97a3;font-size:13px;line-height:1.5}
.panel{padding:20px;border:1px solid #ffffff10;border-radius:16px;background:#19171bcc;box-shadow:0 18px 65px #0002}.panel-head{display:flex;align-items:flex-start;justify-content:space-between;gap:18px;margin-bottom:18px}.panel-copy{max-width:620px;margin:7px 0 0;color:#908793;font-size:11px;line-height:1.5}.status{padding:6px 8px;border-radius:999px;background:#ffffff0a;color:#a9a1ad;font-size:9px;font-weight:850;letter-spacing:.08em;text-transform:uppercase}.status.good{background:#b8ff3816;color:#c8ff6b}.status.attention{background:#ff9a5b20;color:#ffb07e}.split{display:grid;grid-template-columns:1fr 1fr;gap:14px;margin-top:14px}.safety-panel{display:flex;align-items:center;justify-content:space-between;gap:24px;background:linear-gradient(135deg,#1c2018,#19171b 58%)}.safety-panel p{max-width:520px;margin:9px 0 0;color:#9d95a1;font-size:12px;line-height:1.55}
.rows{display:grid}.data-row{display:flex;align-items:center;justify-content:space-between;gap:20px;padding:13px 0;border-top:1px solid #ffffff0b}.data-row:first-child{border-top:0}.data-row strong{display:block;font-size:12px}.data-row span{display:block;margin-top:4px;color:#817984;font-size:10px}.data-row b{font-size:14px}.data-row b.clear{color:#c8ff6b}.data-row b.attention{color:#ffb07e}
.chart-panel{margin-bottom:14px}.chart-total{text-align:right}.chart-total strong{display:block;font-size:21px}.chart-total span{color:#8e8691;font-size:9px;text-transform:uppercase}.chart{display:grid;grid-template-columns:repeat(14,minmax(24px,1fr));align-items:end;gap:8px;height:220px;padding-top:10px}.bar-column{display:grid;grid-template-rows:1fr auto auto;gap:6px;height:100%;min-width:0;text-align:center}.bar-wrap{display:flex;align-items:flex-end;justify-content:center;height:100%;border-bottom:1px solid #ffffff10}.bar{width:min(24px,70%);min-height:2px;border-radius:6px 6px 2px 2px;background:linear-gradient(180deg,#c8ff6b,#7cae20);box-shadow:0 0 22px #b8ff3815}.bar-column span{color:#8f8793;font-size:8px;white-space:nowrap}.bar-column small{color:#c3bbc6;font-size:9px}
.privacy-note{display:flex;align-items:flex-start;gap:14px;margin-top:14px;padding:16px;border:1px solid #59b7ff25;border-radius:12px;background:#59b7ff08}.privacy-note strong{flex:none;color:#87cbff;font-size:11px}.privacy-note span{color:#99909c;font-size:11px;line-height:1.5}.audit-table{display:grid}.audit-row{display:grid;grid-template-columns:1.1fr .8fr 1.2fr 1fr;gap:14px;padding:12px 0;border-top:1px solid #ffffff0b;color:#a69eaa;font-size:11px}.audit-row strong{color:#eee8ef}.audit-row time{color:#88808b}.audit-head{color:#716a74;font-size:9px;font-weight:850;letter-spacing:.1em;text-transform:uppercase}.empty{padding:45px;border:1px dashed #ffffff18;border-radius:12px;color:#918894;text-align:center;font-size:12px}footer{display:flex;flex-wrap:wrap;gap:8px;margin-top:26px;color:#6f6872;font-size:9px;text-transform:uppercase}footer span+span:before{content:"·";margin-right:8px}
@media(max-width:900px){.topbar{align-items:flex-start;flex-wrap:wrap;padding:14px 18px}.brand{margin-right:0}.topbar nav{order:3;width:100%;overflow:auto}.private-pill{margin-left:auto}.metric-grid{grid-template-columns:repeat(2,minmax(0,1fr))}.split{grid-template-columns:1fr}.chart{gap:4px}.bar-column span{font-size:7px}}
@media(max-width:580px){main{width:min(100% - 20px,1180px);padding-top:32px}.page-head{align-items:flex-start;flex-direction:column}.metric-grid{grid-template-columns:1fr 1fr;gap:8px}.metric{min-height:130px;padding:14px}.metric strong{margin-top:19px;font-size:27px}.chart{grid-template-columns:repeat(14,18px);overflow-x:auto;justify-content:start}.safety-panel{align-items:flex-start;flex-direction:column}.audit-row{grid-template-columns:1fr 1fr}.audit-head{display:none}.privacy-note{flex-direction:column}}
"#;

const DOCUMENT_STYLE_END: &str = "</style></head><body>";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_escapes_untrusted_values() {
        assert_eq!(
            escape_html("<script>alert('admin')</script>"),
            "&lt;script&gt;alert(&#39;admin&#39;)&lt;/script&gt;"
        );
    }

    #[test]
    fn admin_access_requires_exact_tailnet_configuration() {
        let allowed = vec!["cmcelvogue91@gmail.com".to_owned()];
        assert!(
            validate_tailscale_access(&allowed, Some("cyphers-vps.yakalo-lizard.ts.net:8443"))
                .is_ok()
        );
        assert!(validate_tailscale_access(&allowed, None).is_err());
        assert!(validate_tailscale_access(&[], Some("cyphers-vps")).is_err());
        assert!(!host_matches("cyphers-vps:8443", "cyphers-vps"));
    }

    #[test]
    fn admin_formats_aggregate_values() {
        assert_eq!(format_number(1_234_567), "1,234,567");
        assert_eq!(format_bytes(1_572_864), "1.5 MB");
    }

    fn sample_snapshot() -> AdminSnapshot {
        AdminSnapshot {
            captured_at: "now".to_owned(),
            schema_version: 9,
            database_bytes: 1_000,
            total_accounts: 10,
            active_accounts: 8,
            new_accounts_24h: 1,
            new_accounts_7d: 2,
            active_accounts_24h: 3,
            active_accounts_7d: 5,
            active_accounts_30d: 7,
            active_devices: 4,
            active_groups: 2,
            new_groups_7d: 1,
            memberships: 6,
            total_events: 20,
            events_24h: 4,
            events_7d: 12,
            group_events_24h: 3,
            direct_events_24h: 1,
            authors_24h: 2,
            available_media: 5,
            media_bytes: 2_000,
            media_7d: 1,
            active_sessions: 3,
            active_push_subscriptions: 3,
            active_restrictions: 0,
            daily: Vec::new(),
            audit: Vec::new(),
        }
    }

    #[test]
    fn service_status_stays_plain_when_healthy() {
        let healthy = service_status();
        assert_eq!(healthy.title, "All good");
        assert_eq!(healthy.tone, "good");
    }

    #[test]
    fn overview_uses_everyday_words_and_the_noise_logo() {
        let html = render_dashboard(&sample_snapshot(), AdminPage::Overview, "chris@example.com");
        assert!(html.contains("people using it today"));
        assert!(html.contains("messages today"));
        assert!(html.contains("photos &amp; videos"));
        assert!(html.contains("messages each day"));
        assert!(!html.contains("encrypted events"));
        assert!(!html.contains("ciphertext"));
        assert!(!html.contains("waiting to send"));
        assert!(!html.contains("backed up"));
        assert!(!html.contains(r#"class="mark">n"#));
        assert!(html.contains("noise-logo-wave"));
        assert!(html.contains(">today</a>"));
    }

    #[test]
    fn service_page_does_not_treat_outbox_as_a_backlog() {
        let html =
            render_dashboard(&sample_snapshot(), AdminPage::Infrastructure, "chris@example.com");
        assert!(html.contains("signed in right now"));
        assert!(!html.contains("waiting to send"));
        assert!(!html.contains("backed up"));
        assert!(!html.contains("outbox"));
        assert!(!html.contains("durable job"));
    }
}
