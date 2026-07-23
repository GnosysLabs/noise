#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    collections::HashSet,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use serde_json::{Value, json};
use tauri::Manager;
#[cfg(not(target_os = "macos"))]
use tauri_plugin_notification::NotificationExt;

static NOTIFICATION_WATCHERS_STARTED: AtomicBool = AtomicBool::new(false);

#[derive(Clone)]
struct PendingNotification {
    event_id: String,
    title: String,
    body: String,
    created_at_millis: u64,
}

fn state_path() -> Result<PathBuf, String> {
    dirs::data_dir()
        .map(|directory| directory.join("noise").join("profile.json"))
        .ok_or_else(|| "this device has no application data directory".to_owned())
}

async fn execute_noise_request(app: &tauri::AppHandle, mut request: Value) -> Value {
    let Ok(path) = state_path() else {
        return json!({ "ok": false, "error": "could not locate Noise identity storage" });
    };
    let Some(request_object) = request.as_object_mut() else {
        return json!({ "ok": false, "error": "Noise request must be an object" });
    };
    request_object.insert(
        "state_path".into(),
        Value::String(path.to_string_lossy().into_owned()),
    );
    let Ok(cache_path) = app.path().app_cache_dir() else {
        return json!({ "ok": false, "error": "could not locate Noise media cache" });
    };
    request_object.insert(
        "cache_path".into(),
        Value::String(cache_path.to_string_lossy().into_owned()),
    );

    match tauri::async_runtime::spawn_blocking(move || {
        noise_ffi::response_json(&request.to_string())
    })
    .await
    {
        Ok(response) => serde_json::from_str(&response)
            .unwrap_or_else(|error| json!({ "ok": false, "error": error.to_string() })),
        Err(error) => json!({ "ok": false, "error": error.to_string() }),
    }
}

async fn noise_request_data(app: &tauri::AppHandle, request: Value) -> Result<Value, String> {
    let response = execute_noise_request(app, request).await;
    if response.get("ok").and_then(Value::as_bool) == Some(true) {
        Ok(response.get("data").cloned().unwrap_or(Value::Null))
    } else {
        Err(response
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("Noise request failed")
            .to_owned())
    }
}

#[tauri::command]
async fn noise_invoke(app: tauri::AppHandle, request: Value) -> Value {
    execute_noise_request(&app, request).await
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
            eprintln!("Noise could not deliver a macOS notification: {error}");
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        if let Err(error) = _app.notification().builder().title(title).body(body).show() {
            eprintln!("Noise could not deliver a desktop notification: {error}");
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
            let group_name = reply.get("group_name")?.as_str().unwrap_or("Noise");
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
            let snapshot = noise_request_data(
                &app,
                network_request(
                    "reply_notification_snapshot",
                    &relays,
                    &mask_relays,
                    [("group_id", Value::String(group_id.clone()))],
                ),
            )
            .await;
            if let (Ok(change), Ok(snapshot)) = (initial_watch, snapshot) {
                revision = change.get("revision").and_then(Value::as_u64);
                known = reply_notifications(&snapshot)
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
            if known.insert(notification.event_id) {
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
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            noise_invoke,
            ensure_native_notification_permission
        ])
        .run(tauri::generate_context!())
        .expect("error while running Noise");
}
