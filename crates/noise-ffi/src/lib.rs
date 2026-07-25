use std::{
    ffi::{CStr, CString, c_char},
    future::Future,
    panic::{AssertUnwindSafe, catch_unwind},
    path::Path,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use futures_util::future::{AbortHandle, Abortable};
use noise_client::{MediaAttachment, NoiseClient, ProfileAlbum, ProfileAlbumItem, ProfileImage};
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
        #[serde(default)]
        interruptible: bool,
    },
    SyncReadState {
        state_path: String,
        cache_path: String,
        relays: Vec<String>,
        #[serde(default)]
        interruptible: bool,
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
    FetchProfileAlbum {
        state_path: String,
        public_key: String,
        album: ProfileAlbum,
        relays: Vec<String>,
    },
    UpdateProfileAlbum {
        state_path: String,
        items: Vec<ProfileAlbumItem>,
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
        mobile_background_data_base64: Option<String>,
        #[serde(default)]
        mobile_background_mime_type: Option<String>,
        #[serde(default)]
        remove_mobile_background: bool,
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
    FetchAttachmentRange {
        state_path: String,
        cache_path: String,
        scope_id: Option<String>,
        attachment: MediaAttachment,
        offset: u64,
        byte_length: u64,
        relays: Vec<String>,
    },
    FetchLinkPreview {
        url: String,
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
    UploadProfileMediaChunk {
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
    SyncTopicActivity {
        state_path: String,
        group_id: String,
        topic_id: String,
        relays: Vec<String>,
    },
    LoadOlderGroupHistory {
        state_path: String,
        group_id: String,
        relays: Vec<String>,
    },
    LoadOlderTopicHistory {
        state_path: String,
        group_id: String,
        topic_id: String,
        relays: Vec<String>,
    },
    MarkGroupRead {
        state_path: String,
        group_id: String,
    },
    MarkTopicRead {
        state_path: String,
        group_id: String,
        topic_id: String,
    },
    CreateTopic {
        state_path: String,
        name: String,
        icon: String,
        relays: Vec<String>,
    },
    UpdateTopic {
        state_path: String,
        topic_id: String,
        name: String,
        icon: String,
        locked: bool,
        relays: Vec<String>,
    },
    ArchiveTopic {
        state_path: String,
        topic_id: String,
        relays: Vec<String>,
    },
    ReorderTopics {
        state_path: String,
        topic_ids: Vec<String>,
        relays: Vec<String>,
    },
    Say {
        state_path: String,
        text: String,
        attachment: Option<MediaAttachment>,
        #[serde(default)]
        reply_to_message_id: Option<String>,
        #[serde(default)]
        topic_id: Option<String>,
        relays: Vec<String>,
    },
    StartDirect {
        state_path: String,
        public_key: String,
        username: String,
        bio: String,
        avatar: Option<ProfileImage>,
        album: Option<ProfileAlbum>,
        accepts_direct_messages: bool,
    },
    SetBlock {
        state_path: String,
        cache_path: String,
        public_key: String,
        username: String,
        bio: String,
        avatar: Option<ProfileImage>,
        album: Option<ProfileAlbum>,
        accepts_direct_messages: bool,
        blocked: bool,
        relays: Vec<String>,
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
        #[serde(default)]
        interruptible: bool,
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
    CachedConversation {
        state_path: String,
        group_id: String,
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
        active: Option<bool>,
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
    CancelGroupLoading,
    CancelBackgroundLoading,
    CancelMediaLoading,
}

struct SessionRuntime(&'static Runtime);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LoadingClass {
    Group,
    Background,
    Media,
}

#[derive(Clone, Copy)]
struct LoadingTicket {
    class: LoadingClass,
    generation: u64,
}

impl SessionRuntime {
    fn block_on<F, T>(&self, future: F) -> anyhow::Result<T>
    where
        F: Future<Output = anyhow::Result<T>>,
    {
        self.block_on_registered(future, None)
    }

    fn block_on_loading<F, T>(&self, future: F, ticket: LoadingTicket) -> anyhow::Result<T>
    where
        F: Future<Output = anyhow::Result<T>>,
    {
        self.block_on_registered(future, Some(ticket))
    }

    fn block_on_registered<F, T>(
        &self,
        future: F,
        loading: Option<LoadingTicket>,
    ) -> anyhow::Result<T>
    where
        F: Future<Output = anyhow::Result<T>>,
    {
        if session_ending().load(Ordering::Acquire) {
            anyhow::bail!("session ended")
        }
        let (abort_handle, abort_registration) = AbortHandle::new_pair();
        let operation_id = next_session_operation_id().fetch_add(1, Ordering::Relaxed);
        {
            let mut operations = active_session_operations()
                .lock()
                .map_err(|_| anyhow::anyhow!("session operation lock is unavailable"))?;
            operations.push((
                operation_id,
                abort_handle.clone(),
                loading.map(|ticket| ticket.class),
            ));
        }
        if session_ending().load(Ordering::Acquire)
            || loading.is_some_and(|ticket| {
                loading_generation(ticket.class).load(Ordering::Acquire) != ticket.generation
            })
        {
            abort_handle.abort();
        }
        let result = self.0.block_on(Abortable::new(future, abort_registration));
        if let Ok(mut operations) = active_session_operations().lock() {
            operations.retain(|(candidate, _, _)| *candidate != operation_id);
        }
        result.map_err(|_| {
            if session_ending().load(Ordering::Acquire) {
                anyhow::anyhow!("session ended")
            } else {
                anyhow::anyhow!("loading superseded")
            }
        })?
    }
}

fn runtime() -> Result<SessionRuntime, String> {
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
        .map(SessionRuntime)
}

fn state_lock() -> &'static Mutex<()> {
    static STATE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    STATE_LOCK.get_or_init(|| Mutex::new(()))
}

fn session_ending() -> &'static AtomicBool {
    static SESSION_ENDING: AtomicBool = AtomicBool::new(false);
    &SESSION_ENDING
}

fn next_session_operation_id() -> &'static AtomicU64 {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    &NEXT_ID
}

fn active_session_operations() -> &'static Mutex<Vec<(u64, AbortHandle, Option<LoadingClass>)>> {
    static OPERATIONS: OnceLock<Mutex<Vec<(u64, AbortHandle, Option<LoadingClass>)>>> =
        OnceLock::new();
    OPERATIONS.get_or_init(|| Mutex::new(Vec::new()))
}

fn cancel_session_operations() {
    if let Ok(mut operations) = active_session_operations().lock() {
        for (_, operation, _) in operations.drain(..) {
            operation.abort();
        }
    }
}

fn loading_generation(class: LoadingClass) -> &'static AtomicU64 {
    static GROUP: AtomicU64 = AtomicU64::new(1);
    static BACKGROUND: AtomicU64 = AtomicU64::new(1);
    static MEDIA: AtomicU64 = AtomicU64::new(1);
    match class {
        LoadingClass::Group => &GROUP,
        LoadingClass::Background => &BACKGROUND,
        LoadingClass::Media => &MEDIA,
    }
}

fn cancel_loading_operations(class: LoadingClass) {
    loading_generation(class).fetch_add(1, Ordering::AcqRel);
    if let Ok(operations) = active_session_operations().lock() {
        for (_, operation, candidate) in operations.iter() {
            if *candidate == Some(class) {
                operation.abort();
            }
        }
    }
}

struct SessionEndGuard;

impl Drop for SessionEndGuard {
    fn drop(&mut self) {
        session_ending().store(false, Ordering::Release);
    }
}

fn begin_session_end() -> SessionEndGuard {
    session_ending().store(true, Ordering::Release);
    cancel_session_operations();
    SessionEndGuard
}

fn request_loading_ticket(request: &Request) -> Option<LoadingTicket> {
    let class = match request {
        Request::SyncGroupEncryption { .. }
        | Request::Conversation { .. }
        | Request::LoadOlderGroupHistory { .. }
        | Request::LoadOlderTopicHistory { .. } => LoadingClass::Group,
        Request::SyncAccount {
            interruptible: true,
            ..
        }
        | Request::SyncReadState {
            interruptible: true,
            ..
        }
        | Request::DirectInbox {
            interruptible: true,
            ..
        }
        | Request::SyncGroupActivity { .. }
        | Request::SyncTopicActivity { .. }
        | Request::DirectConversation { .. } => LoadingClass::Background,
        Request::FetchAttachment { .. } | Request::FetchAttachmentRange { .. } => {
            LoadingClass::Media
        }
        _ => return None,
    };
    Some(LoadingTicket {
        class,
        generation: loading_generation(class).load(Ordering::Acquire),
    })
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
    match &request {
        Request::CancelGroupLoading => {
            cancel_loading_operations(LoadingClass::Group);
            return Ok(Value::Null);
        }
        Request::CancelBackgroundLoading => {
            cancel_loading_operations(LoadingClass::Background);
            return Ok(Value::Null);
        }
        Request::CancelMediaLoading => {
            cancel_loading_operations(LoadingClass::Media);
            return Ok(Value::Null);
        }
        _ => {}
    }
    let loading_ticket = request_loading_ticket(&request);
    let is_logout = matches!(&request, Request::Logout { .. });
    let _session_end_guard = is_logout.then(begin_session_end);
    if !is_logout && session_ending().load(Ordering::Acquire) {
        return Err("session ended".to_owned());
    }
    // The watch holds a network request for up to 20 seconds and never writes
    // local state. Everything else is serialized so a refresh cannot save an
    // older state snapshot over a message or profile update in progress.
    let _state_guard = if matches!(
        &request,
        Request::DiscoverRelayMasks { .. }
            | Request::WatchGroup { .. }
            | Request::WatchGroupId { .. }
            | Request::SyncGroupActivity { .. }
            | Request::SyncTopicActivity { .. }
            | Request::CachedConversation { .. }
            | Request::HeartbeatPresence { .. }
            | Request::ReplyNotificationSnapshot { .. }
            | Request::WatchDirect { .. }
            | Request::WatchAccount { .. }
            | Request::FetchAvatar { .. }
            | Request::FetchAttachment { .. }
            | Request::FetchAttachmentRange { .. }
            | Request::FetchLinkPreview { .. }
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
    if !is_logout && session_ending().load(Ordering::Acquire) {
        return Err("session ended".to_owned());
    }
    if loading_ticket.is_some_and(|ticket| {
        loading_generation(ticket.class).load(Ordering::Acquire) != ticket.generation
    }) {
        return Err("loading superseded".to_owned());
    }
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
        Request::SyncAccount {
            state_path,
            relays,
            interruptible,
        } => {
            let runtime = runtime()?;
            let future = client.sync_account(state_path, relays);
            let result = if interruptible {
                runtime.block_on_loading(
                    future,
                    loading_ticket
                        .ok_or_else(|| "background loading ticket is missing".to_owned())?,
                )
            } else {
                runtime.block_on(future)
            }
            .map_err(|error| error.to_string())?;
            serde_json::to_value(result).map_err(|error| error.to_string())
        }
        Request::SyncReadState {
            state_path,
            cache_path,
            relays,
            interruptible,
        } => {
            let runtime = runtime()?;
            let future = client.sync_read_state(state_path, cache_path, relays);
            let result = if interruptible {
                runtime.block_on_loading(
                    future,
                    loading_ticket
                        .ok_or_else(|| "background loading ticket is missing".to_owned())?,
                )
            } else {
                runtime.block_on(future)
            }
            .map_err(|error| error.to_string())?;
            serde_json::to_value(result).map_err(|error| error.to_string())
        }
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
        Request::FetchProfileAlbum {
            state_path,
            public_key,
            album,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on(client.fetch_profile_album(state_path, &public_key, &album, relays))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::UpdateProfileAlbum {
            state_path,
            items,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on(client.update_profile_album(state_path, items, relays))
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
            mobile_background_data_base64,
            mobile_background_mime_type,
            remove_mobile_background,
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
                    mobile_background_data_base64,
                    mobile_background_mime_type,
                    remove_mobile_background,
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
                .block_on_loading(
                    client.fetch_attachment(state_path, cache_path, scope_id, &attachment, relays),
                    loading_ticket.expect("attachment requests have a loading ticket"),
                )
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::FetchAttachmentRange {
            state_path,
            cache_path,
            scope_id,
            attachment,
            offset,
            byte_length,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on_loading(
                    client.fetch_attachment_range(
                        state_path,
                        cache_path,
                        scope_id,
                        &attachment,
                        offset,
                        byte_length,
                        relays,
                    ),
                    loading_ticket.expect("attachment range requests have a loading ticket"),
                )
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::FetchLinkPreview { url, relays } => serde_json::to_value(
            runtime()?
                .block_on(client.fetch_link_preview(url, relays))
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
        Request::UploadProfileMediaChunk {
            state_path,
            data_base64,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on(client.upload_profile_media_chunk(state_path, data_base64, relays))
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
                .block_on_loading(
                    client.sync_active_group_encryption(state_path, cache_path, relays),
                    loading_ticket.ok_or_else(|| "group loading ticket is missing".to_owned())?,
                )
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::SyncGroupActivity {
            state_path,
            group_id,
            relays,
        } => {
            let update = runtime()?
                .block_on_loading(
                    client.fetch_group_activity(&state_path, &group_id, relays),
                    loading_ticket
                        .ok_or_else(|| "background loading ticket is missing".to_owned())?,
                )
                .map_err(|error| error.to_string())?;
            if session_ending().load(Ordering::Acquire) {
                return Err("session ended".to_owned());
            }
            let _state_guard = state_lock()
                .lock()
                .map_err(|_| "local state lock is unavailable".to_owned())?;
            if session_ending().load(Ordering::Acquire) {
                return Err("session ended".to_owned());
            }
            serde_json::to_value(
                client
                    .apply_group_activity(state_path, update)
                    .map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())
        }
        Request::SyncTopicActivity {
            state_path,
            group_id,
            topic_id,
            relays,
        } => {
            let update = runtime()?
                .block_on_loading(
                    client.fetch_topic_activity(&state_path, &group_id, &topic_id, relays),
                    loading_ticket
                        .ok_or_else(|| "background loading ticket is missing".to_owned())?,
                )
                .map_err(|error| error.to_string())?;
            if session_ending().load(Ordering::Acquire) {
                return Err("session ended".to_owned());
            }
            let _state_guard = state_lock()
                .lock()
                .map_err(|_| "local state lock is unavailable".to_owned())?;
            serde_json::to_value(
                client
                    .apply_group_activity(state_path, update)
                    .map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())
        }
        Request::LoadOlderGroupHistory {
            state_path,
            group_id,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on_loading(
                    client.load_older_group_history(state_path, &group_id, relays),
                    loading_ticket.ok_or_else(|| "group loading ticket is missing".to_owned())?,
                )
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::LoadOlderTopicHistory {
            state_path,
            group_id,
            topic_id,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on_loading(
                    client.load_older_topic_history(state_path, &group_id, &topic_id, relays),
                    loading_ticket.ok_or_else(|| "group loading ticket is missing".to_owned())?,
                )
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
        Request::MarkTopicRead {
            state_path,
            group_id,
            topic_id,
        } => serde_json::to_value(
            client
                .mark_topic_read(state_path, &group_id, Some(&topic_id))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::CreateTopic {
            state_path,
            name,
            icon,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on(client.create_topic(state_path, name, icon, relays))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::UpdateTopic {
            state_path,
            topic_id,
            name,
            icon,
            locked,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on(client.update_topic(state_path, &topic_id, name, icon, locked, relays))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::ArchiveTopic {
            state_path,
            topic_id,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on(client.archive_topic(state_path, &topic_id, relays))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::ReorderTopics {
            state_path,
            topic_ids,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on(client.reorder_topics(state_path, topic_ids, relays))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::Say {
            state_path,
            text,
            attachment,
            reply_to_message_id,
            topic_id,
            relays,
        } => {
            let sent = if let Some(topic_id) = topic_id {
                runtime()?
                    .block_on(client.say_topic(
                        state_path,
                        &topic_id,
                        text,
                        attachment,
                        reply_to_message_id,
                        relays,
                    ))
                    .map_err(|error| error.to_string())?
            } else {
                match attachment {
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
                }
            };
            serde_json::to_value(sent).map_err(|error| error.to_string())
        }
        Request::StartDirect {
            state_path,
            public_key,
            username,
            bio,
            avatar,
            album,
            accepts_direct_messages,
        } => serde_json::to_value(
            client
                .start_direct(
                    state_path,
                    &public_key,
                    username,
                    bio,
                    avatar,
                    album,
                    accepts_direct_messages,
                )
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::SetBlock {
            state_path,
            cache_path,
            public_key,
            username,
            bio,
            avatar,
            album,
            accepts_direct_messages,
            blocked,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on(client.set_block(
                    state_path,
                    cache_path,
                    &public_key,
                    username,
                    bio,
                    avatar,
                    album,
                    accepts_direct_messages,
                    blocked,
                    relays,
                ))
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
            interruptible,
        } => {
            let runtime = runtime()?;
            let future = client.direct_inbox(state_path, cache_path, relays);
            let result = if interruptible {
                runtime.block_on_loading(
                    future,
                    loading_ticket
                        .ok_or_else(|| "background loading ticket is missing".to_owned())?,
                )
            } else {
                runtime.block_on(future)
            }
            .map_err(|error| error.to_string())?;
            serde_json::to_value(result).map_err(|error| error.to_string())
        }
        Request::DirectConversation {
            state_path,
            cache_path,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on_loading(
                    client.direct_conversation(state_path, cache_path, relays),
                    loading_ticket
                        .ok_or_else(|| "background loading ticket is missing".to_owned())?,
                )
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
                .block_on_loading(
                    client.conversation(state_path, relays),
                    loading_ticket.ok_or_else(|| "group loading ticket is missing".to_owned())?,
                )
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::CachedConversation {
            state_path,
            group_id,
        } => serde_json::to_value(
            client
                .cached_conversation(state_path, &group_id)
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
        Request::HeartbeatPresence {
            state_path,
            active,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on(client.heartbeat_presence(state_path, active.unwrap_or(true), relays))
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
        Request::CancelGroupLoading
        | Request::CancelBackgroundLoading
        | Request::CancelMediaLoading => {
            unreachable!("loading cancellation is handled before dispatch")
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
    .unwrap_or_else(|_| json!({ "ok": false, "error": "noise core panicked" }).to_string());

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
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};

    use super::{
        LoadingClass, LoadingTicket, active_session_operations, begin_session_end,
        cancel_loading_operations, loading_generation, response_json, runtime, session_ending,
    };

    fn session_test_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn invalid_requests_are_structured_errors() {
        let response: serde_json::Value =
            serde_json::from_str(&response_json("{}")).expect("response is JSON");
        assert_eq!(response["ok"], false);
    }

    #[test]
    fn ending_a_session_cancels_active_async_work() {
        let _test_guard = session_test_lock().lock().expect("test lock");
        session_ending().store(false, std::sync::atomic::Ordering::Release);
        let worker = std::thread::spawn(|| {
            runtime().expect("runtime").block_on(async {
                tokio::time::sleep(Duration::from_secs(30)).await;
                Ok::<(), anyhow::Error>(())
            })
        });
        let wait_started = Instant::now();
        while active_session_operations()
            .lock()
            .expect("operation lock")
            .is_empty()
        {
            assert!(
                wait_started.elapsed() < Duration::from_secs(2),
                "async work did not register"
            );
            std::thread::yield_now();
        }

        let cancel_started = Instant::now();
        let ending = begin_session_end();
        let result = worker.join().expect("worker did not panic");
        assert!(result.is_err());
        assert!(cancel_started.elapsed() < Duration::from_secs(1));
        drop(ending);
    }

    #[test]
    fn superseding_group_loading_aborts_the_registered_operation() {
        let _test_guard = session_test_lock().lock().expect("test lock");
        let ticket = LoadingTicket {
            class: LoadingClass::Group,
            generation: loading_generation(LoadingClass::Group)
                .load(std::sync::atomic::Ordering::Acquire),
        };
        let worker = std::thread::spawn(move || {
            runtime().expect("runtime").block_on_loading(
                async {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    Ok::<(), anyhow::Error>(())
                },
                ticket,
            )
        });
        let wait_started = Instant::now();
        while !active_session_operations()
            .lock()
            .expect("operation lock")
            .iter()
            .any(|(_, _, class)| *class == Some(LoadingClass::Group))
        {
            assert!(
                wait_started.elapsed() < Duration::from_secs(2),
                "group loading did not register"
            );
            std::thread::yield_now();
        }

        let cancel_started = Instant::now();
        cancel_loading_operations(LoadingClass::Group);
        let result = worker.join().expect("worker did not panic");
        assert_eq!(
            result
                .expect_err("group loading was not aborted")
                .to_string(),
            "loading superseded"
        );
        assert!(cancel_started.elapsed() < Duration::from_secs(1));
    }
}
