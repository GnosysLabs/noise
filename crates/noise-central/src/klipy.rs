use std::{sync::Arc, time::Duration};

use anyhow::Context;
use axum::{
    Json,
    extract::{Query, State},
    http::HeaderMap,
};
use reqwest::{StatusCode, redirect::Policy};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{AppState, authenticate_session, config::CentralConfig, error::ApiError};

const KLIPY_API_ROOT: &str = "https://api.klipy.com/api/v1";
const MAX_QUERY_CHARS: usize = 100;
const MAX_RESULTS: usize = 50;
const DEFAULT_RESULTS: usize = 24;
const MAX_UPSTREAM_BYTES: usize = 2_000_000;

#[derive(Clone)]
pub(crate) struct KlipyProxy {
    api_key: Arc<str>,
    http: reqwest::Client,
}

#[derive(Deserialize)]
pub(crate) struct KlipyQuery {
    kind: Option<String>,
    q: Option<String>,
    limit: Option<usize>,
}

#[derive(Serialize)]
pub(crate) struct KlipyResponse {
    results: Vec<KlipyResult>,
}

#[derive(Serialize)]
struct KlipyResult {
    id: String,
    kind: String,
    title: String,
    preview_url: String,
    preview_blur: Option<String>,
    full_url: String,
    full_mp4_url: Option<String>,
    full_webp_url: Option<String>,
    width: u64,
    height: u64,
    mime_type: &'static str,
}

impl KlipyProxy {
    pub(crate) fn open(config: &CentralConfig) -> anyhow::Result<Option<Self>> {
        let Some(api_key) = config
            .klipy_api_key
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return Ok(None);
        };
        let http = reqwest::Client::builder()
            .redirect(Policy::none())
            .user_agent("noise-central/0.3")
            .connect_timeout(Duration::from_secs(4))
            .timeout(Duration::from_secs(12))
            .build()
            .context("could not initialize Klipy transport")?;
        Ok(Some(Self {
            api_key: Arc::from(api_key),
            http,
        }))
    }

    async fn fetch(
        &self,
        action: &'static str,
        kind: &'static str,
        query: Option<&str>,
        limit: usize,
    ) -> Result<Vec<KlipyResult>, ApiError> {
        let kind_path = match kind {
            "gif" => "gifs",
            "sticker" => "stickers",
            "clip" => "clips",
            _ => return Err(ApiError::bad_request("invalid_klipy_kind")),
        };
        let mut upstream = reqwest::Url::parse(KLIPY_API_ROOT)
            .map_err(|_| ApiError::bad_gateway("klipy_unavailable"))?;
        upstream
            .path_segments_mut()
            .map_err(|_| ApiError::bad_gateway("klipy_unavailable"))?
            .extend([self.api_key.as_ref(), kind_path, action]);
        upstream
            .query_pairs_mut()
            .append_pair("per_page", &limit.to_string())
            .append_pair("rating", "pg-13");
        if let Some(query) = query {
            upstream.query_pairs_mut().append_pair("q", query);
        }

        let response = self.http.get(upstream).send().await.map_err(|_| {
            eprintln!("noise-central Klipy request failed");
            ApiError::bad_gateway("klipy_unavailable")
        })?;
        if response.status() != StatusCode::OK {
            eprintln!(
                "noise-central Klipy request returned {}",
                response.status().as_u16()
            );
            return Err(ApiError::bad_gateway("klipy_unavailable"));
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_UPSTREAM_BYTES as u64)
        {
            return Err(ApiError::bad_gateway("klipy_unavailable"));
        }
        let bytes = response.bytes().await.map_err(|_| {
            eprintln!("noise-central Klipy response could not be read");
            ApiError::bad_gateway("klipy_unavailable")
        })?;
        if bytes.len() > MAX_UPSTREAM_BYTES {
            return Err(ApiError::bad_gateway("klipy_unavailable"));
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| {
            eprintln!("noise-central Klipy response was invalid");
            ApiError::bad_gateway("klipy_unavailable")
        })?;
        let items = value
            .pointer("/data/data")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                eprintln!("noise-central Klipy response omitted results");
                ApiError::bad_gateway("klipy_unavailable")
            })?;
        Ok(items
            .iter()
            .filter_map(|item| normalize_result(item, kind))
            .take(limit)
            .collect())
    }
}

pub(crate) async fn trending(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<KlipyQuery>,
) -> Result<Json<KlipyResponse>, ApiError> {
    serve(state, headers, query, "trending").await
}

pub(crate) async fn search(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<KlipyQuery>,
) -> Result<Json<KlipyResponse>, ApiError> {
    serve(state, headers, query, "search").await
}

async fn serve(
    state: Arc<AppState>,
    headers: HeaderMap,
    query: KlipyQuery,
    action: &'static str,
) -> Result<Json<KlipyResponse>, ApiError> {
    authenticate_session(&state, &headers).await?;
    let kind = match query.kind.as_deref().unwrap_or("gif") {
        "gif" => "gif",
        "sticker" => "sticker",
        "clip" => "clip",
        _ => return Err(ApiError::bad_request("invalid_klipy_kind")),
    };
    let search_query = query.q.as_deref().map(str::trim).filter(|q| !q.is_empty());
    if action == "search" && search_query.is_none() {
        return Err(ApiError::bad_request("klipy_query_required"));
    }
    if search_query.is_some_and(|q| q.chars().count() > MAX_QUERY_CHARS) {
        return Err(ApiError::bad_request("klipy_query_too_long"));
    }
    let limit = query.limit.unwrap_or(DEFAULT_RESULTS).clamp(8, MAX_RESULTS);
    let proxy = state
        .klipy
        .as_ref()
        .ok_or_else(|| ApiError::not_found("klipy_not_configured"))?;
    let results = proxy.fetch(action, kind, search_query, limit).await?;
    Ok(Json(KlipyResponse { results }))
}

fn normalize_result(item: &Value, kind: &str) -> Option<KlipyResult> {
    let file = item.get("file")?;
    let id = item
        .get("id")
        .and_then(|value| match value {
            Value::String(value) => Some(value.clone()),
            Value::Number(value) => Some(value.to_string()),
            _ => None,
        })
        .or_else(|| item.get("slug")?.as_str().map(str::to_owned))?;
    if id.is_empty() {
        return None;
    }

    let tiered = ["md", "sm", "hd"]
        .iter()
        .any(|tier| file.get(tier).is_some_and(Value::is_object));
    let (preview_url, full_url, full_mp4_url, full_webp_url, width, height) = if tiered {
        let preview_url = first_string(
            file,
            &[
                &["sm", "webp", "url"],
                &["sm", "gif", "url"],
                &["sm", "jpg", "url"],
            ],
        )?;
        let full_url = match kind {
            "sticker" => first_string(file, &[&["md", "webp", "url"], &["md", "gif", "url"]])?,
            "clip" => string_at(file, &["md", "mp4", "url"])?,
            _ => string_at(file, &["md", "gif", "url"])?,
        };
        let dimension_path = match kind {
            "sticker" if string_at(file, &["md", "webp", "url"]).is_some() => &["md", "webp"][..],
            "clip" => &["md", "mp4"][..],
            _ => &["md", "gif"][..],
        };
        (
            preview_url,
            full_url,
            string_at(file, &["md", "mp4", "url"]),
            string_at(file, &["md", "webp", "url"]),
            unsigned_at(file, &[dimension_path, &["width"]].concat()),
            unsigned_at(file, &[dimension_path, &["height"]].concat()),
        )
    } else {
        let metadata = item.get("file_meta").unwrap_or(&Value::Null);
        let preview_url = first_string(file, &[&["webp"], &["gif"], &["jpg"]])?;
        let full_url = match kind {
            "sticker" => first_string(file, &[&["webp"], &["gif"]])?,
            "clip" => string_at(file, &["mp4"])?,
            _ => string_at(file, &["gif"])?,
        };
        let dimension_format = match kind {
            "sticker" if string_at(file, &["webp"]).is_some() => "webp",
            "clip" => "mp4",
            _ => "gif",
        };
        (
            preview_url,
            full_url,
            string_at(file, &["mp4"]),
            string_at(file, &["webp"]),
            unsigned_at(metadata, &[dimension_format, "width"]),
            unsigned_at(metadata, &[dimension_format, "height"]),
        )
    };
    if !valid_https_url(&preview_url) || !valid_https_url(&full_url) {
        return None;
    }

    Some(KlipyResult {
        id,
        kind: kind.to_owned(),
        title: item
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .chars()
            .take(200)
            .collect(),
        preview_url,
        preview_blur: item
            .get("blur_preview")
            .and_then(Value::as_str)
            .filter(|value| value.len() <= 10_000)
            .map(str::to_owned),
        full_url,
        full_mp4_url: full_mp4_url.filter(|url| valid_https_url(url)),
        full_webp_url: full_webp_url.filter(|url| valid_https_url(url)),
        width,
        height,
        mime_type: match kind {
            "sticker" => "image/webp",
            "clip" => "video/mp4",
            _ => "image/gif",
        },
    })
}

fn first_string(value: &Value, paths: &[&[&str]]) -> Option<String> {
    paths.iter().find_map(|path| string_at(value, path))
}

fn string_at(value: &Value, path: &[&str]) -> Option<String> {
    path.iter()
        .try_fold(value, |current, key| current.get(*key))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn unsigned_at(value: &Value, path: &[&str]) -> u64 {
    path.iter()
        .try_fold(value, |current, key| current.get(*key))
        .and_then(Value::as_u64)
        .unwrap_or_default()
}

fn valid_https_url(value: &str) -> bool {
    reqwest::Url::parse(value).is_ok_and(|url| {
        url.scheme() == "https"
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
    })
}
