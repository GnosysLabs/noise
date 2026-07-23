use std::{
    ffi::{CStr, CString, c_char},
    panic::{AssertUnwindSafe, catch_unwind},
    path::Path,
    sync::{Mutex, OnceLock},
};

use noise_client::{MediaAttachment, NoiseClient, ProfileImage};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::runtime::Runtime;

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum Request {
    DiscoverRelayMasks {
        cache_path: String,
        relays: Vec<String>,
    },
    Status {
        state_path: String,
    },
    Initialize {
        state_path: String,
        username: String,
        password: String,
        avatar_data_base64: Option<String>,
        avatar_mime_type: Option<String>,
        relays: Vec<String>,
    },
    SignIn {
        state_path: String,
        noise_id: String,
        password: String,
        relays: Vec<String>,
    },
    SyncAccount {
        state_path: String,
        relays: Vec<String>,
    },
    SyncReadState {
        state_path: String,
        relays: Vec<String>,
    },
    WatchAccount {
        state_path: String,
        since: Option<u64>,
        relays: Vec<String>,
    },
    Logout {
        state_path: String,
        cache_path: String,
    },
    SelectGroup {
        state_path: String,
        group_id: String,
    },
    UpdateProfile {
        state_path: String,
        username: String,
        bio: String,
        avatar_data_base64: Option<String>,
        avatar_mime_type: Option<String>,
        remove_avatar: bool,
        accepts_direct_messages: bool,
        relays: Vec<String>,
    },
    UpdateGroupProfile {
        state_path: String,
        name: String,
        description: String,
        #[serde(default)]
        rules: String,
        avatar_data_base64: Option<String>,
        avatar_mime_type: Option<String>,
        remove_avatar: bool,
        #[serde(default)]
        background_data_base64: Option<String>,
        #[serde(default)]
        background_mime_type: Option<String>,
        #[serde(default)]
        remove_background: bool,
        #[serde(default)]
        accent_color: Option<String>,
        members_can_send_messages: Option<bool>,
        members_can_send_media: Option<bool>,
        relays: Vec<String>,
    },
    RotateFrequency {
        state_path: String,
        revoke_only: bool,
        relays: Vec<String>,
    },
    FetchAvatar {
        cache_path: String,
        image: ProfileImage,
        relays: Vec<String>,
    },
    FetchAttachment {
        state_path: String,
        cache_path: String,
        scope_id: Option<String>,
        attachment: MediaAttachment,
        relays: Vec<String>,
    },
    UploadMediaChunk {
        state_path: String,
        data_base64: String,
        relays: Vec<String>,
    },
    UploadDirectMediaChunk {
        state_path: String,
        data_base64: String,
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
    SyncGroupEncryption {
        state_path: String,
        cache_path: String,
        relays: Vec<String>,
    },
    SyncGroupActivity {
        state_path: String,
        group_id: String,
        relays: Vec<String>,
    },
    MarkGroupRead {
        state_path: String,
        group_id: String,
    },
    Say {
        state_path: String,
        text: String,
        attachment: Option<MediaAttachment>,
        #[serde(default)]
        reply_to_message_id: Option<String>,
        relays: Vec<String>,
    },
    StartDirect {
        state_path: String,
        public_key: String,
        username: String,
        bio: String,
        avatar: Option<ProfileImage>,
        accepts_direct_messages: bool,
    },
    SelectDirect {
        state_path: String,
        public_key: String,
    },
    SyncDirects {
        state_path: String,
        cache_path: String,
        relays: Vec<String>,
    },
    DirectInbox {
        state_path: String,
        cache_path: String,
        relays: Vec<String>,
    },
    DirectConversation {
        state_path: String,
        cache_path: String,
        relays: Vec<String>,
    },
    MarkDirectRead {
        state_path: String,
        public_key: String,
    },
    WatchDirect {
        state_path: String,
        since: Option<u64>,
        relays: Vec<String>,
    },
    SayDirect {
        state_path: String,
        text: String,
        attachment: Option<MediaAttachment>,
        #[serde(default)]
        reply_to_message_id: Option<String>,
        relays: Vec<String>,
    },
    DeleteDirect {
        state_path: String,
        cache_path: String,
        public_key: String,
        for_both: bool,
        relays: Vec<String>,
    },
    SetModerator {
        state_path: String,
        member_public_key: String,
        enabled: bool,
        relays: Vec<String>,
    },
    DeleteMessage {
        state_path: String,
        message_event_id: String,
        relays: Vec<String>,
    },
    SetReaction {
        state_path: String,
        message_event_id: String,
        emoji: String,
        enabled: bool,
        relays: Vec<String>,
    },
    ReportMessage {
        state_path: String,
        message_event_id: String,
        reason: String,
        relays: Vec<String>,
    },
    ResolveReport {
        state_path: String,
        report_event_id: String,
        relays: Vec<String>,
    },
    BanMember {
        state_path: String,
        member_public_key: String,
        delete_messages: bool,
        relays: Vec<String>,
    },
    UnbanMember {
        state_path: String,
        member_public_key: String,
        relays: Vec<String>,
    },
    Conversation {
        state_path: String,
        relays: Vec<String>,
    },
    WatchGroup {
        state_path: String,
        since: Option<u64>,
        relays: Vec<String>,
    },
    WatchGroupId {
        state_path: String,
        group_id: String,
        since: Option<u64>,
        relays: Vec<String>,
    },
    HeartbeatPresence {
        state_path: String,
        relays: Vec<String>,
    },
    ReplyNotificationSnapshot {
        state_path: String,
        group_id: String,
        relays: Vec<String>,
    },
    Leave {
        state_path: String,
        cache_path: String,
        relays: Vec<String>,
    },
    DeleteGroup {
        state_path: String,
        cache_path: String,
        group_id: String,
        relays: Vec<String>,
    },
    DeleteAccount {
        state_path: String,
        cache_path: String,
        delete_group_messages: bool,
        delete_direct_threads: bool,
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

fn state_lock() -> &'static Mutex<()> {
    static STATE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    STATE_LOCK.get_or_init(|| Mutex::new(()))
}

fn invoke(request_json: &str) -> Result<Value, String> {
    let request_value =
        serde_json::from_str::<Value>(request_json).map_err(|error| error.to_string())?;
    let mask_relays = request_value
        .get("mask_relays")
        .and_then(Value::as_array)
        .map(|relays| {
            relays
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let request =
        serde_json::from_value::<Request>(request_value).map_err(|error| error.to_string())?;
    // The watch holds a network request for up to 20 seconds and never writes
    // local state. Everything else is serialized so a refresh cannot save an
    // older state snapshot over a message or profile update in progress.
    let _state_guard = if matches!(
        &request,
        Request::DiscoverRelayMasks { .. }
            | Request::WatchGroup { .. }
            | Request::WatchGroupId { .. }
            | Request::HeartbeatPresence { .. }
            | Request::ReplyNotificationSnapshot { .. }
            | Request::WatchDirect { .. }
            | Request::WatchAccount { .. }
            | Request::FetchAvatar { .. }
            | Request::FetchAttachment { .. }
            | Request::UploadMediaChunk { .. }
            | Request::UploadDirectMediaChunk { .. }
    ) {
        None
    } else {
        Some(
            state_lock()
                .lock()
                .map_err(|_| "local state lock is unavailable".to_owned())?,
        )
    };
    let client = NoiseClient::with_mask_relays(mask_relays).map_err(|error| error.to_string())?;

    match request {
        Request::DiscoverRelayMasks { cache_path, relays } => serde_json::to_value(
            runtime()?
                .block_on(client.discover_relay_masks(cache_path, relays))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
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
            password,
            avatar_data_base64,
            avatar_mime_type,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on(client.initialize(
                    state_path,
                    username,
                    password,
                    avatar_data_base64,
                    avatar_mime_type,
                    relays,
                ))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::SignIn {
            state_path,
            noise_id,
            password,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on(client.sign_in(state_path, &noise_id, password, relays))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::SyncAccount { state_path, relays } => serde_json::to_value(
            runtime()?
                .block_on(client.sync_account(state_path, relays))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::SyncReadState { state_path, relays } => serde_json::to_value(
            runtime()?
                .block_on(client.sync_read_state(state_path, relays))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::WatchAccount {
            state_path,
            since,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on(client.watch_account(state_path, since, relays))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::Logout {
            state_path,
            cache_path,
        } => {
            client
                .logout(state_path, cache_path)
                .map_err(|error| error.to_string())?;
            Ok(Value::Null)
        }
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
            username,
            bio,
            avatar_data_base64,
            avatar_mime_type,
            remove_avatar,
            accepts_direct_messages,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on(client.update_profile(
                    state_path,
                    username,
                    bio,
                    avatar_data_base64,
                    avatar_mime_type,
                    remove_avatar,
                    accepts_direct_messages,
                    relays,
                ))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::UpdateGroupProfile {
            state_path,
            name,
            description,
            rules,
            avatar_data_base64,
            avatar_mime_type,
            remove_avatar,
            background_data_base64,
            background_mime_type,
            remove_background,
            accent_color,
            members_can_send_messages,
            members_can_send_media,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on(client.update_group_profile(
                    state_path,
                    name,
                    description,
                    rules,
                    avatar_data_base64,
                    avatar_mime_type,
                    remove_avatar,
                    background_data_base64,
                    background_mime_type,
                    remove_background,
                    accent_color,
                    members_can_send_messages,
                    members_can_send_media,
                    relays,
                ))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::RotateFrequency {
            state_path,
            revoke_only,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on(client.rotate_frequency(state_path, revoke_only, relays))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::FetchAvatar {
            cache_path,
            image,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on(client.fetch_avatar(cache_path, &image, relays))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::FetchAttachment {
            state_path,
            cache_path,
            scope_id,
            attachment,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on(client.fetch_attachment(
                    state_path,
                    cache_path,
                    scope_id,
                    &attachment,
                    relays,
                ))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::UploadMediaChunk {
            state_path,
            data_base64,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on(client.upload_media_chunk(state_path, data_base64, relays))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::UploadDirectMediaChunk {
            state_path,
            data_base64,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on(client.upload_direct_media_chunk(state_path, data_base64, relays))
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
        Request::SyncGroupEncryption {
            state_path,
            cache_path,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on(client.sync_active_group_encryption(state_path, cache_path, relays))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::SyncGroupActivity {
            state_path,
            group_id,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on(client.sync_group_activity(state_path, &group_id, relays))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::MarkGroupRead {
            state_path,
            group_id,
        } => serde_json::to_value(
            client
                .mark_group_read(state_path, &group_id)
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::Say {
            state_path,
            text,
            attachment,
            reply_to_message_id,
            relays,
        } => {
            let sent = match attachment {
                Some(attachment) => runtime()?
                    .block_on(client.say_with_attachment_reply(
                        state_path,
                        text,
                        attachment,
                        reply_to_message_id,
                        relays,
                    ))
                    .map_err(|error| error.to_string())?,
                None => runtime()?
                    .block_on(client.say_reply(state_path, text, reply_to_message_id, relays))
                    .map_err(|error| error.to_string())?,
            };
            serde_json::to_value(sent).map_err(|error| error.to_string())
        }
        Request::StartDirect {
            state_path,
            public_key,
            username,
            bio,
            avatar,
            accepts_direct_messages,
        } => serde_json::to_value(
            client
                .start_direct(
                    state_path,
                    &public_key,
                    username,
                    bio,
                    avatar,
                    accepts_direct_messages,
                )
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::SelectDirect {
            state_path,
            public_key,
        } => serde_json::to_value(
            client
                .select_direct(state_path, &public_key)
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::SyncDirects {
            state_path,
            cache_path,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on(client.sync_directs(state_path, cache_path, relays))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::DirectInbox {
            state_path,
            cache_path,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on(client.direct_inbox(state_path, cache_path, relays))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::DirectConversation {
            state_path,
            cache_path,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on(client.direct_conversation(state_path, cache_path, relays))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::MarkDirectRead {
            state_path,
            public_key,
        } => serde_json::to_value(
            client
                .mark_direct_read(state_path, &public_key)
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::WatchDirect {
            state_path,
            since,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on(client.watch_direct(state_path, since, relays))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::SayDirect {
            state_path,
            text,
            attachment,
            reply_to_message_id,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on(client.say_direct(
                    state_path,
                    text,
                    attachment,
                    reply_to_message_id,
                    relays,
                ))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::DeleteDirect {
            state_path,
            cache_path,
            public_key,
            for_both,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on(client.delete_direct(
                    state_path,
                    cache_path,
                    &public_key,
                    for_both,
                    relays,
                ))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::SetModerator {
            state_path,
            member_public_key,
            enabled,
            relays,
        } => {
            runtime()?
                .block_on(client.set_moderator(state_path, &member_public_key, enabled, relays))
                .map_err(|error| error.to_string())?;
            Ok(Value::Null)
        }
        Request::DeleteMessage {
            state_path,
            message_event_id,
            relays,
        } => {
            runtime()?
                .block_on(client.delete_message(state_path, &message_event_id, relays))
                .map_err(|error| error.to_string())?;
            Ok(Value::Null)
        }
        Request::SetReaction {
            state_path,
            message_event_id,
            emoji,
            enabled,
            relays,
        } => {
            runtime()?
                .block_on(client.set_reaction(
                    state_path,
                    &message_event_id,
                    &emoji,
                    enabled,
                    relays,
                ))
                .map_err(|error| error.to_string())?;
            Ok(Value::Null)
        }
        Request::ReportMessage {
            state_path,
            message_event_id,
            reason,
            relays,
        } => {
            runtime()?
                .block_on(client.report_message(state_path, &message_event_id, reason, relays))
                .map_err(|error| error.to_string())?;
            Ok(Value::Null)
        }
        Request::ResolveReport {
            state_path,
            report_event_id,
            relays,
        } => {
            runtime()?
                .block_on(client.resolve_report(state_path, &report_event_id, relays))
                .map_err(|error| error.to_string())?;
            Ok(Value::Null)
        }
        Request::BanMember {
            state_path,
            member_public_key,
            delete_messages,
            relays,
        } => {
            runtime()?
                .block_on(client.ban_member(
                    state_path,
                    &member_public_key,
                    delete_messages,
                    relays,
                ))
                .map_err(|error| error.to_string())?;
            Ok(Value::Null)
        }
        Request::UnbanMember {
            state_path,
            member_public_key,
            relays,
        } => {
            runtime()?
                .block_on(client.unban_member(state_path, &member_public_key, relays))
                .map_err(|error| error.to_string())?;
            Ok(Value::Null)
        }
        Request::Conversation { state_path, relays } => serde_json::to_value(
            runtime()?
                .block_on(client.conversation(state_path, relays))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::WatchGroup {
            state_path,
            since,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on(client.watch_group(state_path, since, relays))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::WatchGroupId {
            state_path,
            group_id,
            since,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on(client.watch_group_id(state_path, &group_id, since, relays))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::HeartbeatPresence { state_path, relays } => serde_json::to_value(
            runtime()?
                .block_on(client.heartbeat_presence(state_path, relays))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::ReplyNotificationSnapshot {
            state_path,
            group_id,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on(client.reply_notification_snapshot(state_path, &group_id, relays))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::Leave {
            state_path,
            cache_path,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on(client.leave(state_path, cache_path, relays))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::DeleteGroup {
            state_path,
            cache_path,
            group_id,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on(client.delete_group(state_path, cache_path, &group_id, relays))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::DeleteAccount {
            state_path,
            cache_path,
            delete_group_messages,
            delete_direct_threads,
            relays,
        } => {
            runtime()?
                .block_on(client.delete_account(
                    state_path,
                    cache_path,
                    delete_group_messages,
                    delete_direct_threads,
                    relays,
                ))
                .map_err(|error| error.to_string())?;
            Ok(Value::Null)
        }
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
