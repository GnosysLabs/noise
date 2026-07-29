use std::{
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};

use anyhow::{Context, anyhow, bail};
use reqwest::{
    Method, StatusCode,
    header::{AUTHORIZATION, CONTENT_TYPE, HeaderName, HeaderValue},
};
use url::Url;

const MAX_RESPONSE_BYTES: u64 = 3_000_000;

#[derive(Clone)]
pub(crate) struct CentralTransport {
    base_url: Arc<str>,
    http: reqwest::Client,
    access_token: Arc<RwLock<Option<Arc<str>>>>,
    /// The state file that stores the session behind `access_token`.
    ///
    /// A rejected token can only be renewed against the state that holds its
    /// installation, and any request can be the one that meets the rejection.
    /// Recording the path with the session lets every caller recover without
    /// carrying it through each publisher.
    session_state_path: Arc<RwLock<Option<PathBuf>>>,
}

pub(crate) struct CentralResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

impl CentralTransport {
    pub(crate) fn new(base_url: &str) -> anyhow::Result<Self> {
        let base_url = base_url.trim().trim_end_matches('/');
        let parsed = Url::parse(base_url).context("central service URL is invalid")?;
        let local_http = parsed.scheme() == "http"
            && parsed
                .host_str()
                .is_some_and(|host| matches!(host, "127.0.0.1" | "localhost" | "::1"));
        if (parsed.scheme() != "https" && !local_http)
            || parsed.host_str().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
            || parsed.path() != "/"
        {
            bail!("central service URL must be HTTPS without a path")
        }
        let http_builder = reqwest::Client::builder();
        #[cfg(not(target_arch = "wasm32"))]
        let http_builder = http_builder
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(40));
        let http = http_builder
            .build()
            .context("could not initialize central service transport")?;
        Ok(Self {
            base_url: Arc::from(base_url),
            http,
            access_token: Arc::new(RwLock::new(None)),
            session_state_path: Arc::new(RwLock::new(None)),
        })
    }

    pub(crate) fn base_url(&self) -> &str {
        &self.base_url
    }

    pub(crate) fn bind_session_state(&self, path: &Path) {
        if let Ok(mut current) = self.session_state_path.write() {
            *current = Some(path.to_path_buf());
        }
    }

    pub(crate) fn session_state_path(&self) -> Option<PathBuf> {
        self.session_state_path.read().ok()?.clone()
    }

    pub(crate) async fn set_access_token(&self, token: Option<String>) {
        if let Ok(mut current) = self.access_token.write() {
            *current = token.map(Arc::from);
        }
    }

    pub(crate) fn access_token(&self) -> anyhow::Result<Option<String>> {
        Ok(self
            .access_token
            .read()
            .map_err(|_| anyhow!("central service session lock is unavailable"))?
            .as_deref()
            .map(str::to_owned))
    }

    pub(crate) async fn request(
        &self,
        method: Method,
        path: &str,
        body: &[u8],
        authenticated: bool,
        extra_headers: &[(&'static str, String)],
        content_type: Option<&'static str>,
    ) -> anyhow::Result<CentralResponse> {
        if !path.starts_with('/') || path.starts_with("//") {
            bail!("central service path is invalid")
        }
        let target_url = Url::parse(&format!("{}{path}", self.base_url))
            .context("central service path is invalid")?;
        let mut request = self.http.request(method, target_url.clone());
        if authenticated {
            let token = self
                .access_token
                .read()
                .map_err(|_| anyhow!("central service session lock is unavailable"))?
                .clone()
                .context("central service session is unavailable")?;
            request = request.header(AUTHORIZATION, format!("Bearer {token}"));
        }
        if !body.is_empty() {
            request = request
                .header(CONTENT_TYPE, content_type.unwrap_or("application/json"))
                .body(body.to_vec());
        }
        for (name, value) in extra_headers {
            request = request.header(
                HeaderName::from_static(name),
                HeaderValue::from_str(value).context("central request header is invalid")?,
            );
        }
        let response = request
            .send()
            .await
            .context("central service is unreachable")?;
        if response.url() != &target_url {
            bail!("central service redirected the request")
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_RESPONSE_BYTES)
        {
            bail!("central service response is too large")
        }
        let status = response.status();
        let body = response
            .bytes()
            .await
            .context("central service response could not be read")?;
        if body.len() as u64 > MAX_RESPONSE_BYTES {
            bail!("central service response is too large")
        }
        Ok(CentralResponse {
            status: status.as_u16(),
            body: body.to_vec(),
        })
    }

    pub(crate) async fn json(
        &self,
        method: Method,
        path: &str,
        body: &[u8],
        authenticated: bool,
    ) -> anyhow::Result<CentralResponse> {
        self.request(method, path, body, authenticated, &[], None)
            .await
    }

    /// Reject anything but a success status, naming the service's error code.
    ///
    /// Kept separate from `require_success` so a caller that renews a rejected
    /// session can apply the same check after its retry.
    pub(crate) fn require_ok(response: CentralResponse) -> anyhow::Result<CentralResponse> {
        let status =
            StatusCode::from_u16(response.status).context("central service returned bad status")?;
        if !status.is_success() {
            let code = serde_json::from_slice::<serde_json::Value>(&response.body)
                .ok()
                .and_then(|value| value.get("error")?.as_str().map(str::to_owned))
                .unwrap_or_else(|| status.as_str().to_owned());
            bail!("central service rejected the request ({code})")
        }
        Ok(response)
    }

    pub(crate) async fn require_success(
        &self,
        method: Method,
        path: &str,
        body: &[u8],
        authenticated: bool,
    ) -> anyhow::Result<CentralResponse> {
        let response = self.json(method, path, body, authenticated).await?;
        Self::require_ok(response)
    }
}
