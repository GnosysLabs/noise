#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;

use serde_json::{Value, json};
use tauri::Manager;

fn state_path() -> Result<PathBuf, String> {
    dirs::data_dir()
        .map(|directory| directory.join("noise").join("profile.json"))
        .ok_or_else(|| "this device has no application data directory".to_owned())
}

#[tauri::command]
async fn noise_invoke(app: tauri::AppHandle, mut request: Value) -> Value {
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

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .invoke_handler(tauri::generate_handler![noise_invoke])
        .run(tauri::generate_context!())
        .expect("error while running Noise");
}
