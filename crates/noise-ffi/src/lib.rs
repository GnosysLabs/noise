use std::{
    collections::HashMap,
    ffi::{CStr, CString, c_char, c_void},
    future::Future,
    panic::{AssertUnwindSafe, catch_unwind},
    path::Path,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use futures_util::{
    FutureExt,
    future::{AbortHandle, Abortable},
};
use noise_client::{
    DirectMessagePolicy, ForwardedFrom, GroupContentRating, MediaAttachment, ModeratorPermissions,
    NoiseClient, ProfileAlbum, ProfileAlbumItem, ProfileImage, SafetyReportCategoryV1,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::runtime::Runtime;

fn default_search_limit() -> usize {
    60
}

fn default_klipy_limit() -> usize {
    24
}

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
    SyncSafetyDirectives {
        state_path: String,
        cache_path: String,
        safety_url: String,
        signing_public_key_base64: Option<String>,
    },
    PrepareLocalSearch {
        state_path: String,
    },
    SearchLocal {
        state_path: String,
        query: String,
        #[serde(default = "default_search_limit")]
        limit: usize,
    },
    PublishContactSignal {
        state_path: String,
        relays: Vec<String>,
    },
    ResolveContactSignal {
        signature: String,
        relays: Vec<String>,
    },
    Initialize {
        state_path: String,
        username: String,
        password: String,
        birth_date: String,
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
    #[serde(alias = "set_adult_content_enabled")]
    SetExplicitContentEnabled {
        state_path: String,
        enabled: bool,
        relays: Vec<String>,
    },
    RegisterDevice {
        state_path: String,
        name: String,
        platform: String,
        relays: Vec<String>,
    },
    RevokeDevice {
        state_path: String,
        device_id: String,
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
    RefreshAccountState {
        state_path: String,
        cache_path: String,
        relays: Vec<String>,
        #[serde(default)]
        interruptible: bool,
    },
    PublishReadState {
        state_path: String,
        relays: Vec<String>,
    },
    WatchAccount {
        state_path: String,
        since: Option<u64>,
        relays: Vec<String>,
    },
    WatchReadState {
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
        #[serde(default)]
        direct_message_policy: Option<DirectMessagePolicy>,
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
        #[serde(default)]
        content_rating: Option<GroupContentRating>,
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
    FetchKlipyMedia {
        kind: String,
        query: Option<String>,
        #[serde(default = "default_klipy_limit")]
        limit: usize,
        relays: Vec<String>,
    },
    UploadMediaChunk {
        state_path: String,
        data_base64: String,
        relays: Vec<String>,
    },
    UploadMediaChunkToGroup {
        state_path: String,
        group_id: String,
        data_base64: String,
        relays: Vec<String>,
    },
    UploadDirectMediaChunk {
        state_path: String,
        data_base64: String,
        relays: Vec<String>,
    },
    UploadDirectMediaChunkTo {
        state_path: String,
        public_key: String,
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
        #[serde(default)]
        content_rating: GroupContentRating,
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
        /// Absent for the open group. Present for a background pass that only
        /// admits the members another group is waiting on.
        #[serde(default)]
        group_id: Option<String>,
        relays: Vec<String>,
    },
    GroupHasPendingAdmissions {
        state_path: String,
        group_id: String,
        relays: Vec<String>,
    },
    SyncGroupActivity {
        state_path: String,
        group_id: String,
        #[serde(default)]
        mark_read: bool,
        #[serde(default)]
        read_topic_id: Option<String>,
        relays: Vec<String>,
    },
    SyncTopicActivity {
        state_path: String,
        group_id: String,
        topic_id: String,
        #[serde(default)]
        mark_read: bool,
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
    LoadOlderDirectHistory {
        state_path: String,
        cache_path: String,
        relays: Vec<String>,
    },
    MarkGroupRead {
        state_path: String,
        group_id: String,
    },
    MarkEntireGroupRead {
        state_path: String,
        group_id: String,
    },
    MarkTopicRead {
        state_path: String,
        group_id: String,
        topic_id: String,
    },
    ReorderGroups {
        state_path: String,
        group_ids: Vec<String>,
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
    SayToGroup {
        state_path: String,
        group_id: String,
        #[serde(default)]
        topic_id: Option<String>,
        text: String,
        attachment: Option<MediaAttachment>,
        #[serde(default)]
        forwarded_from: Option<ForwardedFrom>,
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
        #[serde(default)]
        direct_message_policy: Option<DirectMessagePolicy>,
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
    CachedDirectInbox {
        state_path: String,
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
        relays: Vec<String>,
    },
    SetDirectDisappearingAfterRead {
        state_path: String,
        public_key: String,
        seconds: Option<u32>,
        relays: Vec<String>,
    },
    WatchDirect {
        state_path: String,
        since: Option<u64>,
        relays: Vec<String>,
    },
    RegisterPushSubscription {
        state_path: String,
        installation_id: String,
        device_token: String,
        environment: String,
        topic: String,
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
    SayDirectTo {
        state_path: String,
        public_key: String,
        text: String,
        attachment: Option<MediaAttachment>,
        #[serde(default)]
        forwarded_from: Option<ForwardedFrom>,
        relays: Vec<String>,
    },
    DeleteDirect {
        state_path: String,
        cache_path: String,
        public_key: String,
        for_both: bool,
        relays: Vec<String>,
    },
    ClearDirect {
        state_path: String,
        cache_path: String,
        public_key: String,
        relays: Vec<String>,
    },
    DeleteDirectMessage {
        state_path: String,
        cache_path: String,
        public_key: String,
        message_id: String,
        relays: Vec<String>,
    },
    SetModerator {
        state_path: String,
        member_public_key: String,
        enabled: bool,
        relays: Vec<String>,
    },
    SetModeratorPermissions {
        state_path: String,
        member_public_key: String,
        permissions: ModeratorPermissions,
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
    ReportMessageToNoise {
        state_path: String,
        message_event_id: String,
        category: SafetyReportCategoryV1,
        details: Option<String>,
        safety_url: String,
        recipient_public_key_base64: Option<String>,
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
    CancelDirectLoading,
    CancelBackgroundLoading,
    CancelMediaLoading,
}

struct SessionRuntime(&'static Runtime);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LoadingClass {
    Group,
    Direct,
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

/// Read-state publication no longer holds the local write lock, so this keeps
/// two publishes from racing for the same vault revision and burning their
/// retry budget against each other. It blocks nothing else.
fn read_state_publish_lock() -> &'static Mutex<()> {
    static PUBLISH_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    PUBLISH_LOCK.get_or_init(|| Mutex::new(()))
}

fn search_lock() -> &'static Mutex<()> {
    static SEARCH_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    SEARCH_LOCK.get_or_init(|| Mutex::new(()))
}

fn search_generation() -> &'static AtomicU64 {
    static SEARCH_GENERATION: AtomicU64 = AtomicU64::new(0);
    &SEARCH_GENERATION
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

fn next_async_watch_id() -> &'static AtomicU64 {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    &NEXT_ID
}

fn active_async_watches() -> &'static Mutex<HashMap<u64, AbortHandle>> {
    static WATCHES: OnceLock<Mutex<HashMap<u64, AbortHandle>>> = OnceLock::new();
    WATCHES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cancel_async_watch(operation_id: u64) {
    if let Ok(mut watches) = active_async_watches().lock()
        && let Some(operation) = watches.remove(&operation_id)
    {
        operation.abort();
    }
}

fn cancel_async_watches() {
    if let Ok(mut watches) = active_async_watches().lock() {
        for (_, operation) in watches.drain() {
            operation.abort();
        }
    }
}

fn cancel_session_operations() {
    if let Ok(mut operations) = active_session_operations().lock() {
        for (_, operation, _) in operations.drain(..) {
            operation.abort();
        }
    }
    cancel_async_watches();
}

fn loading_generation(class: LoadingClass) -> &'static AtomicU64 {
    static GROUP: AtomicU64 = AtomicU64::new(1);
    static DIRECT: AtomicU64 = AtomicU64::new(1);
    static BACKGROUND: AtomicU64 = AtomicU64::new(1);
    static MEDIA: AtomicU64 = AtomicU64::new(1);
    match class {
        LoadingClass::Group => &GROUP,
        LoadingClass::Direct => &DIRECT,
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
        Request::SyncGroupEncryption { group_id: None, .. }
        | Request::Conversation { .. }
        | Request::LoadOlderGroupHistory { .. }
        | Request::LoadOlderTopicHistory { .. } => LoadingClass::Group,
        Request::SyncGroupEncryption {
            group_id: Some(_), ..
        }
        | Request::GroupHasPendingAdmissions { .. } => LoadingClass::Background,
        Request::SyncAccount {
            interruptible: true,
            ..
        }
        | Request::SyncReadState {
            interruptible: true,
            ..
        }
        | Request::RefreshAccountState {
            interruptible: true,
            ..
        }
        | Request::DirectInbox {
            interruptible: true,
            ..
        }
        | Request::SyncGroupActivity { .. }
        | Request::SyncTopicActivity { .. } => LoadingClass::Background,
        Request::DirectConversation { .. } | Request::LoadOlderDirectHistory { .. } => {
            LoadingClass::Direct
        }
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
    let central_url = request_value
        .get("central_url")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let state_path = request_value
        .get("state_path")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let has_network = request_value.get("relays").is_some();
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
        Request::CancelDirectLoading => {
            cancel_loading_operations(LoadingClass::Direct);
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
    let requested_search_generation = matches!(&request, Request::SearchLocal { .. }).then(|| {
        search_generation()
            .fetch_add(1, Ordering::AcqRel)
            .saturating_add(1)
    });
    let preparation_search_generation = matches!(&request, Request::PrepareLocalSearch { .. })
        .then(|| search_generation().load(Ordering::Acquire));
    let _search_guard = if matches!(
        &request,
        Request::PrepareLocalSearch { .. } | Request::SearchLocal { .. }
    ) {
        Some(
            search_lock()
                .lock()
                .map_err(|_| "local search lock is unavailable".to_owned())?,
        )
    } else {
        None
    };
    if requested_search_generation
        .is_some_and(|generation| search_generation().load(Ordering::Acquire) != generation)
    {
        return Err("search superseded".to_owned());
    }
    // The watch holds a network request for up to 20 seconds and never writes
    // local state. Everything else is serialized so a refresh cannot save an
    // older state snapshot over a message or profile update in progress.
    let _state_guard = if matches!(
        &request,
        Request::DiscoverRelayMasks { .. }
            | Request::PrepareLocalSearch { .. }
            | Request::SearchLocal { .. }
            | Request::ResolveContactSignal { .. }
            | Request::WatchGroup { .. }
            | Request::WatchGroupId { .. }
            | Request::SyncAccount { .. }
            | Request::SyncReadState { .. }
            | Request::RefreshAccountState { .. }
            | Request::SyncGroupActivity { .. }
            | Request::SyncTopicActivity { .. }
            | Request::PublishReadState { .. }
            | Request::GroupHasPendingAdmissions { .. }
            | Request::CachedConversation { .. }
            | Request::CachedDirectInbox { .. }
            | Request::HeartbeatPresence { .. }
            | Request::ReplyNotificationSnapshot { .. }
            | Request::WatchDirect { .. }
            | Request::WatchAccount { .. }
            | Request::WatchReadState { .. }
            | Request::FetchAvatar { .. }
            | Request::FetchAttachment { .. }
            | Request::FetchAttachmentRange { .. }
            | Request::FetchLinkPreview { .. }
            | Request::FetchKlipyMedia { .. }
            | Request::FetchProfileAlbum { .. }
            | Request::UploadMediaChunk { .. }
            | Request::UploadMediaChunkToGroup { .. }
            | Request::UploadDirectMediaChunk { .. }
            | Request::UploadDirectMediaChunkTo { .. }
            | Request::UploadProfileMediaChunk { .. }
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
    let client = NoiseClient::with_central_url(mask_relays, central_url)
        .map_err(|error| error.to_string())?;
    if has_network
        && let Some(state_path) = state_path.as_deref()
        && Path::new(state_path).exists()
    {
        runtime()?
            .block_on(client.bind_central_state(state_path))
            .map_err(|error| error.to_string())?;
    }

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
        Request::SyncSafetyDirectives {
            state_path,
            cache_path,
            safety_url,
            signing_public_key_base64,
        } => serde_json::to_value(
            runtime()?
                .block_on(client.sync_safety_directives(
                    state_path,
                    cache_path,
                    &safety_url,
                    signing_public_key_base64,
                ))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::PrepareLocalSearch { state_path } => {
            let result = client.prepare_local_search_until(state_path, || {
                preparation_search_generation.is_some_and(|generation| {
                    search_generation().load(Ordering::Acquire) != generation
                })
            });
            result.map_err(|error| error.to_string())?;
            Ok(Value::Null)
        }
        Request::SearchLocal {
            state_path,
            query,
            limit,
        } => {
            let result = client.search_local_until(state_path, &query, limit, || {
                requested_search_generation.is_some_and(|generation| {
                    search_generation().load(Ordering::Acquire) != generation
                })
            });
            serde_json::to_value(result.map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())
        }
        Request::PublishContactSignal { state_path, relays } => serde_json::to_value(
            runtime()?
                .block_on(client.publish_contact_signal(state_path, relays))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::ResolveContactSignal { signature, relays } => serde_json::to_value(
            runtime()?
                .block_on(client.resolve_contact_signal(&signature, relays))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::Initialize {
            state_path,
            username,
            password,
            birth_date,
            avatar_data_base64,
            avatar_mime_type,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on(client.initialize(
                    state_path,
                    username,
                    password,
                    &birth_date,
                    avatar_data_base64,
                    avatar_mime_type,
                    relays,
                ))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::SetExplicitContentEnabled {
            state_path,
            enabled,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on(client.set_explicit_content_enabled(state_path, enabled, relays))
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
        Request::RegisterDevice {
            state_path,
            name,
            platform,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on(client.register_device(state_path, &name, &platform, relays))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::RevokeDevice {
            state_path,
            device_id,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on(client.revoke_device(state_path, &device_id, relays))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::SyncAccount {
            state_path,
            relays,
            interruptible,
        } => {
            let runtime = runtime()?;
            let future = client.prepare_account_sync(&state_path, relays);
            let update = if interruptible {
                runtime.block_on_loading(
                    future,
                    loading_ticket
                        .ok_or_else(|| "background loading ticket is missing".to_owned())?,
                )
            } else {
                runtime.block_on(future)
            }
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
            let result = if let Some(update) = update {
                client.apply_account_state_update(&state_path, update)
            } else {
                client.local_summary(&state_path)
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
            let future = client.prepare_read_state_sync(&state_path, relays);
            let update = if interruptible {
                runtime.block_on_loading(
                    future,
                    loading_ticket
                        .ok_or_else(|| "background loading ticket is missing".to_owned())?,
                )
            } else {
                runtime.block_on(future)
            }
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
            let result = if let Some(update) = update {
                client.apply_read_state_update(&state_path, &cache_path, update)
            } else {
                client.local_summary(&state_path)
            }
            .map_err(|error| error.to_string())?;
            serde_json::to_value(result).map_err(|error| error.to_string())
        }
        Request::PublishReadState { state_path, relays } => {
            // Sealing and verifying the read-state vault is several relay round
            // trips. Holding the local write lock across them is what used to
            // put the next click, send, or reaction behind a whole publish.
            let _publish_guard = read_state_publish_lock()
                .lock()
                .map_err(|_| "read state publication is unavailable".to_owned())?;
            let update = runtime()?
                .block_on(client.prepare_read_state_publish(&state_path, relays))
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
                match update {
                    Some(update) => client.apply_read_state_publication(state_path, update),
                    None => client.local_summary(state_path),
                }
                .map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())
        }
        Request::RefreshAccountState {
            state_path,
            cache_path,
            relays,
            interruptible,
        } => {
            let runtime = runtime()?;
            let future = client.prepare_account_state_refresh(&state_path, relays);
            let update = if interruptible {
                runtime.block_on_loading(
                    future,
                    loading_ticket
                        .ok_or_else(|| "background loading ticket is missing".to_owned())?,
                )
            } else {
                runtime.block_on(future)
            }
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
            let result = if let Some(update) = update {
                client.apply_read_state_update(&state_path, &cache_path, update)
            } else {
                client.local_summary(&state_path)
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
        Request::WatchReadState {
            state_path,
            since,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on(client.watch_read_state(state_path, since, relays))
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
            direct_message_policy,
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
                    direct_message_policy.unwrap_or(if accepts_direct_messages {
                        DirectMessagePolicy::Everyone
                    } else {
                        DirectMessagePolicy::Nobody
                    }),
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
            content_rating,
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
                    content_rating,
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
        Request::FetchKlipyMedia {
            kind,
            query,
            limit,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on(client.fetch_klipy_media(kind, query, limit, relays))
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
        Request::UploadMediaChunkToGroup {
            state_path,
            group_id,
            data_base64,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on(client.upload_media_chunk_to_group(
                    state_path,
                    &group_id,
                    data_base64,
                    relays,
                ))
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
        Request::UploadDirectMediaChunkTo {
            state_path,
            public_key,
            data_base64,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on(client.upload_direct_media_chunk_to(
                    state_path,
                    &public_key,
                    data_base64,
                    relays,
                ))
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
            content_rating,
            avatar_data_base64,
            avatar_mime_type,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on(client.make(
                    state_path,
                    name,
                    content_rating,
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
        Request::GroupHasPendingAdmissions {
            state_path,
            group_id,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on_loading(
                    client.group_has_pending_admissions(state_path, &group_id, relays),
                    loading_ticket
                        .ok_or_else(|| "background loading ticket is missing".to_owned())?,
                )
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::SyncGroupEncryption {
            state_path,
            cache_path,
            group_id,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on_loading(
                    client.sync_group_encryption(
                        state_path,
                        cache_path,
                        group_id.as_deref(),
                        relays,
                    ),
                    loading_ticket.ok_or_else(|| "group loading ticket is missing".to_owned())?,
                )
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::SyncGroupActivity {
            state_path,
            group_id,
            mark_read,
            read_topic_id,
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
                if mark_read {
                    client.apply_group_activity_and_mark_read(
                        state_path,
                        update,
                        read_topic_id.as_deref(),
                    )
                } else {
                    client.apply_group_activity(state_path, update)
                }
                .map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())
        }
        Request::SyncTopicActivity {
            state_path,
            group_id,
            topic_id,
            mark_read,
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
                if mark_read {
                    client.apply_group_activity_and_mark_read(state_path, update, Some(&topic_id))
                } else {
                    client.apply_group_activity(state_path, update)
                }
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
        Request::LoadOlderDirectHistory {
            state_path,
            cache_path,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on_loading(
                    client.load_older_direct_history(state_path, cache_path, relays),
                    loading_ticket.ok_or_else(|| "direct loading ticket is missing".to_owned())?,
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
        Request::MarkEntireGroupRead {
            state_path,
            group_id,
        } => serde_json::to_value(
            client
                .mark_entire_group_read(state_path, &group_id)
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
        Request::ReorderGroups {
            state_path,
            group_ids,
        } => serde_json::to_value(
            client
                .reorder_groups(state_path, group_ids)
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
        Request::SayToGroup {
            state_path,
            group_id,
            topic_id,
            text,
            attachment,
            forwarded_from,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on(client.say_to_group(
                    state_path,
                    &group_id,
                    topic_id.as_deref(),
                    text,
                    attachment,
                    forwarded_from,
                    relays,
                ))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::StartDirect {
            state_path,
            public_key,
            username,
            bio,
            avatar,
            album,
            accepts_direct_messages,
            direct_message_policy,
        } => serde_json::to_value(
            client
                .start_direct(
                    state_path,
                    &public_key,
                    username,
                    bio,
                    avatar,
                    album,
                    direct_message_policy.unwrap_or(if accepts_direct_messages {
                        DirectMessagePolicy::Everyone
                    } else {
                        DirectMessagePolicy::Nobody
                    }),
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
        Request::CachedDirectInbox { state_path } => serde_json::to_value(
            client
                .cached_direct_inbox(state_path)
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
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on(client.mark_direct_read(state_path, &public_key, relays))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::SetDirectDisappearingAfterRead {
            state_path,
            public_key,
            seconds,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on(client.set_direct_disappearing_after_read(
                    state_path,
                    &public_key,
                    seconds,
                    relays,
                ))
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
        Request::RegisterPushSubscription {
            state_path,
            installation_id,
            device_token,
            environment,
            topic,
            relays,
        } => {
            runtime()?
                .block_on(client.register_push_subscription(
                    state_path,
                    installation_id,
                    device_token,
                    environment,
                    topic,
                    relays,
                ))
                .map_err(|error| error.to_string())?;
            Ok(Value::Bool(true))
        }
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
        Request::SayDirectTo {
            state_path,
            public_key,
            text,
            attachment,
            forwarded_from,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on(client.say_direct_to(
                    state_path,
                    &public_key,
                    text,
                    attachment,
                    None,
                    forwarded_from,
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
        Request::ClearDirect {
            state_path,
            cache_path,
            public_key,
            relays,
        } => serde_json::to_value(
            runtime()?
                .block_on(client.clear_direct(state_path, cache_path, &public_key, relays))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::DeleteDirectMessage {
            state_path,
            cache_path,
            public_key,
            message_id,
            relays,
        } => {
            runtime()?
                .block_on(client.delete_direct_message(
                    state_path,
                    cache_path,
                    &public_key,
                    &message_id,
                    relays,
                ))
                .map_err(|error| error.to_string())?;
            Ok(Value::Null)
        }
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
        Request::SetModeratorPermissions {
            state_path,
            member_public_key,
            permissions,
            relays,
        } => {
            runtime()?
                .block_on(client.set_moderator_permissions(
                    state_path,
                    &member_public_key,
                    permissions,
                    relays,
                ))
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
        Request::ReportMessageToNoise {
            state_path,
            message_event_id,
            category,
            details,
            safety_url,
            recipient_public_key_base64,
        } => serde_json::to_value(
            runtime()?
                .block_on(client.report_message_to_noise(
                    state_path,
                    &message_event_id,
                    category,
                    details,
                    &safety_url,
                    recipient_public_key_base64,
                ))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
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
        | Request::CancelDirectLoading
        | Request::CancelBackgroundLoading
        | Request::CancelMediaLoading => {
            unreachable!("loading cancellation is handled before dispatch")
        }
    }
}

async fn invoke_async_watch(request_json: String) -> Result<Value, String> {
    let request_value =
        serde_json::from_str::<Value>(&request_json).map_err(|error| error.to_string())?;
    let central_url = request_value
        .get("central_url")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let state_path = request_value
        .get("state_path")
        .and_then(Value::as_str)
        .map(str::to_owned);
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

    if session_ending().load(Ordering::Acquire) {
        return Err("session ended".to_owned());
    }

    let client = NoiseClient::with_central_url(mask_relays, central_url)
        .map_err(|error| error.to_string())?;
    if let Some(state_path) = state_path.as_deref()
        && Path::new(state_path).exists()
    {
        client
            .bind_central_state(state_path)
            .await
            .map_err(|error| error.to_string())?;
    }
    match request {
        Request::WatchAccount {
            state_path,
            since,
            relays,
        } => serde_json::to_value(
            client
                .watch_account(state_path, since, relays)
                .await
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::WatchReadState {
            state_path,
            since,
            relays,
        } => serde_json::to_value(
            client
                .watch_read_state(state_path, since, relays)
                .await
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::WatchDirect {
            state_path,
            since,
            relays,
        } => serde_json::to_value(
            client
                .watch_direct(state_path, since, relays)
                .await
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::WatchGroup {
            state_path,
            since,
            relays,
        } => serde_json::to_value(
            client
                .watch_group(state_path, since, relays)
                .await
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        Request::WatchGroupId {
            state_path,
            group_id,
            since,
            relays,
        } => serde_json::to_value(
            client
                .watch_group_id(state_path, &group_id, since, relays)
                .await
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string()),
        _ => Err("request is not an asynchronous watch".to_owned()),
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

type NoiseAsyncCallback = unsafe extern "C" fn(u64, *mut c_char, *mut c_void);

fn async_response_json(result: Result<Value, String>) -> String {
    match result {
        Ok(data) => json!({ "ok": true, "data": data }).to_string(),
        Err(error) => json!({ "ok": false, "error": error }).to_string(),
    }
}

fn deliver_async_response(
    operation_id: u64,
    context: usize,
    callback: NoiseAsyncCallback,
    response: String,
) {
    let response = owned_c_string(response);
    // SAFETY: The caller promises that the callback and context remain valid
    // until exactly one completion. This function is the sole completion path
    // for every accepted asynchronous request.
    unsafe {
        callback(operation_id, response, context as *mut c_void);
    }
}

/// Start a long-polling watch on the shared async Rust runtime.
///
/// The callback is invoked exactly once with a string that belongs to the
/// caller and must be released with `noise_free_string`. Cancelling an
/// operation causes its callback to receive a cancellation error promptly.
///
/// # Safety
///
/// `request_json` must point to a valid, NUL-terminated string for this call.
/// `callback` and `context` must remain valid until the callback is invoked.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn noise_invoke_watch_async(
    request_json: *const c_char,
    context: *mut c_void,
    callback: NoiseAsyncCallback,
) -> u64 {
    let operation_id = next_async_watch_id().fetch_add(1, Ordering::Relaxed);
    let context = context as usize;

    let request = if request_json.is_null() {
        Err("request was null".to_owned())
    } else {
        catch_unwind(AssertUnwindSafe(|| {
            // SAFETY: The pointer contract is documented on this export and
            // the bytes are copied before the function returns.
            let request = unsafe { CStr::from_ptr(request_json) };
            request
                .to_str()
                .map(str::to_owned)
                .map_err(|_| "request was not UTF-8".to_owned())
        }))
        .unwrap_or_else(|_| Err("noise core panicked".to_owned()))
    };

    let request = match request {
        Ok(request) => request,
        Err(error) => {
            deliver_async_response(
                operation_id,
                context,
                callback,
                async_response_json(Err(error)),
            );
            return operation_id;
        }
    };

    let runtime = match runtime() {
        Ok(runtime) => runtime,
        Err(error) => {
            deliver_async_response(
                operation_id,
                context,
                callback,
                async_response_json(Err(error)),
            );
            return operation_id;
        }
    };

    let (abort_handle, abort_registration) = AbortHandle::new_pair();
    match active_async_watches().lock() {
        Ok(mut watches) => {
            watches.insert(operation_id, abort_handle);
        }
        Err(_) => {
            deliver_async_response(
                operation_id,
                context,
                callback,
                async_response_json(Err("async watch lock is unavailable".to_owned())),
            );
            return operation_id;
        }
    }

    runtime.0.spawn(async move {
        let future = AssertUnwindSafe(invoke_async_watch(request)).catch_unwind();
        let result = match Abortable::new(future, abort_registration).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err("noise core panicked".to_owned()),
            Err(_) => Err("request canceled".to_owned()),
        };
        if let Ok(mut watches) = active_async_watches().lock() {
            watches.remove(&operation_id);
        }
        deliver_async_response(operation_id, context, callback, async_response_json(result));
    });

    operation_id
}

/// Cancel a watch started by `noise_invoke_watch_async`.
#[unsafe(no_mangle)]
pub extern "C" fn noise_cancel_watch(operation_id: u64) {
    cancel_async_watch(operation_id);
}

/// Invoke one noise client operation.
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
