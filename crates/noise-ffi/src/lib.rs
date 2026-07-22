use std::{
    ffi::{CStr, CString, c_char},
    panic::{AssertUnwindSafe, catch_unwind},
    path::Path,
    sync::OnceLock,
};

use noise_client::{NoiseClient, ProfileImage};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::runtime::Runtime;

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum Request {
    Status {
        state_path: String,
    },
    Initialize {
        state_path: String,
        username: String,
    },
    SelectGroup {
        state_path: String,
        group_id: String,
    },
    UpdateProfile {
        state_path: String,
        bio: String,
        avatar_data_base64: Option<String>,
        avatar_mime_type: Option<String>,
        remove_avatar: bool,
        relays: Vec<String>,
    },
    UpdateGroupProfile {
        state_path: String,
        name: String,
        description: String,
        avatar_data_base64: Option<String>,
        avatar_mime_type: Option<String>,
        remove_avatar: bool,
        relays: Vec<String>,
    },
    FetchAvatar {
        image: ProfileImage,
        relays: Vec<String>,
    },
    Make {
        state_path: String,
        name: String,
        avatar_data_base64: Option<String>,
        avatar_mime_type: Option<String>,
        relays: Vec<String>,
    },
    Join {
        state_path: String,
        frequency: String,
        relays: Vec<String>,
    },
    Say {
        state_path: String,
        text: String,
        relays: Vec<String>,
    },
    Conversation {
        state_path: String,
        relays: Vec<String>,
    },
    Leave {
        state_path: String,
        relays: Vec<String>,
    },
    DeleteGroup {
        state_path: String,
        group_id: String,
        relays: Vec<String>,
    },
}

fn runtime() -> Result<&'static Runtime, String> {
    static RUNTIME: OnceLock<Result<Runtime, String>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(|error| error.to_string())
        })
        .as_ref()
        .map_err(Clone::clone)
}

fn invoke(request_json: &str) -> Result<Value, String> {
    let request =
        serde_json::from_str::<Request>(request_json).map_err(|error| error.to_string())?;
    let client = NoiseClient::default();

    match request {
        Request::Status { state_path } => {
            if !Path::new(&state_path).exists() {
                return Ok(Value::Null);
            }
            serde_json::to_value(
                client
                    .local_summary(state_path)
                    .map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())
        }
        Request::Initialize {
            state_path,
            username,
        } => serde_json::to_value(
            client
                .initialize(state_path, username)
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::SelectGroup {
            state_path,
            group_id,
        } => serde_json::to_value(
            client
                .select_group(state_path, &group_id)
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::UpdateProfile {
            state_path,
            bio,
            avatar_data_base64,
            avatar_mime_type,
            remove_avatar,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on(client.update_profile(
                    state_path,
                    bio,
                    avatar_data_base64,
                    avatar_mime_type,
                    remove_avatar,
                    relays,
                ))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::UpdateGroupProfile {
            state_path,
            name,
            description,
            avatar_data_base64,
            avatar_mime_type,
            remove_avatar,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on(client.update_group_profile(
                    state_path,
                    name,
                    description,
                    avatar_data_base64,
                    avatar_mime_type,
                    remove_avatar,
                    relays,
                ))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::FetchAvatar { image, relays } => serde_json::to_value(
            runtime()?
                .block_on(client.fetch_avatar(&image, relays))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::Make {
            state_path,
            name,
            avatar_data_base64,
            avatar_mime_type,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on(client.make(
                    state_path,
                    name,
                    avatar_data_base64,
                    avatar_mime_type,
                    relays,
                ))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::Join {
            state_path,
            frequency,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on(client.join(state_path, &frequency, relays))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::Say {
            state_path,
            text,
            relays,
        } => {
            runtime()?
                .block_on(client.say(state_path, text, relays))
                .map_err(|error| error.to_string())?;
            Ok(Value::Null)
        }
        Request::Conversation { state_path, relays } => serde_json::to_value(
            runtime()?
                .block_on(client.conversation(state_path, relays))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::Leave { state_path, relays } => {
            runtime()?
                .block_on(client.leave(state_path, relays))
                .map_err(|error| error.to_string())?;
            Ok(Value::Null)
        }
        Request::DeleteGroup {
            state_path,
            group_id,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on(client.delete_group(state_path, &group_id, relays))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
    }
}

pub fn response_json(request_json: &str) -> String {
    match invoke(request_json) {
        Ok(data) => json!({ "ok": true, "data": data }).to_string(),
        Err(error) => json!({ "ok": false, "error": error }).to_string(),
    }
}

fn owned_c_string(value: String) -> *mut c_char {
    match CString::new(value) {
        Ok(value) => value.into_raw(),
        Err(_) => CString::new(r#"{"ok":false,"error":"response contained invalid bytes"}"#)
            .expect("static error response is a valid C string")
            .into_raw(),
    }
}

/// Invoke one Noise client operation.
///
/// The returned string belongs to the caller and must be released with
/// `noise_free_string`.
///
/// # Safety
///
/// `request_json` must be either null or point to a valid, NUL-terminated C
/// string for the duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn noise_invoke(request_json: *const c_char) -> *mut c_char {
    if request_json.is_null() {
        return owned_c_string(json!({ "ok": false, "error": "request was null" }).to_string());
    }

    let result = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: The pointer contract is documented on this exported function.
        let request = unsafe { CStr::from_ptr(request_json) };
        match request.to_str() {
            Ok(request) => response_json(request),
            Err(_) => json!({ "ok": false, "error": "request was not UTF-8" }).to_string(),
        }
    }))
    .unwrap_or_else(|_| json!({ "ok": false, "error": "Noise core panicked" }).to_string());

    owned_c_string(result)
}

/// Release a string returned by `noise_invoke`.
///
/// # Safety
///
/// `value` must be null or a pointer returned by `noise_invoke` that has not
/// previously been freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn noise_free_string(value: *mut c_char) {
    if !value.is_null() {
        // SAFETY: The pointer contract is documented on this exported function.
        drop(unsafe { CString::from_raw(value) });
    }
}

#[cfg(test)]
mod tests {
    use super::response_json;

    #[test]
    fn invalid_requests_are_structured_errors() {
        let response: serde_json::Value =
            serde_json::from_str(&response_json("{}")).expect("response is JSON");
        assert_eq!(response["ok"], false);
    }
}
