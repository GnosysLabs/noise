#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    collections::{HashMap, HashSet},
    fs,
    path::Path,
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axum::{
    Router,
    body::Body,
    extract::{Path as AxumPath, State},
    http::{HeaderMap, Method, Response, StatusCode, header},
    routing::any,
};
use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use rand::random;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tauri::Manager;
#[cfg(not(target_os = "macos"))]
use tauri_plugin_notification::NotificationExt;

static NOTIFICATION_WATCHERS_STARTED: AtomicBool = AtomicBool::new(false);
static NEXT_MEDIA_STREAM_ID: AtomicU64 = AtomicU64::new(1);
static MEDIA_SERVER_BASE_URL: OnceLock<String> = OnceLock::new();
static ACCOUNT_REGISTRY_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[derive(Clone, Default)]
struct MediaStreamRegistry(Arc<Mutex<HashMap<String, Value>>>);

#[derive(Serialize)]
struct OptimizedMediaUpload {
    file_path: String,
    file_name: String,
    mime_type: String,
    byte_length: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct LocalAccountRegistry {
    version: u32,
    active_account_id: Option<String>,
    adding_from_account_id: Option<String>,
    accounts: Vec<LocalAccountRecord>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct LocalAccountRecord {
    id: String,
    public_key: String,
    username: String,
    bio: String,
    avatar: Option<Value>,
    pending: bool,
}

#[derive(Clone, Debug, Serialize)]
struct LocalAccountList {
    active_account_id: Option<String>,
    adding_account: bool,
    accounts: Vec<LocalAccountRecord>,
}

struct ActiveAccountPaths {
    id: String,
    state: PathBuf,
    cache: PathBuf,
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[derive(Clone)]
struct PendingNotification {
    event_id: String,
    title: String,
    body: String,
    created_at_millis: u64,
}

fn noise_data_directory() -> Result<PathBuf, String> {
    dirs::data_dir()
        .map(|directory| directory.join("noise"))
        .ok_or_else(|| "this device has no application data directory".to_owned())
}

fn account_registry_path() -> Result<PathBuf, String> {
    Ok(noise_data_directory()?.join("accounts.json"))
}

fn read_account_registry() -> Result<LocalAccountRegistry, String> {
    let path = account_registry_path()?;
    if !path.exists() {
        return Ok(LocalAccountRegistry {
            version: 1,
            active_account_id: Some("legacy".to_owned()),
            adding_from_account_id: None,
            accounts: Vec::new(),
        });
    }
    let bytes = fs::read(&path).map_err(|error| error.to_string())?;
    let mut registry = serde_json::from_slice::<LocalAccountRegistry>(&bytes)
        .map_err(|error| format!("local account list is invalid: {error}"))?;
    if registry.version == 0 {
        registry.version = 1;
    }
    Ok(registry)
}

fn write_account_registry(registry: &LocalAccountRegistry) -> Result<(), String> {
    let path = account_registry_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let bytes = serde_json::to_vec_pretty(registry).map_err(|error| error.to_string())?;
    fs::write(path, bytes).map_err(|error| error.to_string())
}

fn active_account_paths(app: &tauri::AppHandle) -> Result<ActiveAccountPaths, String> {
    let _guard = ACCOUNT_REGISTRY_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| "local account list is unavailable".to_owned())?;
    let registry = read_account_registry()?;
    let id = registry
        .active_account_id
        .unwrap_or_else(|| "legacy".to_owned());
    let data = noise_data_directory()?;
    let cache_root = app
        .path()
        .app_cache_dir()
        .map_err(|error| error.to_string())?;
    if id == "legacy" {
        Ok(ActiveAccountPaths {
            id,
            state: data.join("profile.json"),
            cache: cache_root,
        })
    } else {
        Ok(ActiveAccountPaths {
            state: data.join("accounts").join(&id).join("profile.json"),
            cache: cache_root.join("accounts").join(&id),
            id,
        })
    }
}

fn update_active_account_metadata(account_id: &str, response: &Value) -> Result<(), String> {
    let Some(identity) = response
        .get("data")
        .and_then(|data| data.get("identity"))
        .and_then(Value::as_object)
    else {
        return Ok(());
    };
    let public_key = identity
        .get("public_key")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let username = identity
        .get("username")
        .and_then(Value::as_str)
        .unwrap_or("noise user")
        .to_owned();
    if public_key.is_empty() {
        return Ok(());
    }
    let _guard = ACCOUNT_REGISTRY_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| "local account list is unavailable".to_owned())?;
    let mut registry = read_account_registry()?;
    let response_is_for_active_account = registry.active_account_id.as_deref() == Some(account_id);
    let account_is_still_registered = registry
        .accounts
        .iter()
        .any(|account| account.id == account_id);
    // A request started before an account switch may finish afterward. It may
    // refresh metadata for a still-signed-in account, but it must never restore
    // a removed account or undo the user's newer active-account selection.
    if !response_is_for_active_account && !account_is_still_registered {
        return Ok(());
    }
    if response_is_for_active_account {
        registry
            .accounts
            .retain(|account| !account.pending || account.id == account_id);
    }
    let record = LocalAccountRecord {
        id: account_id.to_owned(),
        public_key,
        username,
        bio: identity
            .get("bio")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        avatar: identity
            .get("avatar")
            .cloned()
            .filter(|value| !value.is_null()),
        pending: false,
    };
    if let Some(existing) = registry
        .accounts
        .iter_mut()
        .find(|existing| existing.id == account_id)
    {
        *existing = record;
    } else {
        registry.accounts.push(record);
    }
    if response_is_for_active_account {
        registry.adding_from_account_id = None;
    }
    write_account_registry(&registry)
}

fn remove_active_account_from_registry(account_id: &str) -> Result<(), String> {
    let _guard = ACCOUNT_REGISTRY_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| "local account list is unavailable".to_owned())?;
    let mut registry = read_account_registry()?;
    registry.accounts.retain(|account| account.id != account_id);
    registry.adding_from_account_id = None;
    registry.active_account_id = registry
        .accounts
        .iter()
        .find(|account| !account.pending)
        .map(|account| account.id.clone())
        .or_else(|| Some("legacy".to_owned()));
    write_account_registry(&registry)
}

async fn execute_noise_request(app: &tauri::AppHandle, mut request: Value) -> Value {
    let action = request
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let Ok(paths) = active_account_paths(app) else {
        return json!({ "ok": false, "error": "could not locate noise identity storage" });
    };
    let Some(request_object) = request.as_object_mut() else {
        return json!({ "ok": false, "error": "noise request must be an object" });
    };
    request_object.insert(
        "state_path".into(),
        Value::String(paths.state.to_string_lossy().into_owned()),
    );
    request_object.insert(
        "cache_path".into(),
        Value::String(paths.cache.to_string_lossy().into_owned()),
    );

    let response = match tauri::async_runtime::spawn_blocking(move || {
        noise_ffi::response_json(&request.to_string())
    })
    .await
    {
        Ok(response) => serde_json::from_str(&response)
            .unwrap_or_else(|error| json!({ "ok": false, "error": error.to_string() })),
        Err(error) => json!({ "ok": false, "error": error.to_string() }),
    };
    if response.get("ok").and_then(Value::as_bool) == Some(true) {
        if action == "fetch_attachment"
            && let Some(file_path) = response
                .get("data")
                .and_then(|data| data.get("file_path"))
                .and_then(Value::as_str)
        {
            let source = match fs::canonicalize(file_path) {
                Ok(source) => source,
                Err(error) => {
                    return json!({
                        "ok": false,
                        "error": format!("could not open cached media: {error}"),
                    });
                }
            };
            let cache = match fs::canonicalize(&paths.cache) {
                Ok(cache) => cache,
                Err(error) => {
                    return json!({
                        "ok": false,
                        "error": format!("could not locate the media cache: {error}"),
                    });
                }
            };
            if !source.starts_with(&cache) {
                return json!({
                    "ok": false,
                    "error": "cached media is outside this account's private cache",
                });
            }
            if let Err(error) = app.asset_protocol_scope().allow_file(&source) {
                return json!({
                    "ok": false,
                    "error": format!("could not authorize cached media for display: {error}"),
                });
            }
        }
        if matches!(action.as_str(), "logout" | "delete_account") {
            let _ = remove_active_account_from_registry(&paths.id);
        } else if matches!(action.as_str(), "initialize" | "sign_in") {
            // Creating or restoring an identity is a storage transaction:
            // noise-client has already committed the encrypted vault, and the
            // call must not report success until the account registry agrees.
            if let Err(error) = update_active_account_metadata(&paths.id, &response) {
                return json!({
                    "ok": false,
                    "error": format!("could not commit the local account: {error}"),
                });
            }
        } else {
            let _ = update_active_account_metadata(&paths.id, &response);
        }
    }
    response
}

async fn noise_request_data(app: &tauri::AppHandle, request: Value) -> Result<Value, String> {
    let response = execute_noise_request(app, request).await;
    if response.get("ok").and_then(Value::as_bool) == Some(true) {
        Ok(response.get("data").cloned().unwrap_or(Value::Null))
    } else {
        Err(response
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("noise request failed")
            .to_owned())
    }
}

#[tauri::command]
async fn noise_invoke(app: tauri::AppHandle, request: Value) -> Value {
    execute_noise_request(&app, request).await
}

#[tauri::command]
fn list_local_accounts() -> Result<LocalAccountList, String> {
    let _guard = ACCOUNT_REGISTRY_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| "local account list is unavailable".to_owned())?;
    let registry = read_account_registry()?;
    Ok(LocalAccountList {
        active_account_id: registry.active_account_id,
        adding_account: registry.adding_from_account_id.is_some(),
        accounts: registry
            .accounts
            .into_iter()
            .filter(|account| !account.pending)
            .collect(),
    })
}

#[tauri::command]
fn start_adding_local_account() -> Result<(), String> {
    let _guard = ACCOUNT_REGISTRY_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| "local account list is unavailable".to_owned())?;
    let mut registry = read_account_registry()?;
    if registry.adding_from_account_id.is_some() {
        return Ok(());
    }
    registry.accounts.retain(|account| !account.pending);
    let previous = registry
        .active_account_id
        .clone()
        .unwrap_or_else(|| "legacy".to_owned());
    let id = format!("account-{:016x}{:016x}", unix_millis(), random::<u64>());
    registry.adding_from_account_id = Some(previous);
    registry.active_account_id = Some(id.clone());
    registry.accounts.push(LocalAccountRecord {
        id,
        public_key: String::new(),
        username: "new account".to_owned(),
        bio: String::new(),
        avatar: None,
        pending: true,
    });
    write_account_registry(&registry)
}

#[tauri::command]
fn cancel_adding_local_account(app: tauri::AppHandle) -> Result<(), String> {
    let _guard = ACCOUNT_REGISTRY_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| "local account list is unavailable".to_owned())?;
    let mut registry = read_account_registry()?;
    let Some(previous) = registry.adding_from_account_id.take() else {
        return Ok(());
    };
    let pending_id = registry.active_account_id.clone();
    if let Some(id) = pending_id.as_deref() {
        if id != "legacy" {
            let data = noise_data_directory()?.join("accounts").join(id);
            if data.exists() {
                let _ = fs::remove_dir_all(data);
            }
            if let Ok(cache) = app.path().app_cache_dir() {
                let cache = cache.join("accounts").join(id);
                if cache.exists() {
                    let _ = fs::remove_dir_all(cache);
                }
            }
        }
        registry.accounts.retain(|account| account.id != id);
    }
    registry.active_account_id = Some(previous);
    write_account_registry(&registry)
}

#[tauri::command]
fn switch_local_account(account_id: String) -> Result<(), String> {
    let _guard = ACCOUNT_REGISTRY_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| "local account list is unavailable".to_owned())?;
    let mut registry = read_account_registry()?;
    if !registry
        .accounts
        .iter()
        .any(|account| account.id == account_id && !account.pending)
    {
        return Err("that account is no longer signed in on this device".to_owned());
    }
    registry.active_account_id = Some(account_id);
    registry.adding_from_account_id = None;
    write_account_registry(&registry)
}

#[tauri::command]
fn register_media_stream(
    registry: tauri::State<'_, MediaStreamRegistry>,
    request: Value,
) -> Result<String, String> {
    let attachment = request
        .get("attachment")
        .ok_or_else(|| "media stream is missing its attachment".to_owned())?;
    let byte_length = attachment
        .get("byte_length")
        .and_then(Value::as_u64)
        .ok_or_else(|| "media stream has an invalid length".to_owned())?;
    if byte_length == 0 || byte_length > 500 * 1024 * 1024 {
        return Err("media stream has an invalid length".to_owned());
    }
    let file_extension = attachment
        .get("file_name")
        .and_then(Value::as_str)
        .and_then(|file_name| Path::new(file_name).extension())
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .filter(|extension| {
            !extension.is_empty()
                && extension.len() <= 8
                && extension
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric())
        })
        .unwrap_or_else(|| {
            match attachment
                .get("mime_type")
                .and_then(Value::as_str)
                .unwrap_or_default()
            {
                "video/quicktime" => "mov",
                "video/x-m4v" => "m4v",
                _ => "mp4",
            }
            .to_owned()
        });
    let id = format!(
        "{:016x}{:016x}",
        unix_millis(),
        NEXT_MEDIA_STREAM_ID.fetch_add(1, Ordering::Relaxed)
    );
    let mut streams = registry
        .0
        .lock()
        .map_err(|_| "media stream registry is unavailable".to_owned())?;
    if streams.len() >= 256
        && let Some(oldest) = streams.keys().next().cloned()
    {
        streams.remove(&oldest);
    }
    streams.insert(id.clone(), request);
    let base_url = MEDIA_SERVER_BASE_URL
        .get()
        .ok_or_else(|| "desktop media server is not ready".to_owned())?;
    Ok(format!(
        "{base_url}/noise-media/{id}/media.{file_extension}"
    ))
}

#[tauri::command]
async fn optimize_video_upload(
    app: tauri::AppHandle,
    source_path: String,
) -> Result<OptimizedMediaUpload, String> {
    prepare_media_upload(app, source_path).await
}

#[tauri::command]
async fn convert_cached_image_for_display(
    app: tauri::AppHandle,
    source_path: String,
) -> Result<String, String> {
    let cache_directory = app
        .path()
        .app_cache_dir()
        .map_err(|error| error.to_string())?;
    let output = tauri::async_runtime::spawn_blocking(move || {
        convert_cached_image_for_display_blocking(&source_path, &cache_directory)
    })
    .await
    .map_err(|error| error.to_string())??;
    app.asset_protocol_scope()
        .allow_file(&output)
        .map_err(|error| error.to_string())?;
    Ok(output.to_string_lossy().into_owned())
}

#[cfg(target_os = "macos")]
fn convert_cached_image_for_display_blocking(
    source_path: &str,
    cache_directory: &Path,
) -> Result<PathBuf, String> {
    let source = fs::canonicalize(source_path).map_err(|error| error.to_string())?;
    let cache = fs::canonicalize(cache_directory).map_err(|error| error.to_string())?;
    if !source.starts_with(&cache) {
        return Err("noise can only convert media from its private cache".to_owned());
    }
    let metadata = source.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > 100 * 1024 * 1024 {
        return Err("cached image has an invalid size".to_owned());
    }
    let stem = source
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("noise-photo");
    let output = source.with_file_name(format!("{stem}.render.jpg"));
    if output
        .metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0)
    {
        return Ok(output);
    }
    let temporary = source.with_file_name(format!(".{stem}.render-{}.jpg", unix_millis()));
    let status = Command::new("/usr/bin/sips")
        .args(["-s", "format", "jpeg"])
        .arg(&source)
        .arg("--out")
        .arg(&temporary)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|error| error.to_string())?;
    if !status.success()
        || !temporary
            .metadata()
            .is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0)
    {
        let _ = fs::remove_file(&temporary);
        return Err("macOS could not decode this cached image".to_owned());
    }
    fs::rename(&temporary, &output).map_err(|error| error.to_string())?;
    Ok(output)
}

#[cfg(not(target_os = "macos"))]
fn convert_cached_image_for_display_blocking(
    _source_path: &str,
    _cache_directory: &Path,
) -> Result<PathBuf, String> {
    Err("native cached-image conversion is unavailable on this platform".to_owned())
}

#[tauri::command]
fn inspect_media_upload(
    app: tauri::AppHandle,
    source_path: String,
) -> Result<OptimizedMediaUpload, String> {
    let source = fs::canonicalize(source_path).map_err(|error| error.to_string())?;
    let metadata = source.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > 500 * 1024 * 1024 {
        return Err("media has an invalid size".to_owned());
    }
    let extension = source
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let mime_type = match extension.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "heic" => "image/heic",
        "heif" => "image/heif",
        "mov" => "video/quicktime",
        "mp4" | "m4v" => "video/mp4",
        "avi" => "video/x-msvideo",
        "mpeg" | "mpg" => "video/mpeg",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "wav" => "audio/wav",
        "aac" => "audio/aac",
        "ogg" => "audio/ogg",
        _ => return Err("media has an unsupported format".to_owned()),
    };
    app.asset_protocol_scope()
        .allow_file(&source)
        .map_err(|error| error.to_string())?;
    let file_name = source
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("noise-media")
        .to_owned();
    Ok(OptimizedMediaUpload {
        file_path: source.to_string_lossy().into_owned(),
        file_name,
        mime_type: mime_type.to_owned(),
        byte_length: metadata.len(),
    })
}

#[tauri::command]
async fn prepare_media_upload(
    app: tauri::AppHandle,
    source_path: String,
) -> Result<OptimizedMediaUpload, String> {
    let output_directory = app
        .path()
        .app_cache_dir()
        .map_err(|error| error.to_string())?
        .join("outgoing");
    tauri::async_runtime::spawn_blocking(move || {
        prepare_media_upload_blocking(&source_path, &output_directory)
    })
    .await
    .map_err(|error| error.to_string())?
}

fn prepare_media_upload_blocking(
    source_path: &str,
    output_directory: &Path,
) -> Result<OptimizedMediaUpload, String> {
    let source = fs::canonicalize(source_path).map_err(|error| error.to_string())?;
    let metadata = source.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > 500 * 1024 * 1024 {
        return Err("media has an invalid size".to_owned());
    }
    let extension = source
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let (mime_type, is_video) = match extension.as_str() {
        "jpg" | "jpeg" => ("image/jpeg", false),
        "png" => ("image/png", false),
        "gif" => ("image/gif", false),
        "webp" => ("image/webp", false),
        "heic" => ("image/heic", false),
        "heif" => ("image/heif", false),
        "mov" => ("video/quicktime", true),
        "mp4" | "m4v" => ("video/mp4", true),
        "avi" => ("video/x-msvideo", true),
        "mpeg" | "mpg" => ("video/mpeg", true),
        "mp3" => ("audio/mpeg", false),
        "m4a" => ("audio/mp4", false),
        "wav" => ("audio/wav", false),
        "aac" => ("audio/aac", false),
        "ogg" => ("audio/ogg", false),
        _ => return Err("media has an unsupported format".to_owned()),
    };
    fs::create_dir_all(output_directory).map_err(|error| error.to_string())?;
    let stem = source
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("noise-media");
    let id = format!(
        "{:016x}-{:016x}",
        unix_millis(),
        NEXT_MEDIA_STREAM_ID.fetch_add(1, Ordering::Relaxed)
    );

    #[cfg(target_os = "macos")]
    if is_video {
        let output = output_directory.join(format!("noise-video-{id}.m4v"));
        let source_arg = source.to_string_lossy().into_owned();
        let output_arg = output.to_string_lossy().into_owned();
        // Preserve the source tracks and only relocate the movie metadata for
        // fast-start playback. Size is not a proxy for codec compatibility:
        // Apple's 1080p preset can inflate an efficient 45 MB source to nearly
        // 300 MB by forcing a high-bitrate H.264 encode.
        let preset = "PresetPassthrough";
        let status = Command::new("/usr/bin/avconvert")
            .args([
                "--source",
                &source_arg,
                "--preset",
                preset,
                "--output",
                &output_arg,
                "--replace",
            ])
            .status()
            .map_err(|error| error.to_string())?;
        if status.success() {
            let output_metadata = output.metadata().map_err(|error| error.to_string())?;
            // The 1080p compatibility transcode can be slightly larger than
            // an already-efficient HEVC source while still being the correct
            // streamable output. Reject invalid or oversized files, not valid
            // MP4s merely because their byte count grew.
            let usable_output =
                output_metadata.len() > 0 && output_metadata.len() <= 500 * 1024 * 1024;
            if usable_output {
                return Ok(OptimizedMediaUpload {
                    file_path: output.to_string_lossy().into_owned(),
                    file_name: format!("{stem}.m4v"),
                    mime_type: "video/mp4".to_owned(),
                    byte_length: output_metadata.len(),
                });
            }
        }
        let _ = fs::remove_file(&output);
        return Err(
            "this video could not be converted to noise's streamable MP4 format".to_owned(),
        );
    }

    let output = output_directory.join(format!("noise-media-{id}.{extension}"));
    fs::copy(&source, &output).map_err(|error| error.to_string())?;
    Ok(OptimizedMediaUpload {
        file_path: output.to_string_lossy().into_owned(),
        file_name: format!("{stem}.{extension}"),
        mime_type: mime_type.to_owned(),
        byte_length: metadata.len(),
    })
}

#[tauri::command]
async fn read_prepared_media(
    app: tauri::AppHandle,
    source_path: String,
) -> Result<tauri::ipc::Response, String> {
    let outgoing_directory = app
        .path()
        .app_cache_dir()
        .map_err(|error| error.to_string())?
        .join("outgoing");
    tauri::async_runtime::spawn_blocking(move || {
        let source = fs::canonicalize(source_path).map_err(|error| error.to_string())?;
        let outgoing = fs::canonicalize(outgoing_directory).map_err(|error| error.to_string())?;
        if !source.starts_with(outgoing) {
            return Err("noise can only read its prepared outgoing media".to_owned());
        }
        let metadata = source.metadata().map_err(|error| error.to_string())?;
        if !metadata.is_file() || metadata.len() == 0 || metadata.len() > 500 * 1024 * 1024 {
            return Err("prepared media has an invalid size".to_owned());
        }
        fs::read(source)
            .map(tauri::ipc::Response::new)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[derive(Clone)]
struct MediaHttpState {
    registry: MediaStreamRegistry,
    app: tauri::AppHandle,
    token: String,
}

async fn media_http_response(
    State(state): State<MediaHttpState>,
    AxumPath((token, id, _file_name)): AxumPath<(String, String, String)>,
    method: Method,
    headers: HeaderMap,
) -> Response<Body> {
    if token != state.token {
        return media_http_error(StatusCode::NOT_FOUND, "media stream is unavailable");
    }
    let registered = state
        .registry
        .0
        .lock()
        .ok()
        .and_then(|streams| streams.get(&id).cloned());
    let Some(mut core_request) = registered else {
        return media_http_error(StatusCode::NOT_FOUND, "media stream is unavailable");
    };
    let total = core_request
        .pointer("/attachment/byte_length")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let mime_type = core_request
        .pointer("/attachment/mime_type")
        .and_then(Value::as_str)
        .unwrap_or("application/octet-stream")
        .to_owned();
    if method == Method::HEAD {
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, mime_type)
            .header(header::CONTENT_LENGTH, total)
            .header(header::ACCEPT_RANGES, "bytes")
            .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
            .header(header::CACHE_CONTROL, "no-store")
            .body(Body::empty())
            .expect("media HTTP response is valid");
    }
    if method != Method::GET || total == 0 {
        return media_http_error(StatusCode::BAD_REQUEST, "media stream request is invalid");
    }
    let (start, end) = requested_media_range(
        headers
            .get(header::RANGE)
            .and_then(|value| value.to_str().ok()),
        total,
    );
    let Some(object) = core_request.as_object_mut() else {
        return media_http_error(StatusCode::BAD_REQUEST, "media stream request is invalid");
    };
    object.insert(
        "action".to_owned(),
        Value::String("fetch_attachment_range".to_owned()),
    );
    object.insert("offset".to_owned(), Value::from(start));
    object.insert("byte_length".to_owned(), Value::from(end - start + 1));

    let response = match noise_request_data(&state.app, core_request).await {
        Ok(data) => data,
        Err(error) => return media_http_error(StatusCode::BAD_GATEWAY, &error),
    };
    let decoded = response
        .get("data_base64")
        .and_then(Value::as_str)
        .and_then(|value| STANDARD.decode(value).ok());
    let Some(mut body) = decoded else {
        return media_http_error(
            StatusCode::BAD_GATEWAY,
            "relay returned an invalid media range",
        );
    };
    let maximum_length = (end - start + 1) as usize;
    if body.len() > maximum_length {
        body.truncate(maximum_length);
    }
    if body.is_empty() {
        return media_http_error(
            StatusCode::BAD_GATEWAY,
            "relay returned an empty media range",
        );
    }
    let actual_end = start
        .saturating_add(body.len() as u64)
        .saturating_sub(1)
        .min(total.saturating_sub(1));
    Response::builder()
        .status(StatusCode::PARTIAL_CONTENT)
        .header(header::CONTENT_TYPE, mime_type)
        .header(header::CONTENT_LENGTH, body.len())
        .header(header::ACCEPT_RANGES, "bytes")
        .header(
            header::CONTENT_RANGE,
            format!("bytes {start}-{actual_end}/{total}"),
        )
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(body))
        .expect("media HTTP response is valid")
}

fn media_http_error(status: StatusCode, message: &str) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain")
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(message.to_owned()))
        .expect("media HTTP error response is valid")
}

fn requested_media_range(range: Option<&str>, total: u64) -> (u64, u64) {
    // Match the web service worker: standard HTTP lets WebKit overlap range
    // requests without relying on oversized custom-protocol responses.
    const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;
    let requested = range
        .and_then(|value| value.strip_prefix("bytes="))
        .and_then(|value| value.split(',').next())
        .and_then(|value| {
            let (start, end) = value.split_once('-')?;
            if start.is_empty() {
                let suffix_length = end.parse::<u64>().ok()?.min(total);
                return Some((total.saturating_sub(suffix_length), total.saturating_sub(1)));
            }
            let start = start.parse::<u64>().ok()?;
            let end = if end.is_empty() {
                total.saturating_sub(1)
            } else {
                end.parse::<u64>().ok()?
            };
            Some((start, end))
        });
    let (start, requested_end) = requested.unwrap_or((0, total.saturating_sub(1)));
    let start = start.min(total.saturating_sub(1));
    let end = requested_end
        .min(start.saturating_add(MAX_RESPONSE_BYTES - 1))
        .min(total.saturating_sub(1))
        .max(start);
    (start, end)
}

#[tauri::command]
async fn download_media(
    app: tauri::AppHandle,
    source_path: String,
    file_name: String,
    destination_path: Option<String>,
) -> Result<String, String> {
    let cache_directory = app
        .path()
        .app_cache_dir()
        .map_err(|error| error.to_string())?
        .join("media");
    let download_directory = destination_path
        .is_none()
        .then(|| app.path().download_dir().map_err(|error| error.to_string()))
        .transpose()?;
    tauri::async_runtime::spawn_blocking(move || {
        let source = fs::canonicalize(source_path).map_err(|error| error.to_string())?;
        let cache = fs::canonicalize(cache_directory).map_err(|error| error.to_string())?;
        if !source.starts_with(&cache) {
            return Err(
                "noise can only download decrypted media from its private cache".to_owned(),
            );
        }
        if let Some(download_directory) = download_directory.as_ref() {
            fs::create_dir_all(download_directory).map_err(|error| error.to_string())?;
        }
        let raw_name = Path::new(&file_name)
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("noise-media");
        let sanitized = raw_name
            .chars()
            .map(|character| {
                if character.is_control()
                    || matches!(
                        character,
                        '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
                    )
                {
                    '_'
                } else {
                    character
                }
            })
            .collect::<String>();
        let sanitized = sanitized.trim_matches([' ', '.']);
        let sanitized = if sanitized.is_empty() {
            "noise-media"
        } else {
            sanitized
        };
        let requested = Path::new(sanitized);
        let stem = requested
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("noise-media");
        let extension = requested.extension().and_then(|value| value.to_str());
        let mut destination = if let Some(destination_path) = destination_path.as_ref() {
            let destination = PathBuf::from(destination_path);
            if !destination.is_absolute() {
                return Err("the selected download destination is invalid".to_owned());
            }
            destination
        } else {
            download_directory
                .as_ref()
                .expect("the default download directory is available")
                .join(sanitized)
        };
        if destination_path.is_none() {
            let mut copy_number = 2_u32;
            while destination.exists() {
                let candidate = if let Some(extension) = extension {
                    format!("{stem} ({copy_number}).{extension}")
                } else {
                    format!("{stem} ({copy_number})")
                };
                destination = download_directory
                    .as_ref()
                    .expect("the default download directory is available")
                    .join(candidate);
                copy_number += 1;
            }
        }
        fs::copy(&source, &destination).map_err(|error| error.to_string())?;
        Ok(destination.to_string_lossy().into_owned())
    })
    .await
    .map_err(|error| error.to_string())?
}

fn network_request(
    action: &str,
    relays: &[String],
    mask_relays: &[String],
    fields: impl IntoIterator<Item = (&'static str, Value)>,
) -> Value {
    let mut request = serde_json::Map::from_iter([
        ("action".to_owned(), Value::String(action.to_owned())),
        ("relays".to_owned(), json!(relays)),
        ("mask_relays".to_owned(), json!(mask_relays)),
    ]);
    for (key, value) in fields {
        request.insert(key.to_owned(), value);
    }
    Value::Object(request)
}

fn notification_preview(text: &str, attachment_mime_type: Option<&str>) -> String {
    let clean = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if !clean.is_empty() {
        let mut characters = clean.chars();
        let preview = characters.by_ref().take(179).collect::<String>();
        return if characters.next().is_some() {
            format!("{preview}…")
        } else {
            preview
        };
    }
    match attachment_mime_type {
        Some(value) if value.starts_with("image/") => "sent a photo".to_owned(),
        Some(value) if value.starts_with("video/") => "sent a video".to_owned(),
        Some(value) if value.starts_with("audio/") => "sent audio".to_owned(),
        _ => "sent a message".to_owned(),
    }
}

fn show_native_notification(_app: &tauri::AppHandle, title: &str, body: &str) {
    #[cfg(target_os = "macos")]
    {
        if let Err(error) = notify_rust::Notification::new()
            .summary(title)
            .body(body)
            .show()
        {
            eprintln!("noise could not deliver a macOS notification: {error}");
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        if let Err(error) = _app.notification().builder().title(title).body(body).show() {
            eprintln!("noise could not deliver a desktop notification: {error}");
        }
    }
}

fn incoming_direct_notifications(inbox: &Value) -> Option<(String, Vec<PendingNotification>)> {
    let self_public_key = inbox
        .pointer("/summary/identity/public_key")?
        .as_str()?
        .to_owned();
    let mut incoming = Vec::new();
    for conversation in inbox.get("conversations")?.as_array()? {
        let contact_username = conversation
            .pointer("/contact/username")
            .and_then(Value::as_str)
            .unwrap_or("new message");
        for message in conversation.get("messages")?.as_array()? {
            if message.get("author_public_key").and_then(Value::as_str)
                == Some(self_public_key.as_str())
            {
                continue;
            }
            let Some(event_id) = message.get("event_id").and_then(Value::as_str) else {
                continue;
            };
            let text = message
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let mime_type = message
                .pointer("/attachment/mime_type")
                .and_then(Value::as_str);
            incoming.push(PendingNotification {
                event_id: event_id.to_owned(),
                title: contact_username.to_owned(),
                body: notification_preview(text, mime_type),
                created_at_millis: message
                    .get("created_at_millis")
                    .and_then(Value::as_u64)
                    .unwrap_or_default(),
            });
        }
    }
    incoming.sort_by(|left, right| {
        left.created_at_millis
            .cmp(&right.created_at_millis)
            .then_with(|| left.event_id.cmp(&right.event_id))
    });
    Some((self_public_key, incoming))
}

fn reply_notifications(snapshot: &Value) -> Vec<PendingNotification> {
    let mut replies = snapshot
        .get("replies")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|reply| {
            let event_id = reply.get("event_id")?.as_str()?.to_owned();
            let group_name = reply.get("group_name")?.as_str().unwrap_or("noise");
            let username = reply.get("username")?.as_str().unwrap_or("someone");
            let text = reply
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let mime_type = reply.get("attachment_mime_type").and_then(Value::as_str);
            Some(PendingNotification {
                event_id,
                title: format!("{group_name} · {username} replied"),
                body: notification_preview(text, mime_type),
                created_at_millis: reply
                    .get("created_at_millis")
                    .and_then(Value::as_u64)
                    .unwrap_or_default(),
            })
        })
        .collect::<Vec<_>>();
    replies.sort_by(|left, right| {
        left.created_at_millis
            .cmp(&right.created_at_millis)
            .then_with(|| left.event_id.cmp(&right.event_id))
    });
    replies
}

async fn direct_notification_loop(
    app: tauri::AppHandle,
    relays: Vec<String>,
    mask_relays: Vec<String>,
) {
    let mut revision = None;
    let mut identity_public_key = String::new();
    let mut known = HashSet::new();
    let mut baseline_ready = false;

    loop {
        if !baseline_ready {
            let initial_watch = noise_request_data(
                &app,
                network_request(
                    "watch_direct",
                    &relays,
                    &mask_relays,
                    [("since", Value::Null)],
                ),
            )
            .await;
            let inbox = noise_request_data(
                &app,
                network_request("direct_inbox", &relays, &mask_relays, []),
            )
            .await;
            if let (Ok(change), Ok(inbox)) = (initial_watch, inbox)
                && let Some((public_key, incoming)) = incoming_direct_notifications(&inbox)
            {
                revision = change.get("revision").and_then(Value::as_u64);
                identity_public_key = public_key;
                known = incoming
                    .into_iter()
                    .map(|notification| notification.event_id)
                    .collect();
                baseline_ready = true;
                continue;
            }
            tokio::time::sleep(Duration::from_millis(1500)).await;
            continue;
        }

        let change = noise_request_data(
            &app,
            network_request(
                "watch_direct",
                &relays,
                &mask_relays,
                [("since", revision.map(Value::from).unwrap_or(Value::Null))],
            ),
        )
        .await;
        let Ok(change) = change else {
            baseline_ready = false;
            revision = None;
            tokio::time::sleep(Duration::from_millis(1500)).await;
            continue;
        };
        revision = change.get("revision").and_then(Value::as_u64).or(revision);
        if change.get("changed").and_then(Value::as_bool) != Some(true) {
            continue;
        }
        let Ok(inbox) = noise_request_data(
            &app,
            network_request("direct_inbox", &relays, &mask_relays, []),
        )
        .await
        else {
            continue;
        };
        let Some((public_key, incoming)) = incoming_direct_notifications(&inbox) else {
            continue;
        };
        if public_key != identity_public_key {
            identity_public_key = public_key;
            known = incoming
                .into_iter()
                .map(|notification| notification.event_id)
                .collect();
            continue;
        }
        for notification in incoming {
            if known.insert(notification.event_id) {
                show_native_notification(&app, &notification.title, &notification.body);
            }
        }
    }
}

async fn group_reply_notification_loop(
    app: tauri::AppHandle,
    group_id: String,
    relays: Vec<String>,
    mask_relays: Vec<String>,
) {
    let mut revision = None;
    let mut known = HashSet::new();
    let mut baseline_ready = false;
    let mut notify_after_millis = 0;

    loop {
        if !baseline_ready {
            let initial_watch = noise_request_data(
                &app,
                network_request(
                    "watch_group_id",
                    &relays,
                    &mask_relays,
                    [
                        ("group_id", Value::String(group_id.clone())),
                        ("since", Value::Null),
                    ],
                ),
            )
            .await;
            if let Ok(change) = initial_watch {
                revision = change.get("revision").and_then(Value::as_u64);
                notify_after_millis = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |duration| duration.as_millis() as u64);
                known.clear();
                baseline_ready = true;
                continue;
            }
            tokio::time::sleep(Duration::from_millis(1500)).await;
            continue;
        }

        let change = noise_request_data(
            &app,
            network_request(
                "watch_group_id",
                &relays,
                &mask_relays,
                [
                    ("group_id", Value::String(group_id.clone())),
                    ("since", revision.map(Value::from).unwrap_or(Value::Null)),
                ],
            ),
        )
        .await;
        let Ok(change) = change else {
            return;
        };
        revision = change.get("revision").and_then(Value::as_u64).or(revision);
        if change.get("changed").and_then(Value::as_bool) != Some(true) {
            continue;
        }
        let Ok(snapshot) = noise_request_data(
            &app,
            network_request(
                "reply_notification_snapshot",
                &relays,
                &mask_relays,
                [("group_id", Value::String(group_id.clone()))],
            ),
        )
        .await
        else {
            continue;
        };
        for notification in reply_notifications(&snapshot) {
            if notification.created_at_millis > notify_after_millis
                && known.insert(notification.event_id)
            {
                show_native_notification(&app, &notification.title, &notification.body);
            }
        }
    }
}

async fn group_notification_supervisor(
    app: tauri::AppHandle,
    relays: Vec<String>,
    mask_relays: Vec<String>,
) {
    let running = Arc::new(Mutex::new(HashSet::<String>::new()));
    loop {
        if let Ok(summary) = noise_request_data(&app, json!({ "action": "status" })).await {
            for group_id in summary
                .get("groups")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|group| group.get("group_id").and_then(Value::as_str))
            {
                let should_start = running
                    .lock()
                    .is_ok_and(|mut groups| groups.insert(group_id.to_owned()));
                if !should_start {
                    continue;
                }
                let group_id = group_id.to_owned();
                let child_app = app.clone();
                let child_relays = relays.clone();
                let child_mask_relays = mask_relays.clone();
                let child_running = running.clone();
                tauri::async_runtime::spawn(async move {
                    group_reply_notification_loop(
                        child_app,
                        group_id.clone(),
                        child_relays,
                        child_mask_relays,
                    )
                    .await;
                    if let Ok(mut groups) = child_running.lock() {
                        groups.remove(&group_id);
                    }
                });
            }
        }
        tokio::time::sleep(Duration::from_secs(10)).await;
    }
}

fn start_notification_watchers(app: tauri::AppHandle, relays: Vec<String>) {
    if NOTIFICATION_WATCHERS_STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    tauri::async_runtime::spawn(async move {
        let mask_relays = noise_request_data(
            &app,
            json!({ "action": "discover_relay_masks", "relays": relays }),
        )
        .await
        .ok()
        .and_then(|value| {
            value.as_array().map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
        })
        .unwrap_or_default();
        tauri::async_runtime::spawn(direct_notification_loop(
            app.clone(),
            relays.clone(),
            mask_relays.clone(),
        ));
        tauri::async_runtime::spawn(group_notification_supervisor(app, relays, mask_relays));
    });
}

#[tauri::command]
async fn ensure_native_notification_permission(
    app: tauri::AppHandle,
    relays: Vec<String>,
) -> Result<bool, String> {
    #[cfg(target_os = "macos")]
    let granted = tauri::async_runtime::spawn_blocking(|| {
        notify_rust::request_auth_blocking().map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())??;

    #[cfg(not(target_os = "macos"))]
    let granted = true;

    if granted {
        start_notification_watchers(app, relays);
    }
    Ok(granted)
}

fn main() {
    let media_streams = MediaStreamRegistry::default();
    let media_listener = std::net::TcpListener::bind(("127.0.0.1", 0))
        .expect("desktop media server can bind to localhost");
    media_listener
        .set_nonblocking(true)
        .expect("desktop media server can run asynchronously");
    let media_address = media_listener
        .local_addr()
        .expect("desktop media server has a local address");
    let media_token = URL_SAFE_NO_PAD.encode(random::<[u8; 32]>());
    MEDIA_SERVER_BASE_URL
        .set(format!("http://{media_address}/{media_token}"))
        .expect("desktop media server is configured once");
    let server_streams = media_streams.clone();
    let server_token = media_token.clone();
    tauri::Builder::default()
        .manage(media_streams)
        .setup(move |app| {
            let state = MediaHttpState {
                registry: server_streams,
                app: app.handle().clone(),
                token: server_token,
            };
            let router = Router::new()
                .route(
                    "/{token}/noise-media/{id}/{file_name}",
                    any(media_http_response),
                )
                .with_state(state);
            tauri::async_runtime::spawn(async move {
                let listener = match tokio::net::TcpListener::from_std(media_listener) {
                    Ok(listener) => listener,
                    Err(error) => {
                        eprintln!("desktop media server could not start: {error}");
                        return;
                    }
                };
                if let Err(error) = axum::serve(listener, router).await {
                    eprintln!("desktop media server stopped: {error}");
                }
            });
            Ok(())
        })
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            noise_invoke,
            list_local_accounts,
            start_adding_local_account,
            cancel_adding_local_account,
            switch_local_account,
            register_media_stream,
            optimize_video_upload,
            convert_cached_image_for_display,
            inspect_media_upload,
            prepare_media_upload,
            read_prepared_media,
            download_media,
            ensure_native_notification_permission
        ])
        .run(tauri::generate_context!())
        .expect("error while running noise");
}
