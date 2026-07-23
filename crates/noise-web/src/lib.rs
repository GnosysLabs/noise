use noise_client::{MediaAttachment, NoiseClient, ProfileImage};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use wasm_bindgen::prelude::*;

const STATE_PATH: &str = "noise-browser-session";
const CACHE_PATH: &str = "noise-browser-cache";

fn required<T: DeserializeOwned>(request: &Value, name: &str) -> Result<T, String> {
    request
        .get(name)
        .cloned()
        .ok_or_else(|| format!("{name} is missing"))
        .and_then(|value| serde_json::from_value(value).map_err(|error| error.to_string()))
}

fn optional<T: DeserializeOwned>(request: &Value, name: &str) -> Result<Option<T>, String> {
    request
        .get(name)
        .cloned()
        .filter(|value| !value.is_null())
        .map(serde_json::from_value)
        .transpose()
        .map_err(|error| error.to_string())
}

fn relays(request: &Value) -> Result<Vec<String>, String> {
    required(request, "relays")
}

fn data<T: serde::Serialize>(value: T) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|error| error.to_string())
}

async fn dispatch(request: Value) -> Result<Value, String> {
    let action = required::<String>(&request, "action")?;
    let mask_relays = optional::<Vec<String>>(&request, "mask_relays")?.unwrap_or_default();
    let client = NoiseClient::with_mask_relays(mask_relays).map_err(|error| error.to_string())?;

    match action.as_str() {
        "discover_relay_masks" => data(
            client
                .discover_relay_masks(CACHE_PATH, relays(&request)?)
                .await
                .map_err(|error| error.to_string())?,
        ),
        "status" => {
            if !client.has_local_state(STATE_PATH) {
                Ok(Value::Null)
            } else {
                data(
                    client
                        .local_summary(STATE_PATH)
                        .map_err(|error| error.to_string())?,
                )
            }
        }
        "initialize" => data(
            client
                .initialize(
                    STATE_PATH,
                    required::<String>(&request, "username")?,
                    required::<String>(&request, "password")?,
                    optional::<String>(&request, "avatar_data_base64")?,
                    optional::<String>(&request, "avatar_mime_type")?,
                    relays(&request)?,
                )
                .await
                .map_err(|error| error.to_string())?,
        ),
        "sign_in" => data(
            client
                .sign_in(
                    STATE_PATH,
                    &required::<String>(&request, "noise_id")?,
                    required::<String>(&request, "password")?,
                    relays(&request)?,
                )
                .await
                .map_err(|error| error.to_string())?,
        ),
        "sync_account" => data(
            client
                .sync_account(STATE_PATH, relays(&request)?)
                .await
                .map_err(|error| error.to_string())?,
        ),
        "sync_read_state" => data(
            client
                .sync_read_state(STATE_PATH, relays(&request)?)
                .await
                .map_err(|error| error.to_string())?,
        ),
        "watch_account" => data(
            client
                .watch_account(
                    STATE_PATH,
                    optional::<u64>(&request, "since")?,
                    relays(&request)?,
                )
                .await
                .map_err(|error| error.to_string())?,
        ),
        "logout" => {
            client
                .logout(STATE_PATH, CACHE_PATH)
                .map_err(|error| error.to_string())?;
            Ok(Value::Null)
        }
        "select_group" => data(
            client
                .select_group(STATE_PATH, &required::<String>(&request, "group_id")?)
                .map_err(|error| error.to_string())?,
        ),
        "update_profile" => data(
            client
                .update_profile(
                    STATE_PATH,
                    required::<String>(&request, "username")?,
                    required::<String>(&request, "bio")?,
                    optional::<String>(&request, "avatar_data_base64")?,
                    optional::<String>(&request, "avatar_mime_type")?,
                    required::<bool>(&request, "remove_avatar")?,
                    required::<bool>(&request, "accepts_direct_messages")?,
                    relays(&request)?,
                )
                .await
                .map_err(|error| error.to_string())?,
        ),
        "update_group_profile" => data(
            client
                .update_group_profile(
                    STATE_PATH,
                    required::<String>(&request, "name")?,
                    required::<String>(&request, "description")?,
                    optional::<String>(&request, "rules")?.unwrap_or_default(),
                    optional::<String>(&request, "avatar_data_base64")?,
                    optional::<String>(&request, "avatar_mime_type")?,
                    required::<bool>(&request, "remove_avatar")?,
                    optional::<String>(&request, "background_data_base64")?,
                    optional::<String>(&request, "background_mime_type")?,
                    optional::<bool>(&request, "remove_background")?.unwrap_or(false),
                    optional::<String>(&request, "mobile_background_data_base64")?,
                    optional::<String>(&request, "mobile_background_mime_type")?,
                    optional::<bool>(&request, "remove_mobile_background")?.unwrap_or(false),
                    optional::<String>(&request, "accent_color")?,
                    optional::<bool>(&request, "members_can_send_messages")?,
                    optional::<bool>(&request, "members_can_send_media")?,
                    relays(&request)?,
                )
                .await
                .map_err(|error| error.to_string())?,
        ),
        "rotate_frequency" => data(
            client
                .rotate_frequency(
                    STATE_PATH,
                    required::<bool>(&request, "revoke_only")?,
                    relays(&request)?,
                )
                .await
                .map_err(|error| error.to_string())?,
        ),
        "fetch_avatar" => data(
            client
                .fetch_avatar(
                    CACHE_PATH,
                    &required::<ProfileImage>(&request, "image")?,
                    relays(&request)?,
                )
                .await
                .map_err(|error| error.to_string())?,
        ),
        "fetch_attachment" => data(
            client
                .fetch_attachment(
                    STATE_PATH,
                    CACHE_PATH,
                    optional::<String>(&request, "scope_id")?,
                    &required::<MediaAttachment>(&request, "attachment")?,
                    relays(&request)?,
                )
                .await
                .map_err(|error| error.to_string())?,
        ),
        "upload_media_chunk" => data(
            client
                .upload_media_chunk(
                    STATE_PATH,
                    required::<String>(&request, "data_base64")?,
                    relays(&request)?,
                )
                .await
                .map_err(|error| error.to_string())?,
        ),
        "upload_direct_media_chunk" => data(
            client
                .upload_direct_media_chunk(
                    STATE_PATH,
                    required::<String>(&request, "data_base64")?,
                    relays(&request)?,
                )
                .await
                .map_err(|error| error.to_string())?,
        ),
        "make" => data(
            client
                .make(
                    STATE_PATH,
                    required::<String>(&request, "name")?,
                    optional::<String>(&request, "avatar_data_base64")?,
                    optional::<String>(&request, "avatar_mime_type")?,
                    relays(&request)?,
                )
                .await
                .map_err(|error| error.to_string())?,
        ),
        "join" => data(
            client
                .join(
                    STATE_PATH,
                    &required::<String>(&request, "frequency")?,
                    relays(&request)?,
                )
                .await
                .map_err(|error| error.to_string())?,
        ),
        "sync_group_encryption" => data(
            client
                .sync_active_group_encryption(STATE_PATH, CACHE_PATH, relays(&request)?)
                .await
                .map_err(|error| error.to_string())?,
        ),
        "say" => {
            let text = required::<String>(&request, "text")?;
            let attachment = optional::<MediaAttachment>(&request, "attachment")?;
            let reply_to = optional::<String>(&request, "reply_to_message_id")?;
            let relay_list = relays(&request)?;
            let sent = if let Some(attachment) = attachment {
                client
                    .say_with_attachment_reply(STATE_PATH, text, attachment, reply_to, relay_list)
                    .await
            } else {
                client
                    .say_reply(STATE_PATH, text, reply_to, relay_list)
                    .await
            }
            .map_err(|error| error.to_string())?;
            data(sent)
        }
        "start_direct" => data(
            client
                .start_direct(
                    STATE_PATH,
                    &required::<String>(&request, "public_key")?,
                    required::<String>(&request, "username")?,
                    required::<String>(&request, "bio")?,
                    optional::<ProfileImage>(&request, "avatar")?,
                    required::<bool>(&request, "accepts_direct_messages")?,
                )
                .map_err(|error| error.to_string())?,
        ),
        "select_direct" => data(
            client
                .select_direct(STATE_PATH, &required::<String>(&request, "public_key")?)
                .map_err(|error| error.to_string())?,
        ),
        "sync_directs" => data(
            client
                .sync_directs(STATE_PATH, CACHE_PATH, relays(&request)?)
                .await
                .map_err(|error| error.to_string())?,
        ),
        "direct_inbox" => data(
            client
                .direct_inbox(STATE_PATH, CACHE_PATH, relays(&request)?)
                .await
                .map_err(|error| error.to_string())?,
        ),
        "direct_conversation" => data(
            client
                .direct_conversation(STATE_PATH, CACHE_PATH, relays(&request)?)
                .await
                .map_err(|error| error.to_string())?,
        ),
        "mark_direct_read" => data(
            client
                .mark_direct_read(STATE_PATH, &required::<String>(&request, "public_key")?)
                .map_err(|error| error.to_string())?,
        ),
        "watch_direct" => data(
            client
                .watch_direct(
                    STATE_PATH,
                    optional::<u64>(&request, "since")?,
                    relays(&request)?,
                )
                .await
                .map_err(|error| error.to_string())?,
        ),
        "say_direct" => data(
            client
                .say_direct(
                    STATE_PATH,
                    required::<String>(&request, "text")?,
                    optional::<MediaAttachment>(&request, "attachment")?,
                    optional::<String>(&request, "reply_to_message_id")?,
                    relays(&request)?,
                )
                .await
                .map_err(|error| error.to_string())?,
        ),
        "delete_direct" => data(
            client
                .delete_direct(
                    STATE_PATH,
                    CACHE_PATH,
                    &required::<String>(&request, "public_key")?,
                    required::<bool>(&request, "for_both")?,
                    relays(&request)?,
                )
                .await
                .map_err(|error| error.to_string())?,
        ),
        "set_moderator" => {
            client
                .set_moderator(
                    STATE_PATH,
                    &required::<String>(&request, "member_public_key")?,
                    required::<bool>(&request, "enabled")?,
                    relays(&request)?,
                )
                .await
                .map_err(|error| error.to_string())?;
            Ok(Value::Null)
        }
        "delete_message" => {
            client
                .delete_message(
                    STATE_PATH,
                    &required::<String>(&request, "message_event_id")?,
                    relays(&request)?,
                )
                .await
                .map_err(|error| error.to_string())?;
            Ok(Value::Null)
        }
        "set_reaction" => {
            client
                .set_reaction(
                    STATE_PATH,
                    &required::<String>(&request, "message_event_id")?,
                    &required::<String>(&request, "emoji")?,
                    required::<bool>(&request, "enabled")?,
                    relays(&request)?,
                )
                .await
                .map_err(|error| error.to_string())?;
            Ok(Value::Null)
        }
        "report_message" => {
            client
                .report_message(
                    STATE_PATH,
                    &required::<String>(&request, "message_event_id")?,
                    required::<String>(&request, "reason")?,
                    relays(&request)?,
                )
                .await
                .map_err(|error| error.to_string())?;
            Ok(Value::Null)
        }
        "resolve_report" => {
            client
                .resolve_report(
                    STATE_PATH,
                    &required::<String>(&request, "report_event_id")?,
                    relays(&request)?,
                )
                .await
                .map_err(|error| error.to_string())?;
            Ok(Value::Null)
        }
        "ban_member" => {
            client
                .ban_member(
                    STATE_PATH,
                    &required::<String>(&request, "member_public_key")?,
                    required::<bool>(&request, "delete_messages")?,
                    relays(&request)?,
                )
                .await
                .map_err(|error| error.to_string())?;
            Ok(Value::Null)
        }
        "unban_member" => {
            client
                .unban_member(
                    STATE_PATH,
                    &required::<String>(&request, "member_public_key")?,
                    relays(&request)?,
                )
                .await
                .map_err(|error| error.to_string())?;
            Ok(Value::Null)
        }
        "conversation" => data(
            client
                .conversation(STATE_PATH, relays(&request)?)
                .await
                .map_err(|error| error.to_string())?,
        ),
        "cached_conversation" => data(
            client
                .cached_conversation(STATE_PATH, &required::<String>(&request, "group_id")?)
                .map_err(|error| error.to_string())?,
        ),
        "watch_group" => data(
            client
                .watch_group(
                    STATE_PATH,
                    optional::<u64>(&request, "since")?,
                    relays(&request)?,
                )
                .await
                .map_err(|error| error.to_string())?,
        ),
        "watch_group_id" => data(
            client
                .watch_group_id(
                    STATE_PATH,
                    &required::<String>(&request, "group_id")?,
                    optional::<u64>(&request, "since")?,
                    relays(&request)?,
                )
                .await
                .map_err(|error| error.to_string())?,
        ),
        "heartbeat_presence" => data(
            client
                .heartbeat_presence(
                    STATE_PATH,
                    optional::<bool>(&request, "active")?.unwrap_or(true),
                    relays(&request)?,
                )
                .await
                .map_err(|error| error.to_string())?,
        ),
        "reply_notification_snapshot" => data(
            client
                .reply_notification_snapshot(
                    STATE_PATH,
                    &required::<String>(&request, "group_id")?,
                    relays(&request)?,
                )
                .await
                .map_err(|error| error.to_string())?,
        ),
        "leave" => data(
            client
                .leave(STATE_PATH, CACHE_PATH, relays(&request)?)
                .await
                .map_err(|error| error.to_string())?,
        ),
        "delete_group" => data(
            client
                .delete_group(
                    STATE_PATH,
                    CACHE_PATH,
                    &required::<String>(&request, "group_id")?,
                    relays(&request)?,
                )
                .await
                .map_err(|error| error.to_string())?,
        ),
        "delete_account" => {
            client
                .delete_account(
                    STATE_PATH,
                    CACHE_PATH,
                    required::<bool>(&request, "delete_group_messages")?,
                    required::<bool>(&request, "delete_direct_threads")?,
                    relays(&request)?,
                )
                .await
                .map_err(|error| error.to_string())?;
            Ok(Value::Null)
        }
        _ => Err(format!("unsupported Noise action: {action}")),
    }
}

#[wasm_bindgen]
pub async fn noise_invoke(request: JsValue) -> JsValue {
    let response = match serde_wasm_bindgen::from_value::<Value>(request) {
        Ok(request) => match dispatch(request).await {
            Ok(data) => json!({ "ok": true, "data": data }),
            Err(error) => json!({ "ok": false, "error": error }),
        },
        Err(error) => json!({ "ok": false, "error": error.to_string() }),
    };
    response
        .serialize(&serde_wasm_bindgen::Serializer::json_compatible())
        .unwrap_or(JsValue::NULL)
}

#[wasm_bindgen]
pub fn restore_session(bytes: Vec<u8>) -> Result<(), JsValue> {
    noise_client::import_web_state(STATE_PATH, bytes)
        .map_err(|error| JsValue::from_str(&error.to_string()))
}

#[wasm_bindgen]
pub fn session_state() -> Vec<u8> {
    noise_client::export_web_state(STATE_PATH).unwrap_or_default()
}
