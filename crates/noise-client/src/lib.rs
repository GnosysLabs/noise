#[cfg(target_arch = "wasm32")]
use std::cell::RefCell;
#[cfg(any(target_arch = "wasm32", test))]
use std::rc::Rc;
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};
#[cfg(not(target_arch = "wasm32"))]
use std::{
    sync::{
        Arc, Condvar, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[cfg(target_arch = "wasm32")]
thread_local! {
    static WEB_STATE: RefCell<HashMap<String, Vec<u8>>> = RefCell::new(HashMap::new());
}

#[cfg(target_arch = "wasm32")]
pub fn import_web_state(path: &str, bytes: Vec<u8>) -> anyhow::Result<()> {
    let _: ClientState =
        serde_json::from_slice(&bytes).context("encrypted browser state is invalid")?;
    WEB_STATE.with(|states| states.borrow_mut().insert(path.to_owned(), bytes));
    Ok(())
}

#[cfg(target_arch = "wasm32")]
pub fn clear_web_state(path: &str) {
    WEB_STATE.with(|states| {
        states.borrow_mut().remove(path);
    });
}

#[cfg(target_arch = "wasm32")]
#[must_use]
pub fn export_web_state(path: &str) -> Option<Vec<u8>> {
    WEB_STATE.with(|states| states.borrow().get(path).cloned())
}

use anyhow::{Context, bail};
use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, STANDARD_NO_PAD},
};
use futures_util::{FutureExt, StreamExt, stream::FuturesUnordered};
use noise_core::{
    AcceptedMessage, AcceptedTopic, AccountCredentials, AccountVault, DirectPushRequest,
    DirectPushTrigger, EncryptedBlob, GroupDeletion, GroupEventPayload, GroupMembership,
    GroupPresence, GroupProfile, GroupState, HistoryKeyLink, Identity, InviteRecord,
    InviteRotation, MlsAccountState, MlsControlLog, MlsEpochRecord, MlsGroupGenesis,
    MlsJoinRequest, MlsRemovalReason, MlsRemovalRequest, Profile, PushSubscriptionRegistration,
    SignedEvent, StorageManifest, derive_account_credentials, direct_mailbox_id, direct_message_id,
    display_frequency, display_noise_id, encode_blob_for_storage, frequency_locator,
    generate_frequency, generate_noise_id, media_preview_is_valid, normalize_frequency,
    profile_media_scope_id, reconstruct_blob_from_storage_payloads, valid_reaction_emoji,
};
pub use noise_core::{
    DirectMessagePolicy, ForwardedFrom, GroupContentRating, MediaAttachment, MediaChunk,
    ProfileAlbum, ProfileAlbumItem, ProfileImage,
};
pub use noise_transport::LinkPreview;
use noise_transport::{
    GATEWAY_HEADER, LinkPreviewRequest, OHTTP_RELAY_PATH, OHTTP_REQUEST_MEDIA_TYPE,
    OHTTP_RESPONSE_MEDIA_TYPE, PlainResponse, RELAY_CAPABILITIES_PATH, RelayCapabilities,
    RelayDescriptor, decode_response, encode_request,
};
use ohttp::ClientRequest;
use serde::{Deserialize, Serialize};
use time::{Date, Month, OffsetDateTime};

#[cfg(not(target_arch = "wasm32"))]
mod relay_pool;

const GROUP_PRESENCE_TTL_MILLIS: u64 = 50_000;
const IDLE_GROUP_PRESENCE_TTL_MILLIS: u64 = 10_000;
const ONLINE_PRESENCE_REMAINING_MILLIS: u64 = 15_000;
const RECENT_GROUP_PRESENCE_MILLIS: u64 = 5 * 60_000;
const EVENT_REPLICA_SETTLE_MILLIS: u64 = 500;
const RELAY_REQUEST_TIMEOUT_SECS: u64 = 30;
#[cfg(not(target_arch = "wasm32"))]
const MEDIA_CHUNK_DOWNLOAD_CONCURRENCY: usize = 8;
#[cfg(target_arch = "wasm32")]
const MEDIA_CHUNK_DOWNLOAD_CONCURRENCY: usize = 4;
const MAX_MEDIA_RANGE_BYTES: u64 = 4 * 1024 * 1024;
#[cfg(any(target_arch = "wasm32", test))]
const WEB_MEDIA_CHUNK_CACHE_BYTES: usize = 96 * 1024 * 1024;
const INITIAL_GROUP_MESSAGE_WINDOW: usize = 30;
const GROUP_EVENT_PAGE_SIZE: usize = 128;
const DIRECT_EVENT_CACHE_LIMIT: usize = 1024;
const TOPIC_RECOVERY_CONCURRENCY: usize = 4;
/// How long a member waits for the member ranked ahead of it to publish a
/// pending admission before taking the turn itself.
const ADMISSION_HANDOFF_MILLIS: u64 = 4_000;
const MAX_ADMISSION_HANDOFF_STEPS: usize = 8;
pub const DEVICE_SESSION_REVOKED_ERROR: &str = "this device was logged out remotely";
const DEVICE_LAST_SEEN_REFRESH_MILLIS: u64 = 5 * 60_000;

#[derive(Clone)]
pub struct NoiseClient {
    http: reqwest::Client,
    mask_relays: Vec<RelayDescriptor>,
}

pub struct GroupActivityUpdate {
    group_id: String,
    events: Vec<SignedEvent>,
    full_snapshot: bool,
    older_cursor: Option<GroupEventCursor>,
    has_older_messages: bool,
    topic_update: Option<TopicActivityUpdate>,
}

pub struct AccountStateUpdate {
    credentials: AccountCredentials,
    vault: AccountVault,
}

pub struct ReadStateUpdate {
    account: Option<AccountStateUpdate>,
    read_state: Option<AccountStateUpdate>,
}

struct TopicActivityUpdate {
    topic_id: String,
    latest_cursor: Option<GroupEventCursor>,
    older_cursor: Option<GroupEventCursor>,
    has_older_messages: bool,
    message_limit: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GroupActivityResult {
    pub summary: LocalSummary,
    pub conversation: Option<Conversation>,
}

impl Default for NoiseClient {
    fn default() -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        let http = shared_http_client();
        #[cfg(target_arch = "wasm32")]
        let http = reqwest::Client::builder()
            .build()
            .expect("noise HTTP configuration is valid");
        Self {
            http,
            mask_relays: Vec::new(),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn shared_http_client() -> reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(std::time::Duration::from_secs(40))
                .build()
                .expect("noise HTTP configuration is valid")
        })
        .clone()
}

#[cfg(not(target_arch = "wasm32"))]
fn active_media_chunk_downloads()
-> &'static Mutex<HashMap<PathBuf, tokio::sync::watch::Sender<bool>>> {
    static DOWNLOADS: OnceLock<Mutex<HashMap<PathBuf, tokio::sync::watch::Sender<bool>>>> =
        OnceLock::new();
    DOWNLOADS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// How long this installation has been looking at one unchanged situation.
///
/// Admission turns and stale-leaf recovery are both measured from the moment
/// this process first saw the situation, so the observation stays in memory: a
/// restarted client must start its own clock instead of inheriting an expired
/// one and acting immediately.
#[cfg(not(target_arch = "wasm32"))]
fn observations() -> &'static Mutex<HashMap<String, (String, u64)>> {
    static OBSERVATIONS: OnceLock<Mutex<HashMap<String, (String, u64)>>> = OnceLock::new();
    OBSERVATIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

#[cfg(not(target_arch = "wasm32"))]
fn observation_wait_millis(scope: &str, digest: &str) -> u64 {
    let now = current_millis();
    let mut observations = observations()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let observation = observations
        .entry(scope.to_owned())
        .or_insert_with(|| (digest.to_owned(), now));
    if observation.0 != digest {
        *observation = (digest.to_owned(), now);
    }
    now.saturating_sub(observation.1)
}

#[cfg(target_arch = "wasm32")]
thread_local! {
    static OBSERVATIONS: RefCell<HashMap<String, (String, u64)>> = RefCell::new(HashMap::new());
}

#[cfg(target_arch = "wasm32")]
fn observation_wait_millis(scope: &str, digest: &str) -> u64 {
    let now = current_millis();
    OBSERVATIONS.with(|observations| {
        let mut observations = observations.borrow_mut();
        let observation = observations
            .entry(scope.to_owned())
            .or_insert_with(|| (digest.to_owned(), now));
        if observation.0 != digest {
            *observation = (digest.to_owned(), now);
        }
        now.saturating_sub(observation.1)
    })
}

/// In-memory LRU for decrypted media chunks. The browser build has no disk
/// cache, so range requests served to the video element read chunks from here.
#[cfg(any(target_arch = "wasm32", test))]
struct MediaChunkLru {
    chunks: HashMap<String, (Rc<Vec<u8>>, u64)>,
    total_bytes: usize,
    sequence: u64,
    capacity_bytes: usize,
}

#[cfg(any(target_arch = "wasm32", test))]
impl MediaChunkLru {
    fn new(capacity_bytes: usize) -> Self {
        Self {
            chunks: HashMap::new(),
            total_bytes: 0,
            sequence: 0,
            capacity_bytes,
        }
    }

    fn get(&mut self, blob_id: &str, byte_length: u64) -> Option<Rc<Vec<u8>>> {
        let (data, sequence) = self.chunks.get_mut(blob_id)?;
        if data.len() as u64 != byte_length {
            return None;
        }
        self.sequence += 1;
        *sequence = self.sequence;
        Some(data.clone())
    }

    fn insert(&mut self, blob_id: String, data: Rc<Vec<u8>>) {
        if let Some((previous, _)) = self.chunks.remove(&blob_id) {
            self.total_bytes -= previous.len();
        }
        self.sequence += 1;
        self.total_bytes += data.len();
        self.chunks.insert(blob_id, (data, self.sequence));
        while self.total_bytes > self.capacity_bytes && !self.chunks.is_empty() {
            let Some((oldest, _)) = self
                .chunks
                .iter()
                .min_by_key(|(_, (_, sequence))| *sequence)
                .map(|(blob_id, entry)| (blob_id.clone(), entry.0.len()))
            else {
                break;
            };
            if let Some((data, _)) = self.chunks.remove(&oldest) {
                self.total_bytes -= data.len();
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
type WebChunkDownload = futures_util::future::Shared<
    futures_util::future::LocalBoxFuture<'static, Result<Rc<Vec<u8>>, String>>,
>;

#[cfg(target_arch = "wasm32")]
thread_local! {
    static WEB_MEDIA_CHUNKS: RefCell<MediaChunkLru> =
        RefCell::new(MediaChunkLru::new(WEB_MEDIA_CHUNK_CACHE_BYTES));
    static WEB_MEDIA_CHUNK_DOWNLOADS: RefCell<HashMap<String, WebChunkDownload>> =
        RefCell::new(HashMap::new());
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IdentitySummary {
    pub username: String,
    pub public_key: String,
    pub noise_id: Option<String>,
    pub bio: String,
    pub avatar: Option<ProfileImage>,
    pub album: Option<ProfileAlbum>,
    pub accepts_direct_messages: bool,
    pub direct_message_policy: DirectMessagePolicy,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupSummary {
    pub group_id: String,
    pub name: String,
    pub description: String,
    pub rules: String,
    #[serde(default)]
    pub content_rating: GroupContentRating,
    pub avatar: Option<ProfileImage>,
    pub background: Option<ProfileImage>,
    pub mobile_background: Option<ProfileImage>,
    pub accent_color: String,
    pub members_can_send_messages: bool,
    pub members_can_send_media: bool,
    pub frequency: Option<String>,
    pub owner_public_key: String,
    pub remote_deletion_supported: bool,
    pub is_active: bool,
    pub unread_count: usize,
    pub read_state_initialized: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdultAccessSummary {
    pub age_attested: bool,
    #[serde(default, alias = "adult_content_enabled")]
    pub explicit_content_enabled: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LocalSummary {
    pub identity: IdentitySummary,
    pub adult_access: AdultAccessSummary,
    pub devices: Vec<DeviceSummary>,
    pub groups: Vec<GroupSummary>,
    pub directs: Vec<DirectSummary>,
    pub known_people: Vec<DirectSummary>,
    pub blocked_people: Vec<DirectSummary>,
    pub hidden_public_keys: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct AdultAccessSettings {
    age_attested: bool,
    #[serde(default, alias = "adult_content_enabled")]
    explicit_content_enabled: bool,
    preference_updated_at_millis: u64,
}

impl Default for AdultAccessSettings {
    fn default() -> Self {
        // Noise had a handful of known-adult accounts before this setting
        // existed. Missing legacy data is deliberately migrated once as
        // attested, while sexually explicit groups remain hidden by default.
        Self {
            age_attested: true,
            explicit_content_enabled: false,
            preference_updated_at_millis: 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceSummary {
    pub device_id: String,
    pub name: String,
    pub platform: String,
    pub created_at_millis: u64,
    pub last_seen_at_millis: u64,
    pub is_current: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DirectSummary {
    pub public_key: String,
    pub username: String,
    pub bio: String,
    pub avatar: Option<ProfileImage>,
    pub album: Option<ProfileAlbum>,
    pub accepts_direct_messages: bool,
    pub direct_message_policy: DirectMessagePolicy,
    pub is_active: bool,
    pub has_unread: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MakeResult {
    pub group: GroupSummary,
    pub frequency: String,
    pub display_frequency: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JoinResult {
    pub group: GroupSummary,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GroupEncryptionStatus {
    pub group_id: String,
    pub phase: String,
    pub epoch: Option<u64>,
    pub missing_member_public_keys: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberSummary {
    pub public_key: String,
    pub username: String,
    pub bio: String,
    pub avatar: Option<ProfileImage>,
    pub album: Option<ProfileAlbum>,
    pub accepts_direct_messages: bool,
    pub direct_message_policy: DirectMessagePolicy,
    pub is_moderator: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageSummary {
    pub event_id: String,
    pub message_id: String,
    pub author_public_key: String,
    pub username: String,
    pub bio: String,
    pub avatar: Option<ProfileImage>,
    pub album: Option<ProfileAlbum>,
    pub accepts_direct_messages: bool,
    pub direct_message_policy: DirectMessagePolicy,
    pub text: String,
    pub attachment: Option<MediaAttachment>,
    pub reply_to_message_id: Option<String>,
    #[serde(default)]
    pub forwarded_from: Option<ForwardedFrom>,
    #[serde(default)]
    pub topic_id: Option<String>,
    pub created_at_millis: u64,
    pub reactions: Vec<ReactionSummary>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TopicSummary {
    pub topic_id: String,
    pub name: String,
    #[serde(default = "default_topic_icon")]
    pub icon: String,
    pub stream_locator: String,
    pub locked: bool,
    pub archived: bool,
    pub created_by_public_key: String,
    pub created_at_millis: u64,
    pub unread_count: usize,
    #[serde(default)]
    pub has_older_messages: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReactionSummary {
    pub emoji: String,
    pub count: usize,
    pub reactor_public_keys: Vec<String>,
    pub reacted_by_self: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SentMessageResult {
    pub event_id: String,
    pub message_id: String,
    pub created_at_millis: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conversation {
    pub group: GroupSummary,
    #[serde(default)]
    pub topics: Vec<TopicSummary>,
    #[serde(default)]
    pub general_unread_count: usize,
    pub members: Vec<MemberSummary>,
    pub banned_members: Vec<BannedMemberSummary>,
    pub messages: Vec<MessageSummary>,
    pub reports: Vec<ReportSummary>,
    pub reported_message_event_ids: Vec<String>,
    pub rejected_events: usize,
    #[serde(default)]
    pub has_older_messages: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportSummary {
    pub report_event_id: String,
    pub reporter_public_key: String,
    pub reporter_username: String,
    pub reporter_avatar: Option<ProfileImage>,
    pub reason: String,
    pub created_at_millis: u64,
    pub message: MessageSummary,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BannedMemberSummary {
    pub public_key: String,
    pub username: String,
    pub bio: String,
    pub avatar: Option<ProfileImage>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DirectMessageSummary {
    pub event_id: String,
    pub message_id: String,
    pub author_public_key: String,
    pub username: String,
    pub bio: String,
    pub avatar: Option<ProfileImage>,
    pub album: Option<ProfileAlbum>,
    pub accepts_direct_messages: bool,
    pub direct_message_policy: DirectMessagePolicy,
    pub text: String,
    pub attachment: Option<MediaAttachment>,
    pub reply_to_message_id: Option<String>,
    #[serde(default)]
    pub forwarded_from: Option<ForwardedFrom>,
    pub created_at_millis: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DirectConversation {
    pub contact: DirectSummary,
    pub media_scope_id: String,
    pub messages: Vec<DirectMessageSummary>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DirectInbox {
    pub summary: LocalSummary,
    pub conversations: Vec<DirectConversation>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchResults {
    pub messages: Vec<SearchMessageResult>,
    pub locations: Vec<SearchLocationResult>,
    pub people: Vec<SearchPersonResult>,
    pub has_more_history: bool,
    pub older_scopes: Vec<SearchHistoryScope>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchHistoryScope {
    pub group_id: Option<String>,
    pub topic_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchMessageResult {
    pub event_id: String,
    pub author_public_key: String,
    pub username: String,
    pub avatar: Option<ProfileImage>,
    pub text: String,
    pub attachment: Option<MediaAttachment>,
    pub created_at_millis: u64,
    pub group_id: Option<String>,
    pub group_name: Option<String>,
    pub topic_id: Option<String>,
    pub topic_name: Option<String>,
    pub direct_public_key: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchLocationResult {
    pub group_id: String,
    pub group_name: String,
    pub group_avatar: Option<ProfileImage>,
    pub topic_id: Option<String>,
    pub topic_name: Option<String>,
    pub topic_icon: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchPersonResult {
    pub public_key: String,
    pub username: String,
    pub bio: String,
    pub avatar: Option<ProfileImage>,
    pub album: Option<ProfileAlbum>,
    pub accepts_direct_messages: bool,
    pub direct_message_policy: DirectMessagePolicy,
    pub has_direct: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProfileAlbumData {
    pub scope_id: String,
    pub items: Vec<ProfileAlbumItem>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GroupWatch {
    pub revision: u64,
    pub changed: bool,
    #[serde(default)]
    pub deleted: bool,
    #[serde(default)]
    pub online_public_keys: Vec<String>,
    #[serde(default)]
    pub recently_active_public_keys: Vec<String>,
    #[serde(default)]
    pub changed_stream_locators: Vec<String>,
    #[serde(default)]
    pub control_changed: bool,
    #[serde(default)]
    pub change_hints_complete: bool,
}

#[derive(Clone, Debug, Deserialize)]
struct RelayGroupWatch {
    revision: u64,
    changed: bool,
    #[serde(default)]
    presences: Vec<GroupPresence>,
    #[serde(default)]
    changed_stream_locators: Vec<String>,
    #[serde(default)]
    control_changed: bool,
    #[serde(default)]
    change_hints_complete: bool,
}

/// The outcome of publishing a group control record.
///
/// Losing the head to another member is an ordinary result now that every
/// member can admit: the winning epoch arrives on the next sync.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ControlHead {
    Extended,
    Changed,
}

#[derive(Debug)]
struct EventPublishFailure {
    details: String,
    all_relays_deleted: bool,
}

impl std::fmt::Display for EventPublishFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "no relay accepted the event ({})", self.details)
    }
}

impl std::error::Error for EventPublishFailure {}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReplyNotificationSummary {
    pub event_id: String,
    pub group_id: String,
    pub group_name: String,
    pub username: String,
    pub text: String,
    pub attachment_mime_type: Option<String>,
    pub created_at_millis: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReplyNotificationSnapshot {
    pub group_id: String,
    pub replies: Vec<ReplyNotificationSummary>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AvatarData {
    pub mime_type: String,
    pub data_base64: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AttachmentData {
    pub mime_type: String,
    pub file_path: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AttachmentRangeData {
    pub mime_type: String,
    pub data_base64: String,
    pub offset: u64,
    pub byte_length: u64,
    pub total_byte_length: u64,
}

struct DecryptedDirectMessage {
    counterparty_public_key: String,
    contact: DirectContact,
    message: DirectMessageSummary,
}

enum DecryptedDirectEvent {
    Message(DecryptedDirectMessage),
    ThreadDeleted {
        counterparty_public_key: String,
        deleted_at_millis: u64,
    },
    BlockChanged {
        counterparty_public_key: String,
        contact: DirectContact,
        blocked: bool,
        sequence: u64,
        changed_at_millis: u64,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ClientState {
    version: u32,
    profile: Profile,
    #[serde(default)]
    adult_access: AdultAccessSettings,
    #[serde(default)]
    profile_sequence: u64,
    #[serde(default)]
    contact_signal_published_sequence: u64,
    identity_secret_base64: String,
    #[serde(default)]
    local_device_id: String,
    #[serde(default)]
    devices: HashMap<String, DeviceRecord>,
    /// Legacy installation-wide MLS state. Existing local profiles may still
    /// contain this field; group access is migrated into `mls_group_states`
    /// before it is synchronized to the account vault.
    #[serde(default)]
    mls_device: Option<MlsAccountState>,
    /// Password-vault-backed MLS recovery state, isolated per group so a fresh
    /// installation can resume every group without another device.
    #[serde(default)]
    mls_group_states: HashMap<String, MlsAccountState>,
    #[serde(default)]
    mls_recovery_dirty: bool,
    #[serde(default)]
    mls_join_requests: HashMap<String, MlsJoinRequest>,
    #[serde(default)]
    mls_local_geneses: HashMap<String, MlsGroupGenesis>,
    #[serde(default)]
    mls_control_logs: HashMap<String, MlsControlLog>,
    #[serde(default)]
    group_event_caches: HashMap<String, GroupEventCache>,
    #[serde(default)]
    direct_event_cache: DirectEventCache,
    groups: Vec<GroupMembership>,
    #[serde(default)]
    group_memberships: HashMap<String, GroupMembershipRecord>,
    active_group_id: Option<String>,
    #[serde(default)]
    direct_contacts: Vec<DirectContact>,
    #[serde(default)]
    known_people: Vec<DirectContact>,
    #[serde(default)]
    known_profile_versions_hydrated: bool,
    #[serde(default)]
    block_states: HashMap<String, BlockState>,
    #[serde(default)]
    blocked_by_states: HashMap<String, BlockState>,
    #[serde(default)]
    active_direct_public_key: Option<String>,
    #[serde(default)]
    direct_deleted_before: HashMap<String, u64>,
    #[serde(default)]
    direct_policy_rejected_through: HashMap<String, DirectMessageMarker>,
    #[serde(default)]
    direct_closed_periods: Vec<DirectClosedPeriod>,
    #[serde(default)]
    direct_latest_incoming: HashMap<String, DirectMessageMarker>,
    #[serde(default)]
    direct_latest_activity: HashMap<String, DirectMessageMarker>,
    #[serde(default)]
    direct_read_through: HashMap<String, DirectMessageMarker>,
    #[serde(default)]
    group_latest_incoming: HashMap<String, MessageMarker>,
    #[serde(default)]
    group_latest_activity: HashMap<String, MessageMarker>,
    #[serde(default)]
    group_read_through: HashMap<String, MessageMarker>,
    #[serde(default)]
    group_unread_messages: HashMap<String, Vec<MessageMarker>>,
    #[serde(default)]
    topic_read_through: HashMap<String, MessageMarker>,
    #[serde(default)]
    topic_unread_messages: HashMap<String, Vec<MessageMarker>>,
    #[serde(default)]
    topic_latest_incoming: HashMap<String, MessageMarker>,
    #[serde(default)]
    topic_activity_initialized: HashSet<String>,
    #[serde(default)]
    group_activity_initialized: HashSet<String>,
    // This is a materialized view of group_event_caches. Keeping it in memory
    // makes navigation instant, but persisting it duplicated roughly half of
    // the desktop state file and made every sync rewrite the same data twice.
    #[serde(default, skip_serializing)]
    group_conversation_cache: HashMap<String, Conversation>,
    #[serde(default)]
    group_frequencies: HashMap<String, String>,
    #[serde(default)]
    next_author_sequence: u64,
    #[serde(default)]
    account: Option<AccountSession>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct AccountSession {
    noise_id: String,
    locator: String,
    vault_key_base64: String,
    revision: u64,
    #[serde(default)]
    read_state_revision: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct AccountReadStateContents {
    version: u32,
    #[serde(default)]
    direct_read_through: HashMap<String, DirectMessageMarker>,
    #[serde(default)]
    group_read_through: HashMap<String, MessageMarker>,
    #[serde(default)]
    topic_read_through: HashMap<String, MessageMarker>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct AccountVaultContents {
    version: u32,
    profile: Profile,
    #[serde(default)]
    adult_access: AdultAccessSettings,
    #[serde(default)]
    profile_sequence: u64,
    #[serde(default)]
    contact_signal_published_sequence: u64,
    identity_secret_base64: String,
    #[serde(default)]
    devices: HashMap<String, DeviceRecord>,
    #[serde(default)]
    mls_group_states: HashMap<String, MlsAccountState>,
    #[serde(default)]
    mls_control_logs: HashMap<String, MlsControlLog>,
    #[serde(default)]
    group_event_caches: HashMap<String, GroupEventCache>,
    groups: Vec<GroupMembership>,
    #[serde(default)]
    group_memberships: HashMap<String, GroupMembershipRecord>,
    active_group_id: Option<String>,
    direct_contacts: Vec<DirectContact>,
    known_people: Vec<DirectContact>,
    #[serde(default)]
    block_states: HashMap<String, BlockState>,
    #[serde(default)]
    blocked_by_states: HashMap<String, BlockState>,
    active_direct_public_key: Option<String>,
    direct_deleted_before: HashMap<String, u64>,
    #[serde(default)]
    direct_policy_rejected_through: HashMap<String, DirectMessageMarker>,
    direct_closed_periods: Vec<DirectClosedPeriod>,
    #[serde(default)]
    direct_read_through: HashMap<String, DirectMessageMarker>,
    #[serde(default)]
    direct_latest_activity: HashMap<String, DirectMessageMarker>,
    #[serde(default)]
    group_read_through: HashMap<String, MessageMarker>,
    #[serde(default)]
    group_latest_activity: HashMap<String, MessageMarker>,
    #[serde(default)]
    topic_read_through: HashMap<String, MessageMarker>,
    #[serde(default)]
    topic_activity_initialized: HashSet<String>,
    #[serde(default)]
    group_activity_initialized: HashSet<String>,
    group_frequencies: HashMap<String, String>,
    next_author_sequence: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct DeviceRecord {
    device_id: String,
    name: String,
    platform: String,
    created_at_millis: u64,
    last_seen_at_millis: u64,
    #[serde(default)]
    revoked_at_millis: Option<u64>,
    sequence: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct GroupEventCache {
    #[serde(default)]
    events: Vec<SignedEvent>,
    #[serde(default)]
    latest_cursor: Option<GroupEventCursor>,
    #[serde(default)]
    older_cursor: Option<GroupEventCursor>,
    #[serde(default = "initial_group_message_window")]
    message_limit: usize,
    #[serde(default)]
    has_older_messages: bool,
    #[serde(default)]
    needs_recent_hydration: bool,
    #[serde(default = "control_hydration_needed")]
    needs_control_hydration: bool,
    #[serde(default)]
    topic_streams: HashMap<String, TopicStreamCache>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    portable_conversation: Option<Conversation>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct DirectEventCache {
    #[serde(default)]
    events: Vec<SignedEvent>,
    #[serde(default)]
    latest_cursor: Option<GroupEventCursor>,
    #[serde(default)]
    has_older_events: bool,
}

fn control_hydration_needed() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct TopicStreamCache {
    #[serde(default)]
    latest_cursor: Option<GroupEventCursor>,
    #[serde(default)]
    older_cursor: Option<GroupEventCursor>,
    #[serde(default = "initial_group_message_window")]
    message_limit: usize,
    #[serde(default)]
    has_older_messages: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct GroupEventCursor {
    created_at_millis: u64,
    event_id: String,
}

impl GroupEventCursor {
    fn from_event(event: &SignedEvent) -> Self {
        Self {
            created_at_millis: event.created_at_millis,
            event_id: event.event_id.clone(),
        }
    }

    fn key(&self) -> (u64, &str) {
        (self.created_at_millis, self.event_id.as_str())
    }
}

#[derive(Clone, Debug, Deserialize)]
struct GroupEventPage {
    events: Vec<SignedEvent>,
    has_more: bool,
    #[serde(default, skip)]
    continuation_cursor: Option<GroupEventCursor>,
}

#[derive(Clone, Copy)]
enum GroupEventPageRequest<'a> {
    Latest,
    Before(&'a GroupEventCursor),
    After(&'a GroupEventCursor),
}

fn initial_group_message_window() -> usize {
    INITIAL_GROUP_MESSAGE_WINDOW
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct GroupMembershipRecord {
    sequence: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    group: Option<GroupMembership>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct DirectContact {
    public_key: String,
    username: String,
    #[serde(default)]
    bio: String,
    #[serde(default)]
    avatar: Option<ProfileImage>,
    #[serde(default)]
    album: Option<ProfileAlbum>,
    #[serde(default = "default_true")]
    accepts_direct_messages: bool,
    #[serde(default)]
    direct_message_policy: DirectMessagePolicy,
    #[serde(default)]
    profile_sequence: u64,
}

impl DirectContact {
    fn effective_direct_message_policy(&self) -> DirectMessagePolicy {
        if self.accepts_direct_messages {
            self.direct_message_policy
        } else {
            DirectMessagePolicy::Nobody
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct BlockState {
    person: DirectContact,
    blocked: bool,
    sequence: u64,
    #[serde(default)]
    changed_at_millis: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct DirectClosedPeriod {
    closed_at_millis: u64,
    reopened_at_millis: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredProfileAlbum {
    version: u8,
    owner_public_key: String,
    items: Vec<ProfileAlbumItem>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct MessageMarker {
    created_at_millis: u64,
    event_id: String,
}

type DirectMessageMarker = MessageMarker;

fn topic_activity_key(group_id: &str, topic_id: Option<&str>) -> String {
    format!("{group_id}:{}", topic_id.unwrap_or("general"))
}

fn topic_key_belongs_to_group(key: &str, group_id: &str) -> bool {
    key.strip_prefix(group_id)
        .is_some_and(|suffix| suffix.starts_with(':'))
}

fn merge_read_markers(
    current: &mut HashMap<String, MessageMarker>,
    incoming: &HashMap<String, MessageMarker>,
) -> bool {
    let before = current.clone();
    for (public_key, marker) in incoming {
        current
            .entry(public_key.clone())
            .and_modify(|existing| *existing = existing.clone().max(marker.clone()))
            .or_insert_with(|| marker.clone());
    }
    *current != before
}

fn advance_message_marker(
    markers: &mut HashMap<String, MessageMarker>,
    key: &str,
    candidate: MessageMarker,
) -> bool {
    match markers.get_mut(key) {
        Some(existing) if *existing >= candidate => false,
        Some(existing) => {
            *existing = candidate;
            true
        }
        None => {
            markers.insert(key.to_owned(), candidate);
            true
        }
    }
}

fn search_score<'a>(query: &str, fields: impl IntoIterator<Item = &'a str>) -> Option<u32> {
    let query = query.to_lowercase();
    let tokens = query.split_whitespace().collect::<Vec<_>>();
    let fields = fields
        .into_iter()
        .map(str::to_lowercase)
        .collect::<Vec<_>>();
    let combined = fields.join(" ");
    if !tokens.iter().all(|token| combined.contains(token)) {
        return None;
    }
    let mut score = 10 + tokens.len() as u32;
    for field in &fields {
        if field == &query {
            score += 100;
        } else if field.starts_with(&query) {
            score += 55;
        } else if field.contains(&query) {
            score += 30;
        }
    }
    Some(score)
}

fn search_message_score(query: &str, text: &str) -> Option<u32> {
    let query = query.trim().to_lowercase();
    let tokens = query.split_whitespace().collect::<Vec<_>>();
    if tokens.is_empty() {
        return None;
    }
    let text = text.to_lowercase();
    if !tokens.iter().all(|token| text.contains(token)) {
        return None;
    }

    let mut score = 10 + tokens.len() as u32;
    if text == query {
        score += 100;
    } else if text.starts_with(&query) {
        score += 55;
    } else if text.contains(&query) {
        score += 30;
    }
    Some(score)
}

fn current_profiles_by_public_key(
    state: &ClientState,
    self_public_key: &str,
) -> HashMap<String, (u64, Profile)> {
    let mut profiles = HashMap::<String, (u64, Profile)>::new();
    profiles.insert(
        self_public_key.to_owned(),
        (u64::MAX, state.profile.clone()),
    );
    for person in state
        .known_people
        .iter()
        .chain(state.direct_contacts.iter())
    {
        let profile = Profile {
            username: person.username.clone(),
            bio: person.bio.clone(),
            avatar: person.avatar.clone(),
            album: person.album.clone(),
            accepts_direct_messages: person.accepts_direct_messages,
            direct_message_policy: person.effective_direct_message_policy(),
        };
        match profiles.get_mut(&person.public_key) {
            Some((sequence, current)) if person.profile_sequence >= *sequence => {
                *sequence = person.profile_sequence;
                *current = profile;
            }
            None => {
                profiles.insert(
                    person.public_key.clone(),
                    (person.profile_sequence, profile),
                );
            }
            _ => {}
        }
    }
    profiles
}

fn normalize_contact_signal(value: &str) -> anyhow::Result<String> {
    let normalized = value
        .chars()
        .filter(|character| !character.is_whitespace() && *character != '-')
        .flat_map(char::to_uppercase)
        .collect::<String>();
    const ALPHABET: &str = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    if normalized.len() != 12
        || !normalized
            .chars()
            .all(|character| ALPHABET.contains(character))
    {
        bail!("enter a 12-character noise signature")
    }
    Ok(normalized)
}

fn contact_signal_for_public_key(public_key: &str) -> anyhow::Result<String> {
    const ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let bytes = STANDARD_NO_PAD
        .decode(public_key)
        .context("noise signature public key is invalid")?;
    if bytes.len() != 32 {
        bail!("noise signature public key is invalid")
    }
    let mut signature = String::with_capacity(13);
    for character_index in 0..12 {
        let mut value = 0usize;
        for bit_index in 0..5 {
            let source_bit = character_index * 5 + bit_index;
            value =
                (value << 1) | usize::from((bytes[source_bit / 8] >> (7 - (source_bit % 8))) & 1);
        }
        signature.push(ALPHABET[value] as char);
        if character_index == 5 {
            signature.push('-');
        }
    }
    Ok(signature)
}

fn contact_signal_membership(signature: &str) -> anyhow::Result<GroupMembership> {
    let normalized = normalize_contact_signal(signature)?;
    let group_id = blake3::derive_key(
        "xyz.gnosyslabs.noise.contact-signal-locator.v1",
        normalized.as_bytes(),
    )
    .iter()
    .map(|byte| format!("{byte:02x}"))
    .collect();
    let secret = blake3::derive_key(
        "xyz.gnosyslabs.noise.contact-signal-key.v1",
        normalized.as_bytes(),
    );
    Ok(GroupMembership {
        group_id,
        name: "noise contact signal".to_owned(),
        description: String::new(),
        rules: String::new(),
        content_rating: GroupContentRating::General,
        avatar: None,
        background: None,
        mobile_background: None,
        accent_color: "#7758ed".to_owned(),
        members_can_send_messages: false,
        members_can_send_media: false,
        owner_public_key: String::new(),
        authority_nonce_base64: String::new(),
        secret_base64: STANDARD_NO_PAD.encode(secret),
    })
}

fn merge_block_states(
    current: &mut HashMap<String, BlockState>,
    incoming: &HashMap<String, BlockState>,
) -> bool {
    let before = current.clone();
    for (public_key, candidate) in incoming {
        current
            .entry(public_key.clone())
            .and_modify(|existing| {
                if (candidate.sequence, candidate.blocked) > (existing.sequence, existing.blocked) {
                    *existing = candidate.clone();
                }
            })
            .or_insert_with(|| candidate.clone());
    }
    *current != before
}

fn merge_group_membership_records(
    current: &mut HashMap<String, GroupMembershipRecord>,
    incoming: HashMap<String, GroupMembershipRecord>,
) -> bool {
    let before = current.clone();
    for (group_id, mut candidate) in incoming {
        match current.entry(group_id) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(candidate);
            }
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                let existing = entry.get();
                if existing
                    .group
                    .as_ref()
                    .is_some_and(|group| group.content_rating == GroupContentRating::Explicit)
                    && let Some(candidate_group) = candidate.group.as_mut()
                {
                    candidate_group.content_rating = GroupContentRating::Explicit;
                }
                let candidate_wins = candidate.sequence > existing.sequence
                    || (candidate.sequence == existing.sequence
                        && candidate.group.is_none()
                        && existing.group.is_some());
                if candidate_wins {
                    entry.insert(candidate);
                }
            }
        }
    }
    *current != before
}

fn merge_device_records(
    current: &mut HashMap<String, DeviceRecord>,
    incoming: HashMap<String, DeviceRecord>,
) -> bool {
    let before = current.clone();
    for (device_id, candidate) in incoming {
        if !valid_device_record(&device_id, &candidate) {
            continue;
        }
        match current.entry(device_id) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(candidate);
            }
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                let existing = entry.get();
                let candidate_wins = candidate.sequence > existing.sequence
                    || (candidate.sequence == existing.sequence
                        && candidate.revoked_at_millis.is_some()
                        && existing.revoked_at_millis.is_none());
                if candidate_wins {
                    entry.insert(candidate);
                }
            }
        }
    }
    *current != before
}

fn valid_device_record(device_id: &str, record: &DeviceRecord) -> bool {
    device_id == record.device_id
        && device_id.len() == 64
        && device_id.bytes().all(|byte| byte.is_ascii_hexdigit())
        && !record.name.trim().is_empty()
        && record.name.chars().count() <= 80
        && !record.platform.trim().is_empty()
        && record.platform.chars().count() <= 120
        && record.created_at_millis > 0
        && record.last_seen_at_millis >= record.created_at_millis
        && record
            .revoked_at_millis
            .is_none_or(|revoked| revoked >= record.created_at_millis)
}

fn default_true() -> bool {
    true
}

impl ClientState {
    fn ensure_local_device_id(&mut self) -> String {
        if self.local_device_id.is_empty() {
            let seed = format!(
                "{}:{}:{}",
                self.identity_secret_base64,
                current_nanos(),
                self.next_author_sequence,
            );
            self.local_device_id = blake3::hash(seed.as_bytes()).to_hex().to_string();
        }
        self.local_device_id.clone()
    }

    fn register_current_device(
        &mut self,
        name: &str,
        platform: &str,
        force_activity_update: bool,
    ) -> anyhow::Result<bool> {
        let name = name.trim();
        let platform = platform.trim();
        if name.is_empty() || name.chars().count() > 80 {
            bail!("device name must contain between 1 and 80 characters")
        }
        if platform.is_empty() || platform.chars().count() > 120 {
            bail!("device platform must contain between 1 and 120 characters")
        }
        let device_id = self.ensure_local_device_id();
        if self
            .devices
            .get(&device_id)
            .is_some_and(|device| device.revoked_at_millis.is_some())
        {
            bail!(DEVICE_SESSION_REVOKED_ERROR)
        }
        let now = current_millis().max(1);
        let needs_update = self.devices.get(&device_id).is_none_or(|device| {
            device.name != name
                || device.platform != platform
                || (force_activity_update
                    && now.saturating_sub(device.last_seen_at_millis)
                        >= DEVICE_LAST_SEEN_REFRESH_MILLIS)
        });
        if !needs_update {
            return Ok(false);
        }
        let sequence = self.take_sequence();
        let created_at_millis = self
            .devices
            .get(&device_id)
            .map_or(now, |device| device.created_at_millis);
        self.devices.insert(
            device_id.clone(),
            DeviceRecord {
                device_id,
                name: name.to_owned(),
                platform: platform.to_owned(),
                created_at_millis,
                last_seen_at_millis: now,
                revoked_at_millis: None,
                sequence,
            },
        );
        Ok(true)
    }

    fn current_device_is_revoked(&self) -> bool {
        !self.local_device_id.is_empty()
            && self
                .devices
                .get(&self.local_device_id)
                .is_some_and(|device| device.revoked_at_millis.is_some())
    }

    fn hydrate_known_profile_versions(&mut self) {
        if self.known_profile_versions_hydrated {
            return;
        }
        let candidates = self
            .groups
            .iter()
            .filter_map(|group| {
                let cache = self.group_event_caches.get(&group.group_id)?;
                rebuild_group_state(self, group, &cache.events).ok()
            })
            .flat_map(|view| view.members.into_values())
            .map(|member| DirectContact {
                public_key: member.public_key,
                username: member.username,
                bio: member.bio,
                avatar: member.avatar,
                album: member.album,
                accepts_direct_messages: member.accepts_direct_messages,
                direct_message_policy: member.direct_message_policy,
                profile_sequence: member.profile_sequence,
            })
            .collect::<Vec<_>>();
        for candidate in candidates {
            self.upsert_known_person(candidate);
        }
        self.known_profile_versions_hydrated = true;
    }

    fn identity(&self) -> anyhow::Result<Identity> {
        Identity::from_secret_base64(&self.identity_secret_base64)
            .context("stored identity is invalid")
    }

    fn active_group(&self) -> anyhow::Result<&GroupMembership> {
        let Some(group_id) = self.active_group_id.as_deref() else {
            bail!("no active group; make a group or join with a frequency first")
        };
        let group = self
            .groups
            .iter()
            .find(|group| group.group_id == group_id)
            .context("active group is missing from local state")?;
        self.ensure_group_access(group)?;
        Ok(group)
    }

    fn ensure_group_access(&self, group: &GroupMembership) -> anyhow::Result<()> {
        if group.content_rating == GroupContentRating::Explicit
            && !self.adult_access.explicit_content_enabled
        {
            bail!("enable explicit groups in settings before opening this group")
        }
        Ok(())
    }

    fn mls_group_state(&self, group_id: &str) -> Option<&MlsAccountState> {
        self.mls_group_states.get(group_id).or_else(|| {
            self.mls_device
                .as_ref()
                .filter(|mls| mls.epoch(group_id).is_ok())
        })
    }

    fn ensure_mls_group_state(&mut self, group_id: &str) -> anyhow::Result<&mut MlsAccountState> {
        if !self.mls_group_states.contains_key(group_id) {
            let identity = self.identity()?;
            let mut candidate = if let Some(legacy) = self
                .mls_device
                .as_ref()
                .filter(|mls| mls.epoch(group_id).is_ok())
            {
                legacy.clone()
            } else {
                MlsAccountState::create(&identity)
                    .context("could not create this group's recoverable MLS identity")?
            };
            for other_group_id in self
                .groups
                .iter()
                .map(|group| group.group_id.as_str())
                .filter(|candidate_group_id| *candidate_group_id != group_id)
            {
                let _ = candidate.forget_group(other_group_id);
            }
            let recoverable = candidate.epoch(group_id).is_ok();
            self.mls_group_states.insert(group_id.to_owned(), candidate);
            if recoverable {
                self.mls_recovery_dirty = true;
            }
        }
        self.mls_group_states
            .get_mut(group_id)
            .context("this group has no recoverable MLS identity")
    }

    fn set_mls_group_state(&mut self, group_id: &str, mls: MlsAccountState) {
        let changed = self.mls_group_states.get(group_id) != Some(&mls);
        let recoverable = mls.epoch(group_id).is_ok();
        self.mls_group_states.insert(group_id.to_owned(), mls);
        if changed && recoverable {
            self.mls_recovery_dirty = true;
        }
    }

    fn forget_mls_group(&mut self, group_id: &str) -> anyhow::Result<()> {
        if self.mls_group_states.remove(group_id).is_some() {
            self.mls_recovery_dirty = true;
        }
        if let Some(mls) = self.mls_device.as_mut() {
            mls.forget_group(group_id)
                .context("could not erase this group's legacy encryption state")?;
        }
        Ok(())
    }

    fn migrate_recoverable_mls_groups(&mut self) -> anyhow::Result<bool> {
        let before = self.mls_group_states.clone();
        let group_ids = self
            .groups
            .iter()
            .map(|group| group.group_id.clone())
            .collect::<Vec<_>>();
        for group_id in group_ids {
            if self
                .mls_device
                .as_ref()
                .is_some_and(|mls| mls.epoch(&group_id).is_ok())
            {
                self.ensure_mls_group_state(&group_id)?;
            }
        }
        Ok(before != self.mls_group_states)
    }

    fn ensure_group_membership_records(&mut self) {
        for group in &self.groups {
            self.group_memberships
                .entry(group.group_id.clone())
                .or_insert_with(|| GroupMembershipRecord {
                    sequence: 0,
                    group: Some(group.clone()),
                });
        }
        self.reconcile_group_memberships();
    }

    fn vault_group_memberships(&self) -> HashMap<String, GroupMembershipRecord> {
        let mut records = self.group_memberships.clone();
        for group in &self.groups {
            records
                .entry(group.group_id.clone())
                .and_modify(|record| {
                    if record.group.is_some() {
                        record.group = Some(group.clone());
                    }
                })
                .or_insert_with(|| GroupMembershipRecord {
                    sequence: 0,
                    group: Some(group.clone()),
                });
        }
        records
    }

    fn vault_mls_group_states(&self) -> HashMap<String, MlsAccountState> {
        self.mls_group_states
            .iter()
            .filter(|(group_id, mls)| mls.epoch(group_id).is_ok())
            .map(|(group_id, mls)| (group_id.clone(), mls.clone()))
            .collect()
    }

    fn reconcile_group_memberships(&mut self) -> bool {
        let before = self.groups.clone();
        let mut present = self
            .group_memberships
            .iter()
            .filter_map(|(group_id, record)| {
                record
                    .group
                    .as_ref()
                    .map(|group| (group_id.clone(), record.sequence, group.clone()))
            })
            .collect::<Vec<_>>();
        present.sort_by(|left, right| {
            let left_position = before
                .iter()
                .position(|group| group.group_id == left.0)
                .unwrap_or(usize::MAX);
            let right_position = before
                .iter()
                .position(|group| group.group_id == right.0)
                .unwrap_or(usize::MAX);
            left_position
                .cmp(&right_position)
                .then_with(|| left.1.cmp(&right.1))
                .then_with(|| left.0.cmp(&right.0))
        });
        self.groups = present
            .into_iter()
            .map(|(_, _, membership)| membership)
            .collect();

        let present_ids = self
            .groups
            .iter()
            .map(|group| group.group_id.clone())
            .collect::<HashSet<_>>();
        let removed_ids = before
            .iter()
            .map(|group| group.group_id.clone())
            .filter(|group_id| !present_ids.contains(group_id))
            .collect::<Vec<_>>();
        for group_id in removed_ids {
            self.group_frequencies.remove(&group_id);
            self.forget_group_activity(&group_id);
            self.group_conversation_cache.remove(&group_id);
            self.group_event_caches.remove(&group_id);
            self.mls_join_requests.remove(&group_id);
            self.mls_local_geneses.remove(&group_id);
            self.mls_control_logs.remove(&group_id);
            let _ = self.forget_mls_group(&group_id);
        }
        if self
            .active_group_id
            .as_ref()
            .is_some_and(|group_id| !present_ids.contains(group_id))
        {
            self.active_group_id = self.groups.first().map(|group| group.group_id.clone());
        }
        before != self.groups
    }

    fn add_group(&mut self, group: GroupMembership) {
        let group_id = group.group_id.clone();
        let sequence = self.take_sequence();
        self.group_memberships.insert(
            group_id.clone(),
            GroupMembershipRecord {
                sequence,
                group: Some(group.clone()),
            },
        );
        if let Some(existing) = self
            .groups
            .iter_mut()
            .find(|existing| existing.group_id == group_id)
        {
            *existing = group;
        } else {
            self.groups.push(group);
        }
        self.active_group_id = Some(group_id);
    }

    fn apply_resolved_group_profile(
        &mut self,
        group_id: &str,
        profile: &GroupProfile,
        owner_public_key: &str,
    ) -> bool {
        let Some(group) = self
            .groups
            .iter_mut()
            .find(|group| group.group_id == group_id)
        else {
            return false;
        };
        let group_changed = group.name != profile.name
            || group.description != profile.description
            || group.rules != profile.rules
            || group.content_rating != profile.content_rating
            || group.avatar != profile.avatar
            || group.background != profile.background
            || group.mobile_background != profile.mobile_background
            || group.accent_color != profile.accent_color
            || group.members_can_send_messages != profile.members_can_send_messages
            || group.members_can_send_media != profile.members_can_send_media
            || group.owner_public_key != owner_public_key;
        if group_changed {
            group.name = profile.name.clone();
            group.description = profile.description.clone();
            group.rules = profile.rules.clone();
            group.content_rating = profile.content_rating;
            group.avatar = profile.avatar.clone();
            group.background = profile.background.clone();
            group.mobile_background = profile.mobile_background.clone();
            group.accent_color = profile.accent_color.clone();
            group.members_can_send_messages = profile.members_can_send_messages;
            group.members_can_send_media = profile.members_can_send_media;
            group.owner_public_key = owner_public_key.to_owned();
        }
        let resolved_group = group.clone();
        let mut membership_changed = false;
        if let Some(record) = self.group_memberships.get_mut(group_id)
            && record
                .group
                .as_ref()
                .is_some_and(|group| group != &resolved_group)
        {
            record.group = Some(resolved_group);
            membership_changed = true;
        }
        group_changed || membership_changed
    }

    fn tombstone_group(&mut self, group_id: &str) {
        let sequence = self.take_sequence();
        self.group_memberships.insert(
            group_id.to_owned(),
            GroupMembershipRecord {
                sequence,
                group: None,
            },
        );
        self.reconcile_group_memberships();
    }

    fn take_sequence(&mut self) -> u64 {
        // One Noise identity can be active on several devices. A purely local
        // counter can therefore fall behind another device and make otherwise
        // valid events look like replays. Use wall-clock nanoseconds as a
        // shared ordering floor while preserving monotonicity on this device.
        let wall_clock_sequence = current_nanos();
        let sequence = self.next_author_sequence.max(wall_clock_sequence);
        self.next_author_sequence = sequence.saturating_add(1);
        sequence
    }

    fn account_credentials(&self) -> anyhow::Result<AccountCredentials> {
        let account = self
            .account
            .as_ref()
            .context("this identity has no noise ID")?;
        Ok(AccountCredentials {
            noise_id: account.noise_id.clone(),
            locator: account.locator.clone(),
            vault_key_base64: account.vault_key_base64.clone(),
        })
    }

    fn account_read_state_credentials(&self) -> anyhow::Result<AccountCredentials> {
        let account = self
            .account
            .as_ref()
            .context("this identity has no noise ID")?;
        let locator =
            blake3::hash(format!("noise-account-read-state-v1:{}", account.locator).as_bytes())
                .to_hex()
                .to_string();
        Ok(AccountCredentials {
            noise_id: account.noise_id.clone(),
            locator,
            vault_key_base64: account.vault_key_base64.clone(),
        })
    }

    fn read_state_contents(&self) -> AccountReadStateContents {
        AccountReadStateContents {
            version: 1,
            direct_read_through: self.direct_read_through.clone(),
            group_read_through: self.group_read_through.clone(),
            topic_read_through: self.topic_read_through.clone(),
        }
    }

    fn portable_group_state_snapshots(&self) -> HashMap<String, GroupEventCache> {
        self.groups
            .iter()
            .filter_map(|group| {
                let mut conversation = self
                    .group_conversation_cache
                    .get(&group.group_id)
                    .cloned()
                    .or_else(|| {
                        self.group_event_caches
                            .get(&group.group_id)
                            .and_then(|cache| cache.portable_conversation.clone())
                    })?;
                conversation.messages.clear();
                conversation.reports.clear();
                conversation.reported_message_event_ids.clear();
                conversation.has_older_messages = true;
                Some((
                    group.group_id.clone(),
                    GroupEventCache {
                        events: Vec::new(),
                        latest_cursor: None,
                        older_cursor: None,
                        message_limit: INITIAL_GROUP_MESSAGE_WINDOW,
                        has_older_messages: true,
                        needs_recent_hydration: true,
                        needs_control_hydration: true,
                        topic_streams: HashMap::new(),
                        portable_conversation: Some(conversation),
                    },
                ))
            })
            .collect()
    }

    fn vault_contents(&self) -> AccountVaultContents {
        AccountVaultContents {
            version: 1,
            profile: self.profile.clone(),
            adult_access: self.adult_access.clone(),
            profile_sequence: self.profile_sequence,
            contact_signal_published_sequence: self.contact_signal_published_sequence,
            identity_secret_base64: self.identity_secret_base64.clone(),
            devices: self.devices.clone(),
            mls_group_states: self.vault_mls_group_states(),
            mls_control_logs: self
                .mls_control_logs
                .iter()
                .filter(|(group_id, log)| {
                    self.groups.iter().any(|group| group.group_id == **group_id)
                        && log.verify().is_ok()
                })
                .map(|(group_id, log)| (group_id.clone(), log.clone()))
                .collect(),
            // Messages remain relay data and a device-local cache. The small
            // message-free conversation snapshot is account recovery material:
            // it carries the current group artwork, members, and topic index so
            // a new device can render the group before paging relay history.
            group_event_caches: self.portable_group_state_snapshots(),
            groups: self.groups.clone(),
            group_memberships: self.vault_group_memberships(),
            active_group_id: self.active_group_id.clone(),
            direct_contacts: self.direct_contacts.clone(),
            known_people: self.known_people.clone(),
            block_states: self.block_states.clone(),
            blocked_by_states: self.blocked_by_states.clone(),
            active_direct_public_key: self.active_direct_public_key.clone(),
            direct_deleted_before: self.direct_deleted_before.clone(),
            direct_policy_rejected_through: self.direct_policy_rejected_through.clone(),
            direct_closed_periods: self.direct_closed_periods.clone(),
            direct_read_through: self.direct_read_through.clone(),
            direct_latest_activity: self.direct_latest_activity.clone(),
            group_read_through: self.group_read_through.clone(),
            group_latest_activity: self.group_latest_activity.clone(),
            topic_read_through: self.topic_read_through.clone(),
            topic_activity_initialized: self.topic_activity_initialized.clone(),
            group_activity_initialized: self.group_activity_initialized.clone(),
            group_frequencies: self.group_frequencies.clone(),
            next_author_sequence: self.next_author_sequence,
        }
    }

    fn from_vault(contents: AccountVaultContents, account: AccountSession) -> anyhow::Result<Self> {
        if contents.version != 1 {
            bail!("this account vault was created by an unsupported noise version")
        }
        let identity = Identity::from_secret_base64(&contents.identity_secret_base64)
            .context("stored identity is invalid")?;
        let identity_public_key = identity.public_key_base64();
        for (group_id, mls) in &contents.mls_group_states {
            if mls.device_credential().account_public_key != identity_public_key
                || mls.epoch(group_id).is_err()
            {
                bail!("encrypted account vault contains invalid group recovery state")
            }
        }
        let mut state = Self {
            version: 3,
            profile: contents.profile,
            adult_access: contents.adult_access,
            profile_sequence: contents.profile_sequence,
            contact_signal_published_sequence: contents.contact_signal_published_sequence,
            identity_secret_base64: contents.identity_secret_base64,
            local_device_id: String::new(),
            devices: contents.devices,
            mls_device: None,
            mls_group_states: contents.mls_group_states,
            mls_recovery_dirty: false,
            mls_join_requests: HashMap::new(),
            mls_local_geneses: HashMap::new(),
            mls_control_logs: contents.mls_control_logs,
            group_event_caches: contents.group_event_caches,
            direct_event_cache: DirectEventCache::default(),
            groups: contents.groups,
            group_memberships: contents.group_memberships,
            active_group_id: contents.active_group_id,
            direct_contacts: contents.direct_contacts,
            known_people: contents.known_people,
            known_profile_versions_hydrated: false,
            block_states: contents.block_states,
            blocked_by_states: contents.blocked_by_states,
            active_direct_public_key: contents.active_direct_public_key,
            direct_deleted_before: contents.direct_deleted_before,
            direct_policy_rejected_through: contents.direct_policy_rejected_through,
            direct_closed_periods: contents.direct_closed_periods,
            direct_latest_incoming: HashMap::new(),
            direct_latest_activity: contents.direct_latest_activity,
            direct_read_through: contents.direct_read_through,
            group_latest_incoming: HashMap::new(),
            group_latest_activity: contents.group_latest_activity,
            group_read_through: contents.group_read_through,
            group_unread_messages: HashMap::new(),
            topic_read_through: contents.topic_read_through,
            topic_unread_messages: HashMap::new(),
            topic_latest_incoming: HashMap::new(),
            topic_activity_initialized: contents.topic_activity_initialized,
            group_activity_initialized: contents.group_activity_initialized,
            group_conversation_cache: HashMap::new(),
            group_frequencies: contents.group_frequencies,
            next_author_sequence: contents.next_author_sequence,
            account: Some(account),
        };
        state.ensure_group_membership_records();
        state.identity()?;
        for (group_id, log) in &state.mls_control_logs {
            if log.genesis.group_id != *group_id || log.verify().is_err() {
                bail!("encrypted account vault contains invalid group encryption history")
            }
        }
        let raw_event_caches = std::mem::take(&mut state.group_event_caches);
        for (group_id, cache) in raw_event_caches {
            let Some(group) = state
                .groups
                .iter()
                .find(|group| group.group_id == group_id)
                .cloned()
            else {
                continue;
            };
            let mut portable_conversation = cache.portable_conversation;
            if portable_conversation.as_ref().is_some_and(|conversation| {
                conversation.group.group_id != group_id
                    || conversation.messages.len() > INITIAL_GROUP_MESSAGE_WINDOW
            }) {
                bail!("encrypted account vault contains invalid cached conversation")
            }
            if let Some(conversation) = portable_conversation.as_mut() {
                strip_conversation_media_previews(conversation);
            }
            let needs_recent_hydration =
                cache.needs_recent_hydration && portable_conversation.is_some();
            let mut compacted = compact_group_event_cache(
                &state,
                &group,
                cache.events,
                cache.message_limit.max(INITIAL_GROUP_MESSAGE_WINDOW),
                cache.latest_cursor,
                cache.older_cursor,
                cache.has_older_messages,
                cache.topic_streams,
            )
            .context("encrypted account vault contains invalid group history")?;
            compacted.needs_recent_hydration = needs_recent_hydration;
            compacted.needs_control_hydration = cache.needs_control_hydration;
            compacted.portable_conversation = portable_conversation;
            state.group_event_caches.insert(group_id, compacted);
        }
        state.apply_block_filters();
        Ok(state)
    }

    fn direct_messages_blocked_at(&self, created_at_millis: u64) -> bool {
        self.direct_closed_periods.iter().any(|period| {
            created_at_millis >= period.closed_at_millis
                && period
                    .reopened_at_millis
                    .is_none_or(|reopened| created_at_millis < reopened)
        })
    }

    fn shares_active_group_with(&self, public_key: &str) -> bool {
        self.groups.iter().any(|group| {
            if let Some(log) = self.mls_control_logs.get(&group.group_id) {
                let (epoch, _) = log.head();
                if log
                    .member_accounts_at(epoch)
                    .is_some_and(|members| members.iter().any(|member| member == public_key))
                {
                    return true;
                }
            }
            cached_group_view(self, group).is_ok_and(|view| view.members.contains_key(public_key))
        })
    }

    fn direct_has_unread(&self, public_key: &str) -> bool {
        self.direct_latest_incoming
            .get(public_key)
            .is_some_and(|latest| {
                self.direct_read_through
                    .get(public_key)
                    .is_none_or(|read| latest > read)
            })
    }

    fn merge_direct_read_through(
        &mut self,
        incoming: &HashMap<String, DirectMessageMarker>,
    ) -> bool {
        merge_read_markers(&mut self.direct_read_through, incoming)
    }

    fn group_unread_count(&self, group_id: &str) -> usize {
        let topic_count = self
            .topic_unread_messages
            .iter()
            .filter(|(key, _)| topic_key_belongs_to_group(key, group_id))
            .map(|(_, unread)| unread.len())
            .sum::<usize>();
        if self
            .topic_activity_initialized
            .iter()
            .any(|key| topic_key_belongs_to_group(key, group_id))
        {
            topic_count
        } else {
            self.group_unread_messages
                .get(group_id)
                .map(Vec::len)
                .unwrap_or_default()
        }
    }

    fn topic_unread_count(&self, group_id: &str, topic_id: Option<&str>) -> usize {
        self.topic_unread_messages
            .get(&topic_activity_key(group_id, topic_id))
            .map(Vec::len)
            .unwrap_or_default()
    }

    fn topic_read_state_initialized(&self, group_id: &str, topic_id: Option<&str>) -> bool {
        self.topic_activity_initialized
            .contains(&topic_activity_key(group_id, topic_id))
    }

    fn merge_topic_read_through(&mut self, incoming: &HashMap<String, MessageMarker>) -> bool {
        let changed = merge_read_markers(&mut self.topic_read_through, incoming);
        if changed {
            for (topic_key, marker) in &self.topic_read_through {
                if let Some(unread) = self.topic_unread_messages.get_mut(topic_key) {
                    unread.retain(|candidate| candidate > marker);
                }
            }
            self.topic_unread_messages
                .retain(|_, unread| !unread.is_empty());
        }
        changed
    }

    fn merge_group_read_through(&mut self, incoming: &HashMap<String, MessageMarker>) -> bool {
        let changed = merge_read_markers(&mut self.group_read_through, incoming);
        if changed {
            for (group_id, marker) in &self.group_read_through {
                if let Some(unread) = self.group_unread_messages.get_mut(group_id) {
                    unread.retain(|candidate| candidate > marker);
                }
            }
            self.group_unread_messages
                .retain(|_, unread| !unread.is_empty());
        }
        changed
    }

    fn record_group_activity(
        &mut self,
        group_id: &str,
        messages: &[AcceptedMessage],
        self_public_key: &str,
    ) -> bool {
        let topic_read_before = self.topic_read_through.clone();
        let topic_unread_before = self.topic_unread_messages.clone();
        let topic_latest_before = self.topic_latest_incoming.clone();
        let topic_initialized_before = self.topic_activity_initialized.clone();
        let before = (
            self.group_latest_activity.get(group_id).cloned(),
            self.group_latest_incoming.get(group_id).cloned(),
            self.group_read_through.get(group_id).cloned(),
            self.group_unread_messages.get(group_id).cloned(),
            self.group_activity_initialized.contains(group_id),
        );
        let mut activity = messages
            .iter()
            .filter(|message| !self.is_hidden(&message.author_public_key))
            .map(|message| MessageMarker {
                created_at_millis: message.created_at_millis,
                event_id: message.event_id.clone(),
            })
            .collect::<Vec<_>>();
        activity.sort();
        activity.dedup();
        let incoming = messages
            .iter()
            .filter(|message| message.author_public_key != self_public_key)
            .filter(|message| !self.is_hidden(&message.author_public_key))
            .map(|message| MessageMarker {
                created_at_millis: message.created_at_millis,
                event_id: message.event_id.clone(),
            })
            .collect::<Vec<_>>();
        let mut incoming = incoming;
        incoming.sort();
        incoming.dedup();

        if let Some(latest) = activity.last().cloned() {
            advance_message_marker(&mut self.group_latest_activity, group_id, latest);
        }
        if let Some(latest) = incoming.last().cloned() {
            advance_message_marker(&mut self.group_latest_incoming, group_id, latest);
        }

        if self.group_activity_initialized.insert(group_id.to_owned()) {
            if let Some(latest) = incoming.last().cloned() {
                advance_message_marker(&mut self.group_read_through, group_id, latest);
            }
            self.group_unread_messages.remove(group_id);
        } else {
            let read_through = self.group_read_through.get(group_id);
            let unread = incoming
                .into_iter()
                .filter(|marker| read_through.is_none_or(|read| marker > read))
                .collect::<Vec<_>>();
            if unread.is_empty() {
                self.group_unread_messages.remove(group_id);
            } else {
                self.group_unread_messages
                    .insert(group_id.to_owned(), unread);
            }
        }

        let mut incoming_by_topic = HashMap::<String, Vec<MessageMarker>>::new();
        for message in messages
            .iter()
            .filter(|message| message.author_public_key != self_public_key)
            .filter(|message| !self.is_hidden(&message.author_public_key))
        {
            incoming_by_topic
                .entry(topic_activity_key(group_id, message.topic_id.as_deref()))
                .or_default()
                .push(MessageMarker {
                    created_at_millis: message.created_at_millis,
                    event_id: message.event_id.clone(),
                });
        }
        for (topic_key, mut topic_incoming) in incoming_by_topic {
            topic_incoming.sort();
            topic_incoming.dedup();
            if let Some(latest) = topic_incoming.last().cloned() {
                advance_message_marker(&mut self.topic_latest_incoming, &topic_key, latest.clone());
            }
            if self.topic_activity_initialized.insert(topic_key.clone()) {
                let initial_read = if topic_key == topic_activity_key(group_id, None) {
                    self.group_read_through
                        .get(group_id)
                        .cloned()
                        .or_else(|| topic_incoming.last().cloned())
                } else {
                    topic_incoming.last().cloned()
                };
                if let Some(marker) = initial_read {
                    advance_message_marker(&mut self.topic_read_through, &topic_key, marker);
                }
                self.topic_unread_messages.remove(&topic_key);
            } else {
                let read_through = self.topic_read_through.get(&topic_key);
                let unread = topic_incoming
                    .into_iter()
                    .filter(|marker| read_through.is_none_or(|read| marker > read))
                    .collect::<Vec<_>>();
                if unread.is_empty() {
                    self.topic_unread_messages.remove(&topic_key);
                } else {
                    self.topic_unread_messages.insert(topic_key, unread);
                }
            }
        }

        before
            != (
                self.group_latest_activity.get(group_id).cloned(),
                self.group_latest_incoming.get(group_id).cloned(),
                self.group_read_through.get(group_id).cloned(),
                self.group_unread_messages.get(group_id).cloned(),
                self.group_activity_initialized.contains(group_id),
            )
            || topic_read_before != self.topic_read_through
            || topic_unread_before != self.topic_unread_messages
            || topic_latest_before != self.topic_latest_incoming
            || topic_initialized_before != self.topic_activity_initialized
    }

    fn forget_group_activity(&mut self, group_id: &str) {
        self.group_latest_incoming.remove(group_id);
        self.group_latest_activity.remove(group_id);
        self.group_read_through.remove(group_id);
        self.group_unread_messages.remove(group_id);
        self.group_activity_initialized.remove(group_id);
        self.topic_read_through
            .retain(|key, _| !topic_key_belongs_to_group(key, group_id));
        self.topic_unread_messages
            .retain(|key, _| !topic_key_belongs_to_group(key, group_id));
        self.topic_latest_incoming
            .retain(|key, _| !topic_key_belongs_to_group(key, group_id));
        self.topic_activity_initialized
            .retain(|key| !topic_key_belongs_to_group(key, group_id));
    }

    fn is_blocked(&self, public_key: &str) -> bool {
        self.block_states
            .get(public_key)
            .is_some_and(|state| state.blocked)
    }

    fn is_hidden(&self, public_key: &str) -> bool {
        self.is_blocked(public_key)
            || self
                .blocked_by_states
                .get(public_key)
                .is_some_and(|state| state.blocked)
    }

    fn active_blocked_keys(&self) -> HashSet<String> {
        self.block_states
            .iter()
            .filter(|(_, state)| state.blocked)
            .map(|(public_key, _)| public_key.clone())
            .collect()
    }

    fn active_hidden_keys(&self) -> HashSet<String> {
        let mut hidden = self.active_blocked_keys();
        hidden.extend(
            self.blocked_by_states
                .iter()
                .filter(|(_, state)| state.blocked)
                .map(|(public_key, _)| public_key.clone()),
        );
        hidden
    }

    fn apply_block_filters(&mut self) {
        let block_cutoffs = self
            .block_states
            .iter()
            .chain(self.blocked_by_states.iter())
            .filter(|(_, state)| state.changed_at_millis > 0)
            .map(|(public_key, state)| (public_key.clone(), state.changed_at_millis))
            .collect::<Vec<_>>();
        for (public_key, cutoff) in block_cutoffs {
            self.direct_deleted_before
                .entry(public_key)
                .and_modify(|existing| *existing = (*existing).max(cutoff))
                .or_insert(cutoff);
        }
        let unblocked_people = self
            .block_states
            .values()
            .chain(self.blocked_by_states.values())
            .filter(|state| !state.blocked)
            .map(|state| state.person.clone())
            .collect::<Vec<_>>();
        for person in unblocked_people {
            self.upsert_known_person(person);
        }
        let hidden = self.active_hidden_keys();
        if hidden.is_empty() {
            self.group_conversation_cache.clear();
            return;
        }
        self.direct_contacts
            .retain(|contact| !hidden.contains(&contact.public_key));
        self.known_people
            .retain(|contact| !hidden.contains(&contact.public_key));
        for public_key in &hidden {
            self.direct_latest_incoming.remove(public_key);
            self.direct_latest_activity.remove(public_key);
            self.direct_read_through.remove(public_key);
        }
        if self
            .active_direct_public_key
            .as_ref()
            .is_some_and(|public_key| hidden.contains(public_key))
        {
            self.active_direct_public_key = self
                .direct_contacts
                .first()
                .map(|contact| contact.public_key.clone());
        }
        let blocked_event_ids = self
            .group_conversation_cache
            .values()
            .flat_map(|conversation| conversation.messages.iter())
            .filter(|message| hidden.contains(&message.author_public_key))
            .map(|message| message.event_id.clone())
            .collect::<HashSet<_>>();
        self.group_conversation_cache.clear();
        for unread in self.group_unread_messages.values_mut() {
            unread.retain(|marker| !blocked_event_ids.contains(&marker.event_id));
        }
        self.group_unread_messages
            .retain(|_, unread| !unread.is_empty());
    }

    fn upsert_known_person(&mut self, contact: DirectContact) {
        if contact.public_key
            == self
                .identity()
                .ok()
                .map(|identity| identity.public_key_base64())
                .unwrap_or_default()
        {
            return;
        }
        if self.is_hidden(&contact.public_key) {
            return;
        }
        if let Some(existing) = self
            .known_people
            .iter_mut()
            .find(|person| person.public_key == contact.public_key)
        {
            if contact.profile_sequence >= existing.profile_sequence {
                *existing = contact.clone();
            }
        } else {
            self.known_people.push(contact.clone());
        }
        if let Some(existing) = self
            .direct_contacts
            .iter_mut()
            .find(|person| person.public_key == contact.public_key)
        {
            if contact.profile_sequence >= existing.profile_sequence {
                *existing = contact;
            }
        }
    }

    fn add_direct(&mut self, contact: DirectContact) {
        let public_key = contact.public_key.clone();
        self.remember_direct(contact);
        self.active_direct_public_key = Some(public_key);
    }

    fn remember_direct(&mut self, contact: DirectContact) {
        self.upsert_known_person(contact.clone());
        if let Some(existing) = self
            .direct_contacts
            .iter_mut()
            .find(|person| person.public_key == contact.public_key)
        {
            if contact.profile_sequence >= existing.profile_sequence {
                *existing = contact;
            }
        } else {
            self.direct_contacts.push(contact);
        }
    }

    fn known_profile_sequence(&self, public_key: &str) -> u64 {
        self.direct_contacts
            .iter()
            .chain(self.known_people.iter())
            .filter(|person| person.public_key == public_key)
            .map(|person| person.profile_sequence)
            .max()
            .unwrap_or_default()
    }

    fn summary(&self) -> anyhow::Result<LocalSummary> {
        let public_key = self.identity()?.public_key_base64();
        Ok(LocalSummary {
            identity: IdentitySummary {
                username: self.profile.username.clone(),
                public_key,
                noise_id: self
                    .account
                    .as_ref()
                    .and_then(|account| display_noise_id(&account.noise_id).ok()),
                bio: self.profile.bio.clone(),
                avatar: self.profile.avatar.clone(),
                album: self.profile.album.clone(),
                accepts_direct_messages: self.profile.effective_direct_message_policy()
                    != DirectMessagePolicy::Nobody,
                direct_message_policy: self.profile.effective_direct_message_policy(),
            },
            adult_access: AdultAccessSummary {
                age_attested: self.adult_access.age_attested,
                explicit_content_enabled: self.adult_access.explicit_content_enabled,
            },
            devices: {
                let mut devices = self
                    .devices
                    .values()
                    .filter(|device| device.revoked_at_millis.is_none())
                    .map(|device| DeviceSummary {
                        device_id: device.device_id.clone(),
                        name: device.name.clone(),
                        platform: device.platform.clone(),
                        created_at_millis: device.created_at_millis,
                        last_seen_at_millis: device.last_seen_at_millis,
                        is_current: device.device_id == self.local_device_id,
                    })
                    .collect::<Vec<_>>();
                devices.sort_by(|left, right| {
                    right
                        .is_current
                        .cmp(&left.is_current)
                        .then_with(|| right.last_seen_at_millis.cmp(&left.last_seen_at_millis))
                        .then_with(|| left.device_id.cmp(&right.device_id))
                });
                devices
            },
            groups: {
                let mut groups = self
                    .groups
                    .iter()
                    .filter(|group| {
                        group.content_rating != GroupContentRating::Explicit
                            || self.adult_access.explicit_content_enabled
                    })
                    .collect::<Vec<_>>();
                groups.sort_by(|left, right| {
                    let left_unread = self.group_unread_count(&left.group_id) > 0;
                    let right_unread = self.group_unread_count(&right.group_id) > 0;
                    right_unread
                        .cmp(&left_unread)
                        .then_with(|| {
                            let markers = if left_unread && right_unread {
                                &self.group_latest_incoming
                            } else {
                                &self.group_latest_activity
                            };
                            markers
                                .get(&right.group_id)
                                .cmp(&markers.get(&left.group_id))
                        })
                        .then_with(|| left.group_id.cmp(&right.group_id))
                });
                groups
                    .into_iter()
                    .map(|group| GroupSummary {
                        group_id: group.group_id.clone(),
                        name: group.name.clone(),
                        description: group.description.clone(),
                        rules: group.rules.clone(),
                        content_rating: group.content_rating,
                        avatar: group.avatar.clone(),
                        background: group.background.clone(),
                        mobile_background: group.mobile_background.clone(),
                        accent_color: group.accent_color.clone(),
                        members_can_send_messages: group.members_can_send_messages,
                        members_can_send_media: group.members_can_send_media,
                        frequency: self
                            .group_frequencies
                            .get(&group.group_id)
                            .and_then(|frequency| display_frequency(frequency).ok()),
                        owner_public_key: group.owner_public_key.clone(),
                        remote_deletion_supported: !group.authority_nonce_base64.is_empty(),
                        is_active: self.active_group_id.as_deref() == Some(&group.group_id),
                        unread_count: self.group_unread_count(&group.group_id),
                        read_state_initialized: self
                            .group_activity_initialized
                            .contains(&group.group_id),
                    })
                    .collect()
            },
            directs: {
                let mut contacts = self
                    .direct_contacts
                    .iter()
                    .filter(|contact| !self.is_hidden(&contact.public_key))
                    .collect::<Vec<_>>();
                contacts.sort_by(|left, right| {
                    self.direct_latest_activity
                        .get(&right.public_key)
                        .cmp(&self.direct_latest_activity.get(&left.public_key))
                        .then_with(|| left.public_key.cmp(&right.public_key))
                });
                contacts
                    .into_iter()
                    .map(|contact| DirectSummary {
                        public_key: contact.public_key.clone(),
                        username: contact.username.clone(),
                        bio: contact.bio.clone(),
                        avatar: contact.avatar.clone(),
                        album: contact.album.clone(),
                        accepts_direct_messages: contact.accepts_direct_messages,
                        direct_message_policy: contact.effective_direct_message_policy(),
                        is_active: self.active_direct_public_key.as_deref()
                            == Some(&contact.public_key),
                        has_unread: self.direct_has_unread(&contact.public_key),
                    })
                    .collect()
            },
            known_people: self
                .known_people
                .iter()
                .filter(|contact| !self.is_hidden(&contact.public_key))
                .map(|contact| DirectSummary {
                    public_key: contact.public_key.clone(),
                    username: contact.username.clone(),
                    bio: contact.bio.clone(),
                    avatar: contact.avatar.clone(),
                    album: contact.album.clone(),
                    accepts_direct_messages: contact.accepts_direct_messages,
                    direct_message_policy: contact.effective_direct_message_policy(),
                    is_active: false,
                    has_unread: false,
                })
                .collect(),
            blocked_people: {
                let mut people = self
                    .block_states
                    .values()
                    .filter(|blocked| blocked.blocked)
                    .map(|blocked| direct_summary(&blocked.person, false, false))
                    .collect::<Vec<_>>();
                people.sort_by(|left, right| left.username.cmp(&right.username));
                people
            },
            hidden_public_keys: {
                let mut hidden = self.active_hidden_keys().into_iter().collect::<Vec<_>>();
                hidden.sort();
                hidden
            },
        })
    }
}

impl NoiseClient {
    pub fn with_mask_relays(relays: Vec<String>) -> anyhow::Result<Self> {
        let mut client = Self::default();
        for value in relays.into_iter().take(16) {
            let relay = RelayDescriptor::parse(&value)?;
            if !client
                .mask_relays
                .iter()
                .any(|current| current.base_url == relay.base_url)
            {
                client.mask_relays.push(relay);
            }
        }
        Ok(client)
    }

    pub async fn discover_relay_masks(
        &self,
        cache_path: impl AsRef<Path>,
        relays: Vec<String>,
    ) -> anyhow::Result<Vec<String>> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            relay_pool::discover(cache_path.as_ref(), relays).await
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = cache_path;
            Ok(relays)
        }
    }

    pub async fn initialize(
        &self,
        path: impl AsRef<Path>,
        username: impl Into<String>,
        password: impl Into<String>,
        birth_date: &str,
        avatar_data_base64: Option<String>,
        avatar_mime_type: Option<String>,
        relays: Vec<String>,
    ) -> anyhow::Result<LocalSummary> {
        let path = path.as_ref();
        if state_exists(path) {
            bail!("{} already exists", path.display());
        }
        let username = username.into().trim().to_owned();
        validate_display_name(&username)?;
        let password = password.into();
        validate_password(&password)?;
        validate_adult_birth_date(birth_date)?;
        let noise_id = generate_noise_id();
        let credentials = derive_account_credentials(&noise_id, &password)?;
        let relays = relay_list(relays)?;
        let identity = Identity::generate();
        let avatar = if let Some(encoded) = avatar_data_base64 {
            let mime_type = avatar_mime_type.context("avatar media type is missing")?;
            if !matches!(
                mime_type.as_str(),
                "image/jpeg" | "image/png" | "image/webp"
            ) {
                bail!("avatar must be a JPEG, PNG, or WebP image")
            }
            let data = STANDARD
                .decode(encoded)
                .context("avatar image encoding is invalid")?;
            if data.is_empty() || data.len() > 256 * 1024 {
                bail!("avatar images must contain between 1 byte and 256 KiB")
            }
            let (blob, key_base64) = EncryptedBlob::create(&data)?;
            let storage = self.store_blob_shards(&relays, &blob, &key_base64).await?;
            Some(ProfileImage {
                blob_id: blob.blob_id,
                key_base64,
                mime_type,
                byte_length: data.len() as u32,
                storage: Some(storage),
            })
        } else {
            None
        };
        let mut state = ClientState {
            version: 3,
            profile: Profile {
                username,
                bio: String::new(),
                avatar,
                album: None,
                accepts_direct_messages: true,
                direct_message_policy: DirectMessagePolicy::Everyone,
            },
            adult_access: AdultAccessSettings {
                age_attested: true,
                explicit_content_enabled: false,
                preference_updated_at_millis: current_millis(),
            },
            profile_sequence: current_nanos(),
            contact_signal_published_sequence: 0,
            identity_secret_base64: identity.secret_base64(),
            local_device_id: String::new(),
            devices: HashMap::new(),
            mls_device: None,
            mls_group_states: HashMap::new(),
            mls_recovery_dirty: false,
            mls_join_requests: HashMap::new(),
            mls_local_geneses: HashMap::new(),
            mls_control_logs: HashMap::new(),
            group_event_caches: HashMap::new(),
            direct_event_cache: DirectEventCache::default(),
            groups: Vec::new(),
            group_memberships: HashMap::new(),
            active_group_id: None,
            direct_contacts: Vec::new(),
            known_people: Vec::new(),
            known_profile_versions_hydrated: true,
            block_states: HashMap::new(),
            blocked_by_states: HashMap::new(),
            active_direct_public_key: None,
            direct_deleted_before: HashMap::new(),
            direct_policy_rejected_through: HashMap::new(),
            direct_closed_periods: Vec::new(),
            direct_latest_incoming: HashMap::new(),
            direct_latest_activity: HashMap::new(),
            direct_read_through: HashMap::new(),
            group_latest_incoming: HashMap::new(),
            group_latest_activity: HashMap::new(),
            group_read_through: HashMap::new(),
            group_unread_messages: HashMap::new(),
            topic_read_through: HashMap::new(),
            topic_unread_messages: HashMap::new(),
            topic_latest_incoming: HashMap::new(),
            topic_activity_initialized: HashSet::new(),
            group_activity_initialized: HashSet::new(),
            group_conversation_cache: HashMap::new(),
            group_frequencies: HashMap::new(),
            next_author_sequence: 0,
            account: Some(AccountSession {
                noise_id: credentials.noise_id,
                locator: credentials.locator,
                vault_key_base64: credentials.vault_key_base64,
                revision: 0,
                read_state_revision: 0,
            }),
        };
        self.publish_account_state(&mut state, &relays).await?;
        self.publish_read_state_for_state(&mut state, &relays)
            .await?;
        save_state(path, &state)?;
        state.summary()
    }

    pub async fn sign_in(
        &self,
        path: impl AsRef<Path>,
        noise_id: &str,
        password: impl Into<String>,
        relays: Vec<String>,
    ) -> anyhow::Result<LocalSummary> {
        let path = path.as_ref();
        let password = password.into();
        if password.is_empty() || password.chars().count() > 256 {
            bail!("noise ID or password is incorrect")
        }
        let credentials = derive_account_credentials(noise_id, &password)?;
        if state_exists(path) {
            let state = load_state(path)?;
            let is_same_account = state.account.as_ref().is_some_and(|account| {
                account.noise_id == credentials.noise_id
                    && account.locator == credentials.locator
                    && account.vault_key_base64 == credentials.vault_key_base64
            });
            if is_same_account {
                return state.summary();
            }
            bail!("an identity is already active on this device")
        }
        let relays = relay_list(relays)?;
        let vault = self
            .fetch_account_vault(&relays, &credentials.locator)
            .await
            .context("noise ID or password is incorrect")?;
        let plaintext = vault
            .open(&credentials)
            .map_err(|_| anyhow::anyhow!("noise ID or password is incorrect"))?;
        let contents: AccountVaultContents = serde_json::from_slice(&plaintext)
            .map_err(|_| anyhow::anyhow!("noise ID or password is incorrect"))?;
        let account = AccountSession {
            noise_id: credentials.noise_id,
            locator: credentials.locator,
            vault_key_base64: credentials.vault_key_base64,
            revision: vault.revision,
            read_state_revision: 0,
        };
        let mut state = ClientState::from_vault(contents, account)
            .map_err(|_| anyhow::anyhow!("noise ID or password is incorrect"))?;
        if state.identity()?.public_key_base64() != vault.identity_public_key {
            bail!("noise ID or password is incorrect")
        }
        let read_credentials = state.account_read_state_credentials()?;
        let has_read_state = if let Ok(read_vault) = self
            .fetch_account_vault(&relays, &read_credentials.locator)
            .await
        {
            Self::merge_remote_read_state(&mut state, &read_credentials, &read_vault)?;
            true
        } else {
            false
        };
        if !has_read_state {
            let _ = self.publish_read_state_for_state(&mut state, &relays).await;
        }
        save_state(path, &state)?;
        state.summary()
    }

    pub async fn set_explicit_content_enabled(
        &self,
        path: impl AsRef<Path>,
        enabled: bool,
        relays: Vec<String>,
    ) -> anyhow::Result<LocalSummary> {
        let path = path.as_ref();
        let mut state = load_state(path)?;
        if !state.adult_access.age_attested {
            bail!("explicit content requires an 18+ age attestation")
        }
        if state.adult_access.explicit_content_enabled == enabled {
            return state.summary();
        }
        state.adult_access.explicit_content_enabled = enabled;
        state.adult_access.preference_updated_at_millis = current_millis().max(1);
        if !enabled
            && state
                .active_group_id
                .as_ref()
                .is_some_and(|active_group_id| {
                    state.groups.iter().any(|group| {
                        group.group_id == *active_group_id
                            && group.content_rating == GroupContentRating::Explicit
                    })
                })
        {
            state.active_group_id = None;
        }
        self.publish_account_state(&mut state, &relay_list(relays)?)
            .await?;
        save_state(path, &state)?;
        state.summary()
    }

    pub async fn sync_account(
        &self,
        path: impl AsRef<Path>,
        relays: Vec<String>,
    ) -> anyhow::Result<LocalSummary> {
        let path = path.as_ref();
        let Some(update) = self.prepare_account_sync(path, relays).await? else {
            return load_state(path)?.summary();
        };
        self.apply_account_state_update(path, update)
    }

    pub async fn register_device(
        &self,
        path: impl AsRef<Path>,
        name: &str,
        platform: &str,
        relays: Vec<String>,
    ) -> anyhow::Result<LocalSummary> {
        let path = path.as_ref();
        let mut state = load_state(path)?;
        state.register_current_device(name, platform, true)?;
        self.publish_account_state(&mut state, &relay_list(relays)?)
            .await?;
        save_state(path, &state)?;
        state.summary()
    }

    pub async fn revoke_device(
        &self,
        path: impl AsRef<Path>,
        device_id: &str,
        relays: Vec<String>,
    ) -> anyhow::Result<LocalSummary> {
        let path = path.as_ref();
        let mut state = load_state(path)?;
        if device_id == state.local_device_id {
            bail!("log out this device from its own settings")
        }
        let Some(existing) = state.devices.get(device_id) else {
            bail!("this device is no longer signed in")
        };
        if existing.revoked_at_millis.is_some() {
            return state.summary();
        }
        let sequence = state.take_sequence();
        let revoked_at_millis = current_millis().max(1);
        let device = state
            .devices
            .get_mut(device_id)
            .context("this device is no longer signed in")?;
        device.revoked_at_millis = Some(revoked_at_millis);
        device.sequence = sequence;
        self.publish_account_state(&mut state, &relay_list(relays)?)
            .await?;
        save_state(path, &state)?;
        state.summary()
    }

    pub async fn prepare_account_sync(
        &self,
        path: impl AsRef<Path>,
        relays: Vec<String>,
    ) -> anyhow::Result<Option<AccountStateUpdate>> {
        let path = path.as_ref();
        let mut state = load_state(path)?;
        if state.account.is_none() {
            return Ok(None);
        }
        self.publish_account_state(&mut state, &relay_list(relays)?)
            .await?;
        let credentials = state.account_credentials()?;
        let identity = state.identity()?;
        let revision = state
            .account
            .as_ref()
            .context("this identity has no noise ID")?
            .revision;
        let plaintext = serde_json::to_vec(&state.vault_contents())?;
        let vault = AccountVault::seal(&identity, &credentials, revision, &plaintext)?;
        Ok(Some(AccountStateUpdate { credentials, vault }))
    }

    pub fn apply_account_state_update(
        &self,
        path: impl AsRef<Path>,
        update: AccountStateUpdate,
    ) -> anyhow::Result<LocalSummary> {
        let path = path.as_ref();
        let mut state = load_state(path)?;
        Self::merge_remote_account_state(&mut state, &update.credentials, &update.vault)?;
        save_state(path, &state)?;
        state.summary()
    }

    pub async fn sync_read_state(
        &self,
        path: impl AsRef<Path>,
        cache_path: impl AsRef<Path>,
        relays: Vec<String>,
    ) -> anyhow::Result<LocalSummary> {
        let path = path.as_ref();
        let Some(update) = self.prepare_read_state_sync(path, relays).await? else {
            return load_state(path)?.summary();
        };
        self.apply_read_state_update(path, cache_path, update)
    }

    pub async fn prepare_read_state_sync(
        &self,
        path: impl AsRef<Path>,
        relays: Vec<String>,
    ) -> anyhow::Result<Option<ReadStateUpdate>> {
        let state = load_state(path.as_ref())?;
        if state.account.is_none() {
            return Ok(None);
        }
        let relays = relay_list(relays)?;
        let read_credentials = state.account_read_state_credentials()?;
        let read_state = self
            .fetch_account_vault(&relays, &read_credentials.locator)
            .await
            .ok()
            .map(|vault| AccountStateUpdate {
                credentials: read_credentials,
                vault,
            });
        let account = if read_state.is_none() {
            let credentials = state.account_credentials()?;
            let vault = self
                .fetch_account_vault(&relays, &credentials.locator)
                .await?;
            Some(AccountStateUpdate { credentials, vault })
        } else {
            None
        };
        Ok(Some(ReadStateUpdate {
            account,
            read_state,
        }))
    }

    pub async fn refresh_account_state(
        &self,
        path: impl AsRef<Path>,
        cache_path: impl AsRef<Path>,
        relays: Vec<String>,
    ) -> anyhow::Result<LocalSummary> {
        let path = path.as_ref();
        let Some(update) = self.prepare_account_state_refresh(path, relays).await? else {
            return load_state(path)?.summary();
        };
        self.apply_read_state_update(path, cache_path, update)
    }

    pub async fn prepare_account_state_refresh(
        &self,
        path: impl AsRef<Path>,
        relays: Vec<String>,
    ) -> anyhow::Result<Option<ReadStateUpdate>> {
        let path = path.as_ref();
        let state = load_state(path)?;
        if state.account.is_none() {
            return Ok(None);
        }
        let credentials = state.account_credentials()?;
        let vault = self
            .fetch_account_vault(&relay_list(relays)?, &credentials.locator)
            .await?;
        Ok(Some(ReadStateUpdate {
            account: Some(AccountStateUpdate { credentials, vault }),
            read_state: None,
        }))
    }

    pub fn apply_read_state_update(
        &self,
        path: impl AsRef<Path>,
        cache_path: impl AsRef<Path>,
        update: ReadStateUpdate,
    ) -> anyhow::Result<LocalSummary> {
        let path = path.as_ref();
        let mut state = load_state(path)?;
        let groups_before = state
            .groups
            .iter()
            .map(|group| group.group_id.clone())
            .collect::<HashSet<_>>();
        let mut changed = false;
        if let Some(account) = update.account {
            changed |=
                Self::merge_remote_account_state(&mut state, &account.credentials, &account.vault)?;
        }
        if let Some(read_state) = update.read_state {
            changed |= Self::merge_remote_read_state(
                &mut state,
                &read_state.credentials,
                &read_state.vault,
            )?;
        }
        let groups_after = state
            .groups
            .iter()
            .map(|group| group.group_id.clone())
            .collect::<HashSet<_>>();
        let removed_groups = groups_before
            .difference(&groups_after)
            .cloned()
            .collect::<Vec<_>>();
        for group_id in &removed_groups {
            purge_group_cache(cache_path.as_ref(), group_id)?;
        }
        if !removed_groups.is_empty() {
            purge_profile_image_cache(cache_path.as_ref())?;
        }
        let blocked = state.active_hidden_keys();
        if !blocked.is_empty() {
            let identity = state.identity()?;
            for public_key in blocked {
                purge_scope_cache(cache_path.as_ref(), &identity.direct_scope_id(&public_key)?)?;
                purge_scope_cache(cache_path.as_ref(), &profile_media_scope_id(&public_key)?)?;
            }
        }
        if changed || state.mls_recovery_dirty {
            save_state(path, &state)?;
        }
        // Recovery snapshots are durable locally and the client schedules an
        // account sync independently. A remote vault write must never block
        // reading account state or opening a group.
        state.summary()
    }

    pub async fn publish_read_state(
        &self,
        path: impl AsRef<Path>,
        relays: Vec<String>,
    ) -> anyhow::Result<LocalSummary> {
        let path = path.as_ref();
        let mut state = load_state(path)?;
        if state.account.is_none() {
            return state.summary();
        }
        self.publish_read_state_for_state(&mut state, &relay_list(relays)?)
            .await?;
        save_state(path, &state)?;
        state.summary()
    }

    pub fn logout(
        &self,
        path: impl AsRef<Path>,
        cache_path: impl AsRef<Path>,
    ) -> anyhow::Result<()> {
        let path = path.as_ref();
        let cache_path = cache_path.as_ref();
        remove_state(path)?;
        for directory in ["media", "media-chunks", "outgoing"].map(|name| cache_path.join(name)) {
            if directory.exists() {
                fs::remove_dir_all(&directory)
                    .with_context(|| format!("could not erase {}", directory.display()))?;
            }
        }
        purge_profile_image_cache(cache_path)?;
        Ok(())
    }

    pub fn local_summary(&self, path: impl AsRef<Path>) -> anyhow::Result<LocalSummary> {
        load_state(path.as_ref())?.summary()
    }

    pub fn cached_conversation(
        &self,
        path: impl AsRef<Path>,
        group_id: &str,
    ) -> anyhow::Result<Option<Conversation>> {
        let state = load_state(path.as_ref())?;
        let group = state
            .groups
            .iter()
            .find(|group| group.group_id == group_id)
            .context("unknown group")?;
        state.ensure_group_access(group)?;
        let mut cached = state.group_conversation_cache.get(group_id).cloned();
        if cached.is_none()
            && let Some(event_cache) = state.group_event_caches.get(group_id)
        {
            if let Some(portable) = event_cache.portable_conversation.as_ref() {
                cached = Some(portable.clone());
            } else if state
                .mls_group_state(group_id)
                .is_some_and(|mls| mls.epoch(group_id).is_ok())
            {
                let identity_public_key = state.identity()?.public_key_base64();
                let view = rebuild_group_state(&state, group, &event_cache.events)?;
                if view.members.contains_key(&identity_public_key) {
                    let mut conversation =
                        cached_conversation_from_view(&state, group, view, &identity_public_key);
                    conversation.has_older_messages = event_cache.has_older_messages;
                    cached = Some(conversation);
                }
            }
        }
        if let Some(conversation) = cached.as_mut() {
            let identity_public_key = state.identity()?.public_key_base64();
            apply_current_group_profiles(conversation, &state, &identity_public_key);
            let blocked = state.active_hidden_keys();
            filter_conversation_for_blocks(conversation, &blocked);
            conversation.group.is_active = state.active_group_id.as_deref() == Some(group_id);
            conversation.group.unread_count = state.group_unread_count(group_id);
            conversation.group.read_state_initialized =
                state.group_activity_initialized.contains(group_id);
            conversation.general_unread_count = state.topic_unread_count(group_id, None);
            for topic in &mut conversation.topics {
                topic.unread_count = state.topic_unread_count(group_id, Some(&topic.topic_id));
            }
        }
        Ok(cached)
    }

    #[must_use]
    pub fn has_local_state(&self, path: impl AsRef<Path>) -> bool {
        state_exists(path.as_ref())
    }

    pub fn select_group(
        &self,
        path: impl AsRef<Path>,
        group_id: &str,
    ) -> anyhow::Result<LocalSummary> {
        let path = path.as_ref();
        let mut state = load_state(path)?;
        let group = state
            .groups
            .iter()
            .find(|group| group.group_id == group_id)
            .context("unknown group")?;
        state.ensure_group_access(group)?;
        state.active_group_id = Some(group_id.to_owned());
        save_state(path, &state)?;
        state.summary()
    }

    pub async fn watch_group(
        &self,
        path: impl AsRef<Path>,
        since: Option<u64>,
        relays: Vec<String>,
    ) -> anyhow::Result<GroupWatch> {
        let path = path.as_ref();
        let state = load_state(path)?;
        let group = state.active_group()?.clone();
        drop(state);
        self.watch_group_membership(path, group, since, relay_list(relays)?)
            .await
    }

    pub async fn watch_group_id(
        &self,
        path: impl AsRef<Path>,
        group_id: &str,
        since: Option<u64>,
        relays: Vec<String>,
    ) -> anyhow::Result<GroupWatch> {
        let path = path.as_ref();
        let state = load_state(path)?;
        let group = state
            .groups
            .iter()
            .find(|group| group.group_id == group_id)
            .cloned()
            .context("unknown group")?;
        state.ensure_group_access(&group)?;
        drop(state);
        self.watch_group_membership(path, group, since, relay_list(relays)?)
            .await
    }

    async fn watch_group_membership(
        &self,
        path: &Path,
        group: GroupMembership,
        since: Option<u64>,
        relays: Vec<RelayDescriptor>,
    ) -> anyhow::Result<GroupWatch> {
        match self.watch_id(&group, since, relays).await {
            Ok(change) => Ok(change),
            Err(error) if error.to_string() == "group has been deleted" => {
                let mut state = load_state(path)?;
                if state
                    .groups
                    .iter()
                    .any(|candidate| candidate.group_id == group.group_id)
                {
                    state.tombstone_group(&group.group_id);
                    save_state(path, &state)?;
                }
                Ok(GroupWatch {
                    revision: since.unwrap_or_default(),
                    changed: true,
                    deleted: true,
                    online_public_keys: Vec::new(),
                    recently_active_public_keys: Vec::new(),
                    changed_stream_locators: Vec::new(),
                    control_changed: true,
                    change_hints_complete: true,
                })
            }
            Err(error) => Err(error),
        }
    }

    pub async fn heartbeat_presence(
        &self,
        path: impl AsRef<Path>,
        active: bool,
        relays: Vec<String>,
    ) -> anyhow::Result<usize> {
        let state = load_state(path.as_ref())?;
        let identity = state.identity()?;
        let relays = relay_list(relays)?;
        let mut accepted = 0usize;
        for group in &state.groups {
            accepted += self
                .publish_group_presence(group, &identity, &relays, active)
                .await;
        }
        for contact in state
            .direct_contacts
            .iter()
            .filter(|contact| !state.is_hidden(&contact.public_key))
        {
            let Ok(mailbox) = identity.direct_mailbox(&contact.public_key, &contact.public_key)
            else {
                continue;
            };
            accepted += self
                .publish_group_presence(&mailbox, &identity, &relays, active)
                .await;
        }
        Ok(accepted)
    }

    pub async fn reply_notification_snapshot(
        &self,
        path: impl AsRef<Path>,
        group_id: &str,
        relays: Vec<String>,
    ) -> anyhow::Result<ReplyNotificationSnapshot> {
        let state = load_state(path.as_ref())?;
        let group = state
            .groups
            .iter()
            .find(|group| group.group_id == group_id)
            .cloned()
            .context("unknown group")?;
        let identity_public_key = state.identity()?.public_key_base64();
        let events = self.fetch_events(&group, relay_list(relays)?).await?;
        let view = rebuild_group_state(&state, &group, &events)?;
        let own_message_ids = view
            .messages
            .iter()
            .filter(|message| !state.is_hidden(&message.author_public_key))
            .filter(|message| message.author_public_key == identity_public_key)
            .map(|message| message.message_id.clone())
            .collect::<HashSet<_>>();
        let group_name = view.profile.name.clone();
        let mut replies = view
            .messages
            .iter()
            .filter(|message| !state.is_hidden(&message.author_public_key))
            .filter(|message| message.author_public_key != identity_public_key)
            .filter(|message| {
                message
                    .reply_to_message_id
                    .as_ref()
                    .is_some_and(|message_id| own_message_ids.contains(message_id))
            })
            .map(|message| ReplyNotificationSummary {
                event_id: message.event_id.clone(),
                group_id: group.group_id.clone(),
                group_name: group_name.clone(),
                username: message.username.clone(),
                text: message.text.clone(),
                attachment_mime_type: message
                    .attachment
                    .as_ref()
                    .map(|attachment| attachment.mime_type.clone()),
                created_at_millis: message.created_at_millis,
            })
            .collect::<Vec<_>>();
        replies.sort_by(|left, right| {
            left.created_at_millis
                .cmp(&right.created_at_millis)
                .then_with(|| left.event_id.cmp(&right.event_id))
        });
        Ok(ReplyNotificationSnapshot {
            group_id: group.group_id,
            replies,
        })
    }

    pub async fn watch_account(
        &self,
        path: impl AsRef<Path>,
        since: Option<u64>,
        relays: Vec<String>,
    ) -> anyhow::Result<GroupWatch> {
        let state = load_state(path.as_ref())?;
        let account = state
            .account
            .as_ref()
            .context("this identity has no noise ID")?;
        let revision = since
            .map(|revision| revision.to_string())
            .unwrap_or_else(|| "initial".to_owned());
        let endpoint = format!("/v1/accounts/{}/watch/{revision}", account.locator);
        let relays = relay_list(relays)?;

        for index in 0..relays.len() {
            let Ok(response) = self
                .relay_request(&relays, index, "GET", &endpoint, &[])
                .await
            else {
                continue;
            };
            if response.status == 410 {
                bail!("this noise account has been deleted")
            }
            if (200..300).contains(&response.status)
                && let Ok(change) = serde_json::from_slice::<GroupWatch>(&response.body)
            {
                return Ok(change);
            }
        }
        bail!("no relay could hold the private account watch")
    }

    pub async fn watch_read_state(
        &self,
        path: impl AsRef<Path>,
        since: Option<u64>,
        relays: Vec<String>,
    ) -> anyhow::Result<GroupWatch> {
        let state = load_state(path.as_ref())?;
        let credentials = state.account_read_state_credentials()?;
        let revision = since
            .map(|revision| revision.to_string())
            .unwrap_or_else(|| "initial".to_owned());
        let endpoint = format!("/v1/accounts/{}/watch/{revision}", credentials.locator);
        let relay_values = relays.clone();
        let relays = relay_list(relays)?;

        for index in 0..relays.len() {
            let Ok(response) = self
                .relay_request(&relays, index, "GET", &endpoint, &[])
                .await
            else {
                continue;
            };
            if response.status == 404 {
                return self.watch_account(path, since, relay_values).await;
            }
            if (200..300).contains(&response.status)
                && let Ok(change) = serde_json::from_slice::<GroupWatch>(&response.body)
            {
                return Ok(change);
            }
        }
        bail!("no relay could hold the private read-state watch")
    }

    async fn watch_id(
        &self,
        group: &GroupMembership,
        since: Option<u64>,
        relays: Vec<RelayDescriptor>,
    ) -> anyhow::Result<GroupWatch> {
        let revision = since
            .map(|revision| revision.to_string())
            .unwrap_or_else(|| "initial".to_owned());
        let endpoint = format!("/v1/groups/{}/watch/{revision}", group.group_id);

        let mut deleted_relays = 0usize;
        for index in 0..relays.len() {
            let Ok(response) = self
                .relay_request(&relays, index, "GET", &endpoint, &[])
                .await
            else {
                continue;
            };
            if response.status == 410 {
                deleted_relays += 1;
                continue;
            }
            if (200..300).contains(&response.status)
                && let Ok(change) = serde_json::from_slice::<RelayGroupWatch>(&response.body)
            {
                let now = current_millis();
                let mut online_public_keys = Vec::new();
                let mut recently_active_public_keys = Vec::new();
                for presence in change.presences {
                    let Ok(public_key) = presence.open(group) else {
                        continue;
                    };
                    if presence.expires_at_millis
                        > now.saturating_add(ONLINE_PRESENCE_REMAINING_MILLIS)
                    {
                        online_public_keys.push(public_key);
                    } else if presence
                        .expires_at_millis
                        .saturating_add(RECENT_GROUP_PRESENCE_MILLIS)
                        > now
                    {
                        recently_active_public_keys.push(public_key);
                    }
                }
                online_public_keys.sort();
                online_public_keys.dedup();
                recently_active_public_keys.sort();
                recently_active_public_keys.dedup();
                return Ok(GroupWatch {
                    revision: change.revision,
                    changed: change.changed,
                    deleted: false,
                    online_public_keys,
                    recently_active_public_keys,
                    changed_stream_locators: change.changed_stream_locators,
                    control_changed: change.control_changed,
                    change_hints_complete: change.change_hints_complete,
                });
            }
        }
        if deleted_relays == relays.len() {
            bail!("group has been deleted")
        }
        bail!("no relay could hold the conversation watch")
    }

    async fn publish_group_presence(
        &self,
        group: &GroupMembership,
        identity: &Identity,
        relays: &[RelayDescriptor],
        active: bool,
    ) -> usize {
        let ttl = if active {
            GROUP_PRESENCE_TTL_MILLIS
        } else {
            IDLE_GROUP_PRESENCE_TTL_MILLIS
        };
        let Ok(presence) =
            GroupPresence::create(identity, group, current_millis().saturating_add(ttl))
        else {
            return 0;
        };
        let Ok(body) = serde_json::to_vec(&presence) else {
            return 0;
        };
        let endpoint = format!("/v1/groups/{}/presence", group.group_id);
        let mut accepted = 0usize;
        for index in 0..relays.len() {
            if let Ok(response) = self
                .relay_request(relays, index, "POST", &endpoint, &body)
                .await
                && (200..300).contains(&response.status)
            {
                accepted += 1;
            }
        }
        accepted
    }

    async fn watch_direct_id(
        &self,
        id: &str,
        direct_mailboxes: &[(String, GroupMembership)],
        since: Option<u64>,
        relays: Vec<RelayDescriptor>,
    ) -> anyhow::Result<GroupWatch> {
        let revision = since
            .map(|revision| revision.to_string())
            .unwrap_or_else(|| "initial".to_owned());
        let endpoint = format!("/v1/groups/{id}/watch/{revision}");
        for index in 0..relays.len() {
            let Ok(response) = self
                .relay_request(&relays, index, "GET", &endpoint, &[])
                .await
            else {
                continue;
            };
            if (200..300).contains(&response.status)
                && let Ok(change) = serde_json::from_slice::<RelayGroupWatch>(&response.body)
            {
                let now = current_millis();
                let mut online_public_keys = Vec::new();
                let mut recently_active_public_keys = Vec::new();
                for presence in change.presences {
                    let Some(public_key) =
                        direct_mailboxes
                            .iter()
                            .find_map(|(expected_public_key, mailbox)| {
                                presence
                                    .open(mailbox)
                                    .ok()
                                    .filter(|opened| opened == expected_public_key)
                            })
                    else {
                        continue;
                    };
                    if presence.expires_at_millis
                        > now.saturating_add(ONLINE_PRESENCE_REMAINING_MILLIS)
                    {
                        online_public_keys.push(public_key);
                    } else if presence
                        .expires_at_millis
                        .saturating_add(RECENT_GROUP_PRESENCE_MILLIS)
                        > now
                    {
                        recently_active_public_keys.push(public_key);
                    }
                }
                online_public_keys.sort();
                online_public_keys.dedup();
                recently_active_public_keys.sort();
                recently_active_public_keys.dedup();
                return Ok(GroupWatch {
                    revision: change.revision,
                    changed: change.changed,
                    deleted: false,
                    online_public_keys,
                    recently_active_public_keys,
                    changed_stream_locators: change.changed_stream_locators,
                    control_changed: change.control_changed,
                    change_hints_complete: change.change_hints_complete,
                });
            }
        }
        bail!("no relay could hold the conversation watch")
    }

    pub async fn update_profile(
        &self,
        path: impl AsRef<Path>,
        username: impl Into<String>,
        bio: impl Into<String>,
        avatar_data_base64: Option<String>,
        avatar_mime_type: Option<String>,
        remove_avatar: bool,
        direct_message_policy: DirectMessagePolicy,
        relays: Vec<String>,
    ) -> anyhow::Result<LocalSummary> {
        let path = path.as_ref();
        let mut state = load_state(path)?;
        let username = username.into().trim().to_owned();
        validate_display_name(&username)?;
        let bio = bio.into().trim().to_owned();
        if bio.chars().count() > 160 {
            bail!("bios can contain at most 160 characters")
        }
        let relays = relay_list(relays)?;
        let avatar = if remove_avatar {
            None
        } else if let Some(encoded) = avatar_data_base64 {
            let mime_type = avatar_mime_type.context("avatar media type is missing")?;
            if !matches!(
                mime_type.as_str(),
                "image/jpeg" | "image/png" | "image/webp"
            ) {
                bail!("avatar must be a JPEG, PNG, or WebP image")
            }
            let data = STANDARD
                .decode(encoded)
                .context("avatar image encoding is invalid")?;
            if data.is_empty() || data.len() > 256 * 1024 {
                bail!("avatar images must contain between 1 byte and 256 KiB")
            }
            let (blob, key_base64) = EncryptedBlob::create(&data)?;
            let storage = self.store_blob_shards(&relays, &blob, &key_base64).await?;
            Some(ProfileImage {
                blob_id: blob.blob_id,
                key_base64,
                mime_type,
                byte_length: data.len() as u32,
                storage: Some(storage),
            })
        } else {
            state.profile.avatar.clone()
        };

        let previous_direct_message_policy = state.profile.effective_direct_message_policy();
        let accepts_direct_messages = direct_message_policy != DirectMessagePolicy::Nobody;
        if previous_direct_message_policy != DirectMessagePolicy::Nobody
            && direct_message_policy == DirectMessagePolicy::Nobody
        {
            state.direct_closed_periods.push(DirectClosedPeriod {
                closed_at_millis: current_millis(),
                reopened_at_millis: None,
            });
        } else if previous_direct_message_policy == DirectMessagePolicy::Nobody
            && direct_message_policy != DirectMessagePolicy::Nobody
            && let Some(period) = state
                .direct_closed_periods
                .iter_mut()
                .rev()
                .find(|period| period.reopened_at_millis.is_none())
        {
            period.reopened_at_millis = Some(current_millis());
        }
        state.profile.username = username;
        state.profile.bio = bio;
        state.profile.avatar = avatar;
        state.profile.accepts_direct_messages = accepts_direct_messages;
        state.profile.direct_message_policy = direct_message_policy;
        state.profile_sequence = state.take_sequence();
        let identity = state.identity()?;
        let mut profile_events = Vec::with_capacity(state.groups.len());
        for group in state.groups.clone() {
            let sequence = state.take_sequence();
            let event = create_group_event(
                &state,
                &identity,
                &group,
                GroupEventPayload::ProfileUpdated {
                    profile: state.profile.clone(),
                },
                sequence,
            )?;
            profile_events.push((group.group_id, event));
        }
        // Persist the profile and consumed sequences before publication. This
        // prevents a partial multi-group publication from reverting the local
        // profile or reusing a sequence for different event bytes.
        save_state(path, &state)?;
        let mut publications = FuturesUnordered::new();
        for (group_id, event) in &profile_events {
            publications.push(async {
                (
                    group_id.clone(),
                    self.publish_event_diagnostic(&relays, event).await,
                )
            });
        }
        let mut failures = Vec::new();
        while let Some((group_id, result)) = publications.next().await {
            if let Err(failure) = result {
                if failure.all_relays_deleted {
                    state.tombstone_group(&group_id);
                } else {
                    failures.push(failure.to_string());
                }
            }
        }
        let _ = self
            .ensure_contact_signal_published(&mut state, &relays)
            .await;
        save_state(path, &state)?;
        if !failures.is_empty() {
            bail!("{}", failures.join("; "))
        }
        state.summary()
    }

    async fn open_profile_album(
        &self,
        public_key: &str,
        album: &ProfileAlbum,
    ) -> anyhow::Result<StoredProfileAlbum> {
        validate_profile_album_reference(album)?;
        let storage = album
            .storage
            .as_ref()
            .context("this album is no longer available from the storage network")?;
        let blob = self.reconstruct_blob(storage, &album.key_base64).await?;
        if blob.group_id.is_some() {
            bail!("profile album metadata has an invalid scope")
        }
        let plaintext = blob.open(&album.key_base64)?;
        if plaintext.len() != album.byte_length as usize {
            bail!("profile album metadata does not match its reference")
        }
        let stored: StoredProfileAlbum =
            serde_json::from_slice(&plaintext).context("profile album metadata is invalid")?;
        if stored.version != 1
            || stored.owner_public_key != public_key
            || stored.items.len() != usize::from(album.item_count)
        {
            bail!("profile album metadata is invalid")
        }
        validate_profile_album_items(&stored.items)?;
        Ok(stored)
    }

    pub async fn fetch_profile_album(
        &self,
        path: impl AsRef<Path>,
        public_key: &str,
        album: &ProfileAlbum,
        relays: Vec<String>,
    ) -> anyhow::Result<ProfileAlbumData> {
        let state = load_state(path.as_ref())?;
        let identity = state.identity()?;
        let self_public_key = identity.public_key_base64();
        let expected = if public_key == self_public_key {
            state.profile.album.as_ref()
        } else {
            state
                .direct_contacts
                .iter()
                .chain(state.known_people.iter())
                .find(|person| person.public_key == public_key && !state.is_hidden(public_key))
                .and_then(|person| person.album.as_ref())
        }
        .context("this album does not belong to a known profile")?;
        if expected != album {
            bail!("this album reference is out of date")
        }
        let _relays = relay_list(relays)?;
        let stored = self.open_profile_album(public_key, album).await?;
        Ok(ProfileAlbumData {
            scope_id: profile_media_scope_id(public_key)?,
            items: stored.items,
        })
    }

    pub async fn update_profile_album(
        &self,
        path: impl AsRef<Path>,
        items: Vec<ProfileAlbumItem>,
        relays: Vec<String>,
    ) -> anyhow::Result<LocalSummary> {
        validate_profile_album_items(&items)?;
        let path = path.as_ref();
        let mut state = load_state(path)?;
        let identity = state.identity()?;
        let self_public_key = identity.public_key_base64();
        let relays = relay_list(relays)?;
        let old_album = state.profile.album.clone();
        let old_items = if items.is_empty() {
            // Clearing an album must remain possible even when its metadata
            // predates the current streaming storage format. The signed
            // profile update removes the reference; legacy item blobs can
            // expire from storage independently.
            Vec::new()
        } else if let Some(album) = old_album.as_ref() {
            self.open_profile_album(&self_public_key, album)
                .await?
                .items
        } else {
            Vec::new()
        };
        let new_album = if items.is_empty() {
            None
        } else {
            let stored = StoredProfileAlbum {
                version: 1,
                owner_public_key: self_public_key.clone(),
                items: items.clone(),
            };
            let plaintext = serde_json::to_vec(&stored)?;
            if plaintext.len() > 2 * 1024 * 1024 {
                bail!("this album has too much media metadata")
            }
            let (blob, key_base64) = EncryptedBlob::create(&plaintext)?;
            let storage = self.store_blob_shards(&relays, &blob, &key_base64).await?;
            Some(ProfileAlbum {
                blob_id: blob.blob_id,
                key_base64,
                byte_length: plaintext.len() as u32,
                item_count: items.len() as u16,
                storage: Some(storage),
            })
        };
        state.profile.album = new_album.clone();
        state.profile_sequence = state.take_sequence();
        let mut profile_events = Vec::with_capacity(state.groups.len());
        for group in state.groups.clone() {
            let sequence = state.take_sequence();
            let event = create_group_event(
                &state,
                &identity,
                &group,
                GroupEventPayload::ProfileUpdated {
                    profile: state.profile.clone(),
                },
                sequence,
            )?;
            profile_events.push((group.group_id, event));
        }
        save_state(path, &state)?;
        let mut publications = FuturesUnordered::new();
        for (group_id, event) in &profile_events {
            publications.push(async {
                (
                    group_id.clone(),
                    self.publish_event_diagnostic(&relays, event).await,
                )
            });
        }
        let mut failures = Vec::new();
        while let Some((group_id, result)) = publications.next().await {
            if let Err(failure) = result {
                if failure.all_relays_deleted {
                    state.tombstone_group(&group_id);
                } else {
                    failures.push(failure.to_string());
                }
            }
        }
        let _ = self
            .ensure_contact_signal_published(&mut state, &relays)
            .await;
        save_state(path, &state)?;
        if !failures.is_empty() {
            bail!("{}", failures.join("; "))
        }

        let retained = items
            .iter()
            .map(|item| item.id.as_str())
            .collect::<HashSet<_>>();
        let mut cleanup = old_items
            .iter()
            .filter(|item| !retained.contains(item.id.as_str()))
            .flat_map(|item| attachment_storage_references(&item.attachment))
            .collect::<Vec<_>>();
        cleanup.extend(old_album.as_ref().and_then(profile_album_storage_reference));
        if !cleanup.is_empty() {
            let _ = self.erase_storage_references(cleanup, false).await;
        }
        Ok(state.summary()?)
    }

    pub async fn upload_profile_media_chunk(
        &self,
        path: impl AsRef<Path>,
        data_base64: String,
        relays: Vec<String>,
    ) -> anyhow::Result<MediaChunk> {
        let state = load_state(path.as_ref())?;
        let scope_id = profile_media_scope_id(&state.identity()?.public_key_base64())?;
        let data = STANDARD
            .decode(data_base64)
            .context("media chunk encoding is invalid")?;
        if data.is_empty() || data.len() > 1024 * 1024 {
            bail!("media chunks must contain between 1 byte and 1 MiB")
        }
        let (blob, key_base64) = EncryptedBlob::create_for_group(&data, scope_id)?;
        let storage = self
            .store_blob_shards(&relay_list(relays)?, &blob, &key_base64)
            .await?;
        Ok(MediaChunk {
            blob_id: blob.blob_id,
            key_base64,
            byte_length: data.len() as u32,
            storage: Some(storage),
        })
    }

    pub async fn update_group_profile(
        &self,
        path: impl AsRef<Path>,
        name: impl Into<String>,
        description: impl Into<String>,
        rules: impl Into<String>,
        content_rating: Option<GroupContentRating>,
        avatar_data_base64: Option<String>,
        avatar_mime_type: Option<String>,
        remove_avatar: bool,
        background_data_base64: Option<String>,
        background_mime_type: Option<String>,
        remove_background: bool,
        mobile_background_data_base64: Option<String>,
        mobile_background_mime_type: Option<String>,
        remove_mobile_background: bool,
        accent_color: Option<String>,
        members_can_send_messages: Option<bool>,
        members_can_send_media: Option<bool>,
        relays: Vec<String>,
    ) -> anyhow::Result<LocalSummary> {
        let path = path.as_ref();
        let mut state = load_state(path)?;
        let name = name.into().trim().to_owned();
        let description = description.into().trim().to_owned();
        let rules = normalize_group_rules(rules.into())?;
        if name.is_empty() || name.chars().count() > 80 {
            bail!("group names must contain between 1 and 80 characters")
        }
        if description.chars().count() > 200 {
            bail!("group descriptions can contain at most 200 characters")
        }
        let relays = relay_list(relays)?;
        let group_id = state.active_group()?.group_id.clone();
        let group_index = state
            .groups
            .iter()
            .position(|group| group.group_id == group_id)
            .context("active group is missing from local state")?;
        let current_group = state.groups[group_index].clone();
        let identity = state.identity()?;
        let (current_profile, owner_public_key) = if current_group.owner_public_key.is_empty() {
            // Legacy memberships did not persist the founder or complete profile,
            // so they still need one reconciliation before they can be edited.
            let events = self.fetch_events(&current_group, relays.clone()).await?;
            let view = rebuild_group_state(&state, &current_group, &events)?;
            let owner_public_key = view
                .owner_public_key
                .clone()
                .context("group founder is missing")?;
            (view.profile, owner_public_key)
        } else {
            (
                GroupProfile {
                    name: current_group.name.clone(),
                    description: current_group.description.clone(),
                    rules: current_group.rules.clone(),
                    content_rating: current_group.content_rating,
                    avatar: current_group.avatar.clone(),
                    background: current_group.background.clone(),
                    mobile_background: current_group.mobile_background.clone(),
                    mobile_background_updated: true,
                    accent_color: current_group.accent_color.clone(),
                    members_can_send_messages: current_group.members_can_send_messages,
                    members_can_send_media: current_group.members_can_send_media,
                },
                current_group.owner_public_key.clone(),
            )
        };
        if owner_public_key != identity.public_key_base64() {
            bail!("only the group founder can edit its identity right now")
        }
        let members_can_send_messages =
            members_can_send_messages.unwrap_or(current_profile.members_can_send_messages);
        let members_can_send_media =
            members_can_send_media.unwrap_or(current_profile.members_can_send_media);
        let content_rating = content_rating.unwrap_or(current_profile.content_rating);
        let became_explicit = current_profile.content_rating == GroupContentRating::General
            && content_rating == GroupContentRating::Explicit;
        if current_profile.content_rating == GroupContentRating::Explicit
            && content_rating != GroupContentRating::Explicit
        {
            bail!("an explicit group cannot be changed back to general")
        }
        if content_rating == GroupContentRating::Explicit
            && !state.adult_access.explicit_content_enabled
        {
            bail!("enable explicit groups in settings before marking this group explicit")
        }
        let accent_color = normalize_group_accent_color(
            accent_color.unwrap_or_else(|| current_profile.accent_color.clone()),
        )?;

        let avatar = if remove_avatar {
            None
        } else if let Some(encoded) = avatar_data_base64 {
            let mime_type = avatar_mime_type.context("group icon media type is missing")?;
            if !matches!(
                mime_type.as_str(),
                "image/jpeg" | "image/png" | "image/webp"
            ) {
                bail!("group icons must be JPEG, PNG, or WebP images")
            }
            let data = STANDARD
                .decode(encoded)
                .context("group icon encoding is invalid")?;
            if data.is_empty() || data.len() > 256 * 1024 {
                bail!("group icons must contain between 1 byte and 256 KiB")
            }
            let (blob, key_base64) =
                EncryptedBlob::create_for_group(&data, current_group.group_id.clone())?;
            let storage = self.store_blob_shards(&relays, &blob, &key_base64).await?;
            Some(ProfileImage {
                blob_id: blob.blob_id,
                key_base64,
                mime_type,
                byte_length: data.len() as u32,
                storage: Some(storage),
            })
        } else {
            current_profile.avatar.clone()
        };

        let background = if remove_background {
            None
        } else if let Some(encoded) = background_data_base64 {
            let mime_type =
                background_mime_type.context("group background media type is missing")?;
            if !matches!(
                mime_type.as_str(),
                "image/jpeg" | "image/png" | "image/webp"
            ) {
                bail!("group backgrounds must be JPEG, PNG, or WebP images")
            }
            let data = STANDARD
                .decode(encoded)
                .context("group background encoding is invalid")?;
            if data.is_empty() || data.len() > 1536 * 1024 {
                bail!("group backgrounds must contain between 1 byte and 1.5 MiB")
            }
            let (blob, key_base64) =
                EncryptedBlob::create_for_group(&data, current_group.group_id.clone())?;
            let storage = self.store_blob_shards(&relays, &blob, &key_base64).await?;
            Some(ProfileImage {
                blob_id: blob.blob_id,
                key_base64,
                mime_type,
                byte_length: data.len() as u32,
                storage: Some(storage),
            })
        } else {
            current_profile.background.clone()
        };

        let mobile_background = if remove_mobile_background {
            None
        } else if let Some(encoded) = mobile_background_data_base64 {
            let mime_type = mobile_background_mime_type
                .context("mobile group background media type is missing")?;
            if !matches!(
                mime_type.as_str(),
                "image/jpeg" | "image/png" | "image/webp"
            ) {
                bail!("mobile group backgrounds must be JPEG, PNG, or WebP images")
            }
            let data = STANDARD
                .decode(encoded)
                .context("mobile group background encoding is invalid")?;
            if data.is_empty() || data.len() > 1536 * 1024 {
                bail!("mobile group backgrounds must contain between 1 byte and 1.5 MiB")
            }
            let (blob, key_base64) =
                EncryptedBlob::create_for_group(&data, current_group.group_id.clone())?;
            let storage = self.store_blob_shards(&relays, &blob, &key_base64).await?;
            Some(ProfileImage {
                blob_id: blob.blob_id,
                key_base64,
                mime_type,
                byte_length: data.len() as u32,
                storage: Some(storage),
            })
        } else {
            current_profile.mobile_background.clone()
        };

        let resolved_profile = GroupProfile {
            name,
            description,
            rules,
            content_rating,
            avatar,
            background,
            mobile_background,
            mobile_background_updated: true,
            accent_color,
            members_can_send_messages,
            members_can_send_media,
        };
        state.apply_resolved_group_profile(
            &current_group.group_id,
            &resolved_profile,
            &owner_public_key,
        );
        let group = state.groups[group_index].clone();
        if became_explicit {
            if group.authority_nonce_base64.is_empty() {
                bail!("this legacy group cannot safely revoke its unlabeled invitation")
            }
            let new_frequency = generate_frequency();
            let new_invite = InviteRecord::create(&identity, &new_frequency, group.clone())?;
            let rotation_sequence = state.take_sequence();
            let rotation =
                InviteRotation::create(&identity, &group, Some(new_invite), rotation_sequence)?;
            self.publish_invite_rotation(&relays, &rotation).await?;
            state
                .group_frequencies
                .insert(group.group_id.clone(), new_frequency);
            // Persist the newly rotated invitation before publishing the
            // profile event. If that later write is interrupted, retrying can
            // finish the signed profile change without reviving the old,
            // unlabeled invitation.
            save_state(path, &state)?;
        }
        let sequence = state.take_sequence();
        let event = create_group_event(
            &state,
            &identity,
            &group,
            GroupEventPayload::GroupProfileUpdated {
                profile: resolved_profile,
            },
            sequence,
        )?;
        self.publish_event(&relays, &event).await?;
        save_state(path, &state)?;
        state.summary()
    }

    pub async fn rotate_frequency(
        &self,
        path: impl AsRef<Path>,
        revoke_only: bool,
        relays: Vec<String>,
    ) -> anyhow::Result<LocalSummary> {
        let path = path.as_ref();
        let mut state = load_state(path)?;
        let group = state.active_group()?.clone();
        let identity = state.identity()?;
        if group.owner_public_key != identity.public_key_base64() {
            bail!("only the group founder can rotate its frequency")
        }
        if group.authority_nonce_base64.is_empty() {
            bail!("this older group cannot authenticate frequency rotation")
        }
        let new_frequency = (!revoke_only).then(generate_frequency);
        let new_invite = new_frequency
            .as_ref()
            .map(|frequency| InviteRecord::create(&identity, frequency, group.clone()))
            .transpose()?;
        let sequence = state.take_sequence();
        let rotation = InviteRotation::create(&identity, &group, new_invite, sequence)?;
        self.publish_invite_rotation(&relay_list(relays)?, &rotation)
            .await?;
        if let Some(frequency) = new_frequency {
            state
                .group_frequencies
                .insert(group.group_id.clone(), frequency);
        } else {
            state.group_frequencies.remove(&group.group_id);
        }
        save_state(path, &state)?;
        state.summary()
    }

    pub async fn fetch_avatar(
        &self,
        cache_path: impl AsRef<Path>,
        image: &ProfileImage,
        relays: Vec<String>,
    ) -> anyhow::Result<AvatarData> {
        let _relays = relay_list(relays)?;
        if image.byte_length == 0 || image.byte_length > 1536 * 1024 {
            bail!("image reference has an invalid size")
        }
        if image.blob_id.len() != 64 || !image.blob_id.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            bail!("image reference has an invalid blob identifier")
        }
        let cache_directory = cache_path.as_ref().join("profile-blobs");
        let file_path = cache_directory.join(format!("{}.json", image.blob_id));
        #[cfg(not(target_arch = "wasm32"))]
        let cached_blob = fs::read(&file_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<EncryptedBlob>(&bytes).ok())
            .filter(|blob| blob.blob_id == image.blob_id && blob.verify().is_ok());
        #[cfg(target_arch = "wasm32")]
        let cached_blob: Option<EncryptedBlob> = None;
        let blob = if let Some(blob) = cached_blob {
            blob
        } else {
            let storage = image
                .storage
                .as_ref()
                .context("this image predates constellation storage and is no longer available")?;
            let blob = self.reconstruct_blob(storage, &image.key_base64).await?;
            #[cfg(not(target_arch = "wasm32"))]
            {
                fs::create_dir_all(&cache_directory)
                    .context("could not create the profile image cache")?;
                let temporary = file_path.with_extension("json.part");
                fs::write(&temporary, serde_json::to_vec(&blob)?)
                    .context("could not write the profile image cache")?;
                if file_path.exists() {
                    fs::remove_file(&file_path)
                        .context("could not replace an invalid profile image cache entry")?;
                }
                fs::rename(&temporary, &file_path)
                    .context("could not finish the profile image cache entry")?;
            }
            blob
        };
        let data = blob.open(&image.key_base64)?;
        if data.len() != image.byte_length as usize {
            bail!("avatar data does not match its profile reference")
        }
        Ok(AvatarData {
            mime_type: image.mime_type.clone(),
            data_base64: STANDARD.encode(data),
        })
    }

    pub async fn fetch_attachment(
        &self,
        path: impl AsRef<Path>,
        cache_path: impl AsRef<Path>,
        scope_id: Option<String>,
        attachment: &MediaAttachment,
        relays: Vec<String>,
    ) -> anyhow::Result<AttachmentData> {
        validate_media_reference(attachment)?;
        let state = load_state(path.as_ref())?;
        let scope_id = resolve_media_scope(&state, scope_id)?;
        let _relays = relay_list(relays)?;
        #[cfg(target_arch = "wasm32")]
        {
            let mut output = Vec::with_capacity(attachment.byte_length as usize);
            let mut chunks =
                futures_util::stream::iter(attachment.chunks.iter().cloned().map(|chunk| {
                    let scope_id = scope_id.clone();
                    async move { self.fetch_attachment_chunk(&scope_id, &chunk).await }
                }))
                .buffered(MEDIA_CHUNK_DOWNLOAD_CONCURRENCY);
            while let Some(plaintext) = chunks.next().await {
                output.extend_from_slice(&plaintext?);
            }
            if output.len() as u64 != attachment.byte_length {
                bail!("media does not match its manifest")
            }
            return Ok(AttachmentData {
                mime_type: attachment.mime_type.clone(),
                file_path: format!(
                    "data:{};base64,{}",
                    attachment.mime_type,
                    STANDARD.encode(output)
                ),
            });
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let cache_directory = cache_path.as_ref().join("media").join(&scope_id);
            fs::create_dir_all(&cache_directory).context("could not create the media cache")?;
            let extension = media_extension(&attachment.mime_type);
            let file_path =
                cache_directory.join(format!("{}.{}", attachment.chunks[0].blob_id, extension));
            if file_path
                .metadata()
                .is_ok_and(|metadata| metadata.len() == attachment.byte_length)
            {
                return Ok(AttachmentData {
                    mime_type: attachment.mime_type.clone(),
                    file_path: file_path.to_string_lossy().into_owned(),
                });
            }
            let temporary = file_path.with_extension(format!("{extension}.part"));
            let mut output =
                fs::File::create(&temporary).context("could not create media cache file")?;
            let mut chunks = futures_util::stream::iter(attachment.chunks.iter().cloned())
                .map(|chunk| {
                    let scope_id = scope_id.clone();
                    let cache_path = cache_path.as_ref().to_owned();
                    async move {
                        self.fetch_attachment_chunk(&cache_path, &scope_id, &chunk)
                            .await
                    }
                })
                .buffered(MEDIA_CHUNK_DOWNLOAD_CONCURRENCY);
            while let Some(plaintext) = chunks.next().await {
                output
                    .write_all(&plaintext?)
                    .context("could not write media cache file")?;
            }
            output
                .flush()
                .context("could not finish media cache file")?;
            drop(output);
            if temporary.metadata()?.len() != attachment.byte_length {
                let _ = fs::remove_file(&temporary);
                bail!("media does not match its manifest")
            }
            fs::rename(&temporary, &file_path).context("could not finish media cache file")?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&file_path, fs::Permissions::from_mode(0o600))?;
            }
            Ok(AttachmentData {
                mime_type: attachment.mime_type.clone(),
                file_path: file_path.to_string_lossy().into_owned(),
            })
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub async fn fetch_attachment_range(
        &self,
        path: impl AsRef<Path>,
        cache_path: impl AsRef<Path>,
        scope_id: Option<String>,
        attachment: &MediaAttachment,
        offset: u64,
        byte_length: u64,
        relays: Vec<String>,
    ) -> anyhow::Result<AttachmentRangeData> {
        validate_media_reference(attachment)?;
        if byte_length == 0 || byte_length > MAX_MEDIA_RANGE_BYTES {
            bail!("media range length is invalid")
        }
        if offset >= attachment.byte_length {
            bail!("media range starts beyond the attachment")
        }
        let state = load_state(path.as_ref())?;
        let scope_id = resolve_media_scope(&state, scope_id)?;
        let _relays = relay_list(relays)?;
        let requested_end = offset
            .saturating_add(byte_length)
            .min(attachment.byte_length);
        let extension = media_extension(&attachment.mime_type);
        let complete_file = cache_path
            .as_ref()
            .join("media")
            .join(&scope_id)
            .join(format!("{}.{}", attachment.chunks[0].blob_id, extension));
        if complete_file
            .metadata()
            .is_ok_and(|metadata| metadata.len() == attachment.byte_length)
        {
            let mut file =
                fs::File::open(&complete_file).context("could not read cached media range")?;
            file.seek(SeekFrom::Start(offset))
                .context("could not seek cached media")?;
            let mut output = vec![0_u8; (requested_end - offset) as usize];
            file.read_exact(&mut output)
                .context("could not read cached media range")?;
            return Ok(AttachmentRangeData {
                mime_type: attachment.mime_type.clone(),
                data_base64: STANDARD.encode(&output),
                offset,
                byte_length: output.len() as u64,
                total_byte_length: attachment.byte_length,
            });
        }
        let selected = select_media_chunks(attachment, offset, requested_end);

        // Tauri WebKit consumes custom-protocol ranges mostly serially. Fill a
        // multi-chunk lookahead while it plays the current response so the
        // following request is served from the decrypted cache instead of
        // stalling on another relay round trip.
        if let Some((index, _, _)) = selected.last() {
            let lookahead = attachment
                .chunks
                .iter()
                .skip(index + 1)
                .take(4)
                .cloned()
                .collect::<Vec<_>>();
            let client = self.clone();
            let cache_path = cache_path.as_ref().to_owned();
            let scope_id = scope_id.clone();
            tokio::spawn(async move {
                let mut downloads =
                    futures_util::stream::iter(lookahead.into_iter().map(|chunk| {
                        let cache_path = cache_path.clone();
                        let scope_id = scope_id.clone();
                        let client = client.clone();
                        async move {
                            client
                                .fetch_attachment_chunk(&cache_path, &scope_id, &chunk)
                                .await
                        }
                    }))
                    .buffer_unordered(4);
                while downloads.next().await.is_some() {}
            });
        }

        let mut downloaded =
            futures_util::stream::iter(selected.into_iter().map(|(index, start, chunk)| {
                let cache_path = cache_path.as_ref().to_owned();
                let scope_id = scope_id.clone();
                async move {
                    let plaintext = self
                        .fetch_attachment_chunk(&cache_path, &scope_id, &chunk)
                        .await?;
                    Ok::<_, anyhow::Error>((index, start, plaintext))
                }
            }))
            .buffer_unordered(MEDIA_CHUNK_DOWNLOAD_CONCURRENCY);
        let mut chunks = Vec::new();
        while let Some(chunk) = downloaded.next().await {
            chunks.push(chunk?);
        }
        chunks.sort_by_key(|(index, _, _)| *index);

        let mut output = Vec::with_capacity((requested_end - offset) as usize);
        for (_, start, plaintext) in chunks {
            let end = start + plaintext.len() as u64;
            let slice_start = offset.saturating_sub(start) as usize;
            let slice_end = (requested_end.min(end) - start) as usize;
            output.extend_from_slice(&plaintext[slice_start..slice_end]);
        }
        if output.len() as u64 != requested_end - offset {
            bail!("media range does not match its manifest")
        }
        Ok(AttachmentRangeData {
            mime_type: attachment.mime_type.clone(),
            data_base64: STANDARD.encode(&output),
            offset,
            byte_length: output.len() as u64,
            total_byte_length: attachment.byte_length,
        })
    }

    #[cfg(not(target_arch = "wasm32"))]
    async fn fetch_attachment_chunk(
        &self,
        cache_path: &Path,
        scope_id: &str,
        chunk: &MediaChunk,
    ) -> anyhow::Result<Vec<u8>> {
        let cache_directory = cache_path.join("media-chunks").join(scope_id);
        fs::create_dir_all(&cache_directory).context("could not create media chunk cache")?;
        let file_path = cache_directory.join(format!("{}.chunk", chunk.blob_id));
        loop {
            if let Ok(bytes) = fs::read(&file_path)
                && bytes.len() == chunk.byte_length as usize
            {
                return Ok(bytes);
            }

            let mut follower = {
                let mut active = active_media_chunk_downloads()
                    .lock()
                    .map_err(|_| anyhow::anyhow!("media download registry is unavailable"))?;
                if let Some(download) = active.get(&file_path) {
                    Some(download.subscribe())
                } else {
                    let (download, _) = tokio::sync::watch::channel(false);
                    active.insert(file_path.clone(), download);
                    None
                }
            };
            let Some(follower) = follower.as_mut() else {
                break;
            };
            let _ = follower.wait_for(|finished| *finished).await;
        }

        let result = async {
            let storage = chunk
                .storage
                .as_ref()
                .context("this media predates constellation storage and is no longer available")?;
            let blob = self.reconstruct_blob(storage, &chunk.key_base64).await?;
            if blob.group_id.as_deref() != Some(scope_id) {
                bail!("media chunk belongs to a different conversation")
            }
            let plaintext = blob.open(&chunk.key_base64)?;
            if plaintext.len() != chunk.byte_length as usize {
                bail!("media chunk does not match its manifest")
            }

            static TEMPORARY_ID: AtomicU64 = AtomicU64::new(1);
            let temporary = cache_directory.join(format!(
                "{}.{}.part",
                chunk.blob_id,
                TEMPORARY_ID.fetch_add(1, Ordering::Relaxed)
            ));
            fs::write(&temporary, &plaintext).context("could not cache decrypted media chunk")?;
            if let Err(error) = fs::rename(&temporary, &file_path) {
                if !file_path
                    .metadata()
                    .is_ok_and(|metadata| metadata.len() == u64::from(chunk.byte_length))
                {
                    let _ = fs::remove_file(&temporary);
                    return Err(error).context("could not finish decrypted media chunk cache");
                }
                let _ = fs::remove_file(&temporary);
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&file_path, fs::Permissions::from_mode(0o600))?;
            }
            Ok(plaintext)
        }
        .await;

        if let Ok(mut active) = active_media_chunk_downloads().lock()
            && let Some(download) = active.remove(&file_path)
        {
            let _ = download.send(true);
        }
        result
    }

    #[cfg(target_arch = "wasm32")]
    pub async fn fetch_attachment_range(
        &self,
        path: impl AsRef<Path>,
        _cache_path: impl AsRef<Path>,
        scope_id: Option<String>,
        attachment: &MediaAttachment,
        offset: u64,
        byte_length: u64,
        relays: Vec<String>,
    ) -> anyhow::Result<AttachmentRangeData> {
        validate_media_reference(attachment)?;
        if byte_length == 0 || byte_length > MAX_MEDIA_RANGE_BYTES {
            bail!("media range length is invalid")
        }
        if offset >= attachment.byte_length {
            bail!("media range starts beyond the attachment")
        }
        let state = load_state(path.as_ref())?;
        let scope_id = resolve_media_scope(&state, scope_id)?;
        let _relays = relay_list(relays)?;
        let requested_end = offset
            .saturating_add(byte_length)
            .min(attachment.byte_length);
        let selected = select_media_chunks(attachment, offset, requested_end);

        // Browsers usually ask for the first two media chunks sequentially
        // before playback starts. Warm the next chunk alongside the current
        // request so the second range can join the same in-flight download or
        // read it directly from cache.
        if let Some((index, _, _)) = selected.last()
            && let Some(chunk) = attachment.chunks.get(index + 1).cloned()
        {
            let client = self.clone();
            let scope_id = scope_id.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let _ = client.fetch_attachment_chunk(&scope_id, &chunk).await;
            });
        }

        let mut downloaded =
            futures_util::stream::iter(selected.into_iter().map(|(index, start, chunk)| {
                let scope_id = scope_id.clone();
                async move {
                    let plaintext = self.fetch_attachment_chunk(&scope_id, &chunk).await?;
                    Ok::<_, anyhow::Error>((index, start, plaintext))
                }
            }))
            .buffer_unordered(MEDIA_CHUNK_DOWNLOAD_CONCURRENCY);
        let mut chunks = Vec::new();
        while let Some(chunk) = downloaded.next().await {
            chunks.push(chunk?);
        }
        chunks.sort_by_key(|(index, _, _)| *index);

        let mut output = Vec::with_capacity((requested_end - offset) as usize);
        for (_, start, plaintext) in chunks {
            let end = start + plaintext.len() as u64;
            let slice_start = offset.saturating_sub(start) as usize;
            let slice_end = (requested_end.min(end) - start) as usize;
            output.extend_from_slice(&plaintext[slice_start..slice_end]);
        }
        if output.len() as u64 != requested_end - offset {
            bail!("media range does not match its manifest")
        }
        Ok(AttachmentRangeData {
            mime_type: attachment.mime_type.clone(),
            data_base64: STANDARD.encode(&output),
            offset,
            byte_length: output.len() as u64,
            total_byte_length: attachment.byte_length,
        })
    }

    /// Browser media chunks live in a memory LRU instead of the disk cache the
    /// native build uses. Concurrent range requests for the same chunk join a
    /// single in-flight download through the shared future registry.
    #[cfg(target_arch = "wasm32")]
    async fn fetch_attachment_chunk(
        &self,
        scope_id: &str,
        chunk: &MediaChunk,
    ) -> anyhow::Result<Rc<Vec<u8>>> {
        if let Some(cached) = WEB_MEDIA_CHUNKS.with(|cache| {
            cache
                .borrow_mut()
                .get(&chunk.blob_id, u64::from(chunk.byte_length))
        }) {
            return Ok(cached);
        }

        let pending = WEB_MEDIA_CHUNK_DOWNLOADS
            .with(|downloads| downloads.borrow().get(&chunk.blob_id).cloned());
        let download = match pending {
            Some(download) => download,
            None => {
                let client = self.clone();
                let scope_id = scope_id.to_owned();
                let chunk = chunk.clone();
                let download: WebChunkDownload = async move {
                    let Some(storage) = chunk.storage.as_ref() else {
                        return Err(
                            "this media predates constellation storage and is no longer available"
                                .to_owned(),
                        );
                    };
                    let blob = client
                        .reconstruct_blob(storage, &chunk.key_base64)
                        .await
                        .map_err(|error| error.to_string())?;
                    if blob.group_id.as_deref() != Some(scope_id.as_str()) {
                        return Err("media chunk belongs to a different conversation".to_owned());
                    }
                    let plaintext = blob
                        .open(&chunk.key_base64)
                        .map_err(|error| error.to_string())?;
                    if plaintext.len() != chunk.byte_length as usize {
                        return Err("media chunk does not match its manifest".to_owned());
                    }
                    Ok(Rc::new(plaintext))
                }
                .boxed_local()
                .shared();
                WEB_MEDIA_CHUNK_DOWNLOADS.with(|downloads| {
                    downloads
                        .borrow_mut()
                        .insert(chunk.blob_id.clone(), download.clone());
                });
                download
            }
        };

        let plaintext = download.await.map_err(anyhow::Error::msg)?;
        WEB_MEDIA_CHUNK_DOWNLOADS.with(|downloads| {
            downloads.borrow_mut().remove(&chunk.blob_id);
        });
        WEB_MEDIA_CHUNKS.with(|cache| {
            cache
                .borrow_mut()
                .insert(chunk.blob_id.clone(), plaintext.clone());
        });
        Ok(plaintext)
    }

    pub async fn fetch_link_preview(
        &self,
        url: String,
        relays: Vec<String>,
    ) -> anyhow::Result<Option<LinkPreview>> {
        let relays = relay_list(relays)?;
        let body = serde_json::to_vec(&LinkPreviewRequest { url })?;
        let mut last_error = None;
        let start_index = usize::from(blake3::hash(&body).as_bytes()[0]) % relays.len();

        for offset in 0..relays.len() {
            let index = (start_index + offset) % relays.len();
            match self
                .relay_request(&relays, index, "POST", "/v1/link-preview", &body)
                .await
            {
                Ok(response) if response.status == 200 => {
                    return serde_json::from_slice(&response.body)
                        .context("relay returned an invalid link preview")
                        .map(Some);
                }
                Ok(response)
                    if response.status == 204
                        || response.status == 404 && response.body.is_empty() =>
                {
                    return Ok(None);
                }
                Ok(response) => {
                    last_error = Some(anyhow::anyhow!(
                        "relay rejected the link preview with {}",
                        response.status
                    ));
                }
                Err(error) => last_error = Some(error),
            }
        }

        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("no relay was reachable")))
    }

    pub async fn upload_media_chunk(
        &self,
        path: impl AsRef<Path>,
        data_base64: String,
        relays: Vec<String>,
    ) -> anyhow::Result<MediaChunk> {
        let path = path.as_ref();
        let state = load_state(path)?;
        let group_id = state.active_group()?.group_id.clone();
        self.upload_media_chunk_to_group(path, &group_id, data_base64, relays)
            .await
    }

    pub async fn upload_media_chunk_to_group(
        &self,
        path: impl AsRef<Path>,
        group_id: &str,
        data_base64: String,
        relays: Vec<String>,
    ) -> anyhow::Result<MediaChunk> {
        let state = load_state(path.as_ref())?;
        if !state.groups.iter().any(|group| group.group_id == group_id) {
            bail!("unknown group")
        }
        self.upload_media_chunk_to_scope(group_id, data_base64, relays)
            .await
    }

    async fn upload_media_chunk_to_scope(
        &self,
        scope_id: &str,
        data_base64: String,
        relays: Vec<String>,
    ) -> anyhow::Result<MediaChunk> {
        let data = STANDARD
            .decode(data_base64)
            .context("media chunk encoding is invalid")?;
        if data.is_empty() || data.len() > 1024 * 1024 {
            bail!("media chunks must contain between 1 byte and 1 MiB")
        }
        let (blob, key_base64) = EncryptedBlob::create_for_group(&data, scope_id)?;
        let storage = self
            .store_blob_shards(&relay_list(relays)?, &blob, &key_base64)
            .await?;
        Ok(MediaChunk {
            blob_id: blob.blob_id,
            key_base64,
            byte_length: data.len() as u32,
            storage: Some(storage),
        })
    }

    pub async fn sync_active_group_encryption(
        &self,
        path: impl AsRef<Path>,
        cache_path: impl AsRef<Path>,
        relays: Vec<String>,
    ) -> anyhow::Result<GroupEncryptionStatus> {
        self.sync_group_encryption(path, cache_path, None, relays)
            .await
    }

    /// Whether this group needs an encrypted-membership reconciliation pass.
    ///
    /// A group with no local MLS epoch always needs a pass so the client can
    /// publish its signed KeyPackage even before the founder creates genesis.
    /// Established members additionally use the pass to admit waiting devices.
    pub async fn group_has_pending_admissions(
        &self,
        path: impl AsRef<Path>,
        group_id: &str,
        relays: Vec<String>,
    ) -> anyhow::Result<bool> {
        let state = load_state(path.as_ref())?;
        let group = state
            .groups
            .iter()
            .find(|group| group.group_id == group_id)
            .cloned()
            .context("unknown group")?;
        let self_public_key = state.identity()?.public_key_base64();
        let Some(mls) = state.mls_group_state(group_id) else {
            return Ok(true);
        };
        if mls.epoch(group_id).is_err() {
            return Ok(true);
        };
        let relays = relay_list(relays)?;
        let Some(control_log) = self.fetch_mls_control_log(&relays, group_id).await? else {
            return Ok(true);
        };
        let (head_epoch, _) = control_log.head();
        if control_log
            .member_accounts_at(head_epoch)
            .is_none_or(|members| !members.contains(&self_public_key))
        {
            return Ok(false);
        }
        let known_devices = mls
            .member_devices(group_id)
            .unwrap_or_default()
            .into_iter()
            .map(|credential| credential.device_id_base64)
            .collect::<HashSet<_>>();
        let requests = self.fetch_mls_join_requests(&relays, group_id).await?;
        Ok(requests.iter().any(|request| {
            request.group_id == group.group_id
                && !known_devices.contains(&request.device_credential.device_id_base64)
        }))
    }

    /// Reconcile one group's encrypted membership.
    ///
    /// Passing a `group_id` runs an admission-only pass for a group that is not
    /// open on screen. Those passes are what keep a join from waiting until one
    /// specific member opens that specific group: any member already inside the
    /// group can publish the epoch that admits the people waiting for it.
    pub async fn sync_group_encryption(
        &self,
        path: impl AsRef<Path>,
        cache_path: impl AsRef<Path>,
        group_id: Option<&str>,
        relays: Vec<String>,
    ) -> anyhow::Result<GroupEncryptionStatus> {
        let path = path.as_ref();
        let cache_path = cache_path.as_ref();
        let admissions_only = group_id.is_some();
        let mut state = load_state(path)?;
        let relays = relay_list(relays)?;
        let group = match group_id {
            Some(group_id) => state
                .groups
                .iter()
                .find(|group| group.group_id == group_id)
                .cloned()
                .context("unknown group")?,
            None => state.active_group()?.clone(),
        };
        let identity = state.identity()?;
        let self_public_key = identity.public_key_base64();
        // An admission pass works from the signed control log alone. Only the
        // member whose turn it is to admit pays for this group's history.
        let events = if admissions_only {
            Vec::new()
        } else {
            self.fetch_events(&group, relays.clone()).await?
        };
        let legacy_events = events
            .iter()
            .filter(|event| event.encryption_version == 1)
            .cloned()
            .collect::<Vec<_>>();
        let legacy_view = GroupState::rebuild(&group, &legacy_events);
        let mut control_log = self.fetch_mls_control_log(&relays, &group.group_id).await?;
        if let (Some(remote), Some(local)) = (
            control_log.as_ref(),
            state.mls_local_geneses.get(&group.group_id),
        ) && remote.genesis.record_id != local.record_id
        {
            state
                .forget_mls_group(&group.group_id)
                .context("could not replace a losing MLS genesis")?;
            state.mls_local_geneses.remove(&group.group_id);
            save_state(path, &state)?;
        }
        if let Some(log) = control_log.as_ref()
            && discard_superseded_mls_state(&mut state, log)?
        {
            save_state(path, &state)?;
        }
        let active_members = control_log
            .as_ref()
            .map(|log| {
                let (head_epoch, _) = log.head();
                log.member_accounts_at(head_epoch)
                    .unwrap_or_default()
                    .iter()
                    .cloned()
                    .collect::<HashSet<_>>()
            })
            .unwrap_or_else(|| legacy_view.members.keys().cloned().collect::<HashSet<_>>());
        let local_has_mls_group = state
            .mls_group_state(&group.group_id)
            .is_some_and(|mls| mls.epoch(&group.group_id).is_ok());
        if !local_has_mls_group {
            let needs_membership_proof =
                state
                    .mls_join_requests
                    .get(&group.group_id)
                    .is_none_or(|request| {
                        request.account_public_key != self_public_key
                            || join_request_membership_profile(request, &group).is_none()
                    });
            if needs_membership_proof {
                let sequence = state.take_sequence();
                let membership_proof =
                    SignedEvent::member_joined(&identity, &group, &state.profile, sequence)?;
                let request = {
                    let mls = state.ensure_mls_group_state(&group.group_id)?;
                    MlsJoinRequest::create_with_membership_proof(
                        &identity,
                        mls,
                        group.group_id.clone(),
                        membership_proof,
                    )
                    .context("could not create this device's MLS join request")?
                };
                state
                    .mls_join_requests
                    .insert(group.group_id.clone(), request);
                save_state(path, &state)?;
            }
            let join_request = state
                .mls_join_requests
                .get(&group.group_id)
                .cloned()
                .context("this device's MLS join request is missing")?;
            self.publish_mls_join_request(&relays, &join_request)
                .await?;
        }
        let has_pending_membership_proof = state
            .mls_join_requests
            .get(&group.group_id)
            .is_some_and(|request| {
                request.account_public_key == self_public_key
                    && join_request_membership_profile(request, &group).is_some()
            });
        if !active_members.contains(&self_public_key) && !has_pending_membership_proof {
            // Erasing a group is the open group's job. A background admission
            // pass must never delete a conversation the user is not looking at.
            if admissions_only {
                return Ok(GroupEncryptionStatus {
                    group_id: group.group_id,
                    phase: "removed".into(),
                    epoch: None,
                    missing_member_public_keys: Vec::new(),
                });
            }
            purge_group_cache(cache_path, &group.group_id)?;
            purge_profile_image_cache(cache_path)?;
            state
                .forget_mls_group(&group.group_id)
                .context("could not erase this group's local encryption state")?;
            state.mls_join_requests.remove(&group.group_id);
            state.mls_local_geneses.remove(&group.group_id);
            state.mls_control_logs.remove(&group.group_id);
            state.group_frequencies.remove(&group.group_id);
            state.forget_group_activity(&group.group_id);
            state.group_conversation_cache.remove(&group.group_id);
            state.group_event_caches.remove(&group.group_id);
            state.tombstone_group(&group.group_id);
            save_state(path, &state)?;
            return Ok(GroupEncryptionStatus {
                group_id: group.group_id,
                phase: "removed".into(),
                epoch: None,
                missing_member_public_keys: Vec::new(),
            });
        }

        let is_owner = group.owner_public_key == self_public_key;
        if control_log.is_none() {
            // Only the founder can sign a genesis for this group's identifier,
            // and that first cutover needs the group's own history.
            if !is_owner || admissions_only {
                save_state(path, &state)?;
                return Ok(GroupEncryptionStatus {
                    group_id: group.group_id,
                    phase: "waiting_for_founder".into(),
                    epoch: None,
                    missing_member_public_keys: Vec::new(),
                });
            }
            if !state.mls_local_geneses.contains_key(&group.group_id) {
                let mut candidate = state.ensure_mls_group_state(&group.group_id)?.clone();
                let genesis = candidate
                    .create_group_genesis(&identity, &group)
                    .context("could not create the group MLS genesis")?;
                state.set_mls_group_state(&group.group_id, candidate);
                state
                    .mls_local_geneses
                    .insert(group.group_id.clone(), genesis);
                save_state(path, &state)?;
            }
            let genesis = state
                .mls_local_geneses
                .get(&group.group_id)
                .cloned()
                .context("local MLS genesis is missing")?;
            self.publish_mls_genesis(&relays, &genesis).await?;
            control_log = Some(MlsControlLog {
                genesis,
                epochs: Vec::new(),
            });
        }

        let mut control_log = control_log.context("MLS control log is missing")?;
        let local_epoch = sync_mls_state_from_log(&mut state, &control_log)?;
        let (head_epoch, _) = control_log.head();
        // Only a member whose own encryption state sits on the current head can
        // author the next epoch; anything else would build on a stale parent.
        let is_current_member = local_epoch == Some(head_epoch)
            && control_log
                .member_accounts_at(head_epoch)
                .is_some_and(|members| members.contains(&self_public_key));
        let join_requests = if is_current_member {
            self.fetch_mls_join_requests(&relays, &group.group_id)
                .await?
        } else {
            Vec::new()
        };
        // Only the founder's own client turns signed leave and ban requests into
        // removals, so nobody else has to read them.
        let applies_removals = is_owner && !admissions_only;
        if is_current_member && (!join_requests.is_empty() || applies_removals) {
            state
                .mls_control_logs
                .insert(group.group_id.clone(), control_log.clone());
            let removals = self
                .fetch_mls_removal_requests(&relays, &group.group_id)
                .await?;
            let mut latest_removal_by_account = HashMap::<String, u64>::new();
            for request in &removals {
                latest_removal_by_account
                    .entry(request.target_public_key.clone())
                    .and_modify(|current| *current = (*current).max(request.created_at_millis))
                    .or_insert(request.created_at_millis);
            }
            if applies_removals {
                let current_view = rebuild_group_state(&state, &group, &events)?;
                if let Some((candidate, record, applied_removals)) = prepare_pending_member_removal(
                    &state,
                    &identity,
                    &group,
                    &current_view,
                    &control_log,
                    removals,
                )? {
                    if applied_removals
                        .iter()
                        .any(|request| request.reason == MlsRemovalReason::Banned)
                        && !group.authority_nonce_base64.is_empty()
                    {
                        let frequency = generate_frequency();
                        let invite = InviteRecord::create(&identity, &frequency, group.clone())?;
                        let sequence = state.take_sequence();
                        let rotation =
                            InviteRotation::create(&identity, &group, Some(invite), sequence)?;
                        save_state(path, &state)?;
                        self.publish_invite_rotation(&relays, &rotation).await?;
                        state
                            .group_frequencies
                            .insert(group.group_id.clone(), frequency);
                        save_state(path, &state)?;
                    }
                    if self.publish_mls_epoch(&relays, &record).await? == ControlHead::Extended {
                        state.set_mls_group_state(&group.group_id, candidate);
                        control_log.epochs.push(record);
                        state
                            .mls_control_logs
                            .insert(group.group_id.clone(), control_log.clone());
                        save_state(path, &state)?;
                    }
                }
            }

            let (head_epoch, head_record_id) = control_log.head();
            let head_record_id = head_record_id.to_owned();
            let epoch_members = control_log
                .member_accounts_at(head_epoch)
                .unwrap_or_default()
                .iter()
                .cloned()
                .collect::<HashSet<_>>();
            // At the initial cutover, an open group can also authorize older
            // proof-less requests from its decrypted legacy membership. A
            // background pass admits only requests carrying their own signed
            // membership proof, so it never has to download group history.
            let eligible_members =
                if head_epoch == 0 && control_log.epochs.is_empty() && !admissions_only {
                    legacy_view.members.keys().cloned().collect::<HashSet<_>>()
                } else {
                    epoch_members.clone()
                };
            let pending = pending_admission_requests(
                &state,
                &group,
                &eligible_members,
                &latest_removal_by_account,
                join_requests,
            )?;
            let is_this_members_turn = !pending.is_empty()
                && admission_author_rank(&head_record_id, &epoch_members, &self_public_key)
                    .is_some_and(|rank| {
                        admission_turn_reached(
                            &group.group_id,
                            &admission_digest(&head_record_id, &pending),
                            rank,
                        )
                    });
            if is_this_members_turn
                && (is_owner || self.relays_accept_member_admission(&relays).await)
            {
                // Banned accounts can still sign a fresh membership proof, so
                // the member taking this turn checks the group's own moderation
                // history rather than a cached summary of it.
                let admission_events = if admissions_only {
                    Some(self.fetch_events(&group, relays.clone()).await?)
                } else {
                    None
                };
                let current_view = rebuild_group_state(
                    &state,
                    &group,
                    admission_events.as_deref().unwrap_or(&events),
                )?;
                let admitted = pending
                    .into_iter()
                    .filter(|request| {
                        !current_view
                            .banned_members
                            .contains(&request.account_public_key)
                    })
                    .collect::<Vec<_>>();
                if !admitted.is_empty() {
                    let (candidate, record) = prepare_member_add_epoch(
                        &state,
                        &identity,
                        &group,
                        &epoch_members,
                        &control_log,
                        &admitted,
                    )?;
                    if self.publish_mls_epoch(&relays, &record).await? == ControlHead::Extended {
                        state.set_mls_group_state(&group.group_id, candidate);
                        control_log.epochs.push(record);
                    }
                }
            }
        }

        let local_epoch = sync_mls_state_from_log(&mut state, &control_log)?;
        if local_epoch.is_some() {
            state.mls_join_requests.remove(&group.group_id);
        }
        state
            .mls_control_logs
            .insert(group.group_id.clone(), control_log.clone());
        save_state(path, &state)?;
        // Do not couple an active group to the remote account-vault backup.
        // The new MLS recovery material is already saved locally and the
        // client's background account-sync lane retries the encrypted backup.
        let (head_epoch, _) = control_log.head();
        if !admissions_only
            && local_epoch == Some(head_epoch)
            && control_log
                .member_accounts_at(head_epoch)
                .is_some_and(|members| members.contains(&self_public_key))
            && !rebuild_group_state(&state, &group, &events)?
                .members
                .contains_key(&self_public_key)
        {
            let sequence = state.take_sequence();
            let joined = create_group_event(
                &state,
                &identity,
                &group,
                GroupEventPayload::MemberJoined {
                    username: state.profile.username.clone(),
                    bio: state.profile.bio.clone(),
                    avatar: state.profile.avatar.clone(),
                    album: state.profile.album.clone(),
                    accepts_direct_messages: state.profile.effective_direct_message_policy()
                        != DirectMessagePolicy::Nobody,
                    direct_message_policy: state.profile.effective_direct_message_policy(),
                },
                sequence,
            )?;
            // Persist the consumed sequence before the network write so a
            // partially accepted retry can never reuse it for different bytes.
            save_state(path, &state)?;
            self.publish_event(&relays, &joined).await?;
        }
        Ok(GroupEncryptionStatus {
            group_id: group.group_id,
            phase: if local_epoch == Some(head_epoch) {
                "active".into()
            } else if has_pending_membership_proof {
                "waiting_for_admission".into()
            } else if active_members.contains(&self_public_key) {
                "waiting_for_device".into()
            } else {
                "waiting_for_founder".into()
            },
            epoch: local_epoch,
            missing_member_public_keys: Vec::new(),
        })
    }

    pub async fn make(
        &self,
        path: impl AsRef<Path>,
        name: impl Into<String>,
        content_rating: GroupContentRating,
        avatar_data_base64: Option<String>,
        avatar_mime_type: Option<String>,
        relays: Vec<String>,
    ) -> anyhow::Result<MakeResult> {
        let path = path.as_ref();
        let name = name.into();
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 80 {
            bail!("group names must contain between 1 and 80 characters")
        }
        let mut state = load_state(path)?;
        if content_rating == GroupContentRating::Explicit
            && !state.adult_access.explicit_content_enabled
        {
            bail!("enable explicit groups in settings before creating an explicit group")
        }
        let identity = state.identity()?;
        let relays = relay_list(relays)?;
        let mut group = GroupMembership::create_owned(name, identity.public_key_base64());
        group.content_rating = content_rating;
        if let Some(encoded) = avatar_data_base64 {
            let mime_type = avatar_mime_type.context("group icon media type is missing")?;
            if !matches!(
                mime_type.as_str(),
                "image/jpeg" | "image/png" | "image/webp"
            ) {
                bail!("group icons must be JPEG, PNG, or WebP images")
            }
            let data = STANDARD
                .decode(encoded)
                .context("group icon encoding is invalid")?;
            if data.is_empty() || data.len() > 256 * 1024 {
                bail!("group icons must contain between 1 byte and 256 KiB")
            }
            let (blob, key_base64) =
                EncryptedBlob::create_for_group(&data, group.group_id.clone())?;
            let storage = self.store_blob_shards(&relays, &blob, &key_base64).await?;
            group.avatar = Some(ProfileImage {
                blob_id: blob.blob_id,
                key_base64,
                mime_type,
                byte_length: data.len() as u32,
                storage: Some(storage),
            });
        }
        let frequency = generate_frequency();
        let invitation = InviteRecord::create(&identity, &frequency, group.clone())?;
        self.publish_invite(&relays, &invitation).await?;
        state
            .group_frequencies
            .insert(group.group_id.clone(), frequency.clone());
        state.add_group(group);
        let sequence = state.take_sequence();
        let group = state.active_group()?.clone();
        let joined = SignedEvent::member_joined(&identity, &group, &state.profile, sequence)?;
        self.publish_event(&relays, &joined).await?;
        save_state(path, &state)?;
        Ok(MakeResult {
            group: GroupSummary {
                group_id: group.group_id,
                name: group.name,
                description: group.description,
                rules: group.rules,
                content_rating: group.content_rating,
                avatar: group.avatar,
                background: group.background,
                mobile_background: group.mobile_background,
                accent_color: group.accent_color,
                members_can_send_messages: group.members_can_send_messages,
                members_can_send_media: group.members_can_send_media,
                frequency: Some(display_frequency(&frequency)?),
                owner_public_key: group.owner_public_key,
                remote_deletion_supported: !group.authority_nonce_base64.is_empty(),
                is_active: true,
                unread_count: 0,
                read_state_initialized: false,
            },
            display_frequency: display_frequency(&frequency)?,
            frequency,
        })
    }

    pub async fn join(
        &self,
        path: impl AsRef<Path>,
        frequency: &str,
        relays: Vec<String>,
    ) -> anyhow::Result<JoinResult> {
        let path = path.as_ref();
        let mut state = load_state(path)?;
        let relays = relay_list(relays)?;
        let frequency = normalize_frequency(frequency)?;
        let locator = frequency_locator(&frequency);
        let invitation = self.fetch_invite(&relays, &locator).await?;
        let payload = invitation
            .open(&frequency)
            .context("the frequency could not open its invitation")?;
        state.ensure_group_access(&payload.group)?;
        state.add_group(payload.group);
        let group = state.active_group()?.clone();
        let identity = state.identity()?;
        let control_log = self.fetch_mls_control_log(&relays, &group.group_id).await?;
        let sequence = state.take_sequence();
        let membership_proof =
            SignedEvent::member_joined(&identity, &group, &state.profile, sequence)?;
        let request = {
            let mls = state.ensure_mls_group_state(&group.group_id)?;
            MlsJoinRequest::create_with_membership_proof(
                &identity,
                mls,
                group.group_id.clone(),
                membership_proof.clone(),
            )
            .context("could not create the encrypted-group join request")?
        };
        state
            .mls_join_requests
            .insert(group.group_id.clone(), request.clone());
        let has_control_log = control_log.is_some();
        if let Some(control_log) = control_log {
            state
                .mls_control_logs
                .insert(group.group_id.clone(), control_log);
        }
        save_state(path, &state)?;
        self.publish_mls_join_request(&relays, &request).await?;
        if !has_control_log {
            self.publish_event(&relays, &membership_proof).await?;
        }
        Ok(JoinResult {
            group: GroupSummary {
                group_id: group.group_id,
                name: group.name,
                description: group.description,
                rules: group.rules,
                content_rating: group.content_rating,
                avatar: group.avatar,
                background: group.background,
                mobile_background: group.mobile_background,
                accent_color: group.accent_color,
                members_can_send_messages: group.members_can_send_messages,
                members_can_send_media: group.members_can_send_media,
                frequency: None,
                owner_public_key: group.owner_public_key,
                remote_deletion_supported: !group.authority_nonce_base64.is_empty(),
                is_active: true,
                unread_count: 0,
                read_state_initialized: false,
            },
        })
    }

    pub fn start_direct(
        &self,
        path: impl AsRef<Path>,
        public_key: &str,
        username: impl Into<String>,
        bio: impl Into<String>,
        avatar: Option<ProfileImage>,
        album: Option<ProfileAlbum>,
        direct_message_policy: DirectMessagePolicy,
    ) -> anyhow::Result<LocalSummary> {
        let path = path.as_ref();
        let mut state = load_state(path)?;
        let self_public_key = state.identity()?.public_key_base64();
        if public_key == self_public_key {
            bail!("you cannot start a direct message with yourself")
        }
        if state.is_hidden(public_key) {
            bail!("you cannot message this person while either of you has the other blocked")
        }
        direct_mailbox_id(public_key).context("that identity has an invalid public key")?;
        let username = username.into();
        let bio = bio.into();
        validate_username(&username)?;
        if bio.chars().count() > 160 {
            bail!("bios can contain at most 160 characters")
        }
        if direct_message_policy == DirectMessagePolicy::Nobody {
            bail!("this person is not accepting direct messages")
        }
        if direct_message_policy == DirectMessagePolicy::SharedGroups
            && !state.shares_active_group_with(public_key)
        {
            bail!("this person only accepts direct messages from shared groups")
        }
        let profile_sequence = state.known_profile_sequence(public_key);
        state.add_direct(DirectContact {
            public_key: public_key.to_owned(),
            username,
            bio,
            avatar,
            album,
            accepts_direct_messages: true,
            direct_message_policy,
            profile_sequence,
        });
        save_state(path, &state)?;
        state.summary()
    }

    pub async fn publish_contact_signal(
        &self,
        path: impl AsRef<Path>,
        relays: Vec<String>,
    ) -> anyhow::Result<String> {
        let path = path.as_ref();
        let mut state = load_state(path)?;
        let signature = self
            .ensure_contact_signal_published(&mut state, &relay_list(relays)?)
            .await?;
        save_state(path, &state)?;
        Ok(signature)
    }

    async fn ensure_contact_signal_published(
        &self,
        state: &mut ClientState,
        relays: &[RelayDescriptor],
    ) -> anyhow::Result<String> {
        let identity = state.identity()?;
        let signature = contact_signal_for_public_key(&identity.public_key_base64())?;
        if state.profile_sequence > 0
            && state.contact_signal_published_sequence >= state.profile_sequence
        {
            return Ok(signature);
        }
        let membership = contact_signal_membership(&signature)?;
        let sequence = if state.profile_sequence == 0 {
            let sequence = state.take_sequence();
            state.profile_sequence = sequence;
            sequence
        } else {
            state.profile_sequence
        };
        let event = SignedEvent::profile_updated(&identity, &membership, &state.profile, sequence)?;
        self.publish_event(relays, &event).await?;
        state.contact_signal_published_sequence = sequence;
        Ok(signature)
    }

    pub async fn resolve_contact_signal(
        &self,
        signature: &str,
        relays: Vec<String>,
    ) -> anyhow::Result<DirectSummary> {
        let normalized = normalize_contact_signal(signature)?;
        let membership = contact_signal_membership(&normalized)?;
        let events = self
            .fetch_events_for_id(&membership.group_id, relay_list(relays)?)
            .await?;
        let mut profiles = HashMap::<String, (u64, Profile)>::new();
        for event in events {
            if normalize_contact_signal(&contact_signal_for_public_key(&event.author_public_key)?)?
                != normalized
            {
                continue;
            }
            let Ok(GroupEventPayload::ProfileUpdated { profile }) = event.decrypt(&membership)
            else {
                continue;
            };
            if !valid_direct_profile(&profile) {
                continue;
            }
            profiles
                .entry(event.author_public_key)
                .and_modify(|current| {
                    if event.author_sequence > current.0 {
                        *current = (event.author_sequence, profile.clone());
                    }
                })
                .or_insert((event.author_sequence, profile));
        }
        if profiles.len() > 1 {
            bail!("that noise signature is ambiguous; ask for a fresh contact signal")
        }
        let (public_key, (_, profile)) = profiles
            .into_iter()
            .next()
            .context("no user has published that noise signature yet")?;
        let direct_message_policy = profile.effective_direct_message_policy();
        Ok(DirectSummary {
            public_key,
            username: profile.username,
            bio: profile.bio,
            avatar: profile.avatar,
            album: profile.album,
            accepts_direct_messages: direct_message_policy != DirectMessagePolicy::Nobody,
            direct_message_policy,
            is_active: false,
            has_unread: false,
        })
    }

    pub async fn set_block(
        &self,
        path: impl AsRef<Path>,
        cache_path: impl AsRef<Path>,
        public_key: &str,
        username: impl Into<String>,
        bio: impl Into<String>,
        avatar: Option<ProfileImage>,
        album: Option<ProfileAlbum>,
        accepts_direct_messages: bool,
        blocked: bool,
        relays: Vec<String>,
    ) -> anyhow::Result<LocalSummary> {
        let path = path.as_ref();
        let mut state = load_state(path)?;
        let self_public_key = state.identity()?.public_key_base64();
        if public_key == self_public_key {
            bail!("you cannot block yourself")
        }
        direct_mailbox_id(public_key).context("that identity has an invalid public key")?;
        let profile_sequence = state.known_profile_sequence(public_key);
        let direct_message_policy = state
            .direct_contacts
            .iter()
            .chain(state.known_people.iter())
            .find(|person| person.public_key == public_key)
            .map_or_else(
                || {
                    if accepts_direct_messages {
                        DirectMessagePolicy::Everyone
                    } else {
                        DirectMessagePolicy::Nobody
                    }
                },
                |person| person.direct_message_policy,
            );
        let person = DirectContact {
            public_key: public_key.to_owned(),
            username: username.into(),
            bio: bio.into(),
            avatar,
            album,
            accepts_direct_messages,
            direct_message_policy,
            profile_sequence,
        };
        validate_username(&person.username)?;
        if person.bio.chars().count() > 160 {
            bail!("bios can contain at most 160 characters")
        }
        let identity = state.identity()?;
        let recipient_mailbox = identity.direct_mailbox(public_key, public_key)?;
        let sequence = state.take_sequence();
        let notice = SignedEvent::direct_block_changed(
            &identity,
            &recipient_mailbox,
            public_key,
            &state.profile,
            blocked,
            sequence,
        )?;
        self.publish_event(&relay_list(relays)?, &notice).await?;
        let changed_at_millis = notice.created_at_millis;
        state.block_states.insert(
            public_key.to_owned(),
            BlockState {
                person: person.clone(),
                blocked,
                sequence,
                changed_at_millis,
            },
        );
        if blocked {
            let scope_id = state.identity()?.direct_scope_id(public_key)?;
            purge_scope_cache(cache_path.as_ref(), &scope_id)?;
            purge_scope_cache(cache_path.as_ref(), &profile_media_scope_id(public_key)?)?;
        }
        state.apply_block_filters();
        save_state(path, &state)?;
        state.summary()
    }

    pub fn select_direct(
        &self,
        path: impl AsRef<Path>,
        public_key: &str,
    ) -> anyhow::Result<LocalSummary> {
        let path = path.as_ref();
        let mut state = load_state(path)?;
        if state.is_hidden(public_key) {
            bail!("this conversation is unavailable while either person is blocked")
        }
        if !state
            .direct_contacts
            .iter()
            .any(|contact| contact.public_key == public_key)
        {
            bail!("unknown direct conversation")
        }
        state.active_direct_public_key = Some(public_key.to_owned());
        save_state(path, &state)?;
        state.summary()
    }

    pub async fn sync_directs(
        &self,
        path: impl AsRef<Path>,
        cache_path: impl AsRef<Path>,
        relays: Vec<String>,
    ) -> anyhow::Result<LocalSummary> {
        let path = path.as_ref();
        let (state, _) = self
            .sync_direct_inbox(path, cache_path.as_ref(), relay_list(relays)?)
            .await?;
        state.summary()
    }

    pub async fn direct_inbox(
        &self,
        path: impl AsRef<Path>,
        cache_path: impl AsRef<Path>,
        relays: Vec<String>,
    ) -> anyhow::Result<DirectInbox> {
        let path = path.as_ref();
        let (state, messages) = self
            .sync_direct_inbox(path, cache_path.as_ref(), relay_list(relays)?)
            .await?;
        direct_inbox_from_messages(&state, messages)
    }

    pub fn cached_direct_inbox(&self, path: impl AsRef<Path>) -> anyhow::Result<DirectInbox> {
        let state = load_state(path.as_ref())?;
        let messages = cached_direct_messages(&state)?;
        direct_inbox_from_messages(&state, messages)
    }

    pub fn search_local(
        &self,
        path: impl AsRef<Path>,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<SearchResults> {
        let path = path.as_ref();
        let state = load_state(path)?;
        let query = query.trim();
        if query.is_empty() {
            return Ok(SearchResults {
                messages: Vec::new(),
                locations: Vec::new(),
                people: Vec::new(),
                has_more_history: state.direct_event_cache.has_older_events
                    || state.group_event_caches.values().any(|cache| {
                        cache.has_older_messages
                            || cache
                                .topic_streams
                                .values()
                                .any(|topic| topic.has_older_messages)
                    }),
                older_scopes: Vec::new(),
            });
        }
        if query.chars().count() > 128 {
            bail!("search queries can contain at most 128 characters")
        }
        let limit = limit.clamp(1, 100);
        let hidden = state.active_hidden_keys();
        let identity_public_key = state.identity()?.public_key_base64();
        let current_profiles = current_profiles_by_public_key(&state, identity_public_key.as_str());
        let group_summaries = state
            .summary()?
            .groups
            .into_iter()
            .map(|group| (group.group_id.clone(), group))
            .collect::<HashMap<_, _>>();
        let mut message_matches = Vec::<(u32, SearchMessageResult)>::new();
        let mut location_matches = Vec::<(u32, SearchLocationResult)>::new();

        for group in &state.groups {
            let Some(group_summary) = group_summaries.get(&group.group_id) else {
                continue;
            };
            if let Some(score) = search_score(
                query,
                [
                    group_summary.name.as_str(),
                    group_summary.description.as_str(),
                    group_summary.rules.as_str(),
                ],
            ) {
                location_matches.push((
                    score,
                    SearchLocationResult {
                        group_id: group.group_id.clone(),
                        group_name: group_summary.name.clone(),
                        group_avatar: group_summary.avatar.clone(),
                        topic_id: None,
                        topic_name: None,
                        topic_icon: None,
                    },
                ));
            }
            if let Some(score) = search_score(query, ["General"]) {
                location_matches.push((
                    score,
                    SearchLocationResult {
                        group_id: group.group_id.clone(),
                        group_name: group_summary.name.clone(),
                        group_avatar: group_summary.avatar.clone(),
                        topic_id: None,
                        topic_name: Some("General".to_owned()),
                        topic_icon: Some("💬".to_owned()),
                    },
                ));
            }
            let Some(conversation) = self.cached_conversation(path, &group.group_id)? else {
                continue;
            };
            for topic in conversation.topics.iter().filter(|topic| !topic.archived) {
                if let Some(score) = search_score(query, [topic.name.as_str(), topic.icon.as_str()])
                {
                    location_matches.push((
                        score,
                        SearchLocationResult {
                            group_id: group.group_id.clone(),
                            group_name: group_summary.name.clone(),
                            group_avatar: group_summary.avatar.clone(),
                            topic_id: Some(topic.topic_id.clone()),
                            topic_name: Some(topic.name.clone()),
                            topic_icon: Some(topic.icon.clone()),
                        },
                    ));
                }
            }
            let topic_names = conversation
                .topics
                .iter()
                .map(|topic| (topic.topic_id.as_str(), topic.name.as_str()))
                .collect::<HashMap<_, _>>();
            for message in conversation.messages {
                if hidden.contains(&message.author_public_key) {
                    continue;
                }
                let topic_name = message
                    .topic_id
                    .as_deref()
                    .and_then(|topic_id| topic_names.get(topic_id).copied())
                    .unwrap_or("General");
                let Some(score) = search_message_score(query, message.text.as_str()) else {
                    continue;
                };
                let (username, avatar) = current_profiles
                    .get(&message.author_public_key)
                    .map(|(_, profile)| (profile.username.clone(), profile.avatar.clone()))
                    .unwrap_or_else(|| (message.username.clone(), message.avatar.clone()));
                message_matches.push((
                    score,
                    SearchMessageResult {
                        event_id: message.event_id,
                        author_public_key: message.author_public_key,
                        username,
                        avatar,
                        text: message.text,
                        attachment: message.attachment,
                        created_at_millis: message.created_at_millis,
                        group_id: Some(group.group_id.clone()),
                        group_name: Some(group_summary.name.clone()),
                        topic_id: message.topic_id,
                        topic_name: Some(topic_name.to_owned()),
                        direct_public_key: None,
                    },
                ));
            }
        }

        let direct_contacts = state
            .direct_contacts
            .iter()
            .map(|contact| (contact.public_key.as_str(), contact))
            .collect::<HashMap<_, _>>();
        for decrypted in cached_direct_messages(&state)? {
            let message = decrypted.message;
            let Some(contact) = direct_contacts.get(decrypted.counterparty_public_key.as_str())
            else {
                continue;
            };
            let Some(score) = search_message_score(query, message.text.as_str()) else {
                continue;
            };
            let (username, avatar) = current_profiles
                .get(&message.author_public_key)
                .map(|(_, profile)| (profile.username.clone(), profile.avatar.clone()))
                .unwrap_or_else(|| (message.username.clone(), message.avatar.clone()));
            message_matches.push((
                score,
                SearchMessageResult {
                    event_id: message.event_id,
                    author_public_key: message.author_public_key,
                    username,
                    avatar,
                    text: message.text,
                    attachment: message.attachment,
                    created_at_millis: message.created_at_millis,
                    group_id: None,
                    group_name: None,
                    topic_id: None,
                    topic_name: None,
                    direct_public_key: Some(contact.public_key.clone()),
                },
            ));
        }

        let direct_keys = state
            .direct_contacts
            .iter()
            .map(|contact| contact.public_key.as_str())
            .collect::<HashSet<_>>();
        let mut people_matches = current_profiles
            .into_iter()
            .filter(|(public_key, _)| {
                public_key != &identity_public_key && !hidden.contains(public_key)
            })
            .filter_map(|(public_key, (_, person))| {
                search_score(query, [person.username.as_str(), person.bio.as_str()]).map(|score| {
                    (
                        score,
                        SearchPersonResult {
                            direct_message_policy: person.effective_direct_message_policy(),
                            has_direct: direct_keys.contains(public_key.as_str()),
                            public_key,
                            username: person.username,
                            bio: person.bio,
                            avatar: person.avatar,
                            album: person.album,
                            accepts_direct_messages: person.accepts_direct_messages,
                        },
                    )
                })
            })
            .collect::<Vec<_>>();

        message_matches.sort_by(|(left_score, left), (right_score, right)| {
            right_score
                .cmp(left_score)
                .then_with(|| right.created_at_millis.cmp(&left.created_at_millis))
        });
        location_matches.sort_by(|(left_score, left), (right_score, right)| {
            right_score.cmp(left_score).then_with(|| {
                left.topic_name
                    .as_deref()
                    .unwrap_or(&left.group_name)
                    .cmp(right.topic_name.as_deref().unwrap_or(&right.group_name))
            })
        });
        people_matches.sort_by(|(left_score, left), (right_score, right)| {
            right_score
                .cmp(left_score)
                .then_with(|| left.username.cmp(&right.username))
        });

        let mut older_scopes =
            state
                .group_event_caches
                .iter()
                .flat_map(|(group_id, cache)| {
                    let mut scopes = Vec::new();
                    if cache.has_older_messages {
                        scopes.push(SearchHistoryScope {
                            group_id: Some(group_id.clone()),
                            topic_id: None,
                        });
                    }
                    scopes.extend(cache.topic_streams.iter().filter_map(
                        |(topic_id, topic_cache)| {
                            topic_cache.has_older_messages.then(|| SearchHistoryScope {
                                group_id: Some(group_id.clone()),
                                topic_id: Some(topic_id.clone()),
                            })
                        },
                    ));
                    scopes
                })
                .collect::<Vec<_>>();
        if state.direct_event_cache.has_older_events {
            older_scopes.insert(
                0,
                SearchHistoryScope {
                    group_id: None,
                    topic_id: None,
                },
            );
        }
        Ok(SearchResults {
            messages: message_matches
                .into_iter()
                .take(limit)
                .map(|(_, result)| result)
                .collect(),
            locations: location_matches
                .into_iter()
                .take(limit.min(30))
                .map(|(_, result)| result)
                .collect(),
            people: people_matches
                .into_iter()
                .take(limit.min(30))
                .map(|(_, result)| result)
                .collect(),
            has_more_history: !older_scopes.is_empty(),
            older_scopes,
        })
    }

    pub fn mark_direct_read(
        &self,
        path: impl AsRef<Path>,
        public_key: &str,
    ) -> anyhow::Result<LocalSummary> {
        let path = path.as_ref();
        let mut state = load_state(path)?;
        if !state
            .direct_contacts
            .iter()
            .any(|contact| contact.public_key == public_key)
        {
            bail!("unknown direct conversation")
        }
        if let Some(latest) = state.direct_latest_incoming.get(public_key).cloned()
            && state.direct_read_through.get(public_key) != Some(&latest)
        {
            state
                .direct_read_through
                .insert(public_key.to_owned(), latest);
            save_state(path, &state)?;
        }
        state.summary()
    }

    pub async fn direct_conversation(
        &self,
        path: impl AsRef<Path>,
        cache_path: impl AsRef<Path>,
        relays: Vec<String>,
    ) -> anyhow::Result<DirectConversation> {
        let path = path.as_ref();
        let relays = relay_list(relays)?;
        let (mut state, messages) = self
            .sync_complete_direct_inbox(path, cache_path.as_ref(), relays.clone())
            .await?;
        let public_key = state
            .active_direct_public_key
            .clone()
            .context("choose a direct conversation first")?;
        if state.is_hidden(&public_key) {
            bail!("this conversation is unavailable while either person is blocked")
        }
        let contact = state
            .direct_contacts
            .iter()
            .find(|contact| contact.public_key == public_key)
            .cloned()
            .context("active direct conversation is missing")?;
        if let Some(latest) = state.direct_latest_incoming.get(&public_key).cloned()
            && state.direct_read_through.get(&public_key) != Some(&latest)
        {
            state.direct_read_through.insert(public_key.clone(), latest);
            save_state(path, &state)?;
        }
        let identity = state.identity()?;
        let self_public_key = identity.public_key_base64();
        let mut messages = messages
            .into_iter()
            .filter(|message| message.counterparty_public_key == public_key)
            .map(|message| message.message)
            .collect::<Vec<_>>();
        apply_current_direct_profiles(&mut messages, &self_public_key, &state.profile, &contact);
        Ok(DirectConversation {
            contact: direct_summary(&contact, true, false),
            media_scope_id: identity.direct_scope_id(&contact.public_key)?,
            messages,
        })
    }

    pub async fn watch_direct(
        &self,
        path: impl AsRef<Path>,
        since: Option<u64>,
        relays: Vec<String>,
    ) -> anyhow::Result<GroupWatch> {
        let state = load_state(path.as_ref())?;
        let identity = state.identity()?;
        let self_public_key = identity.public_key_base64();
        let mailbox_id = direct_mailbox_id(&self_public_key)?;
        let direct_mailboxes = state
            .direct_contacts
            .iter()
            .filter(|contact| !state.is_hidden(&contact.public_key))
            .filter_map(|contact| {
                identity
                    .direct_mailbox(&contact.public_key, &self_public_key)
                    .ok()
                    .map(|mailbox| (contact.public_key.clone(), mailbox))
            })
            .collect::<Vec<_>>();
        self.watch_direct_id(&mailbox_id, &direct_mailboxes, since, relay_list(relays)?)
            .await
    }

    pub async fn say_direct(
        &self,
        path: impl AsRef<Path>,
        text: impl Into<String>,
        attachment: Option<MediaAttachment>,
        reply_to_message_id: Option<String>,
        relays: Vec<String>,
    ) -> anyhow::Result<SentMessageResult> {
        let path = path.as_ref();
        let public_key = load_state(path)?
            .active_direct_public_key
            .clone()
            .context("choose a direct conversation first")?;
        self.say_direct_to(
            path,
            &public_key,
            text,
            attachment,
            reply_to_message_id,
            None,
            relays,
        )
        .await
    }

    pub async fn say_direct_to(
        &self,
        path: impl AsRef<Path>,
        public_key: &str,
        text: impl Into<String>,
        attachment: Option<MediaAttachment>,
        reply_to_message_id: Option<String>,
        forwarded_from: Option<ForwardedFrom>,
        relays: Vec<String>,
    ) -> anyhow::Result<SentMessageResult> {
        let path = path.as_ref();
        let mut state = load_state(path)?;
        let text = text.into();
        if text.trim().is_empty() && attachment.is_none() {
            bail!("message cannot be empty")
        }
        if text.chars().count() > 10_000 {
            bail!("messages can contain at most 10,000 characters")
        }
        if let Some(attachment) = attachment.as_ref() {
            validate_media_reference(attachment)?;
        }
        validate_forwarded_from(forwarded_from.as_ref())?;
        validate_reply_reference(reply_to_message_id.as_deref())?;
        let contact = state
            .direct_contacts
            .iter()
            .chain(state.known_people.iter())
            .find(|contact| contact.public_key == public_key)
            .cloned()
            .context("this person is not known on this device")?;
        if state.is_hidden(&contact.public_key) {
            bail!("you cannot message this person while either of you has the other blocked")
        }
        if !contact.accepts_direct_messages {
            bail!("this person is not accepting direct messages")
        }
        if contact.effective_direct_message_policy() == DirectMessagePolicy::SharedGroups
            && !state.shares_active_group_with(&contact.public_key)
        {
            bail!("this person only accepts direct messages from shared groups")
        }
        let identity = state.identity()?;
        let self_public_key = identity.public_key_base64();
        let recipient_mailbox =
            identity.direct_mailbox(&contact.public_key, &contact.public_key)?;
        let sender_mailbox = identity.direct_mailbox(&contact.public_key, &self_public_key)?;
        let sequence = state.take_sequence();
        let recipient_event = SignedEvent::direct_message_forwarded(
            &identity,
            &recipient_mailbox,
            &contact.public_key,
            &state.profile,
            text.clone(),
            attachment.clone(),
            reply_to_message_id.clone(),
            forwarded_from.clone(),
            sequence,
        )?;
        let sender_event = SignedEvent::direct_message_forwarded(
            &identity,
            &sender_mailbox,
            &contact.public_key,
            &state.profile,
            text,
            attachment,
            reply_to_message_id,
            forwarded_from,
            sequence,
        )?;
        let push_trigger = identity.direct_push_trigger(
            recipient_event.event_id.clone(),
            recipient_mailbox.group_id.clone(),
            recipient_event.created_at_millis,
        )?;
        let sent = SentMessageResult {
            event_id: sender_event.event_id.clone(),
            message_id: direct_message_id(&self_public_key, sequence),
            created_at_millis: sender_event.created_at_millis,
        };
        let relays = relay_list(relays)?;
        self.publish_event(&relays, &recipient_event).await?;
        self.publish_event(&relays, &sender_event).await?;
        // Push delivery is best effort. A notification outage must never turn a
        // successfully persisted encrypted message into a send failure.
        let _ = self
            .publish_direct_push(&relays, &push_trigger, &recipient_event)
            .await;
        let marker = DirectMessageMarker {
            created_at_millis: sender_event.created_at_millis,
            event_id: sender_event.event_id,
        };
        state
            .direct_latest_activity
            .entry(contact.public_key.clone())
            .and_modify(|latest| *latest = latest.clone().max(marker.clone()))
            .or_insert(marker);
        state.remember_direct(contact);
        save_state(path, &state)?;
        Ok(sent)
    }

    pub async fn register_push_subscription(
        &self,
        path: impl AsRef<Path>,
        installation_id: String,
        device_token: String,
        environment: String,
        topic: String,
        relays: Vec<String>,
    ) -> anyhow::Result<PushSubscriptionRegistration> {
        let state = load_state(path.as_ref())?;
        let registration = state.identity()?.push_subscription_registration(
            installation_id,
            device_token,
            environment,
            topic,
            current_millis(),
        )?;
        let relays = relay_list(relays)?;
        let relays = relays.as_slice();
        let body = serde_json::to_vec(&registration)?;
        let mut requests = FuturesUnordered::new();
        for index in 0..relays.len() {
            let body = &body;
            requests.push(async move {
                (
                    &relays[index].base_url,
                    self.relay_request(relays, index, "POST", "/v1/push/subscriptions", body)
                        .await,
                )
            });
        }
        let mut failures = Vec::new();
        while let Some((relay, result)) = requests.next().await {
            match result {
                Ok(response) if (200..300).contains(&response.status) => {
                    return Ok(registration);
                }
                Ok(response) => failures.push(format!(
                    "{relay} returned {}: {}",
                    response.status,
                    String::from_utf8_lossy(&response.body)
                )),
                Err(error) => failures.push(format!("{relay}: {error}")),
            }
        }
        bail!(
            "no configured relay accepted this device for push notifications ({})",
            failures.join("; ")
        )
    }

    pub async fn upload_direct_media_chunk(
        &self,
        path: impl AsRef<Path>,
        data_base64: String,
        relays: Vec<String>,
    ) -> anyhow::Result<MediaChunk> {
        let path = path.as_ref();
        let state = load_state(path)?;
        let public_key = state
            .active_direct_public_key
            .as_deref()
            .map(str::to_owned)
            .context("choose a direct conversation first")?;
        self.upload_direct_media_chunk_to(path, &public_key, data_base64, relays)
            .await
    }

    pub async fn upload_direct_media_chunk_to(
        &self,
        path: impl AsRef<Path>,
        public_key: &str,
        data_base64: String,
        relays: Vec<String>,
    ) -> anyhow::Result<MediaChunk> {
        let state = load_state(path.as_ref())?;
        let contact = state
            .direct_contacts
            .iter()
            .chain(state.known_people.iter())
            .find(|contact| contact.public_key == public_key)
            .context("this person is not known on this device")?;
        let scope_id = state.identity()?.direct_scope_id(&contact.public_key)?;
        self.upload_media_chunk_to_scope(&scope_id, data_base64, relays)
            .await
    }

    pub async fn delete_direct(
        &self,
        path: impl AsRef<Path>,
        cache_path: impl AsRef<Path>,
        public_key: &str,
        for_both: bool,
        relays: Vec<String>,
    ) -> anyhow::Result<LocalSummary> {
        let path = path.as_ref();
        let mut state = load_state(path)?;
        let contact = state
            .direct_contacts
            .iter()
            .find(|contact| contact.public_key == public_key)
            .cloned()
            .context("direct conversation is missing")?;
        let identity = state.identity()?;
        let self_public_key = identity.public_key_base64();
        let mut deleted_at_millis = current_millis();
        if for_both {
            let recipient_mailbox =
                identity.direct_mailbox(&contact.public_key, &contact.public_key)?;
            let sender_mailbox = identity.direct_mailbox(&contact.public_key, &self_public_key)?;
            let sequence = state.take_sequence();
            let recipient_event = SignedEvent::direct_thread_deleted(
                &identity,
                &recipient_mailbox,
                &contact.public_key,
                sequence,
            )?;
            let sender_event = SignedEvent::direct_thread_deleted(
                &identity,
                &sender_mailbox,
                &contact.public_key,
                sequence,
            )?;
            let relays = relay_list(relays)?;
            self.publish_event(&relays, &recipient_event).await?;
            self.publish_event(&relays, &sender_event).await?;
            let cleanup_client = self.clone();
            let cleanup_relays = relays.clone();
            let cleanup = async move {
                let mut storage = match cleanup_client
                    .fetch_events(&recipient_mailbox, cleanup_relays.clone())
                    .await
                {
                    Ok(events) => direct_storage_references(&recipient_mailbox, &events),
                    Err(_) => Vec::new(),
                };
                if let Ok(events) = cleanup_client
                    .fetch_events(&sender_mailbox, cleanup_relays)
                    .await
                {
                    storage.extend(direct_storage_references(&sender_mailbox, &events));
                }
                if !storage.is_empty() {
                    let _ = cleanup_client.erase_storage_references(storage, true).await;
                }
            };
            #[cfg(not(target_arch = "wasm32"))]
            tokio::spawn(cleanup);
            #[cfg(target_arch = "wasm32")]
            wasm_bindgen_futures::spawn_local(cleanup);
            deleted_at_millis = deleted_at_millis
                .max(recipient_event.created_at_millis)
                .max(sender_event.created_at_millis);
        }
        let scope_id = identity.direct_scope_id(&contact.public_key)?;
        purge_scope_cache(cache_path.as_ref(), &scope_id)?;
        state
            .direct_deleted_before
            .entry(contact.public_key.clone())
            .and_modify(|cutoff| *cutoff = (*cutoff).max(deleted_at_millis))
            .or_insert(deleted_at_millis);
        state
            .direct_contacts
            .retain(|candidate| candidate.public_key != contact.public_key);
        state.direct_latest_incoming.remove(&contact.public_key);
        state.direct_latest_activity.remove(&contact.public_key);
        state.direct_read_through.remove(&contact.public_key);
        state.active_direct_public_key = state
            .direct_contacts
            .first()
            .map(|candidate| candidate.public_key.clone());
        save_state(path, &state)?;
        state.summary()
    }

    pub async fn say(
        &self,
        path: impl AsRef<Path>,
        text: impl Into<String>,
        relays: Vec<String>,
    ) -> anyhow::Result<SentMessageResult> {
        self.send_message(path.as_ref(), text.into(), None, None, relays)
            .await
    }

    pub async fn say_reply(
        &self,
        path: impl AsRef<Path>,
        text: impl Into<String>,
        reply_to_message_id: Option<String>,
        relays: Vec<String>,
    ) -> anyhow::Result<SentMessageResult> {
        self.send_message(
            path.as_ref(),
            text.into(),
            None,
            reply_to_message_id,
            relays,
        )
        .await
    }

    pub async fn say_with_attachment(
        &self,
        path: impl AsRef<Path>,
        text: impl Into<String>,
        attachment: MediaAttachment,
        relays: Vec<String>,
    ) -> anyhow::Result<SentMessageResult> {
        validate_media_reference(&attachment)?;
        self.send_message(path.as_ref(), text.into(), Some(attachment), None, relays)
            .await
    }

    pub async fn say_with_attachment_reply(
        &self,
        path: impl AsRef<Path>,
        text: impl Into<String>,
        attachment: MediaAttachment,
        reply_to_message_id: Option<String>,
        relays: Vec<String>,
    ) -> anyhow::Result<SentMessageResult> {
        validate_media_reference(&attachment)?;
        self.send_message(
            path.as_ref(),
            text.into(),
            Some(attachment),
            reply_to_message_id,
            relays,
        )
        .await
    }

    async fn send_message(
        &self,
        path: &Path,
        text: String,
        attachment: Option<MediaAttachment>,
        reply_to_message_id: Option<String>,
        relays: Vec<String>,
    ) -> anyhow::Result<SentMessageResult> {
        let group_id = load_state(path)?.active_group()?.group_id.clone();
        self.send_message_to_group(
            path,
            &group_id,
            text,
            attachment,
            reply_to_message_id,
            None,
            relays,
        )
        .await
    }

    async fn send_message_to_group(
        &self,
        path: &Path,
        group_id: &str,
        text: String,
        attachment: Option<MediaAttachment>,
        reply_to_message_id: Option<String>,
        forwarded_from: Option<ForwardedFrom>,
        relays: Vec<String>,
    ) -> anyhow::Result<SentMessageResult> {
        if text.trim().is_empty() && attachment.is_none() {
            bail!("message cannot be empty")
        }
        validate_reply_reference(reply_to_message_id.as_deref())?;
        validate_forwarded_from(forwarded_from.as_ref())?;
        let relays = relay_list(relays)?;
        let mut state = load_state(path)?;
        let group = state
            .groups
            .iter()
            .find(|group| group.group_id == group_id)
            .cloned()
            .context("unknown group")?;
        let sequence = state.take_sequence();
        let event = create_group_event(
            &state,
            &state.identity()?,
            &group,
            GroupEventPayload::Message {
                text,
                attachment,
                reply_to_message_id,
                forwarded_from,
            },
            sequence,
        )?;
        let sent = SentMessageResult {
            event_id: event.event_id.clone(),
            message_id: event.event_id.clone(),
            created_at_millis: event.created_at_millis,
        };
        self.publish_event(&relays, &event).await?;
        save_state(path, &state)?;
        Ok(sent)
    }

    pub async fn create_topic(
        &self,
        path: impl AsRef<Path>,
        name: impl Into<String>,
        icon: impl Into<String>,
        relays: Vec<String>,
    ) -> anyhow::Result<GroupActivityResult> {
        let path = path.as_ref();
        let name = name.into().trim().to_owned();
        let icon = icon.into();
        validate_topic_name(&name)?;
        validate_topic_icon(&icon)?;
        let relays = relay_list(relays)?;
        let mut state = load_state(path)?;
        let group = state.active_group()?.clone();
        let identity = state.identity()?;
        ensure_topic_manager(&state, &group, &identity.public_key_base64())?;
        let sequence = state.take_sequence();
        let seed = format!(
            "noise-topic-v1:{}:{}:{}",
            group.group_id,
            identity.public_key_base64(),
            sequence
        );
        let topic_id = blake3::hash(format!("{seed}:id").as_bytes())
            .to_hex()
            .to_string();
        let stream_locator = blake3::hash(format!("{seed}:stream").as_bytes())
            .to_hex()
            .to_string();
        let event = create_group_event(
            &state,
            &identity,
            &group,
            GroupEventPayload::TopicCreated {
                topic_id,
                name,
                icon,
                stream_locator,
            },
            sequence,
        )?;
        self.publish_event(&relays, &event).await?;
        save_state(path, &state)?;
        self.apply_group_activity(
            path,
            GroupActivityUpdate {
                group_id: group.group_id,
                events: vec![event],
                full_snapshot: false,
                older_cursor: None,
                has_older_messages: false,
                topic_update: None,
            },
        )
    }

    pub async fn update_topic(
        &self,
        path: impl AsRef<Path>,
        topic_id: &str,
        name: impl Into<String>,
        icon: impl Into<String>,
        locked: bool,
        relays: Vec<String>,
    ) -> anyhow::Result<GroupActivityResult> {
        let path = path.as_ref();
        let name = name.into().trim().to_owned();
        let icon = icon.into();
        validate_topic_name(&name)?;
        validate_topic_icon(&icon)?;
        let relays = relay_list(relays)?;
        let mut state = load_state(path)?;
        let group = state.active_group()?.clone();
        let identity = state.identity()?;
        ensure_topic_manager(&state, &group, &identity.public_key_base64())?;
        ensure_active_topic(&state, &group, topic_id)?;
        let sequence = state.take_sequence();
        let event = create_group_event(
            &state,
            &identity,
            &group,
            GroupEventPayload::TopicUpdated {
                topic_id: topic_id.to_owned(),
                name,
                icon: Some(icon),
                locked,
            },
            sequence,
        )?;
        self.publish_event(&relays, &event).await?;
        save_state(path, &state)?;
        self.apply_group_activity(
            path,
            GroupActivityUpdate {
                group_id: group.group_id,
                events: vec![event],
                full_snapshot: false,
                older_cursor: None,
                has_older_messages: false,
                topic_update: None,
            },
        )
    }

    pub async fn archive_topic(
        &self,
        path: impl AsRef<Path>,
        topic_id: &str,
        relays: Vec<String>,
    ) -> anyhow::Result<GroupActivityResult> {
        let path = path.as_ref();
        let relays = relay_list(relays)?;
        let mut state = load_state(path)?;
        let group = state.active_group()?.clone();
        let identity = state.identity()?;
        ensure_topic_manager(&state, &group, &identity.public_key_base64())?;
        ensure_active_topic(&state, &group, topic_id)?;
        let sequence = state.take_sequence();
        let event = create_group_event(
            &state,
            &identity,
            &group,
            GroupEventPayload::TopicArchived {
                topic_id: topic_id.to_owned(),
            },
            sequence,
        )?;
        self.publish_event(&relays, &event).await?;
        save_state(path, &state)?;
        self.apply_group_activity(
            path,
            GroupActivityUpdate {
                group_id: group.group_id,
                events: vec![event],
                full_snapshot: false,
                older_cursor: None,
                has_older_messages: false,
                topic_update: None,
            },
        )
    }

    pub async fn reorder_topics(
        &self,
        path: impl AsRef<Path>,
        topic_ids: Vec<String>,
        relays: Vec<String>,
    ) -> anyhow::Result<GroupActivityResult> {
        let path = path.as_ref();
        let relays = relay_list(relays)?;
        let mut state = load_state(path)?;
        let group = state.active_group()?.clone();
        let identity = state.identity()?;
        let view = cached_group_view(&state, &group)?;
        if view.owner_public_key.as_deref() != Some(identity.public_key_base64().as_str()) {
            bail!("only the group founder can rearrange topics")
        }
        let active_topic_ids = view
            .topics
            .values()
            .filter(|topic| !topic.archived)
            .map(|topic| topic.topic_id.clone())
            .collect::<HashSet<_>>();
        let requested_topic_ids = topic_ids.iter().cloned().collect::<HashSet<_>>();
        if topic_ids.len() != requested_topic_ids.len() || requested_topic_ids != active_topic_ids {
            bail!("topic order is out of date")
        }
        let sequence = state.take_sequence();
        let event = create_group_event(
            &state,
            &identity,
            &group,
            GroupEventPayload::TopicsReordered { topic_ids },
            sequence,
        )?;
        self.publish_event(&relays, &event).await?;
        save_state(path, &state)?;
        self.apply_group_activity(
            path,
            GroupActivityUpdate {
                group_id: group.group_id,
                events: vec![event],
                full_snapshot: false,
                older_cursor: None,
                has_older_messages: false,
                topic_update: None,
            },
        )
    }

    pub async fn say_topic(
        &self,
        path: impl AsRef<Path>,
        topic_id: &str,
        text: impl Into<String>,
        attachment: Option<MediaAttachment>,
        reply_to_message_id: Option<String>,
        relays: Vec<String>,
    ) -> anyhow::Result<SentMessageResult> {
        let path = path.as_ref();
        let group_id = load_state(path)?.active_group()?.group_id.clone();
        self.say_topic_to_group(
            path,
            &group_id,
            topic_id,
            text.into(),
            attachment,
            reply_to_message_id,
            None,
            relays,
        )
        .await
    }

    pub async fn say_to_group(
        &self,
        path: impl AsRef<Path>,
        group_id: &str,
        topic_id: Option<&str>,
        text: impl Into<String>,
        attachment: Option<MediaAttachment>,
        forwarded_from: Option<ForwardedFrom>,
        relays: Vec<String>,
    ) -> anyhow::Result<SentMessageResult> {
        let path = path.as_ref();
        let text = text.into();
        if let Some(topic_id) = topic_id {
            self.say_topic_to_group(
                path,
                group_id,
                topic_id,
                text,
                attachment,
                None,
                forwarded_from,
                relays,
            )
            .await
        } else {
            self.send_message_to_group(
                path,
                group_id,
                text,
                attachment,
                None,
                forwarded_from,
                relays,
            )
            .await
        }
    }

    async fn say_topic_to_group(
        &self,
        path: &Path,
        group_id: &str,
        topic_id: &str,
        text: String,
        attachment: Option<MediaAttachment>,
        reply_to_message_id: Option<String>,
        forwarded_from: Option<ForwardedFrom>,
        relays: Vec<String>,
    ) -> anyhow::Result<SentMessageResult> {
        if text.trim().is_empty() && attachment.is_none() {
            bail!("message cannot be empty")
        }
        if let Some(attachment) = attachment.as_ref() {
            validate_media_reference(attachment)?;
        }
        validate_forwarded_from(forwarded_from.as_ref())?;
        validate_reply_reference(reply_to_message_id.as_deref())?;
        let relays = relay_list(relays)?;
        let mut state = load_state(path)?;
        let group = state
            .groups
            .iter()
            .find(|group| group.group_id == group_id)
            .cloned()
            .context("unknown group")?;
        let identity = state.identity()?;
        let topic = ensure_active_topic(&state, &group, topic_id)?;
        let existing_stream = state
            .group_event_caches
            .get(&group.group_id)
            .and_then(|cache| cache.topic_streams.get(topic_id))
            .cloned();
        let actor_public_key = identity.public_key_base64();
        if topic.locked {
            let view = cached_group_view(&state, &group)?;
            let is_manager = view.owner_public_key.as_deref() == Some(actor_public_key.as_str())
                || view.moderators.contains(&actor_public_key);
            if !is_manager {
                bail!("this topic is locked")
            }
        }
        let stream_locator = topic.stream_locator.clone();
        let sequence = state.take_sequence();
        let event = create_group_event_in_stream(
            &state,
            &identity,
            &group,
            &stream_locator,
            GroupEventPayload::TopicMessage {
                topic_id: topic_id.to_owned(),
                text,
                attachment,
                reply_to_message_id,
                forwarded_from,
            },
            sequence,
        )?;
        let sent = SentMessageResult {
            event_id: event.event_id.clone(),
            message_id: event.event_id.clone(),
            created_at_millis: event.created_at_millis,
        };
        self.publish_event(&relays, &event).await?;
        save_state(path, &state)?;
        let cursor = GroupEventCursor::from_event(&event);
        let _ = self.apply_group_activity(
            path,
            GroupActivityUpdate {
                group_id: group.group_id,
                events: vec![event],
                full_snapshot: false,
                older_cursor: None,
                has_older_messages: false,
                topic_update: Some(TopicActivityUpdate {
                    topic_id: topic_id.to_owned(),
                    latest_cursor: Some(cursor),
                    older_cursor: existing_stream
                        .as_ref()
                        .and_then(|stream| stream.older_cursor.clone()),
                    has_older_messages: existing_stream
                        .as_ref()
                        .is_some_and(|stream| stream.has_older_messages),
                    message_limit: existing_stream
                        .as_ref()
                        .map_or(INITIAL_GROUP_MESSAGE_WINDOW, |stream| stream.message_limit),
                }),
            },
        )?;
        Ok(sent)
    }

    pub async fn set_moderator(
        &self,
        path: impl AsRef<Path>,
        member_public_key: &str,
        enabled: bool,
        relays: Vec<String>,
    ) -> anyhow::Result<()> {
        let path = path.as_ref();
        let mut state = load_state(path)?;
        let relays = relay_list(relays)?;
        let group = state.active_group()?.clone();
        let identity = state.identity()?;
        let actor_public_key = identity.public_key_base64();
        // Moderator changes are authorized from the signed control state we
        // already verified while opening the group. Fetching the entire group
        // history here made this tiny mutation wait behind every historical
        // event and attachment-sized envelope before it could even be signed.
        let view = cached_group_view(&state, &group)?;
        if view.owner_public_key.as_deref() != Some(actor_public_key.as_str()) {
            bail!("only the group founder can designate moderators")
        }
        if member_public_key == actor_public_key || !view.members.contains_key(member_public_key) {
            bail!("choose an active group member")
        }
        let sequence = state.take_sequence();
        let event = create_group_event(
            &state,
            &identity,
            &group,
            GroupEventPayload::ModeratorSet {
                member_public_key: member_public_key.to_owned(),
                enabled,
            },
            sequence,
        )?;
        self.publish_event(&relays, &event).await?;
        save_state(path, &state)?;
        self.apply_published_group_event(path, &group.group_id, event)?;
        Ok(())
    }

    fn apply_published_group_event(
        &self,
        path: &Path,
        group_id: &str,
        event: SignedEvent,
    ) -> anyhow::Result<()> {
        self.apply_group_activity(
            path,
            GroupActivityUpdate {
                group_id: group_id.to_owned(),
                events: vec![event],
                full_snapshot: false,
                older_cursor: None,
                has_older_messages: false,
                topic_update: None,
            },
        )?;
        Ok(())
    }

    pub async fn delete_message(
        &self,
        path: impl AsRef<Path>,
        message_event_id: &str,
        relays: Vec<String>,
    ) -> anyhow::Result<()> {
        let path = path.as_ref();
        let mut state = load_state(path)?;
        let relays = relay_list(relays)?;
        let group = state.active_group()?.clone();
        let identity = state.identity()?;
        let actor_public_key = identity.public_key_base64();
        let cached_conversation = state
            .group_conversation_cache
            .get(&group.group_id)
            .cloned()
            .or_else(|| {
                state
                    .group_event_caches
                    .get(&group.group_id)
                    .and_then(|cache| cache.portable_conversation.clone())
            });
        let (target, is_owner, is_moderator, is_active) =
            if let Some(conversation) = cached_conversation {
                let target = conversation
                    .messages
                    .iter()
                    .find(|message| message.event_id == message_event_id)
                    .cloned()
                    .context("that message no longer exists")?;
                let actor = conversation
                    .members
                    .iter()
                    .find(|member| member.public_key == actor_public_key);
                (
                    target,
                    conversation.group.owner_public_key == actor_public_key,
                    actor.is_some_and(|member| member.is_moderator),
                    actor.is_some(),
                )
            } else {
                let cache = state
                    .group_event_caches
                    .get(&group.group_id)
                    .context("open this group and try deleting the message again")?;
                let view = rebuild_group_state(&state, &group, &cache.events)?;
                let target = view
                    .messages
                    .iter()
                    .find(|message| message.event_id == message_event_id)
                    .map(|message| MessageSummary {
                        event_id: message.event_id.clone(),
                        message_id: message.message_id.clone(),
                        author_public_key: message.author_public_key.clone(),
                        username: message.username.clone(),
                        bio: message.bio.clone(),
                        avatar: message.avatar.clone(),
                        album: message.album.clone(),
                        accepts_direct_messages: message.accepts_direct_messages,
                        direct_message_policy: message.direct_message_policy,
                        text: message.text.clone(),
                        attachment: message.attachment.clone(),
                        reply_to_message_id: message.reply_to_message_id.clone(),
                        forwarded_from: message.forwarded_from.clone(),
                        topic_id: message.topic_id.clone(),
                        created_at_millis: message.created_at_millis,
                        reactions: Vec::new(),
                    })
                    .context("that message no longer exists")?;
                (
                    target,
                    view.owner_public_key.as_deref() == Some(actor_public_key.as_str()),
                    view.moderators.contains(&actor_public_key),
                    view.members.contains_key(&actor_public_key),
                )
            };
        if !is_active
            || (!is_owner && !is_moderator && target.author_public_key != actor_public_key)
        {
            bail!("you can only delete your own messages")
        }
        let storage = target
            .attachment
            .as_ref()
            .map(attachment_storage_references)
            .unwrap_or_default();
        let sequence = state.take_sequence();
        let event = create_group_event(
            &state,
            &identity,
            &group,
            GroupEventPayload::MessageDeleted {
                message_event_id: message_event_id.to_owned(),
            },
            sequence,
        )?;
        self.publish_event(&relays, &event).await?;

        if let Some(cache) = state.group_event_caches.get_mut(&group.group_id) {
            cache.events.push(event);
            cache.latest_cursor =
                max_group_event_cursor(cache.latest_cursor.clone(), &cache.events, None);
            if let Some(conversation) = cache.portable_conversation.as_mut() {
                remove_message_from_conversation(conversation, message_event_id);
            }
        }
        if let Some(conversation) = state.group_conversation_cache.get_mut(&group.group_id) {
            remove_message_from_conversation(conversation, message_event_id);
        }
        save_state(path, &state)?;

        #[cfg(not(target_arch = "wasm32"))]
        if !storage.is_empty() {
            let client = self.clone();
            tokio::spawn(async move {
                let _ = client.erase_storage_references(storage, false).await;
            });
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = self.erase_storage_references(storage, false).await;
        }
        Ok(())
    }

    pub async fn set_reaction(
        &self,
        path: impl AsRef<Path>,
        message_event_id: &str,
        emoji: &str,
        enabled: bool,
        relays: Vec<String>,
    ) -> anyhow::Result<()> {
        if message_event_id.len() != 64
            || !message_event_id
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            bail!("reaction target is invalid")
        }
        if !valid_reaction_emoji(emoji) {
            bail!("choose a single emoji reaction")
        }
        let path = path.as_ref();
        let mut state = load_state(path)?;
        let relays = relay_list(relays)?;
        let group = state.active_group()?.clone();
        let sequence = state.take_sequence();
        let event = create_group_event(
            &state,
            &state.identity()?,
            &group,
            GroupEventPayload::ReactionSet {
                message_event_id: message_event_id.to_owned(),
                emoji: emoji.to_owned(),
                enabled,
            },
            sequence,
        )?;
        self.publish_event(&relays, &event).await?;
        save_state(path, &state)?;
        self.apply_published_group_event(path, &group.group_id, event)?;
        Ok(())
    }

    pub async fn report_message(
        &self,
        path: impl AsRef<Path>,
        message_event_id: &str,
        reason: impl Into<String>,
        relays: Vec<String>,
    ) -> anyhow::Result<()> {
        let path = path.as_ref();
        let mut state = load_state(path)?;
        let relays = relay_list(relays)?;
        let group = state.active_group()?.clone();
        let identity = state.identity()?;
        let actor_public_key = identity.public_key_base64();
        let view = cached_group_view(&state, &group)?;
        let target = view
            .messages
            .iter()
            .find(|message| message.event_id == message_event_id)
            .context("that message no longer exists")?;
        if !view.members.contains_key(&actor_public_key) {
            bail!("you are no longer a member of this group")
        }
        if target.author_public_key == actor_public_key {
            bail!("you cannot report your own message")
        }
        if view.reports.iter().any(|report| {
            report.message_event_id == message_event_id
                && report.reporter_public_key == actor_public_key
        }) {
            bail!("you already reported this message")
        }
        let reason = reason.into();
        if reason.chars().count() > 280 {
            bail!("report details can contain at most 280 characters")
        }
        let sequence = state.take_sequence();
        let event = create_group_event(
            &state,
            &identity,
            &group,
            GroupEventPayload::MessageReported {
                message_event_id: message_event_id.to_owned(),
                reason,
            },
            sequence,
        )?;
        self.publish_event(&relays, &event).await?;
        save_state(path, &state)?;
        self.apply_published_group_event(path, &group.group_id, event)?;
        Ok(())
    }

    pub async fn resolve_report(
        &self,
        path: impl AsRef<Path>,
        report_event_id: &str,
        relays: Vec<String>,
    ) -> anyhow::Result<()> {
        let path = path.as_ref();
        let mut state = load_state(path)?;
        let relays = relay_list(relays)?;
        let group = state.active_group()?.clone();
        let identity = state.identity()?;
        let actor_public_key = identity.public_key_base64();
        let view = cached_group_view(&state, &group)?;
        let is_owner = view.owner_public_key.as_deref() == Some(actor_public_key.as_str());
        let is_moderator = view.moderators.contains(&actor_public_key);
        if (!is_owner && !is_moderator) || !view.members.contains_key(&actor_public_key) {
            bail!("only the founder or a moderator can resolve reports")
        }
        if !view
            .reports
            .iter()
            .any(|report| report.event_id == report_event_id)
        {
            bail!("that report has already been actioned")
        }
        let sequence = state.take_sequence();
        let event = create_group_event(
            &state,
            &identity,
            &group,
            GroupEventPayload::ReportResolved {
                report_event_id: report_event_id.to_owned(),
            },
            sequence,
        )?;
        self.publish_event(&relays, &event).await?;
        save_state(path, &state)?;
        self.apply_published_group_event(path, &group.group_id, event)?;
        Ok(())
    }

    pub async fn ban_member(
        &self,
        path: impl AsRef<Path>,
        member_public_key: &str,
        delete_messages: bool,
        relays: Vec<String>,
    ) -> anyhow::Result<()> {
        let path = path.as_ref();
        let mut state = load_state(path)?;
        let relays = relay_list(relays)?;
        let group = state.active_group()?.clone();
        let identity = state.identity()?;
        let actor_public_key = identity.public_key_base64();
        let view = cached_group_view(&state, &group)?;
        let is_owner = view.owner_public_key.as_deref() == Some(actor_public_key.as_str());
        let is_moderator = view.moderators.contains(&actor_public_key);
        if (!is_owner && !is_moderator) || !view.members.contains_key(&actor_public_key) {
            bail!("only the founder or a moderator can ban members")
        }
        if member_public_key == actor_public_key
            || view.owner_public_key.as_deref() == Some(member_public_key)
            || !view.members.contains_key(member_public_key)
        {
            bail!("that member cannot be banned")
        }
        if !is_owner && view.moderators.contains(member_public_key) {
            bail!("only the founder can ban another moderator")
        }
        let removal_request = state
            .mls_control_logs
            .contains_key(&group.group_id)
            .then(|| {
                MlsRemovalRequest::member_banned(
                    &identity,
                    group.group_id.clone(),
                    member_public_key,
                    delete_messages,
                )
            })
            .transpose()?;
        let storage = if delete_messages {
            view.messages
                .iter()
                .filter(|message| message.author_public_key == member_public_key)
                .filter_map(|message| message.attachment.as_ref())
                .flat_map(attachment_storage_references)
                .collect()
        } else {
            Vec::new()
        };
        let sequence = state.take_sequence();
        let event = create_group_event(
            &state,
            &identity,
            &group,
            GroupEventPayload::MemberBanned {
                member_public_key: member_public_key.to_owned(),
                delete_messages,
            },
            sequence,
        )?;
        self.publish_event(&relays, &event).await?;
        save_state(path, &state)?;
        self.apply_published_group_event(path, &group.group_id, event)?;
        if let Some(request) = removal_request {
            self.publish_mls_removal_request(&relays, &request).await?;
        }
        let _ = self.erase_storage_references(storage, false).await;
        Ok(())
    }

    pub async fn unban_member(
        &self,
        path: impl AsRef<Path>,
        member_public_key: &str,
        relays: Vec<String>,
    ) -> anyhow::Result<()> {
        let path = path.as_ref();
        let mut state = load_state(path)?;
        let relays = relay_list(relays)?;
        let group = state.active_group()?.clone();
        let identity = state.identity()?;
        let actor_public_key = identity.public_key_base64();
        let view = cached_group_view(&state, &group)?;
        if view.owner_public_key.as_deref() != Some(actor_public_key.as_str())
            || !view.members.contains_key(&actor_public_key)
        {
            bail!("only the group founder can unban members")
        }
        if !view.banned_members.contains(member_public_key) {
            bail!("that identity is not banned")
        }
        let sequence = state.take_sequence();
        let event = create_group_event(
            &state,
            &identity,
            &group,
            GroupEventPayload::MemberUnbanned {
                member_public_key: member_public_key.to_owned(),
            },
            sequence,
        )?;
        self.publish_event(&relays, &event).await?;
        save_state(path, &state)?;
        self.apply_published_group_event(path, &group.group_id, event)?;
        Ok(())
    }

    pub async fn leave(
        &self,
        path: impl AsRef<Path>,
        cache_path: impl AsRef<Path>,
        relays: Vec<String>,
    ) -> anyhow::Result<LocalSummary> {
        let path = path.as_ref();
        let mut state = load_state(path)?;
        let relays = relay_list(relays)?;
        let identity = state.identity()?;
        let sequence = state.take_sequence();
        let group = state.active_group()?.clone();
        if group.owner_public_key == identity.public_key_base64() {
            bail!("the founder must delete the group instead of leaving it")
        }
        let event = create_group_event(
            &state,
            &identity,
            &group,
            GroupEventPayload::MemberLeft,
            sequence,
        )?;
        let removal_request = state
            .mls_control_logs
            .contains_key(&group.group_id)
            .then(|| MlsRemovalRequest::self_left(&identity, group.group_id.clone()))
            .transpose()?;
        save_state(path, &state)?;
        self.publish_event(&relays, &event).await?;
        if let Some(request) = removal_request {
            self.publish_mls_removal_request(&relays, &request).await?;
        }
        purge_group_cache(cache_path.as_ref(), &group.group_id)?;
        purge_profile_image_cache(cache_path.as_ref())?;
        state
            .forget_mls_group(&group.group_id)
            .context("could not erase this group's local encryption state")?;
        state.mls_join_requests.remove(&group.group_id);
        state.mls_local_geneses.remove(&group.group_id);
        state.mls_control_logs.remove(&group.group_id);
        state.group_frequencies.remove(&group.group_id);
        state.forget_group_activity(&group.group_id);
        state.group_conversation_cache.remove(&group.group_id);
        state.group_event_caches.remove(&group.group_id);
        state.tombstone_group(&group.group_id);
        save_state(path, &state)?;
        state.summary()
    }

    pub async fn delete_group(
        &self,
        path: impl AsRef<Path>,
        cache_path: impl AsRef<Path>,
        group_id: &str,
        relays: Vec<String>,
    ) -> anyhow::Result<LocalSummary> {
        let path = path.as_ref();
        let mut state = load_state(path)?;
        let group_index = state
            .groups
            .iter()
            .position(|group| group.group_id == group_id)
            .context("group is missing from local state")?;
        let group = state.groups[group_index].clone();
        let identity = state.identity()?;
        if group.owner_public_key != identity.public_key_base64() {
            bail!("only the group founder can delete it")
        }

        // Groups created before authenticated deletion existed have no
        // authority nonce. They can still be removed locally; current relays
        // cannot safely accept a purge claim for those legacy identifiers.
        if !group.authority_nonce_base64.is_empty() {
            let deletion = GroupDeletion::create(&identity, &group)?;
            let relays = relay_list(relays)?;
            let cached_events = state
                .group_event_caches
                .get(group_id)
                .map(|cache| cache.events.clone())
                .unwrap_or_default();
            let storage = group_storage_references(&group, &cached_events);
            self.publish_group_deletion(&relays, &deletion).await?;
            if !storage.is_empty() {
                let cleanup_client = self.clone();
                #[cfg(not(target_arch = "wasm32"))]
                tokio::spawn(async move {
                    let _ = cleanup_client.erase_storage_references(storage, true).await;
                });
                #[cfg(target_arch = "wasm32")]
                wasm_bindgen_futures::spawn_local(async move {
                    let _ = cleanup_client.erase_storage_references(storage, true).await;
                });
            }
        }

        purge_group_cache(cache_path.as_ref(), group_id)?;
        purge_profile_image_cache(cache_path.as_ref())?;
        state
            .forget_mls_group(group_id)
            .context("could not erase this group's local encryption state")?;
        state.group_frequencies.remove(group_id);
        state.forget_group_activity(group_id);
        state.group_conversation_cache.remove(group_id);
        state.group_event_caches.remove(group_id);
        state.mls_join_requests.remove(group_id);
        state.mls_local_geneses.remove(group_id);
        state.mls_control_logs.remove(group_id);
        state.tombstone_group(group_id);
        save_state(path, &state)?;
        state.summary()
    }

    pub async fn delete_account(
        &self,
        path: impl AsRef<Path>,
        cache_path: impl AsRef<Path>,
        delete_group_messages: bool,
        delete_direct_threads: bool,
        relays: Vec<String>,
    ) -> anyhow::Result<()> {
        let path = path.as_ref();
        let cache_path = cache_path.as_ref();
        let mut state = load_state(path)?;
        let identity = state.identity()?;
        let self_public_key = identity.public_key_base64();
        let relays = relay_list(relays)?;

        // Remote cleanup must finish before the identity key is removed. If a
        // relay is unavailable, the account remains intact so the user can retry.
        for group in state.groups.clone() {
            if group.owner_public_key == self_public_key && !group.authority_nonce_base64.is_empty()
            {
                let deletion = GroupDeletion::create(&identity, &group)?;
                let events = self.fetch_events(&group, relays.clone()).await?;
                self.erase_storage_references(group_storage_references(&group, &events), true)
                    .await?;
                self.publish_group_deletion(&relays, &deletion).await?;
                continue;
            }
            if delete_group_messages {
                let events = self.fetch_events(&group, relays.clone()).await?;
                let view = rebuild_group_state(&state, &group, &events)?;
                let storage = view
                    .messages
                    .iter()
                    .filter(|message| message.author_public_key == self_public_key)
                    .filter_map(|message| message.attachment.as_ref())
                    .flat_map(attachment_storage_references)
                    .collect();
                let sequence = state.take_sequence();
                let event = create_group_event(
                    &state,
                    &identity,
                    &group,
                    GroupEventPayload::OwnMessagesDeleted,
                    sequence,
                )?;
                self.publish_event(&relays, &event).await?;
                let _ = self.erase_storage_references(storage, false).await;
            }
            let sequence = state.take_sequence();
            let event = create_group_event(
                &state,
                &identity,
                &group,
                GroupEventPayload::MemberLeft,
                sequence,
            )?;
            self.publish_event(&relays, &event).await?;
        }

        if delete_direct_threads {
            for contact in state.direct_contacts.clone() {
                let recipient_mailbox =
                    identity.direct_mailbox(&contact.public_key, &contact.public_key)?;
                let sender_mailbox =
                    identity.direct_mailbox(&contact.public_key, &self_public_key)?;
                let sequence = state.take_sequence();
                let recipient_event = SignedEvent::direct_thread_deleted(
                    &identity,
                    &recipient_mailbox,
                    &contact.public_key,
                    sequence,
                )?;
                let sender_event = SignedEvent::direct_thread_deleted(
                    &identity,
                    &sender_mailbox,
                    &contact.public_key,
                    sequence,
                )?;
                let mut storage = direct_storage_references(
                    &recipient_mailbox,
                    &self
                        .fetch_events(&recipient_mailbox, relays.clone())
                        .await?,
                );
                storage.extend(direct_storage_references(
                    &sender_mailbox,
                    &self.fetch_events(&sender_mailbox, relays.clone()).await?,
                ));
                self.publish_event(&relays, &recipient_event).await?;
                self.publish_event(&relays, &sender_event).await?;
                self.erase_storage_references(storage, true).await?;
            }
        }

        if let Some(album) = state.profile.album.as_ref() {
            let stored = self.open_profile_album(&self_public_key, album).await?;
            let mut storage = stored
                .items
                .iter()
                .flat_map(|item| attachment_storage_references(&item.attachment))
                .collect::<Vec<_>>();
            storage.extend(profile_album_storage_reference(album));
            self.erase_storage_references(storage, true).await?;
        }

        if let Some(account) = state.account.as_ref() {
            let revision = account
                .revision
                .checked_add(1)
                .context("account vault revision is exhausted")?;
            let tombstone = AccountVault::tombstone(&identity, account.locator.clone(), revision)?;
            self.publish_account_vault(&relays, &tombstone).await?;
        }

        let media_directory = cache_path.join("media");
        if media_directory.exists() {
            fs::remove_dir_all(&media_directory)
                .with_context(|| format!("could not erase {}", media_directory.display()))?;
        }
        purge_profile_image_cache(cache_path)?;
        remove_state(path)?;
        Ok(())
    }

    pub async fn sync_group_activity(
        &self,
        path: impl AsRef<Path>,
        group_id: &str,
        relays: Vec<String>,
    ) -> anyhow::Result<GroupActivityResult> {
        let update = self
            .fetch_group_activity(path.as_ref(), group_id, relays)
            .await?;
        self.apply_group_activity(path, update)
    }

    pub async fn sync_group_activity_and_mark_read(
        &self,
        path: impl AsRef<Path>,
        group_id: &str,
        topic_id: Option<&str>,
        relays: Vec<String>,
    ) -> anyhow::Result<GroupActivityResult> {
        let path = path.as_ref();
        let update = self.fetch_group_activity(path, group_id, relays).await?;
        self.apply_group_activity_and_mark_read(path, update, topic_id)
    }

    pub async fn sync_topic_activity(
        &self,
        path: impl AsRef<Path>,
        group_id: &str,
        topic_id: &str,
        relays: Vec<String>,
    ) -> anyhow::Result<GroupActivityResult> {
        let update = self
            .fetch_topic_activity(path.as_ref(), group_id, topic_id, relays)
            .await?;
        self.apply_group_activity(path, update)
    }

    pub async fn sync_topic_activity_and_mark_read(
        &self,
        path: impl AsRef<Path>,
        group_id: &str,
        topic_id: &str,
        relays: Vec<String>,
    ) -> anyhow::Result<GroupActivityResult> {
        let path = path.as_ref();
        let update = self
            .fetch_topic_activity(path, group_id, topic_id, relays)
            .await?;
        self.apply_group_activity_and_mark_read(path, update, Some(topic_id))
    }

    pub async fn fetch_topic_activity(
        &self,
        path: impl AsRef<Path>,
        group_id: &str,
        topic_id: &str,
        relays: Vec<String>,
    ) -> anyhow::Result<GroupActivityUpdate> {
        let path = path.as_ref();
        let state = load_state(path)?;
        let group = state
            .groups
            .iter()
            .find(|group| group.group_id == group_id)
            .cloned()
            .context("unknown group")?;
        let topic = ensure_active_topic(&state, &group, topic_id)?;
        let stream = state
            .group_event_caches
            .get(group_id)
            .and_then(|cache| cache.topic_streams.get(topic_id))
            .cloned();
        let has_cached_stream_events =
            state.group_event_caches.get(group_id).is_some_and(|cache| {
                cache.events.iter().any(|event| {
                    event.stream_locator.as_deref() == Some(topic.stream_locator.as_str())
                })
            });
        let relays = relay_list(relays)?;
        // A stream with no older cursor has never established where its history
        // ends. Account recovery seeds streams that way, and an incremental
        // `After` request can never repair it, so re-anchor from the latest page.
        let stream_anchored = stream
            .as_ref()
            .is_some_and(|stream| stream.older_cursor.is_some());
        let request = if has_cached_stream_events && stream_anchored {
            stream
                .as_ref()
                .and_then(|stream| stream.latest_cursor.as_ref())
                .map_or(GroupEventPageRequest::Latest, GroupEventPageRequest::After)
        } else {
            GroupEventPageRequest::Latest
        };
        let mut page = self
            .fetch_group_event_page(group_id, Some(&topic.stream_locator), request, &relays)
            .await?
            .context("the configured relays do not support topic streams")?;
        let is_latest = matches!(request, GroupEventPageRequest::Latest);
        // Relays cap a page by byte length, so a topic carrying media can answer
        // with only a handful of events. Walk back until the opening window is
        // filled instead of showing a fraction of the recent messages.
        if is_latest {
            let mut events = std::mem::take(&mut page.events);
            let mut oldest = page.continuation_cursor.clone();
            while page.has_more
                && events.len() < INITIAL_GROUP_MESSAGE_WINDOW
                && let Some(cursor) = oldest.clone()
            {
                let Some(older) = self
                    .fetch_group_event_page(
                        group_id,
                        Some(&topic.stream_locator),
                        GroupEventPageRequest::Before(&cursor),
                        &relays,
                    )
                    .await?
                else {
                    break;
                };
                if older.events.is_empty() {
                    page.has_more = older.has_more;
                    break;
                }
                let Some(next) = older.continuation_cursor.clone() else {
                    events.splice(0..0, older.events);
                    page.has_more = older.has_more;
                    break;
                };
                if next.key() >= cursor.key() {
                    break;
                }
                events.splice(0..0, older.events);
                oldest = Some(next);
                page.has_more = older.has_more;
            }
            page.events = events;
            page.continuation_cursor = oldest;
        }
        let latest_cursor = match request {
            GroupEventPageRequest::Latest | GroupEventPageRequest::After(_) => page
                .events
                .last()
                .map(GroupEventCursor::from_event)
                .or_else(|| {
                    stream
                        .as_ref()
                        .and_then(|stream| stream.latest_cursor.clone())
                }),
            GroupEventPageRequest::Before(_) => unreachable!(),
        };
        Ok(GroupActivityUpdate {
            group_id: group_id.to_owned(),
            events: page.events,
            full_snapshot: false,
            older_cursor: None,
            has_older_messages: false,
            topic_update: Some(TopicActivityUpdate {
                topic_id: topic_id.to_owned(),
                latest_cursor,
                older_cursor: if is_latest {
                    page.continuation_cursor
                } else {
                    stream
                        .as_ref()
                        .and_then(|stream| stream.older_cursor.clone())
                },
                has_older_messages: if is_latest {
                    page.has_more
                } else {
                    stream
                        .as_ref()
                        .is_some_and(|stream| stream.has_older_messages)
                },
                message_limit: stream
                    .as_ref()
                    .map_or(INITIAL_GROUP_MESSAGE_WINDOW, |stream| stream.message_limit),
            }),
        })
    }

    pub async fn load_older_topic_history(
        &self,
        path: impl AsRef<Path>,
        group_id: &str,
        topic_id: &str,
        relays: Vec<String>,
    ) -> anyhow::Result<Conversation> {
        let path = path.as_ref();
        let state = load_state(path)?;
        let group = state
            .groups
            .iter()
            .find(|group| group.group_id == group_id)
            .cloned()
            .context("unknown group")?;
        let topic = ensure_active_topic(&state, &group, topic_id)?;
        let stream = state
            .group_event_caches
            .get(group_id)
            .and_then(|cache| cache.topic_streams.get(topic_id))
            .cloned()
            .context("topic history is not available yet")?;
        if !stream.has_older_messages {
            return state
                .group_conversation_cache
                .get(group_id)
                .cloned()
                .context("open this topic and try again");
        }
        let mut cursor = stream
            .older_cursor
            .clone()
            .context("topic history cursor is unavailable")?;
        let relays = relay_list(relays)?;
        let mut events = Vec::new();
        let mut has_older_messages;
        loop {
            let page = self
                .fetch_group_event_page(
                    group_id,
                    Some(&topic.stream_locator),
                    GroupEventPageRequest::Before(&cursor),
                    &relays,
                )
                .await?
                .context("the configured relays do not support topic streams")?;
            events.extend(page.events);
            has_older_messages = page.has_more;
            let Some(next_cursor) = page.continuation_cursor else {
                break;
            };
            if next_cursor.key() >= cursor.key() {
                bail!("relay returned a non-advancing topic history page")
            }
            cursor = next_cursor;
            if events.len() >= INITIAL_GROUP_MESSAGE_WINDOW || !page.has_more {
                break;
            }
        }
        let result = self.apply_group_activity(
            path,
            GroupActivityUpdate {
                group_id: group_id.to_owned(),
                events,
                full_snapshot: false,
                older_cursor: None,
                has_older_messages: false,
                topic_update: Some(TopicActivityUpdate {
                    topic_id: topic_id.to_owned(),
                    latest_cursor: stream.latest_cursor,
                    older_cursor: Some(cursor),
                    has_older_messages,
                    message_limit: stream
                        .message_limit
                        .saturating_add(INITIAL_GROUP_MESSAGE_WINDOW),
                }),
            },
        )?;
        result
            .conversation
            .context("this identity is not an active group member")
    }

    pub async fn load_older_group_history(
        &self,
        path: impl AsRef<Path>,
        group_id: &str,
        relays: Vec<String>,
    ) -> anyhow::Result<Conversation> {
        let path = path.as_ref();
        let mut state = load_state(path)?;
        let group = state
            .groups
            .iter()
            .find(|group| group.group_id == group_id)
            .cloned()
            .context("unknown group")?;
        let identity_public_key = state.identity()?.public_key_base64();
        let mut existing = state
            .group_event_caches
            .get(group_id)
            .cloned()
            .context("group history is not available yet")?;
        if !existing.has_older_messages {
            if let Some(mut conversation) = existing.portable_conversation.clone() {
                filter_conversation_for_blocks(&mut conversation, &state.active_hidden_keys());
                return Ok(conversation);
            }
            let view = rebuild_group_state(&state, &group, &existing.events)?;
            let mut conversation =
                cached_conversation_from_view(&state, &group, view, &identity_public_key);
            conversation.has_older_messages = false;
            filter_conversation_for_blocks(&mut conversation, &state.active_hidden_keys());
            return Ok(conversation);
        }

        let relays = relay_list(relays)?;
        if existing.needs_recent_hydration {
            if let Some(page) = self
                .fetch_group_event_page(group_id, None, GroupEventPageRequest::Latest, &relays)
                .await?
            {
                let mut hydrated_events = existing.events.clone();
                hydrated_events.extend(page.events);
                existing = compact_group_event_cache(
                    &state,
                    &group,
                    hydrated_events,
                    existing.message_limit,
                    existing.latest_cursor.clone(),
                    page.continuation_cursor
                        .or_else(|| existing.older_cursor.clone()),
                    page.has_more || existing.has_older_messages,
                    existing.topic_streams.clone(),
                )?;
            } else {
                existing = compact_group_event_cache(
                    &state,
                    &group,
                    self.fetch_events(&group, relays.clone()).await?,
                    existing.message_limit,
                    existing.latest_cursor.clone(),
                    None,
                    false,
                    existing.topic_streams.clone(),
                )?;
            }
        }
        let mut events = existing.events.clone();
        let mut older_cursor = existing.older_cursor.clone();
        let mut has_older_hint = false;
        let mut used_page = false;
        let target_message_count = rebuild_group_state(&state, &group, &events)?
            .messages
            .len()
            .saturating_add(INITIAL_GROUP_MESSAGE_WINDOW);
        if let Some(mut cursor) = existing.older_cursor.clone() {
            loop {
                let Some(page) = self
                    .fetch_group_event_page(
                        group_id,
                        None,
                        GroupEventPageRequest::Before(&cursor),
                        &relays,
                    )
                    .await?
                else {
                    break;
                };
                used_page = true;
                events.extend(page.events);
                older_cursor = page.continuation_cursor.clone();
                has_older_hint = page.has_more;
                if rebuild_group_state(&state, &group, &events)?.messages.len()
                    >= target_message_count
                    || !page.has_more
                {
                    break;
                }
                let Some(next_cursor) = page.continuation_cursor else {
                    break;
                };
                if next_cursor.key() >= cursor.key() {
                    bail!("relay returned a non-advancing group history page")
                }
                cursor = next_cursor;
            }
        }
        if !used_page {
            events = self.fetch_events(&group, relays).await?;
            older_cursor = None;
        }

        let cache = compact_group_event_cache(
            &state,
            &group,
            events,
            existing
                .message_limit
                .saturating_add(INITIAL_GROUP_MESSAGE_WINDOW),
            existing.latest_cursor.clone(),
            older_cursor,
            has_older_hint,
            existing.topic_streams.clone(),
        )?;
        let view = rebuild_group_state(&state, &group, &cache.events)?;
        if !view.members.contains_key(&identity_public_key) {
            bail!("this identity is not an active group member")
        }
        let mut conversation =
            cached_conversation_from_view(&state, &group, view, &identity_public_key);
        conversation.has_older_messages = cache.has_older_messages;
        filter_conversation_for_blocks(&mut conversation, &state.active_hidden_keys());
        state.group_event_caches.insert(group_id.to_owned(), cache);
        state
            .group_conversation_cache
            .insert(group_id.to_owned(), conversation.clone());
        save_state(path, &state)?;
        Ok(conversation)
    }

    pub async fn load_older_direct_history(
        &self,
        path: impl AsRef<Path>,
        cache_path: impl AsRef<Path>,
        relays: Vec<String>,
    ) -> anyhow::Result<bool> {
        let path = path.as_ref();
        let cache_path = cache_path.as_ref();
        let mut state = load_state(path)?;
        let identity = state.identity()?;
        let mailbox_id = direct_mailbox_id(&identity.public_key_base64())?;
        let mut cache = state.direct_event_cache.clone();
        if !cache.has_older_events {
            return Ok(false);
        }
        let cursor = cache
            .events
            .iter()
            .filter(|event| event.stream_locator.is_none())
            .map(GroupEventCursor::from_event)
            .min_by(|left, right| left.key().cmp(&right.key()))
            .context("direct-message history cursor is unavailable")?;
        let relay_descriptors = relay_list(relays)?;
        let page = self
            .fetch_group_event_page(
                &mailbox_id,
                None,
                GroupEventPageRequest::Before(&cursor),
                &relay_descriptors,
            )
            .await?
            .context("the configured relays do not support direct-message history pages")?;
        cache.events.extend(page.events);
        cache.events = merge_group_events(&mailbox_id, cache.events);
        cache.has_older_events = page.has_more;
        let events = cache.events.clone();
        state.direct_event_cache = cache;
        self.apply_direct_events(path, cache_path, state, events, relay_descriptors, true)
            .await?;
        Ok(page.has_more)
    }

    pub async fn fetch_group_activity(
        &self,
        path: impl AsRef<Path>,
        group_id: &str,
        relays: Vec<String>,
    ) -> anyhow::Result<GroupActivityUpdate> {
        let path = path.as_ref();
        let state = load_state(path)?;
        let group = state
            .groups
            .iter()
            .find(|group| group.group_id == group_id)
            .cloned()
            .context("unknown group")?;
        let relays = relay_list(relays)?;
        if let Some(cache) = state
            .group_event_caches
            .get(group_id)
            .filter(|cache| cache.needs_recent_hydration)
            && let Some(page) = self
                .fetch_group_event_page(group_id, None, GroupEventPageRequest::Latest, &relays)
                .await?
        {
            return Ok(GroupActivityUpdate {
                group_id: group_id.to_owned(),
                events: page.events,
                full_snapshot: false,
                older_cursor: page
                    .continuation_cursor
                    .or_else(|| cache.older_cursor.clone()),
                has_older_messages: page.has_more || cache.has_older_messages,
                topic_update: None,
            });
        }
        if state.group_event_caches.get(group_id).is_some_and(|cache| {
            cache.needs_control_hydration
                && !group_cache_has_usable_control_state(&state, &group, cache)
        }) {
            let mut events = self.fetch_events(&group, relays.clone()).await?;
            let topic_events = self
                .fetch_latest_topic_events(&state, &group, &events, &relays)
                .await;
            events.extend(topic_events);
            return Ok(GroupActivityUpdate {
                group_id: group_id.to_owned(),
                events,
                full_snapshot: true,
                older_cursor: None,
                has_older_messages: false,
                topic_update: None,
            });
        }
        if let Some(cache) = state.group_event_caches.get(group_id)
            && let Ok(view) = rebuild_group_state(&state, &group, &cache.events)
        {
            let missing_topic_events = view.topics.values().any(|topic| {
                !topic.archived
                    && cache
                        .topic_streams
                        .get(&topic.topic_id)
                        .is_some_and(|stream| stream.latest_cursor.is_some())
                    && !cache.events.iter().any(|event| {
                        event.stream_locator.as_deref() == Some(topic.stream_locator.as_str())
                    })
            });
            if missing_topic_events {
                let events = self
                    .fetch_latest_topic_events(&state, &group, &cache.events, &relays)
                    .await;
                if !events.is_empty() {
                    return Ok(GroupActivityUpdate {
                        group_id: group_id.to_owned(),
                        events,
                        full_snapshot: false,
                        older_cursor: cache.older_cursor.clone(),
                        has_older_messages: cache.has_older_messages,
                        topic_update: None,
                    });
                }
            }
        }
        if let Some(cache) = state.group_event_caches.get(group_id)
            && let Some(mut cursor) = cache.latest_cursor.clone()
        {
            let mut events = Vec::new();
            loop {
                let Some(page) = self
                    .fetch_group_event_page(
                        group_id,
                        None,
                        GroupEventPageRequest::After(&cursor),
                        &relays,
                    )
                    .await?
                else {
                    break;
                };
                events.extend(page.events);
                let Some(next_cursor) = page.continuation_cursor else {
                    return Ok(GroupActivityUpdate {
                        group_id: group_id.to_owned(),
                        events,
                        full_snapshot: false,
                        older_cursor: cache.older_cursor.clone(),
                        has_older_messages: cache.has_older_messages,
                        topic_update: None,
                    });
                };
                if next_cursor.key() <= cursor.key() {
                    bail!("relay returned a non-advancing group history page")
                }
                cursor = next_cursor;
                if !page.has_more {
                    return Ok(GroupActivityUpdate {
                        group_id: group_id.to_owned(),
                        events,
                        full_snapshot: false,
                        older_cursor: cache.older_cursor.clone(),
                        has_older_messages: cache.has_older_messages,
                        topic_update: None,
                    });
                }
            }
        }
        if let Some(page) = self
            .fetch_group_event_page(group_id, None, GroupEventPageRequest::Latest, &relays)
            .await?
        {
            return Ok(GroupActivityUpdate {
                group_id: group_id.to_owned(),
                events: page.events,
                full_snapshot: false,
                older_cursor: page.continuation_cursor,
                has_older_messages: page.has_more,
                topic_update: None,
            });
        }
        let mut events = self.fetch_events(&group, relays.clone()).await?;
        let topic_events = self
            .fetch_latest_topic_events(&state, &group, &events, &relays)
            .await;
        events.extend(topic_events);
        Ok(GroupActivityUpdate {
            group_id: group_id.to_owned(),
            events,
            full_snapshot: true,
            older_cursor: None,
            has_older_messages: false,
            topic_update: None,
        })
    }

    async fn fetch_latest_topic_events(
        &self,
        state: &ClientState,
        group: &GroupMembership,
        control_events: &[SignedEvent],
        relays: &[RelayDescriptor],
    ) -> Vec<SignedEvent> {
        let Ok(view) = rebuild_group_state(state, group, control_events) else {
            return Vec::new();
        };
        let requests = futures_util::stream::iter(
            view.topics
                .values()
                .filter(|topic| !topic.archived)
                .map(|topic| topic.stream_locator.clone())
                .collect::<Vec<_>>(),
        )
        .map(|stream_locator| async move {
            self.fetch_group_event_page(
                &group.group_id,
                Some(&stream_locator),
                GroupEventPageRequest::Latest,
                relays,
            )
            .await
        })
        .buffer_unordered(TOPIC_RECOVERY_CONCURRENCY);
        futures_util::pin_mut!(requests);
        let mut events = Vec::new();
        while let Some(result) = requests.next().await {
            if let Ok(Some(page)) = result {
                events.extend(page.events);
            }
        }
        events
    }

    pub fn apply_group_activity(
        &self,
        path: impl AsRef<Path>,
        update: GroupActivityUpdate,
    ) -> anyhow::Result<GroupActivityResult> {
        let path = path.as_ref();
        let mut state = load_state(path)?;
        let group = state
            .groups
            .iter()
            .find(|group| group.group_id == update.group_id)
            .cloned()
            .context("unknown group")?;
        let identity_public_key = state.identity()?.public_key_base64();
        let group_id = group.group_id.clone();
        let existing_cache = state.group_event_caches.get(&group_id).cloned();
        let existing_control_is_usable = existing_cache
            .as_ref()
            .is_some_and(|cache| group_cache_has_usable_control_state(&state, &group, cache));
        let full_snapshot = update.full_snapshot;
        let portable_group_state = existing_cache
            .as_ref()
            .and_then(|cache| cache.portable_conversation.clone());
        let message_limit = existing_cache
            .as_ref()
            .map_or(INITIAL_GROUP_MESSAGE_WINDOW, |cache| cache.message_limit);
        let mut topic_streams = existing_cache
            .as_ref()
            .map(|cache| cache.topic_streams.clone())
            .unwrap_or_default();
        if let Some(topic_update) = update.topic_update {
            let stream = topic_streams
                .entry(topic_update.topic_id)
                .or_insert_with(|| TopicStreamCache {
                    latest_cursor: None,
                    older_cursor: None,
                    message_limit: INITIAL_GROUP_MESSAGE_WINDOW,
                    has_older_messages: false,
                });
            stream.latest_cursor = [stream.latest_cursor.clone(), topic_update.latest_cursor]
                .into_iter()
                .flatten()
                .max_by(|left, right| left.key().cmp(&right.key()));
            stream.older_cursor = topic_update
                .older_cursor
                .or_else(|| stream.older_cursor.clone());
            stream.has_older_messages = topic_update.has_older_messages;
            stream.message_limit = topic_update.message_limit.max(1);
        }
        let mut events = if full_snapshot {
            existing_cache
                .as_ref()
                .map(|cache| {
                    cache
                        .events
                        .iter()
                        .filter(|event| event.stream_locator.is_some())
                        .cloned()
                        .collect()
                })
                .unwrap_or_default()
        } else {
            existing_cache
                .as_ref()
                .map(|cache| cache.events.clone())
                .unwrap_or_default()
        };
        events.extend(update.events);
        let mut cache = compact_group_event_cache(
            &state,
            &group,
            events,
            message_limit,
            existing_cache
                .as_ref()
                .and_then(|cache| cache.latest_cursor.clone()),
            update.older_cursor.or_else(|| {
                existing_cache
                    .as_ref()
                    .and_then(|cache| cache.older_cursor.clone())
            }),
            update.has_older_messages
                || existing_cache
                    .as_ref()
                    .is_some_and(|cache| cache.has_older_messages),
            topic_streams,
        )?;
        cache.needs_control_hydration = !full_snapshot
            && existing_cache.as_ref().map_or(true, |cache| {
                cache.needs_control_hydration && !existing_control_is_usable
            });
        if !full_snapshot {
            cache.portable_conversation = portable_group_state.clone();
        }
        let view = rebuild_group_state(&state, &group, &cache.events)?;
        let is_member = view.members.contains_key(&identity_public_key)
            || portable_group_state.as_ref().is_some_and(|conversation| {
                conversation
                    .members
                    .iter()
                    .any(|member| member.public_key == identity_public_key)
            });
        let mut state_changed = existing_cache.as_ref() != Some(&cache)
            || is_member
                && state.record_group_activity(&group_id, &view.messages, &identity_public_key);
        state
            .group_event_caches
            .insert(group_id.clone(), cache.clone());
        let resolved_owner = view.owner_public_key.clone().unwrap_or_else(|| {
            state
                .groups
                .iter()
                .find(|group| group.group_id == group_id)
                .map(|group| group.owner_public_key.clone())
                .unwrap_or_default()
        });
        if !cache.needs_control_hydration
            && state.apply_resolved_group_profile(&group_id, &view.profile, &resolved_owner)
        {
            state_changed = true;
        }
        let known_people_before = state.known_people.clone();
        let direct_contacts_before = state.direct_contacts.clone();
        for member in view.members.values() {
            state.upsert_known_person(DirectContact {
                public_key: member.public_key.clone(),
                username: member.username.clone(),
                bio: member.bio.clone(),
                avatar: member.avatar.clone(),
                album: member.album.clone(),
                accepts_direct_messages: member.accepts_direct_messages,
                direct_message_policy: member.direct_message_policy,
                profile_sequence: member.profile_sequence,
            });
        }
        if state.known_people != known_people_before
            || state.direct_contacts != direct_contacts_before
        {
            state_changed = true;
        }
        let conversation = is_member.then(|| {
            let mut conversation =
                cached_conversation_from_view(&state, &group, view, &identity_public_key);
            if let Some(portable) = portable_group_state.as_ref()
                && cache.needs_control_hydration
            {
                merge_portable_group_state(&mut conversation, portable);
            }
            conversation.has_older_messages = cache.has_older_messages;
            let blocked = state.active_hidden_keys();
            filter_conversation_for_blocks(&mut conversation, &blocked);
            conversation
        });
        let conversation_cache_changed = conversation.as_ref().is_some_and(|conversation| {
            state.group_conversation_cache.get(&group_id) != Some(conversation)
        });
        if let Some(conversation) = conversation.as_ref() {
            state
                .group_conversation_cache
                .insert(group_id, conversation.clone());
        }
        if state_changed || conversation_cache_changed {
            save_state(path, &state)?;
        }
        Ok(GroupActivityResult {
            summary: state.summary()?,
            conversation,
        })
    }

    pub fn apply_group_activity_and_mark_read(
        &self,
        path: impl AsRef<Path>,
        update: GroupActivityUpdate,
        topic_id: Option<&str>,
    ) -> anyhow::Result<GroupActivityResult> {
        let path = path.as_ref();
        let group_id = update.group_id.clone();
        self.apply_group_activity(path, update)?;
        let summary = self.mark_topic_read(path, &group_id, topic_id)?;
        let conversation = self.cached_conversation(path, &group_id)?;
        Ok(GroupActivityResult {
            summary,
            conversation,
        })
    }

    pub fn mark_group_read(
        &self,
        path: impl AsRef<Path>,
        group_id: &str,
    ) -> anyhow::Result<LocalSummary> {
        self.mark_topic_read(path, group_id, None)
    }

    pub fn mark_entire_group_read(
        &self,
        path: impl AsRef<Path>,
        group_id: &str,
    ) -> anyhow::Result<LocalSummary> {
        let path = path.as_ref();
        let mut state = load_state(path)?;
        if !state.groups.iter().any(|group| group.group_id == group_id) {
            bail!("unknown group")
        }

        let general_key = topic_activity_key(group_id, None);
        let mut topic_keys = state
            .topic_latest_incoming
            .keys()
            .chain(state.topic_unread_messages.keys())
            .chain(state.topic_activity_initialized.iter())
            .filter(|key| topic_key_belongs_to_group(key, group_id))
            .cloned()
            .collect::<HashSet<_>>();
        topic_keys.insert(general_key);

        let mut changed = false;
        for topic_key in topic_keys {
            changed |= state.topic_activity_initialized.insert(topic_key.clone());
            if let Some(marker) = state.topic_latest_incoming.get(&topic_key).cloned() {
                changed |=
                    advance_message_marker(&mut state.topic_read_through, &topic_key, marker);
            }
            changed |= state.topic_unread_messages.remove(&topic_key).is_some();
        }

        changed |= state.group_activity_initialized.insert(group_id.to_owned());
        if let Some(marker) = state.group_latest_incoming.get(group_id).cloned() {
            changed |= advance_message_marker(&mut state.group_read_through, group_id, marker);
        }
        changed |= state.group_unread_messages.remove(group_id).is_some();

        if changed {
            save_state(path, &state)?;
        }
        state.summary()
    }

    pub fn mark_topic_read(
        &self,
        path: impl AsRef<Path>,
        group_id: &str,
        topic_id: Option<&str>,
    ) -> anyhow::Result<LocalSummary> {
        let path = path.as_ref();
        let mut state = load_state(path)?;
        if !state.groups.iter().any(|group| group.group_id == group_id) {
            bail!("unknown group")
        }
        if let Some(topic_id) = topic_id {
            let topic_is_known = state
                .group_conversation_cache
                .get(group_id)
                .or_else(|| {
                    state
                        .group_event_caches
                        .get(group_id)
                        .and_then(|cache| cache.portable_conversation.as_ref())
                })
                .is_some_and(|conversation| {
                    conversation
                        .topics
                        .iter()
                        .any(|topic| topic.topic_id == topic_id && !topic.archived)
                });
            if !topic_is_known {
                bail!("unknown topic")
            }
        }
        let topic_key = topic_activity_key(group_id, topic_id);
        let topic_was_initialized = state.topic_activity_initialized.insert(topic_key.clone());
        let topic_marker_changed = state
            .topic_latest_incoming
            .get(&topic_key)
            .cloned()
            .is_some_and(|marker| {
                advance_message_marker(&mut state.topic_read_through, &topic_key, marker)
            });
        let topic_had_unread = state.topic_unread_messages.remove(&topic_key).is_some();

        let mut legacy_changed = false;
        if topic_id.is_none() {
            let was_initialized = state.group_activity_initialized.insert(group_id.to_owned());
            let marker_changed = state
                .group_latest_incoming
                .get(group_id)
                .cloned()
                .is_some_and(|marker| {
                    advance_message_marker(&mut state.group_read_through, group_id, marker)
                });
            let had_unread = state.group_unread_messages.remove(group_id).is_some();
            legacy_changed = was_initialized || marker_changed || had_unread;
        }
        if topic_was_initialized || topic_marker_changed || topic_had_unread || legacy_changed {
            save_state(path, &state)?;
        }
        state.summary()
    }

    pub async fn conversation(
        &self,
        path: impl AsRef<Path>,
        relays: Vec<String>,
    ) -> anyhow::Result<Conversation> {
        let path = path.as_ref();
        let mut state = load_state(path)?;
        let group_id = state.active_group()?.group_id.clone();
        if state.group_event_caches.contains_key(&group_id) {
            drop(state);
            return self
                .sync_group_activity(path, &group_id, relays)
                .await?
                .conversation
                .context("this identity is not an active group member");
        }
        let group_index = state
            .groups
            .iter()
            .position(|group| group.group_id == group_id)
            .context("active group is missing from local state")?;
        let group = state.groups[group_index].clone();
        let relays = relay_list(relays)?;
        let mut events = self.fetch_events(&group, relays.clone()).await?;
        let identity = state.identity()?;
        let identity_public_key = identity.public_key_base64();
        let founder_join_exists = rebuild_group_state(&state, &group, &events)?
            .members
            .contains_key(&identity_public_key);
        if group.owner_public_key == identity_public_key && !founder_join_exists {
            let sequence = state.take_sequence();
            let joined = create_group_event(
                &state,
                &identity,
                &group,
                GroupEventPayload::MemberJoined {
                    username: state.profile.username.clone(),
                    bio: state.profile.bio.clone(),
                    avatar: state.profile.avatar.clone(),
                    album: state.profile.album.clone(),
                    accepts_direct_messages: state.profile.effective_direct_message_policy()
                        != DirectMessagePolicy::Nobody,
                    direct_message_policy: state.profile.effective_direct_message_policy(),
                },
                sequence,
            )?;
            save_state(path, &state)?;
            self.publish_event(&relays, &joined).await?;
            events.push(joined);
        }
        let cache = compact_group_event_cache(
            &state,
            &group,
            events,
            INITIAL_GROUP_MESSAGE_WINDOW,
            None,
            None,
            false,
            HashMap::new(),
        )?;
        let view = rebuild_group_state(&state, &group, &cache.events)?;
        state
            .group_event_caches
            .insert(group.group_id.clone(), cache.clone());
        state.record_group_activity(&group.group_id, &view.messages, &identity_public_key);
        let mut state_changed = true;
        let resolved_owner = view.owner_public_key.clone().unwrap_or_default();
        let resolved_profile = view.profile.clone();
        let moderators = view.moderators.clone();
        let mut banned_members = view
            .banned_profiles
            .values()
            .cloned()
            .map(|member| BannedMemberSummary {
                public_key: member.public_key,
                username: member.username,
                bio: member.bio,
                avatar: member.avatar,
            })
            .collect::<Vec<_>>();
        banned_members.sort_by(|left, right| left.username.cmp(&right.username));
        let known_people_before = state.known_people.clone();
        for member in view.members.values() {
            state.upsert_known_person(DirectContact {
                public_key: member.public_key.clone(),
                username: member.username.clone(),
                bio: member.bio.clone(),
                avatar: member.avatar.clone(),
                album: member.album.clone(),
                accepts_direct_messages: member.accepts_direct_messages,
                direct_message_policy: member.direct_message_policy,
                profile_sequence: member.profile_sequence,
            });
        }
        if state.apply_resolved_group_profile(&group.group_id, &resolved_profile, &resolved_owner) {
            state_changed = true;
        }
        if state.known_people != known_people_before {
            state_changed = true;
        }
        if state_changed {
            save_state(path, &state)?;
        }
        let can_view_reports =
            resolved_owner == identity_public_key || moderators.contains(&identity_public_key);
        let mut reactions_by_message = HashMap::<String, Vec<ReactionSummary>>::new();
        for reaction in &view.reactions {
            let summaries = reactions_by_message
                .entry(reaction.message_event_id.clone())
                .or_default();
            if let Some(summary) = summaries
                .iter_mut()
                .find(|summary| summary.emoji == reaction.emoji)
            {
                summary.count += 1;
                summary
                    .reactor_public_keys
                    .push(reaction.reactor_public_key.clone());
                summary.reacted_by_self |= reaction.reactor_public_key == identity_public_key;
            } else {
                summaries.push(ReactionSummary {
                    emoji: reaction.emoji.clone(),
                    count: 1,
                    reactor_public_keys: vec![reaction.reactor_public_key.clone()],
                    reacted_by_self: reaction.reactor_public_key == identity_public_key,
                });
            }
        }
        let reported_message_event_ids = view
            .reports
            .iter()
            .filter(|report| report.reporter_public_key == identity_public_key)
            .map(|report| report.message_event_id.clone())
            .collect::<Vec<_>>();
        let reports = view
            .reports
            .iter()
            .filter(|_| can_view_reports)
            .filter_map(|report| {
                let message = view
                    .messages
                    .iter()
                    .find(|message| message.event_id == report.message_event_id)?;
                Some(ReportSummary {
                    report_event_id: report.event_id.clone(),
                    reporter_public_key: report.reporter_public_key.clone(),
                    reporter_username: report.reporter_username.clone(),
                    reporter_avatar: report.reporter_avatar.clone(),
                    reason: report.reason.clone(),
                    created_at_millis: report.created_at_millis,
                    message: MessageSummary {
                        event_id: message.event_id.clone(),
                        message_id: message.message_id.clone(),
                        author_public_key: message.author_public_key.clone(),
                        username: message.username.clone(),
                        bio: message.bio.clone(),
                        avatar: message.avatar.clone(),
                        album: message.album.clone(),
                        accepts_direct_messages: message.accepts_direct_messages,
                        direct_message_policy: message.direct_message_policy,
                        text: message.text.clone(),
                        attachment: message.attachment.clone(),
                        reply_to_message_id: message.reply_to_message_id.clone(),
                        forwarded_from: message.forwarded_from.clone(),
                        topic_id: message.topic_id.clone(),
                        created_at_millis: message.created_at_millis,
                        reactions: reactions_by_message
                            .get(&message.event_id)
                            .cloned()
                            .unwrap_or_default(),
                    },
                })
            })
            .collect::<Vec<_>>();
        let topics = topic_summaries(&state, &group.group_id, &view.topics);
        let mut members = view
            .members
            .into_values()
            .map(|member| MemberSummary {
                is_moderator: moderators.contains(&member.public_key),
                public_key: member.public_key,
                username: member.username,
                bio: member.bio,
                avatar: member.avatar,
                album: member.album,
                accepts_direct_messages: member.accepts_direct_messages,
                direct_message_policy: member.direct_message_policy,
            })
            .collect::<Vec<_>>();
        members.sort_by(|left, right| left.username.cmp(&right.username));
        let mut conversation = Conversation {
            group: GroupSummary {
                group_id: group.group_id.clone(),
                name: resolved_profile.name,
                description: resolved_profile.description,
                rules: resolved_profile.rules,
                content_rating: resolved_profile.content_rating,
                avatar: resolved_profile.avatar,
                background: resolved_profile.background,
                mobile_background: resolved_profile.mobile_background,
                accent_color: resolved_profile.accent_color,
                members_can_send_messages: resolved_profile.members_can_send_messages,
                members_can_send_media: resolved_profile.members_can_send_media,
                frequency: state
                    .group_frequencies
                    .get(&group.group_id)
                    .and_then(|frequency| display_frequency(frequency).ok()),
                owner_public_key: resolved_owner,
                remote_deletion_supported: !group.authority_nonce_base64.is_empty(),
                is_active: true,
                unread_count: state.group_unread_count(&group.group_id),
                read_state_initialized: state.topic_read_state_initialized(&group.group_id, None),
            },
            topics,
            general_unread_count: state.topic_unread_count(&group.group_id, None),
            members,
            banned_members,
            messages: view
                .messages
                .into_iter()
                .map(|message| {
                    let reactions = reactions_by_message
                        .remove(&message.event_id)
                        .unwrap_or_default();
                    MessageSummary {
                        event_id: message.event_id,
                        message_id: message.message_id,
                        author_public_key: message.author_public_key,
                        username: message.username,
                        bio: message.bio,
                        avatar: message.avatar,
                        album: message.album,
                        accepts_direct_messages: message.accepts_direct_messages,
                        direct_message_policy: message.direct_message_policy,
                        text: message.text,
                        attachment: message.attachment,
                        reply_to_message_id: message.reply_to_message_id,
                        forwarded_from: message.forwarded_from,
                        topic_id: message.topic_id,
                        created_at_millis: message.created_at_millis,
                        reactions,
                    }
                })
                .collect(),
            reports,
            reported_message_event_ids,
            rejected_events: view.rejected_events,
            has_older_messages: cache.has_older_messages,
        };
        apply_current_group_profiles(&mut conversation, &state, &identity_public_key);
        let blocked = state.active_hidden_keys();
        filter_conversation_for_blocks(&mut conversation, &blocked);
        state
            .group_conversation_cache
            .insert(group.group_id.clone(), conversation.clone());
        save_state(path, &state)?;
        Ok(conversation)
    }

    async fn sync_direct_inbox(
        &self,
        path: &Path,
        cache_path: &Path,
        relays: Vec<RelayDescriptor>,
    ) -> anyhow::Result<(ClientState, Vec<DecryptedDirectMessage>)> {
        let mut state = load_state(path)?;
        let identity = state.identity()?;
        let self_public_key = identity.public_key_base64();
        let mailbox_id = direct_mailbox_id(&self_public_key)?;
        let previous_cache = state.direct_event_cache.clone();
        let cache = self
            .fetch_direct_event_cache(&mailbox_id, previous_cache.clone(), &relays)
            .await?;
        let events = cache.events.clone();
        state.direct_event_cache = cache;
        let cache_changed = previous_cache != state.direct_event_cache;
        self.apply_direct_events(path, cache_path, state, events, relays, cache_changed)
            .await
    }

    async fn sync_complete_direct_inbox(
        &self,
        path: &Path,
        cache_path: &Path,
        relays: Vec<RelayDescriptor>,
    ) -> anyhow::Result<(ClientState, Vec<DecryptedDirectMessage>)> {
        let mut state = load_state(path)?;
        let identity = state.identity()?;
        let mailbox_id = direct_mailbox_id(&identity.public_key_base64())?;
        let events = self
            .fetch_events_for_id(&mailbox_id, relays.clone())
            .await?;
        let previous_cache = state.direct_event_cache.clone();
        state.direct_event_cache = compact_direct_event_cache(&mailbox_id, events.clone(), false);
        let cache_changed = previous_cache != state.direct_event_cache;
        self.apply_direct_events(path, cache_path, state, events, relays, cache_changed)
            .await
    }

    async fn apply_direct_events(
        &self,
        path: &Path,
        cache_path: &Path,
        mut state: ClientState,
        events: Vec<SignedEvent>,
        relays: Vec<RelayDescriptor>,
        cache_changed: bool,
    ) -> anyhow::Result<(ClientState, Vec<DecryptedDirectMessage>)> {
        let identity = state.identity()?;
        let self_public_key = identity.public_key_base64();
        let contacts_before = state.direct_contacts.clone();
        let active_before = state.active_direct_public_key.clone();
        let deletions_before = state.direct_deleted_before.clone();
        let blocked_by_before = state.blocked_by_states.clone();
        let policy_rejections_before = state.direct_policy_rejected_through.clone();
        let latest_incoming_before = state.direct_latest_incoming.clone();
        let latest_activity_before = state.direct_latest_activity.clone();
        let decoded = events
            .iter()
            .filter_map(|event| decrypt_direct_event(&identity, &state, event))
            .collect::<Vec<_>>();
        if state.profile.effective_direct_message_policy() == DirectMessagePolicy::SharedGroups {
            for event in &decoded {
                let DecryptedDirectEvent::Message(message) = event else {
                    continue;
                };
                if message.message.author_public_key == self_public_key
                    || state.shares_active_group_with(&message.counterparty_public_key)
                {
                    continue;
                }
                let marker = DirectMessageMarker {
                    created_at_millis: message.message.created_at_millis,
                    event_id: message.message.event_id.clone(),
                };
                state
                    .direct_policy_rejected_through
                    .entry(message.counterparty_public_key.clone())
                    .and_modify(|existing| *existing = existing.clone().max(marker.clone()))
                    .or_insert(marker);
            }
        }
        for event in &decoded {
            match event {
                DecryptedDirectEvent::ThreadDeleted {
                    counterparty_public_key,
                    deleted_at_millis,
                } => {
                    state
                        .direct_deleted_before
                        .entry(counterparty_public_key.clone())
                        .and_modify(|cutoff| *cutoff = (*cutoff).max(*deleted_at_millis))
                        .or_insert(*deleted_at_millis);
                }
                DecryptedDirectEvent::BlockChanged {
                    counterparty_public_key,
                    contact,
                    blocked,
                    sequence,
                    changed_at_millis,
                } => {
                    let candidate = BlockState {
                        person: contact.clone(),
                        blocked: *blocked,
                        sequence: *sequence,
                        changed_at_millis: *changed_at_millis,
                    };
                    state
                        .blocked_by_states
                        .entry(counterparty_public_key.clone())
                        .and_modify(|existing| {
                            if (candidate.sequence, candidate.blocked)
                                > (existing.sequence, existing.blocked)
                            {
                                *existing = candidate.clone();
                            }
                        })
                        .or_insert(candidate);
                }
                DecryptedDirectEvent::Message(_) => {}
            }
        }
        let blocked_by_changed = state.blocked_by_states != blocked_by_before;
        if blocked_by_changed {
            state.apply_block_filters();
            for public_key in state.active_hidden_keys() {
                purge_scope_cache(cache_path, &identity.direct_scope_id(&public_key)?)?;
                purge_scope_cache(cache_path, &profile_media_scope_id(&public_key)?)?;
            }
        }
        let newly_deleted = state
            .direct_deleted_before
            .iter()
            .filter(|(public_key, cutoff)| {
                deletions_before
                    .get(*public_key)
                    .copied()
                    .unwrap_or_default()
                    < **cutoff
            })
            .map(|(public_key, _)| public_key.clone())
            .collect::<Vec<_>>();
        for public_key in &newly_deleted {
            purge_scope_cache(cache_path, &identity.direct_scope_id(public_key)?)?;
            state.direct_latest_incoming.remove(public_key);
            state.direct_latest_activity.remove(public_key);
            state.direct_read_through.remove(public_key);
        }
        state
            .direct_contacts
            .retain(|contact| !newly_deleted.contains(&contact.public_key));
        let mut messages = decoded
            .into_iter()
            .filter_map(|event| match event {
                DecryptedDirectEvent::Message(message)
                    if message.message.created_at_millis
                        > state
                            .direct_deleted_before
                            .get(&message.counterparty_public_key)
                            .copied()
                            .unwrap_or_default()
                        && !state.is_hidden(&message.counterparty_public_key)
                        && (message.message.author_public_key == self_public_key
                            || state
                                .direct_policy_rejected_through
                                .get(&message.counterparty_public_key)
                                .is_none_or(|cutoff| {
                                    &DirectMessageMarker {
                                        created_at_millis: message.message.created_at_millis,
                                        event_id: message.message.event_id.clone(),
                                    } > cutoff
                                }))
                        && (message.message.author_public_key == self_public_key
                            || (!state.direct_messages_blocked_at(
                                message.message.created_at_millis,
                            ) && (state.profile.effective_direct_message_policy()
                                != DirectMessagePolicy::SharedGroups
                                || state.shares_active_group_with(
                                    &message.counterparty_public_key,
                                )))) =>
                {
                    Some(message)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        messages.sort_by(|left, right| {
            left.message
                .created_at_millis
                .cmp(&right.message.created_at_millis)
                .then_with(|| left.message.event_id.cmp(&right.message.event_id))
        });
        for message in &messages {
            let marker = DirectMessageMarker {
                created_at_millis: message.message.created_at_millis,
                event_id: message.message.event_id.clone(),
            };
            let contact_is_missing = !state
                .direct_contacts
                .iter()
                .any(|contact| contact.public_key == message.counterparty_public_key);
            let message_is_new_activity = latest_activity_before
                .get(&message.counterparty_public_key)
                .is_none_or(|latest| &marker > latest);
            if contact_is_missing || message_is_new_activity {
                state.remember_direct(message.contact.clone());
            }
            state
                .direct_latest_activity
                .entry(message.counterparty_public_key.clone())
                .and_modify(|latest| *latest = latest.clone().max(marker.clone()))
                .or_insert_with(|| marker.clone());
            if message.message.author_public_key != self_public_key {
                state
                    .direct_latest_incoming
                    .entry(message.counterparty_public_key.clone())
                    .and_modify(|latest| *latest = latest.clone().max(marker.clone()))
                    .or_insert(marker);
            }
        }
        if state
            .active_direct_public_key
            .as_ref()
            .is_some_and(|public_key| {
                !state
                    .direct_contacts
                    .iter()
                    .any(|contact| &contact.public_key == public_key)
            })
        {
            state.active_direct_public_key = None;
        }
        if state.active_direct_public_key.is_none() {
            state.active_direct_public_key = state
                .direct_contacts
                .first()
                .map(|contact| contact.public_key.clone());
        }
        if blocked_by_changed && state.account.is_some() {
            let _ = self.publish_account_state(&mut state, &relays).await;
        }
        if cache_changed
            || state.direct_contacts != contacts_before
            || state.active_direct_public_key != active_before
            || state.direct_deleted_before != deletions_before
            || state.direct_policy_rejected_through != policy_rejections_before
            || state.blocked_by_states != blocked_by_before
            || state.direct_latest_incoming != latest_incoming_before
            || state.direct_latest_activity != latest_activity_before
        {
            save_state(path, &state)?;
        }
        Ok((state, messages))
    }

    async fn publish_invite(
        &self,
        relays: &[RelayDescriptor],
        invitation: &InviteRecord,
    ) -> anyhow::Result<()> {
        let body = serde_json::to_vec(invitation)?;
        let mut accepted = 0usize;
        for index in 0..relays.len() {
            if let Ok(response) = self
                .relay_request(relays, index, "POST", "/v1/invites", &body)
                .await
                && (200..300).contains(&response.status)
            {
                accepted += 1;
            }
        }
        if accepted == 0 {
            bail!("no relay accepted the invitation")
        }
        Ok(())
    }

    async fn publish_mls_join_request(
        &self,
        relays: &[RelayDescriptor],
        request: &MlsJoinRequest,
    ) -> anyhow::Result<()> {
        let body = serde_json::to_vec(request)?;
        let mut accepted = 0usize;
        for index in 0..relays.len() {
            if let Ok(response) = self
                .relay_request(relays, index, "POST", "/v2/mls/join-requests", &body)
                .await
                && (200..300).contains(&response.status)
            {
                accepted += 1;
            }
        }
        if accepted == 0 {
            bail!("no relay accepted this device's encrypted group join request")
        }
        Ok(())
    }

    async fn publish_mls_removal_request(
        &self,
        relays: &[RelayDescriptor],
        request: &MlsRemovalRequest,
    ) -> anyhow::Result<()> {
        let body = serde_json::to_vec(request)?;
        let mut accepted = 0usize;
        for index in 0..relays.len() {
            if let Ok(response) = self
                .relay_request(relays, index, "POST", "/v2/mls/removal-requests", &body)
                .await
                && (200..300).contains(&response.status)
            {
                accepted += 1;
            }
        }
        if accepted != relays.len() {
            bail!("every configured relay must confirm the encrypted-group removal request")
        }
        Ok(())
    }

    async fn publish_mls_genesis(
        &self,
        relays: &[RelayDescriptor],
        genesis: &MlsGroupGenesis,
    ) -> anyhow::Result<()> {
        match self
            .publish_mls_control_object(relays, "/v2/mls/genesis", genesis)
            .await?
        {
            ControlHead::Extended => Ok(()),
            ControlHead::Changed => {
                bail!("another device published this group's encryption genesis; sync and retry")
            }
        }
    }

    async fn publish_mls_epoch(
        &self,
        relays: &[RelayDescriptor],
        epoch: &MlsEpochRecord,
    ) -> anyhow::Result<ControlHead> {
        self.publish_mls_control_object(relays, "/v2/mls/epochs", epoch)
            .await
    }

    async fn publish_mls_control_object<T: Serialize>(
        &self,
        relays: &[RelayDescriptor],
        endpoint: &str,
        object: &T,
    ) -> anyhow::Result<ControlHead> {
        let body = serde_json::to_vec(object)?;
        let mut accepted = 0usize;
        let mut conflicts = 0usize;
        for index in 0..relays.len() {
            match self
                .relay_request(relays, index, "POST", endpoint, &body)
                .await
            {
                Ok(response) if (200..300).contains(&response.status) => accepted += 1,
                Ok(response) if response.status == 409 => conflicts += 1,
                _ => {}
            }
        }
        if accepted == relays.len() {
            return Ok(ControlHead::Extended);
        }
        // Every relay refusing the same record means another member published
        // this epoch first. Nothing was written, so this attempt is discarded
        // and the winning epoch arrives with the next control log.
        if accepted == 0 && conflicts > 0 {
            return Ok(ControlHead::Changed);
        }
        if conflicts > 0 {
            bail!("the group encryption head changed on another device; sync and retry")
        }
        bail!("every configured relay must confirm a group encryption update")
    }

    /// Whether every configured relay accepts an epoch authored by a member who
    /// did not found the group.
    ///
    /// One relay refusing an epoch the others stored would stall this group's
    /// membership until it is upgraded, so admission stays with the founder
    /// until the whole set is ready. This runs only when an admission is
    /// actually pending.
    async fn relays_accept_member_admission(&self, relays: &[RelayDescriptor]) -> bool {
        for index in 0..relays.len() {
            let accepted = self
                .relay_request(relays, index, "GET", RELAY_CAPABILITIES_PATH, &[])
                .await
                .ok()
                .filter(|response| (200..300).contains(&response.status))
                .and_then(|response| {
                    serde_json::from_slice::<RelayCapabilities>(&response.body).ok()
                })
                .is_some_and(|capabilities| capabilities.member_admission);
            if !accepted {
                return false;
            }
        }
        true
    }

    /// Replay control records a replica is missing.
    ///
    /// Every later epoch has to be accepted by every relay, so one replica that
    /// was unavailable for an earlier write would otherwise block the group's
    /// membership permanently.
    async fn repair_mls_control_log(
        &self,
        relays: &[RelayDescriptor],
        index: usize,
        observed: Option<&MlsControlLog>,
        newest: &MlsControlLog,
    ) {
        if observed.is_none() {
            let Ok(body) = serde_json::to_vec(&newest.genesis) else {
                return;
            };
            if !self
                .relay_request(relays, index, "POST", "/v2/mls/genesis", &body)
                .await
                .is_ok_and(|response| (200..300).contains(&response.status))
            {
                return;
            }
        }
        for record in newest
            .epochs
            .iter()
            .skip(observed.map_or(0, |log| log.epochs.len()))
        {
            let Ok(body) = serde_json::to_vec(record) else {
                return;
            };
            if !self
                .relay_request(relays, index, "POST", "/v2/mls/epochs", &body)
                .await
                .is_ok_and(|response| (200..300).contains(&response.status))
            {
                return;
            }
        }
    }

    async fn fetch_mls_control_log(
        &self,
        relays: &[RelayDescriptor],
        group_id: &str,
    ) -> anyhow::Result<Option<MlsControlLog>> {
        let endpoint = format!("/v2/mls/groups/{group_id}");
        let mut observations = Vec::with_capacity(relays.len());
        for index in 0..relays.len() {
            let response = self
                .relay_request(relays, index, "GET", &endpoint, &[])
                .await
                .context("could not read the group encryption head from every relay")?;
            if response.status == 404 {
                observations.push(None);
                continue;
            }
            if !(200..300).contains(&response.status) {
                bail!("a relay rejected the group encryption head request")
            }
            let log: MlsControlLog = serde_json::from_slice(&response.body)
                .context("relay returned an invalid group encryption log")?;
            log.verify()
                .context("relay returned an unauthenticated group encryption log")?;
            observations.push(Some(log));
        }
        let Some(newest) = observations
            .iter()
            .flatten()
            .max_by_key(|log| log.epochs.len())
            .cloned()
        else {
            return Ok(None);
        };
        // Replicas converge by replication, so a replica that is behind holds a
        // prefix of the longest chain. Anything else is a real fork: fail closed
        // rather than choose a branch and strand events published on the other.
        if observations
            .iter()
            .flatten()
            .any(|observation| !control_log_leads_to(observation, &newest))
        {
            bail!("group encryption relays are still converging; retry in a moment")
        }
        for (index, observation) in observations.iter().enumerate() {
            if observation
                .as_ref()
                .is_none_or(|log| log.epochs.len() < newest.epochs.len())
            {
                self.repair_mls_control_log(relays, index, observation.as_ref(), &newest)
                    .await;
            }
        }
        Ok(Some(newest))
    }

    async fn fetch_mls_join_requests(
        &self,
        relays: &[RelayDescriptor],
        group_id: &str,
    ) -> anyhow::Result<Vec<MlsJoinRequest>> {
        let endpoint = format!("/v2/mls/groups/{group_id}/join-requests");
        let mut requests = HashMap::<String, MlsJoinRequest>::new();
        for index in 0..relays.len() {
            let response = self
                .relay_request(relays, index, "GET", &endpoint, &[])
                .await
                .context("could not read member encryption requests from every relay")?;
            if !(200..300).contains(&response.status) {
                bail!("a relay rejected the member encryption request")
            }
            let relay_requests: Vec<MlsJoinRequest> = serde_json::from_slice(&response.body)
                .context("relay returned invalid member encryption requests")?;
            for request in relay_requests {
                request
                    .verify()
                    .context("relay returned an unauthenticated member encryption request")?;
                if request.group_id == group_id {
                    requests
                        .entry(request.request_id.clone())
                        .or_insert(request);
                }
            }
        }
        Ok(requests.into_values().collect())
    }

    async fn fetch_mls_removal_requests(
        &self,
        relays: &[RelayDescriptor],
        group_id: &str,
    ) -> anyhow::Result<Vec<MlsRemovalRequest>> {
        let endpoint = format!("/v2/mls/groups/{group_id}/removal-requests");
        let mut requests = HashMap::<String, MlsRemovalRequest>::new();
        for index in 0..relays.len() {
            let response = self
                .relay_request(relays, index, "GET", &endpoint, &[])
                .await
                .context("could not read encrypted-group removal requests from every relay")?;
            if !(200..300).contains(&response.status) {
                bail!("a relay rejected the encrypted-group removal request")
            }
            let relay_requests: Vec<MlsRemovalRequest> = serde_json::from_slice(&response.body)
                .context("relay returned invalid encrypted-group removal requests")?;
            for request in relay_requests {
                request
                    .verify()
                    .context("relay returned an unauthenticated removal request")?;
                if request.group_id == group_id {
                    requests
                        .entry(request.request_id.clone())
                        .or_insert(request);
                }
            }
        }
        Ok(requests.into_values().collect())
    }

    async fn publish_account_state(
        &self,
        state: &mut ClientState,
        relays: &[RelayDescriptor],
    ) -> anyhow::Result<()> {
        state.migrate_recoverable_mls_groups()?;
        let credentials = state.account_credentials()?;
        let identity = state.identity()?;
        if let Ok(remote) = self.fetch_account_vault(relays, &credentials.locator).await {
            Self::merge_remote_account_state(state, &credentials, &remote)?;
        }
        // Contact discovery is identity infrastructure, not a copy-button side
        // effect. Keep the signed card current whenever the account is
        // published. A failed relay attempt does not block account recovery;
        // the persisted version marker leaves it eligible for the next sync.
        let _ = self.ensure_contact_signal_published(state, relays).await;

        let mut last_error = None;
        for _ in 0..4 {
            let revision = state
                .account
                .as_ref()
                .context("this identity has no noise ID")?
                .revision
                .checked_add(1)
                .context("account vault revision is exhausted")?;
            let plaintext = serde_json::to_vec(&state.vault_contents())?;
            let vault = AccountVault::seal(&identity, &credentials, revision, &plaintext)?;
            let publish_error = self.publish_account_vault(relays, &vault).await.err();

            let remote = match self.fetch_account_vault(relays, &credentials.locator).await {
                Ok(remote) => remote,
                Err(error) => {
                    last_error = Some(publish_error.unwrap_or(error));
                    continue;
                }
            };
            if remote.identity_public_key != identity.public_key_base64() {
                bail!("account identity does not match the encrypted vault")
            }
            if remote.revision == revision && remote.signature_base64 == vault.signature_base64 {
                state
                    .account
                    .as_mut()
                    .context("this identity has no noise ID")?
                    .revision = revision;
                state.mls_recovery_dirty = false;
                return Ok(());
            }

            Self::merge_remote_account_state(state, &credentials, &remote)?;
            last_error = Some(publish_error.unwrap_or_else(|| {
                anyhow::anyhow!("another device updated the encrypted account vault")
            }));
        }

        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("account sync did not complete")))
    }

    async fn publish_read_state_for_state(
        &self,
        state: &mut ClientState,
        relays: &[RelayDescriptor],
    ) -> anyhow::Result<()> {
        let credentials = state.account_read_state_credentials()?;
        let identity = state.identity()?;
        if let Ok(remote) = self.fetch_account_vault(relays, &credentials.locator).await {
            Self::merge_remote_read_state(state, &credentials, &remote)?;
        }

        let mut last_error = None;
        for _ in 0..4 {
            let revision = state
                .account
                .as_ref()
                .context("this identity has no noise ID")?
                .read_state_revision
                .checked_add(1)
                .context("account read-state revision is exhausted")?;
            let plaintext = serde_json::to_vec(&state.read_state_contents())?;
            let vault = AccountVault::seal(&identity, &credentials, revision, &plaintext)?;
            let publish_error = self.publish_account_vault(relays, &vault).await.err();

            let remote = match self.fetch_account_vault(relays, &credentials.locator).await {
                Ok(remote) => remote,
                Err(error) => {
                    last_error = Some(publish_error.unwrap_or(error));
                    continue;
                }
            };
            if remote.identity_public_key != identity.public_key_base64() {
                bail!("account identity does not match the encrypted read state")
            }
            if remote.revision == revision && remote.signature_base64 == vault.signature_base64 {
                state
                    .account
                    .as_mut()
                    .context("this identity has no noise ID")?
                    .read_state_revision = revision;
                return Ok(());
            }

            Self::merge_remote_read_state(state, &credentials, &remote)?;
            last_error = Some(publish_error.unwrap_or_else(|| {
                anyhow::anyhow!("another device updated the encrypted read state")
            }));
        }

        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("read-state sync did not complete")))
    }

    fn merge_remote_read_state(
        state: &mut ClientState,
        credentials: &AccountCredentials,
        remote: &AccountVault,
    ) -> anyhow::Result<bool> {
        if remote.identity_public_key != state.identity()?.public_key_base64() {
            bail!("account identity does not match the encrypted read state")
        }
        let plaintext = remote.open(credentials)?;
        let contents: AccountReadStateContents =
            serde_json::from_slice(&plaintext).context("encrypted read state is invalid")?;
        if contents.version != 1 {
            bail!("this read state was created by an unsupported noise version")
        }
        let direct_changed = state.merge_direct_read_through(&contents.direct_read_through);
        let group_changed = state.merge_group_read_through(&contents.group_read_through);
        let topic_changed = state.merge_topic_read_through(&contents.topic_read_through);
        let account = state
            .account
            .as_mut()
            .context("this identity has no noise ID")?;
        let revision_changed = remote.revision > account.read_state_revision;
        account.read_state_revision = account.read_state_revision.max(remote.revision);
        Ok(direct_changed || group_changed || topic_changed || revision_changed)
    }

    fn merge_remote_account_state(
        state: &mut ClientState,
        credentials: &AccountCredentials,
        remote: &AccountVault,
    ) -> anyhow::Result<bool> {
        if remote.identity_public_key != state.identity()?.public_key_base64() {
            bail!("account identity does not match the encrypted vault")
        }
        let plaintext = remote.open(credentials)?;
        let contents: AccountVaultContents =
            serde_json::from_slice(&plaintext).context("encrypted account vault is invalid")?;
        if contents.version != 1 {
            bail!("this account vault was created by an unsupported noise version")
        }
        let local_account_revision = state
            .account
            .as_ref()
            .context("this identity has no noise ID")?
            .revision;
        let profile_changed = contents.profile_sequence > state.profile_sequence
            || (contents.profile_sequence == 0
                && state.profile_sequence == 0
                && remote.revision > local_account_revision);
        if profile_changed {
            state.profile = contents.profile.clone();
            state.profile_sequence = contents.profile_sequence;
        }
        if contents.adult_access.explicit_content_enabled && !contents.adult_access.age_attested {
            bail!("encrypted account vault contains invalid adult access settings")
        }
        let adult_access_changed = contents.adult_access.preference_updated_at_millis
            > state.adult_access.preference_updated_at_millis
            || (contents.adult_access.preference_updated_at_millis
                == state.adult_access.preference_updated_at_millis
                && remote.revision > local_account_revision);
        if adult_access_changed {
            state.adult_access = contents.adult_access.clone();
            if !state.adult_access.explicit_content_enabled
                && state
                    .active_group_id
                    .as_ref()
                    .is_some_and(|active_group_id| {
                        state.groups.iter().any(|group| {
                            group.group_id == *active_group_id
                                && group.content_rating == GroupContentRating::Explicit
                        })
                    })
            {
                state.active_group_id = None;
            }
        }
        let contact_signal_changed =
            contents.contact_signal_published_sequence > state.contact_signal_published_sequence;
        state.contact_signal_published_sequence = state
            .contact_signal_published_sequence
            .max(contents.contact_signal_published_sequence);
        let devices_changed = merge_device_records(&mut state.devices, contents.devices.clone());
        if state.current_device_is_revoked() {
            bail!(DEVICE_SESSION_REVOKED_ERROR)
        }
        let recovery_changed_before_merge = state.migrate_recoverable_mls_groups()?;
        state.ensure_group_membership_records();
        let memberships_before = state.group_memberships.clone();
        let groups_before = state.groups.clone();
        let frequencies_before = state.group_frequencies.clone();
        let mut remote_memberships = contents.group_memberships.clone();
        for group in &contents.groups {
            remote_memberships
                .entry(group.group_id.clone())
                .or_insert_with(|| GroupMembershipRecord {
                    sequence: 0,
                    group: Some(group.clone()),
                });
        }
        merge_group_membership_records(&mut state.group_memberships, remote_memberships);
        state.reconcile_group_memberships();
        let present_group_ids = state
            .groups
            .iter()
            .map(|group| group.group_id.clone())
            .collect::<HashSet<_>>();
        for (group_id, frequency) in &contents.group_frequencies {
            if present_group_ids.contains(group_id) {
                state
                    .group_frequencies
                    .entry(group_id.clone())
                    .or_insert_with(|| frequency.clone());
            }
        }
        state
            .group_frequencies
            .retain(|group_id, _| present_group_ids.contains(group_id));
        let identity_public_key = state.identity()?.public_key_base64();
        let recovery_before = state.mls_group_states.clone();
        for (group_id, remote_mls) in contents.mls_group_states {
            if !present_group_ids.contains(&group_id) {
                continue;
            }
            if remote_mls.device_credential().account_public_key != identity_public_key {
                bail!("encrypted account vault contains another account's group recovery state")
            }
            let remote_epoch = remote_mls
                .epoch(&group_id)
                .context("encrypted account vault contains invalid group recovery state")?
                .epoch;
            let local_epoch = state
                .mls_group_states
                .get(&group_id)
                .and_then(|local| local.epoch(&group_id).ok())
                .map(|epoch| epoch.epoch);
            if local_epoch.is_none_or(|local_epoch| remote_epoch > local_epoch) {
                state.mls_group_states.insert(group_id, remote_mls);
            }
        }
        state
            .mls_group_states
            .retain(|group_id, _| present_group_ids.contains(group_id));
        let control_logs_before = state.mls_control_logs.clone();
        for (group_id, remote_log) in contents.mls_control_logs {
            if !present_group_ids.contains(&group_id) {
                continue;
            }
            remote_log
                .verify()
                .context("encrypted account vault contains an invalid group encryption log")?;
            if remote_log.genesis.group_id != group_id {
                bail!("encrypted account vault contains a mismatched group encryption log")
            }
            let remote_epoch = remote_log
                .epochs
                .last()
                .map_or(0, |record| record.bundle.epoch);
            let local_epoch = state
                .mls_control_logs
                .get(&group_id)
                .and_then(|log| log.epochs.last())
                .map_or(0, |record| record.bundle.epoch);
            if !state.mls_control_logs.contains_key(&group_id) || remote_epoch > local_epoch {
                state.mls_control_logs.insert(group_id, remote_log);
            }
        }
        state
            .mls_control_logs
            .retain(|group_id, _| present_group_ids.contains(group_id));

        let event_caches_before = state.group_event_caches.clone();
        for (group_id, remote_cache) in contents.group_event_caches {
            let Some(group) = state
                .groups
                .iter()
                .find(|group| group.group_id == group_id)
                .cloned()
            else {
                continue;
            };
            let local = state.group_event_caches.get(&group_id).cloned();
            // Account vault snapshots intentionally carry message-free,
            // not-yet-hydrated group shells for new devices. Once this device
            // has real group events, that remote recovery marker must not
            // poison the established local cache on every account refresh.
            // Doing so turns every ordinary incoming message into a complete
            // group-history download and decrypt.
            let needs_control_hydration =
                local
                    .as_ref()
                    .map_or(remote_cache.needs_control_hydration, |cache| {
                        if cache.events.is_empty() {
                            cache.needs_control_hydration || remote_cache.needs_control_hydration
                        } else {
                            cache.needs_control_hydration
                        }
                    });
            let mut remote_portable_conversation = remote_cache.portable_conversation;
            if remote_portable_conversation
                .as_ref()
                .is_some_and(|conversation| {
                    conversation.group.group_id != group_id
                        || conversation.messages.len() > INITIAL_GROUP_MESSAGE_WINDOW
                })
            {
                bail!("encrypted account vault contains invalid cached conversation")
            }
            if let Some(conversation) = remote_portable_conversation.as_mut() {
                strip_conversation_media_previews(conversation);
            }
            let preserve_remote_portable = remote_cache.needs_recent_hydration
                && remote_portable_conversation.is_some()
                && local
                    .as_ref()
                    .is_none_or(|cache| cache.needs_recent_hydration);
            // Current account vaults carry a message-free conversation
            // snapshot for new-device recovery. It is not group history and
            // must not make an established client decrypt and rebuild its
            // entire local event cache every time read state or profile data
            // is synchronized.
            if remote_cache.events.is_empty() {
                let cache = if let Some(mut local) = local {
                    local.needs_control_hydration = needs_control_hydration;
                    if preserve_remote_portable {
                        local.needs_recent_hydration = true;
                        local.portable_conversation = remote_portable_conversation;
                    }
                    local
                } else {
                    GroupEventCache {
                        events: Vec::new(),
                        latest_cursor: remote_cache.latest_cursor,
                        older_cursor: remote_cache.older_cursor,
                        message_limit: remote_cache.message_limit.max(INITIAL_GROUP_MESSAGE_WINDOW),
                        has_older_messages: remote_cache.has_older_messages,
                        needs_recent_hydration: remote_cache.needs_recent_hydration,
                        needs_control_hydration,
                        topic_streams: remote_cache.topic_streams,
                        portable_conversation: remote_portable_conversation,
                    }
                };
                state.group_event_caches.insert(group_id, cache);
                continue;
            }
            let mut events = local
                .as_ref()
                .map(|cache| cache.events.clone())
                .unwrap_or_default();
            events.extend(remote_cache.events);
            let message_limit = local
                .as_ref()
                .map_or(remote_cache.message_limit, |cache| {
                    cache.message_limit.max(remote_cache.message_limit)
                })
                .max(INITIAL_GROUP_MESSAGE_WINDOW);
            let latest_cursor = [
                local.as_ref().and_then(|cache| cache.latest_cursor.clone()),
                remote_cache.latest_cursor,
            ]
            .into_iter()
            .flatten()
            .max_by(|left, right| left.key().cmp(&right.key()));
            let older_cursor = [
                local.as_ref().and_then(|cache| cache.older_cursor.clone()),
                remote_cache.older_cursor,
            ]
            .into_iter()
            .flatten()
            .max_by(|left, right| left.key().cmp(&right.key()));
            let has_older_messages = remote_cache.has_older_messages
                || local.as_ref().is_some_and(|cache| cache.has_older_messages);
            let mut topic_streams = remote_cache.topic_streams;
            if let Some(local) = local.as_ref() {
                for (topic_id, local_stream) in &local.topic_streams {
                    let stream = topic_streams
                        .entry(topic_id.clone())
                        .or_insert_with(|| local_stream.clone());
                    stream.message_limit = stream.message_limit.max(local_stream.message_limit);
                    stream.has_older_messages |= local_stream.has_older_messages;
                    stream.latest_cursor = [
                        stream.latest_cursor.clone(),
                        local_stream.latest_cursor.clone(),
                    ]
                    .into_iter()
                    .flatten()
                    .max_by(|left, right| left.key().cmp(&right.key()));
                    stream.older_cursor = [
                        stream.older_cursor.clone(),
                        local_stream.older_cursor.clone(),
                    ]
                    .into_iter()
                    .flatten()
                    .max_by(|left, right| left.key().cmp(&right.key()));
                }
            }
            let mut cache = compact_group_event_cache(
                state,
                &group,
                events,
                message_limit,
                latest_cursor,
                older_cursor,
                has_older_messages,
                topic_streams,
            )
            .context("encrypted account vault contains invalid group history")?;
            if preserve_remote_portable {
                cache.needs_recent_hydration = true;
                cache.portable_conversation = remote_portable_conversation;
            }
            cache.needs_control_hydration = needs_control_hydration;
            state.group_event_caches.insert(group_id, cache);
        }
        state
            .group_event_caches
            .retain(|group_id, _| present_group_ids.contains(group_id));
        let recovery_changed = recovery_changed_before_merge
            || recovery_before != state.mls_group_states
            || control_logs_before != state.mls_control_logs;
        let event_caches_changed = event_caches_before != state.group_event_caches;
        if state.active_group_id.is_none()
            && contents.active_group_id.as_ref().is_some_and(|group_id| {
                present_group_ids.contains(group_id)
                    && state.groups.iter().any(|group| {
                        group.group_id == *group_id
                            && (group.content_rating != GroupContentRating::Explicit
                                || state.adult_access.explicit_content_enabled)
                    })
            })
        {
            state.active_group_id = contents.active_group_id.clone();
        }
        let memberships_changed = memberships_before != state.group_memberships
            || groups_before != state.groups
            || frequencies_before != state.group_frequencies;
        let direct_reads_changed = state.merge_direct_read_through(&contents.direct_read_through);
        let direct_policy_rejections_changed = merge_read_markers(
            &mut state.direct_policy_rejected_through,
            &contents.direct_policy_rejected_through,
        );
        let group_reads_changed = state.merge_group_read_through(&contents.group_read_through);
        let topic_reads_changed = state.merge_topic_read_through(&contents.topic_read_through);
        let blocks_changed = merge_block_states(&mut state.block_states, &contents.block_states);
        let blocked_by_changed =
            merge_block_states(&mut state.blocked_by_states, &contents.blocked_by_states);
        if blocks_changed || blocked_by_changed {
            state.apply_block_filters();
        }
        let group_activity_changed = merge_read_markers(
            &mut state.group_latest_activity,
            &contents.group_latest_activity,
        );
        let initialized_before = state.group_activity_initialized.len();
        state
            .group_activity_initialized
            .extend(contents.group_activity_initialized);
        let topic_initialized_before = state.topic_activity_initialized.len();
        state
            .topic_activity_initialized
            .extend(contents.topic_activity_initialized);
        let initialized_changed = state.group_activity_initialized.len() != initialized_before
            || state.topic_activity_initialized.len() != topic_initialized_before;
        let account = state
            .account
            .as_mut()
            .context("this identity has no noise ID")?;
        let revision_changed = remote.revision > account.revision;
        account.revision = account.revision.max(remote.revision);
        Ok(direct_reads_changed
            || direct_policy_rejections_changed
            || profile_changed
            || adult_access_changed
            || contact_signal_changed
            || devices_changed
            || memberships_changed
            || group_reads_changed
            || topic_reads_changed
            || blocks_changed
            || blocked_by_changed
            || group_activity_changed
            || initialized_changed
            || recovery_changed
            || event_caches_changed
            || revision_changed)
    }

    async fn publish_account_vault(
        &self,
        relays: &[RelayDescriptor],
        vault: &AccountVault,
    ) -> anyhow::Result<()> {
        let body = serde_json::to_vec(vault)?;
        let mut requests = FuturesUnordered::new();
        for index in 0..relays.len() {
            let body = &body;
            requests.push(async move {
                (
                    index,
                    self.relay_request(relays, index, "POST", "/v1/accounts", body)
                        .await,
                )
            });
        }

        let mut accepted = 0usize;
        let mut failures = Vec::with_capacity(relays.len());
        while let Some((index, result)) = requests.next().await {
            match result {
                Ok(response) if (200..300).contains(&response.status) => {
                    accepted += 1;
                }
                Ok(response) => {
                    let detail = String::from_utf8_lossy(&response.body);
                    failures.push(format!(
                        "{} returned {}{}",
                        relays[index].base_url,
                        response.status,
                        if detail.trim().is_empty() {
                            String::new()
                        } else {
                            format!(": {}", detail.trim())
                        },
                    ));
                }
                Err(error) => {
                    failures.push(format!("{}: {error:#}", relays[index].base_url));
                }
            }
        }
        if accepted == 0 {
            bail!(
                "no relay accepted the encrypted account vault ({})",
                failures.join("; ")
            )
        }
        Ok(())
    }

    async fn fetch_account_vault(
        &self,
        relays: &[RelayDescriptor],
        locator: &str,
    ) -> anyhow::Result<AccountVault> {
        let endpoint = format!("/v1/accounts/{locator}");
        let mut requests = FuturesUnordered::new();
        for index in 0..relays.len() {
            let endpoint = &endpoint;
            requests.push(async move {
                self.relay_request(relays, index, "GET", endpoint, &[])
                    .await
            });
        }

        let mut newest: Option<AccountVault> = None;
        while let Some(result) = requests.next().await {
            let Ok(response) = result else {
                continue;
            };
            if !(200..300).contains(&response.status) {
                continue;
            }
            let Ok(vault) = serde_json::from_slice::<AccountVault>(&response.body) else {
                continue;
            };
            if vault.verify().is_err() || vault.locator != locator || vault.deleted {
                continue;
            }
            if newest
                .as_ref()
                .is_none_or(|current| vault.revision > current.revision)
            {
                newest = Some(vault);
            }
        }
        newest.context("account vault is unavailable")
    }

    async fn publish_invite_rotation(
        &self,
        relays: &[RelayDescriptor],
        rotation: &InviteRotation,
    ) -> anyhow::Result<()> {
        let body = serde_json::to_vec(rotation)?;
        let mut accepted = 0usize;
        for index in 0..relays.len() {
            if let Ok(response) = self
                .relay_request(relays, index, "POST", "/v1/invite-rotations", &body)
                .await
                && (200..300).contains(&response.status)
            {
                accepted += 1;
            }
        }
        if accepted != relays.len() {
            bail!("the frequency could not be rotated on every relay; try again")
        }
        Ok(())
    }

    async fn publish_event(
        &self,
        relays: &[RelayDescriptor],
        event: &SignedEvent,
    ) -> anyhow::Result<()> {
        self.publish_event_diagnostic(relays, event)
            .await
            .map_err(anyhow::Error::new)
    }

    async fn publish_direct_push(
        &self,
        relays: &[RelayDescriptor],
        trigger: &DirectPushTrigger,
        event: &SignedEvent,
    ) -> anyhow::Result<()> {
        let body = serde_json::to_vec(&DirectPushRequest {
            trigger: trigger.clone(),
            event: event.clone(),
        })?;
        let mut requests = FuturesUnordered::new();
        for index in 0..relays.len() {
            let body = &body;
            requests.push(async move {
                self.relay_request(relays, index, "POST", "/v1/push/direct-events", body)
                    .await
            });
        }
        while let Some(result) = requests.next().await {
            if result.is_ok_and(|response| (200..300).contains(&response.status)) {
                return Ok(());
            }
        }
        bail!("no configured relay accepted the direct push trigger")
    }

    async fn publish_event_diagnostic(
        &self,
        relays: &[RelayDescriptor],
        event: &SignedEvent,
    ) -> Result<(), EventPublishFailure> {
        let body = serde_json::to_vec(event).map_err(|error| EventPublishFailure {
            details: error.to_string(),
            all_relays_deleted: false,
        })?;
        let mut publications = FuturesUnordered::new();
        for (index, relay) in relays.iter().cloned().enumerate() {
            let client = self.clone();
            let body = body.clone();
            let request = async move {
                let result = client
                    .relay_request(std::slice::from_ref(&relay), 0, "POST", "/v1/events", &body)
                    .await;
                (index, result)
            };
            let (request, handle) = request.remote_handle();
            #[cfg(not(target_arch = "wasm32"))]
            tokio::spawn(request);
            #[cfg(target_arch = "wasm32")]
            wasm_bindgen_futures::spawn_local(request);
            publications.push(handle);
        }
        let mut failures = Vec::with_capacity(relays.len());
        let mut deleted_relays = 0usize;
        while let Some((index, result)) = publications.next().await {
            match result {
                Ok(response) if (200..300).contains(&response.status) => {
                    // A lagging replica must not hold the sender UI, but do not
                    // cancel its in-flight publication either. The receiver may
                    // currently be watching that exact relay, and waiting for
                    // peer gossip makes delivery latency nondeterministic.
                    for publication in publications {
                        publication.forget();
                    }
                    return Ok(());
                }
                Ok(response) => {
                    if response.status == 410 {
                        deleted_relays += 1;
                    }
                    let detail = String::from_utf8_lossy(&response.body);
                    let detail = detail.trim();
                    failures.push(if detail.is_empty() {
                        format!("{} returned {}", relays[index].base_url, response.status)
                    } else {
                        format!(
                            "{} returned {}: {detail}",
                            relays[index].base_url, response.status
                        )
                    });
                }
                Err(error) => {
                    failures.push(format!("{}: {error:#}", relays[index].base_url));
                }
            }
        }
        Err(EventPublishFailure {
            details: failures.join("; "),
            all_relays_deleted: deleted_relays == relays.len(),
        })
    }

    fn storage_relays(
        &self,
        relays: &[RelayDescriptor],
        placement_key: &str,
    ) -> Vec<RelayDescriptor> {
        let mut unique = HashMap::<String, RelayDescriptor>::new();
        for relay in relays.iter().chain(self.mask_relays.iter()) {
            unique
                .entry(relay.base_url.clone())
                .and_modify(|current| {
                    if current.ohttp_config.is_none() && relay.ohttp_config.is_some() {
                        *current = relay.clone();
                    }
                })
                .or_insert_with(|| relay.clone());
        }
        let mut candidates = unique.into_values().collect::<Vec<_>>();
        candidates.sort_by(|left, right| {
            storage_relay_score(placement_key, &right.base_url)
                .cmp(&storage_relay_score(placement_key, &left.base_url))
                .then_with(|| left.base_url.cmp(&right.base_url))
        });
        candidates.truncate(noise_core::MAX_STORAGE_SHARDS);
        candidates
    }

    async fn delete_storage_manifest(&self, manifest: &StorageManifest, key_base64: &str) {
        let _ = self
            .erase_storage_references(vec![(manifest.clone(), key_base64.to_owned())], false)
            .await;
    }

    async fn erase_storage_references(
        &self,
        references: Vec<(StorageManifest, String)>,
        require_all: bool,
    ) -> anyhow::Result<()> {
        let mut deletions = FuturesUnordered::new();
        let mut seen = HashSet::new();
        for (manifest, key_base64) in references {
            for placement in manifest.placements {
                if !seen.insert(placement.shard_id.clone()) {
                    continue;
                }
                let deletion = noise_core::shard_deletion_bytes(&key_base64, &placement.shard_id)?;
                let client = self.clone();
                deletions.push(async move {
                    let relay = RelayDescriptor::parse(&placement.relay)?;
                    let response = client
                        .relay_request(
                            std::slice::from_ref(&relay),
                            0,
                            "DELETE",
                            &format!("/v4/shards/{}", placement.shard_id),
                            &deletion,
                        )
                        .await?;
                    anyhow::ensure!(
                        (200..300).contains(&response.status) || response.status == 404,
                        "storage relay rejected shard deletion with status {}",
                        response.status
                    );
                    Ok::<(), anyhow::Error>(())
                });
            }
        }
        let mut failed = 0usize;
        while let Some(result) = deletions.next().await {
            if result.is_err() {
                failed += 1;
            }
        }
        if require_all && failed != 0 {
            bail!("{failed} encrypted media shards could not be erased; try again")
        }
        Ok(())
    }

    async fn store_blob_shards(
        &self,
        relays: &[RelayDescriptor],
        blob: &EncryptedBlob,
        key_base64: &str,
    ) -> anyhow::Result<StorageManifest> {
        let storage_relays = self.storage_relays(relays, key_base64);
        let relay_addresses = storage_relays.iter().map(relay_address).collect::<Vec<_>>();
        let (manifest, shards) = encode_blob_for_storage(blob, key_base64, &relay_addresses)?;
        let required = usize::from(manifest.data_shards);
        let mut uploads = FuturesUnordered::new();
        for (relay, shard) in storage_relays.into_iter().zip(shards) {
            let client = self.clone();
            uploads.push(async move {
                let body = shard.binary_upload()?;
                let response = client
                    .relay_request(std::slice::from_ref(&relay), 0, "POST", "/v4/shards", &body)
                    .await?;
                anyhow::ensure!(
                    (200..300).contains(&response.status),
                    "storage relay rejected shard with status {}",
                    response.status
                );
                Ok::<String, anyhow::Error>(shard.shard_id)
            });
        }
        let mut accepted = HashSet::new();
        while let Some(result) = uploads.next().await {
            if let Ok(shard_id) = result {
                accepted.insert(shard_id);
            }
        }
        if accepted.len() < required {
            let mut cleanup = manifest.clone();
            cleanup
                .placements
                .retain(|placement| accepted.contains(&placement.shard_id));
            self.delete_storage_manifest(&cleanup, key_base64).await;
            bail!(
                "only {} of {required} required media shards reached storage relays",
                accepted.len()
            )
        }
        manifest.verify(&blob.blob_id)?;
        Ok(manifest)
    }

    async fn publish_group_deletion(
        &self,
        relays: &[RelayDescriptor],
        deletion: &GroupDeletion,
    ) -> anyhow::Result<()> {
        let body = serde_json::to_vec(deletion)?;
        let mut accepted = 0usize;
        for index in 0..relays.len() {
            if let Ok(response) = self
                .relay_request(relays, index, "POST", "/v1/group-deletions", &body)
                .await
                && (200..300).contains(&response.status)
            {
                accepted += 1;
            }
        }
        if accepted != relays.len() {
            bail!("the group could not be deleted from every relay; try again")
        }
        Ok(())
    }

    async fn reconstruct_blob(
        &self,
        manifest: &StorageManifest,
        key_base64: &str,
    ) -> anyhow::Result<EncryptedBlob> {
        manifest.verify(&manifest.object_id)?;
        anyhow::ensure!(
            manifest.version == noise_core::STREAMING_STORAGE_VERSION,
            "this attachment uses the retired alpha media format"
        );
        let required = usize::from(manifest.data_shards);
        let mut downloads = FuturesUnordered::new();
        for placement in manifest.placements.clone() {
            let client = self.clone();
            downloads.push(async move {
                let relay = RelayDescriptor::parse(&placement.relay)?;
                let response = client
                    .relay_request(
                        std::slice::from_ref(&relay),
                        0,
                        "GET",
                        &format!("/v4/shards/{}", placement.shard_id),
                        &[],
                    )
                    .await?;
                anyhow::ensure!(
                    (200..300).contains(&response.status),
                    "storage relay does not have shard"
                );
                anyhow::ensure!(
                    response.body.len() == manifest.shard_byte_length as usize
                        && blake3::hash(&response.body).to_hex().as_str() == placement.payload_hash,
                    "storage relay returned a corrupt shard"
                );
                Ok::<(String, Vec<u8>), anyhow::Error>((placement.shard_id, response.body))
            });
        }
        let mut shards = Vec::with_capacity(required);
        let mut healthy_shard_ids = HashSet::new();
        while let Some(result) = downloads.next().await {
            if let Ok((shard_id, payload)) = result {
                healthy_shard_ids.insert(shard_id.clone());
                shards.push((shard_id, payload));
                if shards.len() >= required {
                    break;
                }
            }
        }
        let blob = reconstruct_blob_from_storage_payloads(manifest, &shards)
            .context("encrypted media does not have enough healthy storage shards")?;
        #[cfg(not(target_arch = "wasm32"))]
        if healthy_shard_ids.len() < manifest.placements.len() {
            let client = self.clone();
            let manifest = manifest.clone();
            let key_base64 = key_base64.to_owned();
            let blob_to_repair = blob.clone();
            tokio::spawn(async move {
                client
                    .repair_storage(&blob_to_repair, &key_base64, &manifest, &healthy_shard_ids)
                    .await;
            });
        }
        Ok(blob)
    }

    async fn repair_storage(
        &self,
        blob: &EncryptedBlob,
        key_base64: &str,
        manifest: &StorageManifest,
        healthy_shard_ids: &HashSet<String>,
    ) {
        if manifest.placements.len() != usize::from(manifest.total_shards) {
            return;
        }
        let mut relays = vec![String::new(); usize::from(manifest.total_shards)];
        for placement in &manifest.placements {
            relays[usize::from(placement.shard_index)] = placement.relay.clone();
        }
        if relays.iter().any(String::is_empty) {
            return;
        }
        let Ok((repair_manifest, shards)) = encode_blob_for_storage(blob, key_base64, &relays)
        else {
            return;
        };
        for (placement, shard) in repair_manifest.placements.into_iter().zip(shards) {
            if healthy_shard_ids.contains(&placement.shard_id) {
                continue;
            }
            let Ok(relay) = RelayDescriptor::parse(&placement.relay) else {
                continue;
            };
            let already_healthy = self
                .relay_request(
                    std::slice::from_ref(&relay),
                    0,
                    "GET",
                    &format!("/v4/shards/{}", placement.shard_id),
                    &[],
                )
                .await
                .ok()
                .filter(|response| (200..300).contains(&response.status))
                .is_some_and(|response| {
                    response.body.len() == repair_manifest.shard_byte_length as usize
                        && blake3::hash(&response.body).to_hex().as_str() == placement.payload_hash
                });
            if already_healthy {
                continue;
            }
            let Ok(body) = shard.binary_upload() else {
                continue;
            };
            let _ = self
                .relay_request(std::slice::from_ref(&relay), 0, "POST", "/v4/shards", &body)
                .await;
        }
    }

    async fn fetch_invite(
        &self,
        relays: &[RelayDescriptor],
        locator: &str,
    ) -> anyhow::Result<InviteRecord> {
        for index in 0..relays.len() {
            let Ok(response) = self
                .relay_request(relays, index, "GET", &format!("/v1/invites/{locator}"), &[])
                .await
            else {
                continue;
            };
            if (200..300).contains(&response.status)
                && let Ok(invitation) = serde_json::from_slice::<InviteRecord>(&response.body)
            {
                return Ok(invitation);
            }
        }
        bail!("nothing here")
    }

    async fn fetch_events(
        &self,
        group: &GroupMembership,
        relays: Vec<RelayDescriptor>,
    ) -> anyhow::Result<Vec<SignedEvent>> {
        self.fetch_events_for_id(&group.group_id, relays).await
    }

    async fn fetch_direct_event_cache(
        &self,
        mailbox_id: &str,
        existing: DirectEventCache,
        relays: &[RelayDescriptor],
    ) -> anyhow::Result<DirectEventCache> {
        let latest_cursor =
            max_group_event_cursor(existing.latest_cursor.clone(), &existing.events, None);
        let request = latest_cursor
            .as_ref()
            .map_or(GroupEventPageRequest::Latest, GroupEventPageRequest::After);
        let Some(mut page) = self
            .fetch_group_event_page(mailbox_id, None, request, relays)
            .await?
        else {
            // Compatibility for relays that predate bounded pages. The result
            // is compacted before it reaches disk, so this fallback never
            // recreates the old unbounded device state.
            let events = self
                .fetch_events_for_id(mailbox_id, relays.to_vec())
                .await?;
            return Ok(compact_direct_event_cache(mailbox_id, events, false));
        };

        let mut events = existing.events;
        events.extend(page.events);
        while latest_cursor.is_some() && page.has_more {
            let cursor = page
                .continuation_cursor
                .context("relay direct-message page has no continuation cursor")?;
            page = self
                .fetch_group_event_page(
                    mailbox_id,
                    None,
                    GroupEventPageRequest::After(&cursor),
                    relays,
                )
                .await?
                .context("the configured relays stopped paginating direct messages")?;
            if let Some(next_cursor) = page.continuation_cursor.as_ref()
                && next_cursor.key() <= cursor.key()
            {
                bail!("relay returned a non-advancing direct-message page")
            }
            events.extend(page.events);
        }
        let has_older_events =
            existing.has_older_events || (latest_cursor.is_none() && page.has_more);
        Ok(compact_direct_event_cache(
            mailbox_id,
            events,
            has_older_events,
        ))
    }

    async fn fetch_group_event_page(
        &self,
        group_id: &str,
        stream_locator: Option<&str>,
        request: GroupEventPageRequest<'_>,
        relays: &[RelayDescriptor],
    ) -> anyhow::Result<Option<GroupEventPage>> {
        let endpoint = match (stream_locator, request) {
            (None, GroupEventPageRequest::Latest) => {
                format!("/v2/groups/{group_id}/events/latest")
            }
            (None, GroupEventPageRequest::Before(cursor)) => format!(
                "/v2/groups/{group_id}/events/before/{}/{}",
                cursor.created_at_millis, cursor.event_id
            ),
            (None, GroupEventPageRequest::After(cursor)) => format!(
                "/v2/groups/{group_id}/events/after/{}/{}",
                cursor.created_at_millis, cursor.event_id
            ),
            (Some(stream_locator), GroupEventPageRequest::Latest) => {
                format!("/v3/groups/{group_id}/streams/{stream_locator}/events/latest")
            }
            (Some(stream_locator), GroupEventPageRequest::Before(cursor)) => format!(
                "/v3/groups/{group_id}/streams/{stream_locator}/events/before/{}/{}",
                cursor.created_at_millis, cursor.event_id
            ),
            (Some(stream_locator), GroupEventPageRequest::After(cursor)) => format!(
                "/v3/groups/{group_id}/streams/{stream_locator}/events/after/{}/{}",
                cursor.created_at_millis, cursor.event_id
            ),
        };
        let mut requests = FuturesUnordered::new();
        for index in 0..relays.len() {
            let endpoint = endpoint.clone();
            requests.push(async move {
                self.relay_request(relays, index, "GET", &endpoint, &[])
                    .await
            });
        }

        let mut merged = Vec::new();
        let mut has_more = false;
        let mut continuation_cursors = Vec::new();
        let mut supported = 0usize;
        let mut responded = 0usize;
        while !requests.is_empty() {
            let response = if supported == 0 {
                requests.next().await
            } else {
                use futures_util::future::{Either, select};
                match select(Box::pin(requests.next()), Box::pin(replica_settle_delay())).await {
                    Either::Left((response, _)) => response,
                    Either::Right(_) => break,
                }
            };
            let Some(response) = response else {
                break;
            };
            let Ok(response) = response else {
                continue;
            };
            responded += 1;
            if response.status == 404 {
                continue;
            }
            if !(200..300).contains(&response.status) {
                continue;
            }
            let Ok(page) = serde_json::from_slice::<GroupEventPage>(&response.body) else {
                continue;
            };
            if page.events.len() > GROUP_EVENT_PAGE_SIZE
                || page
                    .events
                    .iter()
                    .any(|event| event.stream_locator.as_deref() != stream_locator)
            {
                continue;
            }
            supported += 1;
            has_more |= page.has_more;
            let continuation = match request {
                GroupEventPageRequest::Latest | GroupEventPageRequest::Before(_) => {
                    page.events.first()
                }
                GroupEventPageRequest::After(_) => page.events.last(),
            }
            .map(GroupEventCursor::from_event);
            if let Some(continuation) = continuation {
                continuation_cursors.push(continuation);
            }
            merged.extend(page.events);
        }
        if supported == 0 {
            if responded > 0 {
                return Ok(None);
            }
            bail!("no relay was reachable")
        }
        let continuation_cursor = match request {
            GroupEventPageRequest::Latest | GroupEventPageRequest::Before(_) => {
                continuation_cursors
                    .into_iter()
                    .max_by(|left, right| left.key().cmp(&right.key()))
            }
            GroupEventPageRequest::After(_) => continuation_cursors
                .into_iter()
                .min_by(|left, right| left.key().cmp(&right.key())),
        };
        Ok(Some(GroupEventPage {
            events: merge_group_events(group_id, merged),
            has_more,
            continuation_cursor,
        }))
    }

    async fn fetch_events_for_id(
        &self,
        id: &str,
        relays: Vec<RelayDescriptor>,
    ) -> anyhow::Result<Vec<SignedEvent>> {
        if let Some(mut page) = self
            .fetch_group_event_page(id, None, GroupEventPageRequest::Latest, &relays)
            .await?
        {
            let mut events = page.events;
            while page.has_more {
                let cursor = page
                    .continuation_cursor
                    .context("relay history page has no continuation cursor")?;
                page = self
                    .fetch_group_event_page(
                        id,
                        None,
                        GroupEventPageRequest::Before(&cursor),
                        &relays,
                    )
                    .await?
                    .context("the configured relays stopped paginating group history")?;
                if let Some(next_cursor) = page.continuation_cursor.as_ref()
                    && next_cursor.key() >= cursor.key()
                {
                    bail!("relay returned a non-advancing group history page")
                }
                events.extend(page.events);
            }
            return Ok(merge_group_events(id, events));
        }

        // Compatibility for older relays that predate bounded event pages.
        let endpoint = format!("/v1/groups/{id}/events");
        let mut requests = FuturesUnordered::new();
        for index in 0..relays.len() {
            let endpoint = endpoint.clone();
            let relays = &relays;
            requests.push(async move {
                (
                    index,
                    self.relay_request(&relays, index, "GET", &endpoint, &[])
                        .await,
                )
            });
        }

        let mut merged = HashMap::<String, SignedEvent>::new();
        let mut reachable = 0usize;
        let mut failures = Vec::new();
        while !requests.is_empty() {
            let response = if reachable == 0 {
                requests.next().await
            } else {
                use futures_util::future::{Either, select};
                match select(Box::pin(requests.next()), Box::pin(replica_settle_delay())).await {
                    Either::Left((response, _)) => response,
                    Either::Right(_) => break,
                }
            };
            let Some((index, result)) = response else {
                continue;
            };
            let response = match result {
                Ok(response) => response,
                Err(error) => {
                    failures.push(format!("{}: {error:#}", relays[index].base_url));
                    continue;
                }
            };
            if !(200..300).contains(&response.status) {
                failures.push(format!(
                    "{} returned {}",
                    relays[index].base_url, response.status
                ));
                continue;
            }
            let Ok(events) = serde_json::from_slice::<Vec<SignedEvent>>(&response.body) else {
                failures.push(format!(
                    "{} returned invalid group history",
                    relays[index].base_url
                ));
                continue;
            };
            reachable += 1;
            for event in events {
                if event.verify().is_ok() {
                    merged.entry(event.event_id.clone()).or_insert(event);
                }
            }
        }
        if reachable == 0 {
            bail!("no relay was reachable ({})", failures.join("; "))
        }
        Ok(merged.into_values().collect())
    }

    async fn relay_request(
        &self,
        relays: &[RelayDescriptor],
        storage_index: usize,
        method: &str,
        path: &str,
        body: &[u8],
    ) -> anyhow::Result<PlainResponse> {
        let storage = relays
            .get(storage_index)
            .context("relay index is invalid")?;
        if let Some(config) = storage.ohttp_config.as_deref() {
            let relay_masks =
                (1..relays.len()).map(|offset| &relays[(storage_index + offset) % relays.len()]);
            let mut seen_masks = HashSet::new();
            let mut requests = FuturesUnordered::new();
            for mask in self.mask_relays.iter().chain(relay_masks) {
                if mask.base_url == storage.base_url || !seen_masks.insert(mask.base_url.clone()) {
                    continue;
                }
                requests.push(self.oblivious_request(storage, mask, config, method, path, body));
            }

            let mut last_error = None;
            while let Some(result) = requests.next().await {
                match result {
                    Ok(response) => return Ok(response),
                    Err(error) => last_error = Some(error),
                }
            }
            if let Some(error) = last_error {
                return Err(error);
            }
        }
        self.direct_request(storage, method, path, body).await
    }

    async fn oblivious_request(
        &self,
        storage: &RelayDescriptor,
        mask: &RelayDescriptor,
        config: &[u8],
        method: &str,
        path: &str,
        body: &[u8],
    ) -> anyhow::Result<PlainResponse> {
        let storage_url =
            reqwest::Url::parse(&storage.base_url).context("storage relay address is invalid")?;
        let host = storage_url
            .host_str()
            .context("storage relay has no host")?;
        let host = if host.contains(':') {
            format!("[{host}]")
        } else {
            host.to_owned()
        };
        let authority = storage_url
            .port()
            .map_or(host.clone(), |port| format!("{host}:{port}"));
        let request = encode_request(method, storage_url.scheme(), &authority, path, body)?;
        let client = ClientRequest::from_encoded_config(config)
            .context("storage relay privacy key is invalid")?;
        let (encrypted_request, response_context) = client
            .encapsulate(&request)
            .context("could not seal private relay request")?;
        let endpoint = format!("{}{OHTTP_RELAY_PATH}", mask.base_url);
        #[cfg(not(target_arch = "wasm32"))]
        let http = if self
            .mask_relays
            .iter()
            .any(|candidate| candidate.base_url == mask.base_url)
        {
            relay_pool::client_for_mask(&mask.base_url).await?
        } else {
            self.http.clone()
        };
        #[cfg(target_arch = "wasm32")]
        let http = self.http.clone();
        let response = http
            .post(endpoint)
            .header(reqwest::header::CONTENT_TYPE, OHTTP_REQUEST_MEDIA_TYPE)
            .header(GATEWAY_HEADER, &storage.base_url)
            .body(encrypted_request)
            .timeout(std::time::Duration::from_secs(RELAY_REQUEST_TIMEOUT_SECS))
            .send()
            .await
            .context("privacy mask is unreachable")?;
        if !response.status().is_success() {
            bail!(
                "privacy mask rejected the request with {}",
                response.status()
            )
        }
        if response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .is_none_or(|value| !value.trim().eq_ignore_ascii_case(OHTTP_RESPONSE_MEDIA_TYPE))
        {
            bail!("privacy mask returned an invalid response")
        }
        if response
            .content_length()
            .is_some_and(|length| length > 2_600_000)
        {
            bail!("privacy response is too large")
        }
        let encrypted_response = response.bytes().await?;
        if encrypted_response.len() > 2_600_000 {
            bail!("privacy response is too large")
        }
        let response = response_context
            .decapsulate(&encrypted_response)
            .context("could not open private relay response")?;
        decode_response(&response)
    }

    async fn direct_request(
        &self,
        relay: &RelayDescriptor,
        method: &str,
        path: &str,
        body: &[u8],
    ) -> anyhow::Result<PlainResponse> {
        let method = reqwest::Method::from_bytes(method.as_bytes())?;
        let mut request = self
            .http
            .request(method, format!("{}{path}", relay.base_url));
        if !body.is_empty() {
            let content_type = if path.starts_with("/v4/shards") {
                "application/octet-stream"
            } else {
                "application/json"
            };
            request = request
                .header(reqwest::header::CONTENT_TYPE, content_type)
                .body(body.to_vec());
        }
        let response = request
            .timeout(std::time::Duration::from_secs(RELAY_REQUEST_TIMEOUT_SECS))
            .send()
            .await?;
        let status = response.status().as_u16();
        if response
            .content_length()
            .is_some_and(|length| length > 2_600_000)
        {
            bail!("relay response is too large")
        }
        let body = response.bytes().await?.to_vec();
        if body.len() > 2_600_000 {
            bail!("relay response is too large")
        }
        Ok(PlainResponse { status, body })
    }
}

fn decrypt_direct_event(
    identity: &Identity,
    state: &ClientState,
    event: &SignedEvent,
) -> Option<DecryptedDirectEvent> {
    let self_public_key = identity.public_key_base64();
    if event.group_id != direct_mailbox_id(&self_public_key).ok()? {
        return None;
    }
    if event.author_public_key == self_public_key {
        for contact in state
            .direct_contacts
            .iter()
            .chain(state.known_people.iter())
        {
            let mailbox = identity
                .direct_mailbox(&contact.public_key, &self_public_key)
                .ok()?;
            match event.decrypt(&mailbox) {
                Ok(GroupEventPayload::DirectMessage {
                    recipient_public_key,
                    sender_profile,
                    text,
                    attachment,
                    reply_to_message_id,
                    forwarded_from,
                }) if recipient_public_key == contact.public_key
                    && valid_direct_profile(&sender_profile)
                    && valid_direct_content(&text, attachment.as_ref())
                    && validate_forwarded_from(forwarded_from.as_ref()).is_ok()
                    && validate_reply_reference(reply_to_message_id.as_deref()).is_ok() =>
                {
                    let direct_message_policy = sender_profile.effective_direct_message_policy();
                    return Some(DecryptedDirectEvent::Message(DecryptedDirectMessage {
                        counterparty_public_key: contact.public_key.clone(),
                        contact: contact.clone(),
                        message: DirectMessageSummary {
                            event_id: event.event_id.clone(),
                            message_id: direct_message_id(
                                &event.author_public_key,
                                event.author_sequence,
                            ),
                            author_public_key: event.author_public_key.clone(),
                            username: sender_profile.username,
                            bio: sender_profile.bio,
                            avatar: sender_profile.avatar,
                            album: sender_profile.album,
                            accepts_direct_messages: direct_message_policy
                                != DirectMessagePolicy::Nobody,
                            direct_message_policy,
                            text,
                            attachment,
                            reply_to_message_id,
                            forwarded_from,
                            created_at_millis: event.created_at_millis,
                        },
                    }));
                }
                Ok(GroupEventPayload::DirectThreadDeleted {
                    recipient_public_key,
                }) if recipient_public_key == contact.public_key => {
                    return Some(DecryptedDirectEvent::ThreadDeleted {
                        counterparty_public_key: contact.public_key.clone(),
                        deleted_at_millis: event.created_at_millis,
                    });
                }
                _ => continue,
            }
        }
        return None;
    }

    let mailbox = identity
        .direct_mailbox(&event.author_public_key, &self_public_key)
        .ok()?;
    match event.decrypt(&mailbox).ok()? {
        GroupEventPayload::DirectMessage {
            recipient_public_key,
            sender_profile,
            text,
            attachment,
            reply_to_message_id,
            forwarded_from,
        } if recipient_public_key == self_public_key
            && valid_direct_profile(&sender_profile)
            && valid_direct_content(&text, attachment.as_ref())
            && validate_forwarded_from(forwarded_from.as_ref()).is_ok()
            && validate_reply_reference(reply_to_message_id.as_deref()).is_ok() =>
        {
            let direct_message_policy = sender_profile.effective_direct_message_policy();
            let contact = DirectContact {
                public_key: event.author_public_key.clone(),
                username: sender_profile.username.clone(),
                bio: sender_profile.bio.clone(),
                avatar: sender_profile.avatar.clone(),
                album: sender_profile.album.clone(),
                accepts_direct_messages: direct_message_policy != DirectMessagePolicy::Nobody,
                direct_message_policy,
                profile_sequence: event.author_sequence,
            };
            Some(DecryptedDirectEvent::Message(DecryptedDirectMessage {
                counterparty_public_key: contact.public_key.clone(),
                contact,
                message: DirectMessageSummary {
                    event_id: event.event_id.clone(),
                    message_id: direct_message_id(&event.author_public_key, event.author_sequence),
                    author_public_key: event.author_public_key.clone(),
                    username: sender_profile.username,
                    bio: sender_profile.bio,
                    avatar: sender_profile.avatar,
                    album: sender_profile.album,
                    accepts_direct_messages: direct_message_policy != DirectMessagePolicy::Nobody,
                    direct_message_policy,
                    text,
                    attachment,
                    reply_to_message_id,
                    forwarded_from,
                    created_at_millis: event.created_at_millis,
                },
            }))
        }
        GroupEventPayload::DirectThreadDeleted {
            recipient_public_key,
        } if recipient_public_key == self_public_key => Some(DecryptedDirectEvent::ThreadDeleted {
            counterparty_public_key: event.author_public_key.clone(),
            deleted_at_millis: event.created_at_millis,
        }),
        GroupEventPayload::DirectBlockChanged {
            recipient_public_key,
            sender_profile,
            blocked,
        } if recipient_public_key == self_public_key && valid_direct_profile(&sender_profile) => {
            let direct_message_policy = sender_profile.effective_direct_message_policy();
            let contact = DirectContact {
                public_key: event.author_public_key.clone(),
                username: sender_profile.username,
                bio: sender_profile.bio,
                avatar: sender_profile.avatar,
                album: sender_profile.album,
                accepts_direct_messages: direct_message_policy != DirectMessagePolicy::Nobody,
                direct_message_policy,
                profile_sequence: event.author_sequence,
            };
            Some(DecryptedDirectEvent::BlockChanged {
                counterparty_public_key: contact.public_key.clone(),
                contact,
                blocked,
                sequence: event.author_sequence,
                changed_at_millis: event.created_at_millis,
            })
        }
        _ => None,
    }
}

fn sync_mls_state_from_log(
    state: &mut ClientState,
    log: &MlsControlLog,
) -> anyhow::Result<Option<u64>> {
    log.verify().context("group MLS control log is invalid")?;
    let group_id = log.genesis.group_id.as_str();
    let mut candidate = state.ensure_mls_group_state(group_id)?.clone();
    let local_epoch = candidate
        .epoch(&log.genesis.group_id)
        .ok()
        .map(|epoch| epoch.epoch);
    let mut current_epoch = if let Some(local_epoch) = local_epoch {
        if local_epoch == 0 {
            validate_mls_member_accounts(
                &candidate,
                &log.genesis.group_id,
                &log.genesis.member_accounts,
            )?;
        }
        local_epoch
    } else {
        let mut joined = None;
        for (index, record) in log.epochs.iter().enumerate().rev() {
            let Some(welcome) = record.bundle.welcome_base64.as_deref() else {
                continue;
            };
            let mut attempt = candidate.clone();
            if attempt.join_group(&log.genesis.group_id, welcome).is_ok()
                && validate_mls_member_accounts(
                    &attempt,
                    &log.genesis.group_id,
                    &record.member_accounts,
                )
                .is_ok()
            {
                joined = Some((attempt, index, record.bundle.epoch));
                break;
            }
        }
        let Some((joined_state, joined_index, joined_epoch)) = joined else {
            return Ok(None);
        };
        candidate = joined_state;
        for record in log.epochs.iter().skip(joined_index + 1) {
            candidate
                .process_commit(&record.bundle)
                .context("could not advance this device to the current MLS epoch")?;
            validate_mls_member_accounts(
                &candidate,
                &log.genesis.group_id,
                &record.member_accounts,
            )?;
        }
        state.set_mls_group_state(group_id, candidate);
        return Ok(Some(
            log.epochs
                .last()
                .map(|record| record.bundle.epoch)
                .unwrap_or(joined_epoch),
        ));
    };

    let self_public_key = state.identity()?.public_key_base64();
    for record in &log.epochs {
        if record.bundle.epoch <= current_epoch {
            continue;
        }
        if let Err(error) = candidate.process_commit(&record.bundle) {
            if record.owner_public_key != self_public_key {
                return Err(error)
                    .context("could not advance this device to the current MLS epoch");
            }
            // Every installation of one account shares that account's
            // recoverable MLS leaf, and a leaf cannot replay a commit it
            // authored itself. Keep the last decryptable leaf intact until the
            // post-commit snapshot arrives through the encrypted account
            // vault. Discarding it on a timer destroys readable history and can
            // strand the installation before the recovery loop gets another
            // chance to merge the newer snapshot.
            return Ok(None);
        }
        validate_mls_member_accounts(&candidate, &log.genesis.group_id, &record.member_accounts)?;
        current_epoch = record.bundle.epoch;
    }
    state.set_mls_group_state(group_id, candidate);
    Ok(Some(current_epoch))
}

fn filter_conversation_for_blocks(conversation: &mut Conversation, blocked: &HashSet<String>) {
    if blocked.is_empty() {
        return;
    }
    conversation
        .members
        .retain(|member| !blocked.contains(&member.public_key));
    conversation
        .messages
        .retain(|message| !blocked.contains(&message.author_public_key));
    for message in &mut conversation.messages {
        for reaction in &mut message.reactions {
            reaction
                .reactor_public_keys
                .retain(|public_key| !blocked.contains(public_key));
            reaction.count = reaction.reactor_public_keys.len();
        }
        message.reactions.retain(|reaction| reaction.count > 0);
    }
    conversation.reports.retain(|report| {
        !blocked.contains(&report.reporter_public_key)
            && !blocked.contains(&report.message.author_public_key)
    });
    conversation.reported_message_event_ids.retain(|event_id| {
        conversation
            .messages
            .iter()
            .any(|message| &message.event_id == event_id)
    });
}

fn remove_message_from_conversation(conversation: &mut Conversation, message_event_id: &str) {
    conversation
        .messages
        .retain(|message| message.event_id != message_event_id);
    conversation
        .reports
        .retain(|report| report.message.event_id != message_event_id);
    conversation
        .reported_message_event_ids
        .retain(|event_id| event_id != message_event_id);
}

fn strip_conversation_media_previews(conversation: &mut Conversation) {
    for attachment in conversation
        .messages
        .iter_mut()
        .filter_map(|message| message.attachment.as_mut())
        .chain(
            conversation
                .reports
                .iter_mut()
                .filter_map(|report| report.message.attachment.as_mut()),
        )
    {
        attachment.preview_data_base64 = None;
        attachment.preview_mime_type = None;
    }
}

fn merge_portable_group_state(conversation: &mut Conversation, portable: &Conversation) {
    let is_active = conversation.group.is_active;
    let unread_count = conversation.group.unread_count;
    let read_state_initialized = conversation.group.read_state_initialized;
    let frequency = conversation
        .group
        .frequency
        .clone()
        .or_else(|| portable.group.frequency.clone());
    conversation.group = portable.group.clone();
    conversation.group.is_active = is_active;
    conversation.group.unread_count = unread_count;
    conversation.group.read_state_initialized = read_state_initialized;
    conversation.group.frequency = frequency;

    let current_topics = std::mem::take(&mut conversation.topics)
        .into_iter()
        .map(|topic| (topic.topic_id.clone(), topic))
        .collect::<HashMap<_, _>>();
    conversation.topics = portable
        .topics
        .iter()
        .map(|topic| {
            current_topics
                .get(&topic.topic_id)
                .cloned()
                .unwrap_or_else(|| topic.clone())
        })
        .collect();
    conversation.topics.extend(
        current_topics
            .into_iter()
            .filter(|(topic_id, _)| {
                !portable
                    .topics
                    .iter()
                    .any(|topic| topic.topic_id == *topic_id)
            })
            .map(|(_, topic)| topic),
    );

    if conversation.members.is_empty() {
        conversation.members = portable.members.clone();
    }
    if conversation.banned_members.is_empty() {
        conversation.banned_members = portable.banned_members.clone();
    }
}

fn topic_summaries(
    state: &ClientState,
    group_id: &str,
    topics: &HashMap<String, AcceptedTopic>,
) -> Vec<TopicSummary> {
    let mut summaries = topics
        .values()
        .map(|topic| TopicSummary {
            topic_id: topic.topic_id.clone(),
            name: topic.name.clone(),
            icon: topic.icon.clone(),
            stream_locator: topic.stream_locator.clone(),
            locked: topic.locked,
            archived: topic.archived,
            created_by_public_key: topic.created_by_public_key.clone(),
            created_at_millis: topic.created_at_millis,
            unread_count: state.topic_unread_count(group_id, Some(&topic.topic_id)),
            has_older_messages: state
                .group_event_caches
                .get(group_id)
                .and_then(|cache| cache.topic_streams.get(&topic.topic_id))
                .is_some_and(|stream| stream.has_older_messages),
        })
        .collect::<Vec<_>>();
    summaries.sort_by(|left, right| {
        let left_order = topics
            .get(&left.topic_id)
            .map_or(usize::MAX, |topic| topic.sort_index);
        let right_order = topics
            .get(&right.topic_id)
            .map_or(usize::MAX, |topic| topic.sort_index);
        left_order
            .cmp(&right_order)
            .then_with(|| left.topic_id.cmp(&right.topic_id))
    });
    summaries
}

fn cached_conversation_from_view(
    state: &ClientState,
    group: &GroupMembership,
    view: GroupState,
    identity_public_key: &str,
) -> Conversation {
    let resolved_owner = view.owner_public_key.clone().unwrap_or_default();
    let resolved_profile = view.profile.clone();
    let moderators = view.moderators.clone();
    let mut banned_members = view
        .banned_profiles
        .values()
        .cloned()
        .map(|member| BannedMemberSummary {
            public_key: member.public_key,
            username: member.username,
            bio: member.bio,
            avatar: member.avatar,
        })
        .collect::<Vec<_>>();
    banned_members.sort_by(|left, right| left.username.cmp(&right.username));

    let can_view_reports =
        resolved_owner == identity_public_key || moderators.contains(identity_public_key);
    let mut reactions_by_message = HashMap::<String, Vec<ReactionSummary>>::new();
    for reaction in &view.reactions {
        let summaries = reactions_by_message
            .entry(reaction.message_event_id.clone())
            .or_default();
        if let Some(summary) = summaries
            .iter_mut()
            .find(|summary| summary.emoji == reaction.emoji)
        {
            summary.count += 1;
            summary
                .reactor_public_keys
                .push(reaction.reactor_public_key.clone());
            summary.reacted_by_self |= reaction.reactor_public_key == identity_public_key;
        } else {
            summaries.push(ReactionSummary {
                emoji: reaction.emoji.clone(),
                count: 1,
                reactor_public_keys: vec![reaction.reactor_public_key.clone()],
                reacted_by_self: reaction.reactor_public_key == identity_public_key,
            });
        }
    }

    let reported_message_event_ids = view
        .reports
        .iter()
        .filter(|report| report.reporter_public_key == identity_public_key)
        .map(|report| report.message_event_id.clone())
        .collect::<Vec<_>>();
    let reports = view
        .reports
        .iter()
        .filter(|_| can_view_reports)
        .filter_map(|report| {
            let message = view
                .messages
                .iter()
                .find(|message| message.event_id == report.message_event_id)?;
            Some(ReportSummary {
                report_event_id: report.event_id.clone(),
                reporter_public_key: report.reporter_public_key.clone(),
                reporter_username: report.reporter_username.clone(),
                reporter_avatar: report.reporter_avatar.clone(),
                reason: report.reason.clone(),
                created_at_millis: report.created_at_millis,
                message: MessageSummary {
                    event_id: message.event_id.clone(),
                    message_id: message.message_id.clone(),
                    author_public_key: message.author_public_key.clone(),
                    username: message.username.clone(),
                    bio: message.bio.clone(),
                    avatar: message.avatar.clone(),
                    album: message.album.clone(),
                    accepts_direct_messages: message.accepts_direct_messages,
                    direct_message_policy: message.direct_message_policy,
                    text: message.text.clone(),
                    attachment: message.attachment.clone(),
                    reply_to_message_id: message.reply_to_message_id.clone(),
                    forwarded_from: message.forwarded_from.clone(),
                    topic_id: message.topic_id.clone(),
                    created_at_millis: message.created_at_millis,
                    reactions: reactions_by_message
                        .get(&message.event_id)
                        .cloned()
                        .unwrap_or_default(),
                },
            })
        })
        .collect::<Vec<_>>();

    let topics = topic_summaries(state, &group.group_id, &view.topics);
    let mut members = view
        .members
        .into_values()
        .map(|member| MemberSummary {
            is_moderator: moderators.contains(&member.public_key),
            public_key: member.public_key,
            username: member.username,
            bio: member.bio,
            avatar: member.avatar,
            album: member.album,
            accepts_direct_messages: member.accepts_direct_messages,
            direct_message_policy: member.direct_message_policy,
        })
        .collect::<Vec<_>>();
    members.sort_by(|left, right| left.username.cmp(&right.username));

    let mut conversation = Conversation {
        group: GroupSummary {
            group_id: group.group_id.clone(),
            name: resolved_profile.name,
            description: resolved_profile.description,
            rules: resolved_profile.rules,
            content_rating: resolved_profile.content_rating,
            avatar: resolved_profile.avatar,
            background: resolved_profile.background,
            mobile_background: resolved_profile.mobile_background,
            accent_color: resolved_profile.accent_color,
            members_can_send_messages: resolved_profile.members_can_send_messages,
            members_can_send_media: resolved_profile.members_can_send_media,
            frequency: state
                .group_frequencies
                .get(&group.group_id)
                .and_then(|frequency| display_frequency(frequency).ok()),
            owner_public_key: resolved_owner,
            remote_deletion_supported: !group.authority_nonce_base64.is_empty(),
            is_active: state.active_group_id.as_deref() == Some(group.group_id.as_str()),
            unread_count: state.group_unread_count(&group.group_id),
            read_state_initialized: state.topic_read_state_initialized(&group.group_id, None),
        },
        topics,
        general_unread_count: state.topic_unread_count(&group.group_id, None),
        members,
        banned_members,
        messages: view
            .messages
            .into_iter()
            .map(|message| {
                let reactions = reactions_by_message
                    .remove(&message.event_id)
                    .unwrap_or_default();
                MessageSummary {
                    event_id: message.event_id,
                    message_id: message.message_id,
                    author_public_key: message.author_public_key,
                    username: message.username,
                    bio: message.bio,
                    avatar: message.avatar,
                    album: message.album,
                    accepts_direct_messages: message.accepts_direct_messages,
                    direct_message_policy: message.direct_message_policy,
                    text: message.text,
                    attachment: message.attachment,
                    reply_to_message_id: message.reply_to_message_id,
                    forwarded_from: message.forwarded_from,
                    topic_id: message.topic_id,
                    created_at_millis: message.created_at_millis,
                    reactions,
                }
            })
            .collect(),
        reports,
        reported_message_event_ids,
        rejected_events: view.rejected_events,
        has_older_messages: false,
    };
    apply_current_group_profiles(&mut conversation, state, identity_public_key);
    conversation
}

fn rebuild_group_state(
    state: &ClientState,
    group: &GroupMembership,
    events: &[SignedEvent],
) -> anyhow::Result<GroupState> {
    let Some(log) = state.mls_control_logs.get(&group.group_id) else {
        if events
            .iter()
            .any(|event| event.encryption_version == 2 || event.epoch.is_some())
        {
            bail!("this group must restore its encryption history before messages can sync")
        }
        return Ok(GroupState::rebuild(group, events));
    };
    log.verify().context("cached MLS control log is invalid")?;
    let mls = state
        .mls_group_state(&group.group_id)
        .context("this group has no recoverable MLS identity")?;
    let current = mls
        .epoch(&group.group_id)
        .context("this device is not in the current encrypted group")?;
    let links = log
        .epochs
        .iter()
        .filter(|record| record.bundle.epoch <= current.epoch)
        .map(|record| record.bundle.history_link.clone())
        .collect::<Vec<HistoryKeyLink>>();
    let epoch_keys = HistoryKeyLink::unlock_history(
        &group.group_id,
        current.epoch,
        &current.archive_key_base64,
        &links,
    )
    .context("could not unlock encrypted group history")?;
    let epoch_zero_key = epoch_keys
        .get(&0)
        .context("encrypted group history is missing epoch zero")?;
    let legacy_key = log
        .genesis
        .legacy_history_bridge
        .open(epoch_zero_key)
        .context("could not open legacy group history")?;
    if legacy_key != group.secret_base64 {
        bail!("legacy group history key does not match this group")
    }
    let mut epoch_members = HashMap::<u64, HashSet<String>>::new();
    epoch_members.insert(0, log.genesis.member_accounts.iter().cloned().collect());
    for record in &log.epochs {
        epoch_members.insert(
            record.bundle.epoch,
            record.member_accounts.iter().cloned().collect(),
        );
    }
    Ok(GroupState::rebuild_with_epoch_keys(
        group,
        events,
        &epoch_keys,
        &epoch_members,
    ))
}

fn group_cache_has_usable_control_state(
    state: &ClientState,
    group: &GroupMembership,
    cache: &GroupEventCache,
) -> bool {
    if cache.events.is_empty() {
        return false;
    }
    let Ok(identity_public_key) = state
        .identity()
        .map(|identity| identity.public_key_base64())
    else {
        return false;
    };
    rebuild_group_state(state, group, &cache.events)
        .is_ok_and(|view| view.members.contains_key(&identity_public_key))
}

fn merge_group_events(
    group_id: &str,
    events: impl IntoIterator<Item = SignedEvent>,
) -> Vec<SignedEvent> {
    let mut merged = HashMap::<String, SignedEvent>::new();
    for event in events {
        if event.group_id == group_id && event.verify().is_ok() {
            merged.entry(event.event_id.clone()).or_insert(event);
        }
    }
    let mut events = merged.into_values().collect::<Vec<_>>();
    events.sort_by(|left, right| {
        left.created_at_millis
            .cmp(&right.created_at_millis)
            .then_with(|| left.event_id.cmp(&right.event_id))
    });
    events
}

fn max_group_event_cursor(
    existing: Option<GroupEventCursor>,
    events: &[SignedEvent],
    stream_locator: Option<&str>,
) -> Option<GroupEventCursor> {
    events
        .iter()
        .filter(|event| event.stream_locator.as_deref() == stream_locator)
        .map(GroupEventCursor::from_event)
        .chain(existing)
        .max_by(|left, right| left.key().cmp(&right.key()))
}

fn compact_direct_event_cache(
    mailbox_id: &str,
    events: Vec<SignedEvent>,
    has_older_hint: bool,
) -> DirectEventCache {
    let mut events = merge_group_events(mailbox_id, events);
    let latest_cursor = max_group_event_cursor(None, &events, None);
    let was_truncated = events.len() > DIRECT_EVENT_CACHE_LIMIT;
    if was_truncated {
        let retained_start = events.len() - DIRECT_EVENT_CACHE_LIMIT;
        events.drain(..retained_start);
    }
    DirectEventCache {
        events,
        latest_cursor,
        has_older_events: has_older_hint || was_truncated,
    }
}

fn cached_direct_messages(state: &ClientState) -> anyhow::Result<Vec<DecryptedDirectMessage>> {
    let identity = state.identity()?;
    let self_public_key = identity.public_key_base64();
    let mut messages = state
        .direct_event_cache
        .events
        .iter()
        .filter_map(|event| decrypt_direct_event(&identity, state, event))
        .filter_map(|event| match event {
            DecryptedDirectEvent::Message(message)
                if message.message.created_at_millis
                    > state
                        .direct_deleted_before
                        .get(&message.counterparty_public_key)
                        .copied()
                        .unwrap_or_default()
                    && !state.is_hidden(&message.counterparty_public_key)
                    && (message.message.author_public_key == self_public_key
                        || !state
                            .direct_messages_blocked_at(message.message.created_at_millis)) =>
            {
                Some(message)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    messages.sort_by(|left, right| {
        left.message
            .created_at_millis
            .cmp(&right.message.created_at_millis)
            .then_with(|| left.message.event_id.cmp(&right.message.event_id))
    });
    Ok(messages)
}

fn direct_inbox_from_messages(
    state: &ClientState,
    messages: Vec<DecryptedDirectMessage>,
) -> anyhow::Result<DirectInbox> {
    let identity = state.identity()?;
    let self_public_key = identity.public_key_base64();
    let mut messages_by_contact = HashMap::<String, Vec<DirectMessageSummary>>::new();
    for message in messages {
        messages_by_contact
            .entry(message.counterparty_public_key)
            .or_default()
            .push(message.message);
    }
    let conversations = state
        .direct_contacts
        .iter()
        .map(|contact| {
            let mut messages = messages_by_contact
                .remove(&contact.public_key)
                .unwrap_or_default();
            apply_current_direct_profiles(&mut messages, &self_public_key, &state.profile, contact);
            Ok(DirectConversation {
                contact: direct_summary(
                    contact,
                    state.active_direct_public_key.as_deref() == Some(&contact.public_key),
                    state.direct_has_unread(&contact.public_key),
                ),
                media_scope_id: identity.direct_scope_id(&contact.public_key)?,
                messages,
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok(DirectInbox {
        summary: state.summary()?,
        conversations,
    })
}

fn compact_group_event_cache(
    state: &ClientState,
    group: &GroupMembership,
    events: Vec<SignedEvent>,
    message_limit: usize,
    latest_cursor: Option<GroupEventCursor>,
    older_cursor: Option<GroupEventCursor>,
    has_older_hint: bool,
    mut topic_streams: HashMap<String, TopicStreamCache>,
) -> anyhow::Result<GroupEventCache> {
    let mut events = merge_group_events(&group.group_id, events);
    let latest_cursor = max_group_event_cursor(latest_cursor, &events, None);
    let view = rebuild_group_state(state, group, &events)?;
    let general_message_ids = view
        .messages
        .iter()
        .filter(|message| message.topic_id.is_none())
        .map(|message| message.event_id.clone())
        .collect::<HashSet<_>>();
    let mut retained_message_ids = view
        .messages
        .iter()
        .filter(|message| message.topic_id.is_none())
        .rev()
        .take(message_limit.max(1))
        .map(|message| message.event_id.clone())
        .collect::<HashSet<_>>();
    let has_older_messages =
        has_older_hint || general_message_ids.len() > retained_message_ids.len();
    let retained_message_cursor = has_older_messages
        .then(|| {
            events
                .iter()
                .filter(|event| retained_message_ids.contains(&event.event_id))
                .map(GroupEventCursor::from_event)
                .min_by(|left, right| left.key().cmp(&right.key()))
        })
        .flatten();
    let older_cursor = [older_cursor, retained_message_cursor]
        .into_iter()
        .flatten()
        .max_by(|left, right| left.key().cmp(&right.key()));

    for topic in view.topics.values() {
        let stream = topic_streams
            .entry(topic.topic_id.clone())
            .or_insert_with(|| TopicStreamCache {
                latest_cursor: None,
                older_cursor: None,
                message_limit: INITIAL_GROUP_MESSAGE_WINDOW,
                has_older_messages: false,
            });
        stream.latest_cursor = max_group_event_cursor(
            stream.latest_cursor.clone(),
            &events,
            Some(&topic.stream_locator),
        );
        let topic_message_ids = view
            .messages
            .iter()
            .filter(|message| message.topic_id.as_deref() == Some(topic.topic_id.as_str()))
            .map(|message| message.event_id.clone())
            .collect::<HashSet<_>>();
        let retained_topic_ids = view
            .messages
            .iter()
            .filter(|message| message.topic_id.as_deref() == Some(topic.topic_id.as_str()))
            .rev()
            .take(stream.message_limit.max(1))
            .map(|message| message.event_id.clone())
            .collect::<HashSet<_>>();
        stream.has_older_messages |= topic_message_ids.len() > retained_topic_ids.len();
        if stream.has_older_messages {
            let retained_cursor = events
                .iter()
                .filter(|event| retained_topic_ids.contains(&event.event_id))
                .map(GroupEventCursor::from_event)
                .min_by(|left, right| left.key().cmp(&right.key()));
            stream.older_cursor = [stream.older_cursor.clone(), retained_cursor]
                .into_iter()
                .flatten()
                .max_by(|left, right| left.key().cmp(&right.key()));
        }
        retained_message_ids.extend(retained_topic_ids);
    }
    let accepted_message_ids = view
        .messages
        .iter()
        .map(|message| message.event_id.clone())
        .collect::<HashSet<_>>();
    events.retain(|event| {
        !accepted_message_ids.contains(&event.event_id)
            || retained_message_ids.contains(&event.event_id)
    });
    Ok(GroupEventCache {
        events,
        latest_cursor,
        older_cursor,
        message_limit: message_limit.max(1),
        has_older_messages,
        needs_recent_hydration: false,
        needs_control_hydration: false,
        topic_streams,
        portable_conversation: None,
    })
}

fn active_group_epoch(
    state: &ClientState,
    group_id: &str,
) -> anyhow::Result<Option<noise_core::MlsEpochSummary>> {
    if let Some(epoch) = state
        .mls_group_state(group_id)
        .and_then(|mls| mls.epoch(group_id).ok())
    {
        // Recoverable MLS state can arrive from the encrypted account vault
        // before this installation has cached the group's signed control log.
        // Do not silently downgrade an already encrypted group to a legacy
        // event during that window.
        return Ok(Some(epoch));
    }

    if state.mls_control_logs.contains_key(group_id) {
        return Ok(Some(
            state
                .mls_group_state(group_id)
                .context("this group has no recoverable MLS identity")?
                .epoch(group_id)
                .context("this device is not in the current encrypted group")?,
        ));
    }

    Ok(None)
}

fn create_group_event(
    state: &ClientState,
    identity: &Identity,
    group: &GroupMembership,
    payload: GroupEventPayload,
    author_sequence: u64,
) -> anyhow::Result<SignedEvent> {
    if let Some(epoch) = active_group_epoch(state, &group.group_id)? {
        Ok(SignedEvent::create_for_epoch(
            identity,
            group.group_id.clone(),
            &epoch.archive_key_base64,
            epoch.epoch,
            payload,
            author_sequence,
        )?)
    } else {
        Ok(SignedEvent::create_legacy(
            identity,
            group,
            payload,
            author_sequence,
        )?)
    }
}

fn create_group_event_in_stream(
    state: &ClientState,
    identity: &Identity,
    group: &GroupMembership,
    stream_locator: &str,
    payload: GroupEventPayload,
    author_sequence: u64,
) -> anyhow::Result<SignedEvent> {
    let epoch =
        active_group_epoch(state, &group.group_id)?.context("topics require an encrypted group")?;
    Ok(SignedEvent::create_for_epoch_in_stream(
        identity,
        group.group_id.clone(),
        &epoch.archive_key_base64,
        epoch.epoch,
        stream_locator.to_owned(),
        payload,
        author_sequence,
    )?)
}

fn cached_group_view(state: &ClientState, group: &GroupMembership) -> anyhow::Result<GroupState> {
    let cache = state
        .group_event_caches
        .get(&group.group_id)
        .context("open this group and try again")?;
    rebuild_group_state(state, group, &cache.events)
}

fn ensure_topic_manager(
    state: &ClientState,
    group: &GroupMembership,
    actor_public_key: &str,
) -> anyhow::Result<()> {
    let view = cached_group_view(state, group)?;
    if !view.members.contains_key(actor_public_key)
        || (view.owner_public_key.as_deref() != Some(actor_public_key)
            && !view.moderators.contains(actor_public_key))
    {
        bail!("only group owners and moderators can manage topics")
    }
    Ok(())
}

fn ensure_active_topic(
    state: &ClientState,
    group: &GroupMembership,
    topic_id: &str,
) -> anyhow::Result<AcceptedTopic> {
    let mut view = cached_group_view(state, group)?;
    let topic = view.topics.remove(topic_id).context("unknown topic")?;
    if topic.archived {
        bail!("this topic is archived")
    }
    Ok(topic)
}

fn validate_mls_member_accounts(
    mls: &MlsAccountState,
    group_id: &str,
    expected: &[String],
) -> anyhow::Result<()> {
    let mut actual = mls
        .members(group_id)
        .context("could not read MLS membership")?;
    actual.sort();
    actual.dedup();
    if actual != expected {
        bail!("MLS membership does not match the signed control record")
    }
    Ok(())
}

/// Every join request this group can admit from its current control head.
///
/// The filter reads only signed control-plane data, so every member derives the
/// same list and can agree on whose turn it is to publish the admission.
fn pending_admission_requests(
    state: &ClientState,
    group: &GroupMembership,
    active_members: &HashSet<String>,
    latest_removal_by_account: &HashMap<String, u64>,
    requests: Vec<MlsJoinRequest>,
) -> anyhow::Result<Vec<MlsJoinRequest>> {
    let mls = state
        .mls_group_state(&group.group_id)
        .context("this group has no recoverable MLS identity")?;
    let current_devices = mls
        .member_devices(&group.group_id)
        .context("could not read current MLS devices")?
        .into_iter()
        .map(|credential| credential.device_id_base64)
        .collect::<HashSet<_>>();
    let mut newest_by_device = HashMap::<String, MlsJoinRequest>::new();
    for request in requests {
        request.verify().context("invalid MLS join request")?;
        let has_membership_proof = join_request_membership_profile(&request, group).is_some();
        if request.group_id != group.group_id
            || latest_removal_by_account
                .get(&request.account_public_key)
                .is_some_and(|removed_at| *removed_at >= request.created_at_millis)
            || (!active_members.contains(&request.account_public_key) && !has_membership_proof)
            || current_devices.contains(&request.device_credential.device_id_base64)
        {
            continue;
        }
        newest_by_device
            .entry(request.device_credential.device_id_base64.clone())
            .and_modify(|current| {
                if (request.created_at_millis, request.request_id.as_str())
                    > (current.created_at_millis, current.request_id.as_str())
                {
                    *current = request.clone();
                }
            })
            .or_insert(request);
    }
    let mut pending = newest_by_device.into_values().collect::<Vec<_>>();
    pending.sort_by(|left, right| left.request_id.cmp(&right.request_id));
    Ok(pending)
}

fn prepare_member_add_epoch(
    state: &ClientState,
    identity: &Identity,
    group: &GroupMembership,
    current_members: &HashSet<String>,
    log: &MlsControlLog,
    requests: &[MlsJoinRequest],
) -> anyhow::Result<(MlsAccountState, MlsEpochRecord)> {
    let mls = state
        .mls_group_state(&group.group_id)
        .context("this group has no recoverable MLS identity")?;
    let packages = requests
        .iter()
        .map(|request| request.key_package_base64.clone())
        .collect::<Vec<_>>();
    let mut candidate = mls.clone();
    let bundle = candidate
        .add_members(&group.group_id, &packages)
        .context("could not add pending MLS devices")?;
    let (_, previous_record_id) = log.head();
    let record = candidate
        .create_epoch_record(identity, previous_record_id, bundle)
        .context("could not sign the next MLS epoch")?;
    let mut expected = current_members.iter().cloned().collect::<Vec<_>>();
    expected.extend(
        requests
            .iter()
            .filter(|request| join_request_membership_profile(request, group).is_some())
            .map(|request| request.account_public_key.clone()),
    );
    expected.sort();
    expected.dedup();
    if record.member_accounts != expected {
        bail!("pending MLS devices do not match the active group membership")
    }
    Ok((candidate, record))
}

/// Where this account sits in the order of members allowed to admit for a head.
///
/// The order is derived from the signed head record, so every member computes
/// the same one without talking to anybody.
fn admission_author_rank(
    head_record_id: &str,
    members: &HashSet<String>,
    self_public_key: &str,
) -> Option<usize> {
    if !members.contains(self_public_key) {
        return None;
    }
    let own_priority = admission_priority(head_record_id, self_public_key);
    Some(
        members
            .iter()
            .filter(|member| member.as_str() != self_public_key)
            .filter(|member| admission_priority(head_record_id, member) < own_priority)
            .count(),
    )
}

fn admission_priority(head_record_id: &str, member: &str) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new_derive_key("xyz.gnosyslabs.noise.mls-admission-order.v1");
    hasher.update(head_record_id.as_bytes());
    hasher.update(member.as_bytes());
    *hasher.finalize().as_bytes()
}

fn admission_digest(head_record_id: &str, requests: &[MlsJoinRequest]) -> String {
    let mut hasher = blake3::Hasher::new_derive_key("xyz.gnosyslabs.noise.mls-admission-set.v1");
    hasher.update(head_record_id.as_bytes());
    for request in requests {
        hasher.update(request.request_id.as_bytes());
    }
    hasher.finalize().to_hex().to_string()
}

/// Whether this member may publish the pending admission yet.
///
/// A relay keeps the first epoch it accepts for a head, so two members
/// publishing at once would leave replicas on competing branches. Members take
/// turns instead: the first-ranked member admits immediately, and later ranks
/// only step in when it stays absent.
fn admission_turn_reached(group_id: &str, digest: &str, rank: usize) -> bool {
    let handoff = rank.min(MAX_ADMISSION_HANDOFF_STEPS) as u64 * ADMISSION_HANDOFF_MILLIS;
    observation_wait_millis(&format!("admission:{group_id}"), digest) >= handoff
}

fn control_log_leads_to(observation: &MlsControlLog, newest: &MlsControlLog) -> bool {
    observation.genesis.record_id == newest.genesis.record_id
        && observation.epochs.len() <= newest.epochs.len()
        && observation
            .epochs
            .iter()
            .zip(newest.epochs.iter())
            .all(|(observed, expected)| observed.record_id == expected.record_id)
}

/// Drop local encryption state that belongs to a superseded branch of the log.
///
/// Competing epochs can only survive if the relays disagreed while two members
/// admitted at once. A device that merged the losing commit holds an archive key
/// nobody else derives, so it must rejoin instead of silently failing to read
/// and write the group.
fn discard_superseded_mls_state(
    state: &mut ClientState,
    log: &MlsControlLog,
) -> anyhow::Result<bool> {
    let group_id = log.genesis.group_id.as_str();
    let Some(epoch) = state
        .mls_group_state(group_id)
        .and_then(|mls| mls.epoch(group_id).ok())
    else {
        return Ok(false);
    };
    let belongs_to_log = if epoch.epoch == 0 {
        log.genesis
            .legacy_history_bridge
            .open(&epoch.archive_key_base64)
            .is_ok()
    } else if let Some(record) = log
        .epochs
        .iter()
        .find(|record| record.bundle.epoch == epoch.epoch)
    {
        record
            .bundle
            .history_link
            .open(&epoch.archive_key_base64)
            .is_ok()
    } else {
        // This device is ahead of what the relays have replayed so far.
        true
    };
    if belongs_to_log {
        return Ok(false);
    }
    state
        .forget_mls_group(group_id)
        .context("could not discard superseded group encryption state")?;
    state.mls_join_requests.remove(group_id);
    Ok(true)
}

fn prepare_pending_member_removal(
    state: &ClientState,
    identity: &Identity,
    group: &GroupMembership,
    view: &GroupState,
    log: &MlsControlLog,
    mut requests: Vec<MlsRemovalRequest>,
) -> anyhow::Result<Option<(MlsAccountState, MlsEpochRecord, Vec<MlsRemovalRequest>)>> {
    let mls = state
        .mls_group_state(&group.group_id)
        .context("this group has no recoverable MLS identity")?;
    let (head_epoch, previous_record_id) = log.head();
    let current_members = log
        .member_accounts_at(head_epoch)
        .unwrap_or_default()
        .iter()
        .cloned()
        .collect::<HashSet<_>>();
    requests.sort_by(|left, right| left.request_id.cmp(&right.request_id));
    let owner = group.owner_public_key.as_str();
    let mut targets = Vec::new();
    let mut applied_requests = Vec::new();
    let mut seen_targets = HashSet::new();
    for request in requests {
        request.verify().context("invalid MLS removal request")?;
        if request.group_id != group.group_id
            || !current_members.contains(&request.target_public_key)
            || !seen_targets.insert(request.target_public_key.clone())
        {
            continue;
        }
        let authorized = match request.reason {
            MlsRemovalReason::SelfLeft => request.requester_public_key == request.target_public_key,
            MlsRemovalReason::Banned => {
                let requester_is_owner = request.requester_public_key == owner;
                let requester_is_moderator =
                    view.moderators.contains(&request.requester_public_key);
                let target_is_moderator = view.moderators.contains(&request.target_public_key);
                request.target_public_key != owner
                    && (requester_is_owner || (requester_is_moderator && !target_is_moderator))
            }
        };
        if authorized {
            targets.push(request.target_public_key.clone());
            applied_requests.push(request);
        }
    }
    if targets.is_empty() {
        return Ok(None);
    }
    let mut candidate = mls.clone();
    let bundle = candidate
        .remove_members(&group.group_id, &targets)
        .context("could not remove pending MLS members")?;
    let record = candidate
        .create_epoch_record(identity, previous_record_id, bundle)
        .context("could not sign the member-removal MLS epoch")?;
    let target_set = targets.into_iter().collect::<HashSet<_>>();
    let mut expected = current_members
        .difference(&target_set)
        .cloned()
        .collect::<Vec<_>>();
    expected.sort();
    if record.member_accounts != expected {
        bail!("member-removal MLS epoch does not match the signed requests")
    }
    Ok(Some((candidate, record, applied_requests)))
}

fn join_request_membership_profile(
    request: &MlsJoinRequest,
    group: &GroupMembership,
) -> Option<Profile> {
    let proof = request.membership_proof.as_ref()?;
    let GroupEventPayload::MemberJoined {
        username,
        bio,
        avatar,
        album,
        accepts_direct_messages,
        direct_message_policy,
    } = proof.decrypt(group).ok()?
    else {
        return None;
    };
    let profile = Profile {
        username,
        bio,
        avatar,
        album,
        accepts_direct_messages,
        direct_message_policy,
    };
    valid_direct_profile(&profile).then_some(profile)
}

fn valid_direct_profile(profile: &Profile) -> bool {
    validate_username(&profile.username).is_ok()
        && profile.bio.chars().count() <= 160
        && profile
            .avatar
            .as_ref()
            .is_none_or(|avatar| avatar.byte_length > 0 && avatar.byte_length <= 256 * 1024)
        && profile
            .album
            .as_ref()
            .is_none_or(|album| validate_profile_album_reference(album).is_ok())
}

fn valid_direct_content(text: &str, attachment: Option<&MediaAttachment>) -> bool {
    (!text.trim().is_empty() || attachment.is_some())
        && text.chars().count() <= 10_000
        && attachment.is_none_or(|attachment| validate_media_reference(attachment).is_ok())
}

fn direct_summary(contact: &DirectContact, is_active: bool, has_unread: bool) -> DirectSummary {
    DirectSummary {
        public_key: contact.public_key.clone(),
        username: contact.username.clone(),
        bio: contact.bio.clone(),
        avatar: contact.avatar.clone(),
        album: contact.album.clone(),
        accepts_direct_messages: contact.accepts_direct_messages,
        direct_message_policy: contact.effective_direct_message_policy(),
        is_active,
        has_unread,
    }
}

fn apply_current_direct_profiles(
    messages: &mut [DirectMessageSummary],
    self_public_key: &str,
    self_profile: &Profile,
    contact: &DirectContact,
) {
    for message in messages {
        if message.author_public_key == self_public_key {
            message.username = self_profile.username.clone();
            message.bio = self_profile.bio.clone();
            message.avatar = self_profile.avatar.clone();
            message.album = self_profile.album.clone();
            message.direct_message_policy = self_profile.effective_direct_message_policy();
            message.accepts_direct_messages =
                message.direct_message_policy != DirectMessagePolicy::Nobody;
        } else if message.author_public_key == contact.public_key {
            message.username = contact.username.clone();
            message.bio = contact.bio.clone();
            message.avatar = contact.avatar.clone();
            message.album = contact.album.clone();
            message.accepts_direct_messages = contact.accepts_direct_messages;
            message.direct_message_policy = contact.effective_direct_message_policy();
        }
    }
}

fn apply_current_group_profiles(
    conversation: &mut Conversation,
    state: &ClientState,
    self_public_key: &str,
) {
    let mut profiles = HashMap::<String, (u64, Profile)>::new();
    profiles.insert(
        self_public_key.to_owned(),
        (u64::MAX, state.profile.clone()),
    );
    for person in state
        .known_people
        .iter()
        .chain(state.direct_contacts.iter())
    {
        let profile = Profile {
            username: person.username.clone(),
            bio: person.bio.clone(),
            avatar: person.avatar.clone(),
            album: person.album.clone(),
            accepts_direct_messages: person.accepts_direct_messages,
            direct_message_policy: person.effective_direct_message_policy(),
        };
        match profiles.get_mut(&person.public_key) {
            Some((sequence, current)) if person.profile_sequence >= *sequence => {
                *sequence = person.profile_sequence;
                *current = profile;
            }
            None => {
                profiles.insert(
                    person.public_key.clone(),
                    (person.profile_sequence, profile),
                );
            }
            _ => {}
        }
    }

    for member in &mut conversation.members {
        let Some((_, profile)) = profiles.get(&member.public_key) else {
            continue;
        };
        member.username = profile.username.clone();
        member.bio = profile.bio.clone();
        member.avatar = profile.avatar.clone();
        member.album = profile.album.clone();
        member.accepts_direct_messages = profile.accepts_direct_messages;
    }
    for message in &mut conversation.messages {
        let Some((_, profile)) = profiles.get(&message.author_public_key) else {
            continue;
        };
        message.username = profile.username.clone();
        message.bio = profile.bio.clone();
        message.avatar = profile.avatar.clone();
        message.album = profile.album.clone();
        message.accepts_direct_messages = profile.accepts_direct_messages;
    }
    for report in &mut conversation.reports {
        if let Some((_, profile)) = profiles.get(&report.reporter_public_key) {
            report.reporter_username = profile.username.clone();
            report.reporter_avatar = profile.avatar.clone();
        }
        if let Some((_, profile)) = profiles.get(&report.message.author_public_key) {
            report.message.username = profile.username.clone();
            report.message.bio = profile.bio.clone();
            report.message.avatar = profile.avatar.clone();
            report.message.album = profile.album.clone();
            report.message.accepts_direct_messages = profile.accepts_direct_messages;
        }
    }
    for member in &mut conversation.banned_members {
        let Some((_, profile)) = profiles.get(&member.public_key) else {
            continue;
        };
        member.username = profile.username.clone();
        member.bio = profile.bio.clone();
        member.avatar = profile.avatar.clone();
    }
}

fn attachment_storage_references(attachment: &MediaAttachment) -> Vec<(StorageManifest, String)> {
    attachment
        .chunks
        .iter()
        .filter_map(|chunk| {
            chunk
                .storage
                .as_ref()
                .map(|storage| (storage.clone(), chunk.key_base64.clone()))
        })
        .collect()
}

fn image_storage_reference(image: &ProfileImage) -> Option<(StorageManifest, String)> {
    image
        .storage
        .as_ref()
        .map(|storage| (storage.clone(), image.key_base64.clone()))
}

fn profile_album_storage_reference(album: &ProfileAlbum) -> Option<(StorageManifest, String)> {
    album
        .storage
        .as_ref()
        .map(|storage| (storage.clone(), album.key_base64.clone()))
}

fn validate_profile_album_reference(album: &ProfileAlbum) -> anyhow::Result<()> {
    if album.blob_id.len() != 64
        || !album.blob_id.bytes().all(|byte| byte.is_ascii_hexdigit())
        || album.byte_length == 0
        || album.byte_length > 2 * 1024 * 1024
        || album.item_count == 0
        || album.item_count > 48
        || album.storage.is_none()
    {
        bail!("profile album reference is invalid")
    }
    Ok(())
}

fn validate_profile_album_items(items: &[ProfileAlbumItem]) -> anyhow::Result<()> {
    if items.len() > 48 {
        bail!("albums can contain at most 48 items")
    }
    let mut ids = HashSet::new();
    for item in items {
        if item.id.len() != 64
            || !item.id.bytes().all(|byte| byte.is_ascii_hexdigit())
            || !ids.insert(item.id.as_str())
            || item
                .attachment
                .chunks
                .first()
                .map(|chunk| chunk.blob_id.as_str())
                != Some(item.id.as_str())
            || item.created_at_millis == 0
            || !(item.attachment.mime_type.starts_with("image/")
                || item.attachment.mime_type.starts_with("video/"))
            || item.attachment.byte_length > 500 * 1024 * 1024
        {
            bail!("profile album item is invalid")
        }
        validate_media_reference(&item.attachment)?;
    }
    Ok(())
}

fn group_storage_references(
    group: &GroupMembership,
    events: &[SignedEvent],
) -> Vec<(StorageManifest, String)> {
    let mut references = Vec::new();
    references.extend(group.avatar.as_ref().and_then(image_storage_reference));
    references.extend(group.background.as_ref().and_then(image_storage_reference));
    references.extend(
        group
            .mobile_background
            .as_ref()
            .and_then(image_storage_reference),
    );
    for event in events {
        let Ok(payload) = event.decrypt(group) else {
            continue;
        };
        match payload {
            GroupEventPayload::GroupProfileUpdated { profile } => {
                references.extend(profile.avatar.as_ref().and_then(image_storage_reference));
                references.extend(
                    profile
                        .background
                        .as_ref()
                        .and_then(image_storage_reference),
                );
                references.extend(
                    profile
                        .mobile_background
                        .as_ref()
                        .and_then(image_storage_reference),
                );
            }
            GroupEventPayload::Message {
                attachment: Some(attachment),
                ..
            } => references.extend(attachment_storage_references(&attachment)),
            _ => {}
        }
    }
    references
}

fn direct_storage_references(
    mailbox: &GroupMembership,
    events: &[SignedEvent],
) -> Vec<(StorageManifest, String)> {
    events
        .iter()
        .filter_map(|event| event.decrypt(mailbox).ok())
        .filter_map(|payload| match payload {
            GroupEventPayload::DirectMessage {
                attachment: Some(attachment),
                ..
            } => Some(attachment),
            _ => None,
        })
        .flat_map(|attachment| attachment_storage_references(&attachment))
        .collect()
}

fn relay_list(relays: Vec<String>) -> anyhow::Result<Vec<RelayDescriptor>> {
    let relays = if relays.is_empty() {
        vec![
            "http://127.0.0.1:4301".into(),
            "http://127.0.0.1:4302".into(),
            "http://127.0.0.1:4303".into(),
        ]
    } else {
        relays
    };
    let relays = relays
        .iter()
        .map(|relay| RelayDescriptor::parse(relay))
        .collect::<anyhow::Result<Vec<_>>>()?;
    if relays.len() > 1
        && relays
            .iter()
            .any(|relay| !is_local_relay(&relay.base_url) && relay.ohttp_config.is_none())
    {
        bail!("multi-relay configurations require pinned privacy keys")
    }
    Ok(relays)
}

fn relay_address(relay: &RelayDescriptor) -> String {
    relay.ohttp_config.as_deref().map_or_else(
        || relay.base_url.clone(),
        |config| RelayDescriptor::shareable(&relay.base_url, config),
    )
}

fn storage_relay_score(placement_key: &str, base_url: &str) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new_derive_key("xyz.gnosyslabs.noise.storage-rendezvous.v1");
    hasher.update(placement_key.as_bytes());
    hasher.update(base_url.as_bytes());
    *hasher.finalize().as_bytes()
}

fn is_local_relay(base_url: &str) -> bool {
    base_url.starts_with("http://127.0.0.1")
        || base_url.starts_with("http://localhost")
        || base_url.starts_with("http://[::1]")
}

fn validate_username(username: &str) -> anyhow::Result<()> {
    if username.trim().is_empty() || username.chars().count() > 32 {
        bail!("display names must contain between 1 and 32 characters")
    }
    if username.chars().any(char::is_control) {
        bail!("display names cannot contain control characters")
    }
    Ok(())
}

fn validate_forwarded_from(source: Option<&ForwardedFrom>) -> anyhow::Result<()> {
    let Some(source) = source else {
        return Ok(());
    };
    STANDARD_NO_PAD
        .decode(&source.public_key)
        .ok()
        .filter(|key| key.len() == 32)
        .context("forwarded account public key is invalid")?;
    validate_username(&source.username)
}

fn validate_display_name(username: &str) -> anyhow::Result<()> {
    if username.trim().is_empty() || username.chars().count() > 16 {
        bail!("display names must contain between 1 and 16 characters")
    }
    if username.chars().any(char::is_control) {
        bail!("display names cannot contain control characters")
    }
    Ok(())
}

fn validate_topic_name(name: &str) -> anyhow::Result<()> {
    let length = name.trim().chars().count();
    if !(1..=80).contains(&length) {
        bail!("topic names must contain between 1 and 80 characters")
    }
    if name.chars().any(char::is_control) {
        bail!("topic names cannot contain control characters")
    }
    Ok(())
}

fn validate_topic_icon(icon: &str) -> anyhow::Result<()> {
    if !valid_reaction_emoji(icon) {
        bail!("topic icons must be a single emoji")
    }
    Ok(())
}

fn default_topic_icon() -> String {
    "💬".to_owned()
}

fn validate_password(password: &str) -> anyhow::Result<()> {
    let length = password.chars().count();
    if !(16..=256).contains(&length) {
        bail!("passwords must contain between 16 and 256 characters")
    }
    let classes = [
        password.chars().any(|character| character.is_lowercase()),
        password.chars().any(|character| character.is_uppercase()),
        password.chars().any(|character| character.is_numeric()),
        password
            .chars()
            .any(|character| !character.is_alphanumeric()),
    ]
    .into_iter()
    .filter(|present| *present)
    .count();
    if length < 24 && classes < 3 {
        bail!("use at least 24 characters, or mix three character types")
    }
    Ok(())
}

fn validate_adult_birth_date(birth_date: &str) -> anyhow::Result<()> {
    let today = OffsetDateTime::from_unix_timestamp((current_millis() / 1_000) as i64)
        .context("system clock is outside the supported range")?
        .date();
    validate_adult_birth_date_on(birth_date, today)
}

fn validate_adult_birth_date_on(birth_date: &str, today: Date) -> anyhow::Result<()> {
    let parts = birth_date.split('-').collect::<Vec<_>>();
    if parts.len() != 3 || parts[0].len() != 4 || parts[1].len() != 2 || parts[2].len() != 2 {
        bail!("birth date must use YYYY-MM-DD")
    }
    let year = parts[0]
        .parse::<i32>()
        .context("birth date must use YYYY-MM-DD")?;
    let month = parts[1]
        .parse::<u8>()
        .ok()
        .and_then(|month| Month::try_from(month).ok())
        .context("birth date is invalid")?;
    let day = parts[2].parse::<u8>().context("birth date is invalid")?;
    let birth_date = Date::from_calendar_date(year, month, day).context("birth date is invalid")?;
    let mut age = today.year() - birth_date.year();
    if (today.month() as u8, today.day()) < (birth_date.month() as u8, birth_date.day()) {
        age -= 1;
    }
    if age < 18 {
        bail!("Noise is for adults 18 and older")
    }
    Ok(())
}

fn normalize_group_rules(rules: String) -> anyhow::Result<String> {
    let rules = rules
        .lines()
        .map(str::trim)
        .filter(|rule| !rule.is_empty())
        .collect::<Vec<_>>();
    if rules.len() > 20 {
        bail!("groups can have at most 20 rules")
    }
    if rules.iter().any(|rule| rule.chars().count() > 200) {
        bail!("each group rule can contain at most 200 characters")
    }
    let rules = rules.join("\n");
    if rules.chars().count() > 4000 {
        bail!("group rules are too long")
    }
    Ok(rules)
}

fn normalize_group_accent_color(color: String) -> anyhow::Result<String> {
    let color = color.trim();
    if color.len() != 7
        || !color.starts_with('#')
        || !color[1..].bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        bail!("group accent colors must use six hexadecimal digits")
    }
    Ok(color.to_ascii_uppercase())
}

fn supported_media_type(mime_type: &str) -> bool {
    mime_type.len() <= 100
        && (mime_type.starts_with("image/")
            || mime_type.starts_with("video/")
            || mime_type.starts_with("audio/"))
}

fn media_extension(mime_type: &str) -> &'static str {
    match mime_type {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "video/mp4" => "mp4",
        "video/webm" => "webm",
        "video/quicktime" => "mov",
        "audio/mpeg" => "mp3",
        "audio/mp4" => "m4a",
        "audio/ogg" => "ogg",
        "audio/wav" | "audio/x-wav" => "wav",
        "audio/webm" => "webm",
        _ if mime_type.starts_with("image/") => "image",
        _ if mime_type.starts_with("video/") => "video",
        _ => "audio",
    }
}

fn select_media_chunks(
    attachment: &MediaAttachment,
    offset: u64,
    requested_end: u64,
) -> Vec<(usize, u64, MediaChunk)> {
    let mut chunk_offset = 0_u64;
    let mut selected = Vec::new();
    for (index, chunk) in attachment.chunks.iter().cloned().enumerate() {
        let chunk_end = chunk_offset + u64::from(chunk.byte_length);
        if chunk_end > offset && chunk_offset < requested_end {
            selected.push((index, chunk_offset, chunk));
        }
        chunk_offset = chunk_end;
    }
    selected
}

fn validate_media_reference(media: &MediaAttachment) -> anyhow::Result<()> {
    if media.file_name.trim().is_empty() || media.file_name.chars().count() > 255 {
        bail!("media has an invalid file name")
    }
    if !supported_media_type(&media.mime_type) {
        bail!("media has an unsupported type")
    }
    if media.byte_length == 0 || media.byte_length > 500 * 1024 * 1024 {
        bail!("media has an invalid size")
    }
    if media.chunks.is_empty() || media.chunks.len() > 500 {
        bail!("media has an invalid chunk manifest")
    }
    let mut byte_length = 0u64;
    for chunk in &media.chunks {
        if chunk.blob_id.len() != 64
            || !chunk.blob_id.bytes().all(|byte| byte.is_ascii_hexdigit())
            || chunk.key_base64.is_empty()
            || chunk.key_base64.len() > 64
            || chunk.byte_length == 0
            || chunk.byte_length > 1024 * 1024
        {
            bail!("media has an invalid chunk manifest")
        }
        byte_length += u64::from(chunk.byte_length);
    }
    if byte_length != media.byte_length {
        bail!("media chunk sizes do not match the manifest")
    }
    if !media_preview_is_valid(media) {
        bail!("media has an invalid preview")
    }
    Ok(())
}

fn resolve_media_scope(
    state: &ClientState,
    requested_scope_id: Option<String>,
) -> anyhow::Result<String> {
    let identity = state.identity()?;
    let Some(scope_id) = requested_scope_id else {
        return Ok(state.active_group()?.group_id.clone());
    };
    let allowed = state.groups.iter().any(|group| group.group_id == scope_id)
        || state.direct_contacts.iter().any(|contact| {
            identity
                .direct_scope_id(&contact.public_key)
                .ok()
                .as_deref()
                == Some(scope_id.as_str())
        })
        || profile_media_scope_id(&identity.public_key_base64())
            .ok()
            .as_deref()
            == Some(scope_id.as_str())
        || state
            .direct_contacts
            .iter()
            .chain(state.known_people.iter())
            .filter(|person| !state.is_hidden(&person.public_key))
            .any(|person| {
                profile_media_scope_id(&person.public_key).ok().as_deref()
                    == Some(scope_id.as_str())
            });
    if !allowed {
        bail!("media does not belong to a known conversation")
    }
    Ok(scope_id)
}

fn validate_reply_reference(reply_to_message_id: Option<&str>) -> anyhow::Result<()> {
    if reply_to_message_id.is_some_and(|message_id| {
        message_id.len() != 64 || !message_id.bytes().all(|byte| byte.is_ascii_hexdigit())
    }) {
        bail!("reply target is invalid")
    }
    Ok(())
}

fn purge_group_cache(cache_path: &Path, group_id: &str) -> anyhow::Result<()> {
    purge_scope_cache(cache_path, group_id)
}

#[cfg(not(target_arch = "wasm32"))]
fn purge_profile_image_cache(cache_path: &Path) -> anyhow::Result<()> {
    let directory = cache_path.join("profile-blobs");
    if directory.exists() {
        fs::remove_dir_all(&directory)
            .with_context(|| format!("could not erase {}", directory.display()))?;
    }
    Ok(())
}

#[cfg(target_arch = "wasm32")]
fn purge_profile_image_cache(_cache_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn purge_scope_cache(cache_path: &Path, scope_id: &str) -> anyhow::Result<()> {
    if scope_id.len() != 64 || !scope_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("conversation has an invalid local cache identifier")
    }
    for directory in [
        cache_path.join("media").join(scope_id),
        cache_path.join("media-chunks").join(scope_id),
    ] {
        if directory.exists() {
            fs::remove_dir_all(&directory)
                .with_context(|| format!("could not erase {}", directory.display()))?;
        }
    }
    Ok(())
}

#[cfg(target_arch = "wasm32")]
fn purge_scope_cache(_cache_path: &Path, scope_id: &str) -> anyhow::Result<()> {
    if scope_id.len() != 64 || !scope_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("conversation has an invalid local cache identifier")
    }
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn current_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock is before Unix epoch")
        .as_millis() as u64
}

#[cfg(target_arch = "wasm32")]
fn current_millis() -> u64 {
    js_sys::Date::now().max(0.0) as u64
}

#[cfg(not(target_arch = "wasm32"))]
fn current_nanos() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_nanos()).ok())
        .unwrap_or_default()
}

#[cfg(target_arch = "wasm32")]
fn current_nanos() -> u64 {
    current_millis().saturating_mul(1_000_000)
}

#[cfg(not(target_arch = "wasm32"))]
async fn replica_settle_delay() {
    tokio::time::sleep(std::time::Duration::from_millis(
        EVENT_REPLICA_SETTLE_MILLIS,
    ))
    .await;
}

#[cfg(target_arch = "wasm32")]
async fn replica_settle_delay() {
    gloo_timers::future::TimeoutFuture::new(EVENT_REPLICA_SETTLE_MILLIS as u32).await;
}

#[cfg(all(not(target_arch = "wasm32"), not(test)))]
const STATE_WRITE_DEBOUNCE: Duration = Duration::from_millis(250);

#[cfg(all(not(target_arch = "wasm32"), not(test)))]
struct NativeStateEntry {
    state: ClientState,
    generation: u64,
    persisted_generation: u64,
    write_after: Option<Instant>,
}

#[cfg(all(not(target_arch = "wasm32"), not(test)))]
#[derive(Default)]
struct NativeStateCache {
    entries: HashMap<PathBuf, NativeStateEntry>,
    next_generation: u64,
}

#[cfg(all(not(target_arch = "wasm32"), not(test)))]
type SharedNativeStateCache = Arc<(Mutex<NativeStateCache>, Condvar)>;

#[cfg(all(not(target_arch = "wasm32"), not(test)))]
fn native_state_cache() -> &'static SharedNativeStateCache {
    static CACHE: OnceLock<SharedNativeStateCache> = OnceLock::new();
    CACHE.get_or_init(|| {
        let cache = Arc::new((Mutex::new(NativeStateCache::default()), Condvar::new()));
        let writer_cache = Arc::clone(&cache);
        thread::Builder::new()
            .name("noise-state-writer".to_owned())
            .spawn(move || native_state_writer(writer_cache))
            .expect("could not start local state writer");
        cache
    })
}

#[cfg(all(not(target_arch = "wasm32"), not(test)))]
fn native_state_writer(cache: SharedNativeStateCache) {
    loop {
        let (path, state, generation) = {
            let (lock, condition) = &*cache;
            let mut guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            loop {
                let now = Instant::now();
                if let Some((path, entry)) = guard.entries.iter().find(|(_, entry)| {
                    entry.generation != entry.persisted_generation
                        && entry.write_after.is_some_and(|deadline| deadline <= now)
                }) {
                    break (path.clone(), entry.state.clone(), entry.generation);
                }
                let next_deadline = guard
                    .entries
                    .values()
                    .filter(|entry| entry.generation != entry.persisted_generation)
                    .filter_map(|entry| entry.write_after)
                    .min();
                guard = if let Some(deadline) = next_deadline {
                    let wait = deadline.saturating_duration_since(now);
                    condition
                        .wait_timeout(guard, wait)
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .0
                } else {
                    condition
                        .wait(guard)
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                };
            }
        };

        let result = serde_json::to_vec(&state).and_then(|bytes| {
            persist_native_state_generation(&cache, &path, generation, &bytes)
                .map_err(serde_json::Error::io)
        });
        let (lock, condition) = &*cache;
        let mut guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(entry) = guard.entries.get_mut(&path)
            && entry.generation == generation
        {
            match result {
                Ok(()) => {
                    entry.persisted_generation = generation;
                    entry.write_after = None;
                }
                Err(error) => {
                    eprintln!("could not persist {}: {error}", path.display());
                    entry.write_after = Some(Instant::now() + Duration::from_secs(1));
                    condition.notify_one();
                }
            }
        }
    }
}

#[cfg(all(not(target_arch = "wasm32"), not(test)))]
fn persist_native_state_generation(
    cache: &SharedNativeStateCache,
    path: &Path,
    generation: u64,
    bytes: &[u8],
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension(format!("tmp-{generation}"));
    fs::write(&temporary, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
    }
    // Keep the cache lock through the rename. Logout removes the cache entry
    // before deleting the file, so it cannot race a pending writer and have
    // that writer recreate an erased identity afterward.
    let guard = cache
        .0
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if guard
        .entries
        .get(path)
        .is_some_and(|entry| entry.generation == generation)
    {
        fs::rename(&temporary, path)?;
    } else {
        let _ = fs::remove_file(&temporary);
    }
    Ok(())
}

#[cfg(all(not(target_arch = "wasm32"), not(test)))]
fn state_exists(path: &Path) -> bool {
    native_state_cache()
        .0
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .entries
        .contains_key(path)
        || path.exists()
}

#[cfg(all(not(target_arch = "wasm32"), test))]
fn state_exists(path: &Path) -> bool {
    path.exists()
}

#[cfg(target_arch = "wasm32")]
fn state_exists(path: &Path) -> bool {
    WEB_STATE.with(|states| {
        states
            .borrow()
            .contains_key(&path.to_string_lossy().into_owned())
    })
}

#[cfg(all(not(target_arch = "wasm32"), not(test)))]
fn load_state(path: &Path) -> anyhow::Result<ClientState> {
    if let Some(state) = native_state_cache()
        .0
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .entries
        .get(path)
        .map(|entry| entry.state.clone())
    {
        return Ok(state);
    }
    let bytes = fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
    let mut state: ClientState =
        serde_json::from_slice(&bytes).context("local state is invalid")?;
    state.ensure_group_membership_records();
    state.hydrate_known_profile_versions();
    let cache = native_state_cache();
    let mut guard = cache
        .0
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    Ok(guard
        .entries
        .entry(path.to_path_buf())
        .or_insert_with(|| NativeStateEntry {
            state: state.clone(),
            generation: 0,
            persisted_generation: 0,
            write_after: None,
        })
        .state
        .clone())
}

#[cfg(all(not(target_arch = "wasm32"), test))]
fn load_state(path: &Path) -> anyhow::Result<ClientState> {
    let bytes = fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
    let mut state: ClientState =
        serde_json::from_slice(&bytes).context("local state is invalid")?;
    state.ensure_group_membership_records();
    state.hydrate_known_profile_versions();
    Ok(state)
}

#[cfg(target_arch = "wasm32")]
fn load_state(path: &Path) -> anyhow::Result<ClientState> {
    let key = path.to_string_lossy().into_owned();
    let bytes = WEB_STATE
        .with(|states| states.borrow().get(&key).cloned())
        .with_context(|| format!("could not read {}", path.display()))?;
    let mut state: ClientState =
        serde_json::from_slice(&bytes).context("local state is invalid")?;
    state.ensure_group_membership_records();
    state.hydrate_known_profile_versions();
    Ok(state)
}

#[cfg(all(not(target_arch = "wasm32"), not(test)))]
fn save_state(path: &Path, state: &ClientState) -> anyhow::Result<()> {
    let cache = native_state_cache();
    let (lock, condition) = &**cache;
    let mut guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.next_generation = guard.next_generation.saturating_add(1);
    let generation = guard.next_generation;
    // Read the previous persisted generation before `insert` takes its own
    // mutable borrow of `entries`.
    let persisted_generation = guard
        .entries
        .get(path)
        .map_or(0, |entry| entry.persisted_generation);
    guard.entries.insert(
        path.to_path_buf(),
        NativeStateEntry {
            state: state.clone(),
            generation,
            persisted_generation,
            write_after: Some(Instant::now() + STATE_WRITE_DEBOUNCE),
        },
    );
    condition.notify_one();
    Ok(())
}

#[cfg(all(not(target_arch = "wasm32"), test))]
fn save_state(path: &Path, state: &ClientState) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    let temporary = temporary_path(path);
    // Compact rather than pretty: this file reaches tens of megabytes and is
    // rewritten on every relay sync, so the indentation is pure overhead.
    fs::write(&temporary, serde_json::to_vec(state)?)
        .with_context(|| format!("could not write {}", temporary.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("could not secure {}", temporary.display()))?;
    }
    fs::rename(&temporary, path)
        .with_context(|| format!("could not replace {}", path.display()))?;
    Ok(())
}

#[cfg(target_arch = "wasm32")]
fn save_state(path: &Path, state: &ClientState) -> anyhow::Result<()> {
    let key = path.to_string_lossy().into_owned();
    let bytes = serde_json::to_vec(state)?;
    WEB_STATE.with(|states| states.borrow_mut().insert(key, bytes));
    Ok(())
}

#[cfg(all(not(target_arch = "wasm32"), not(test)))]
fn remove_state(path: &Path) -> anyhow::Result<()> {
    let cache = native_state_cache();
    cache
        .0
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .entries
        .remove(path);
    if path.exists() {
        fs::remove_file(path)
            .with_context(|| format!("could not erase local identity {}", path.display()))?;
    }
    Ok(())
}

#[cfg(all(not(target_arch = "wasm32"), test))]
fn remove_state(path: &Path) -> anyhow::Result<()> {
    if path.exists() {
        fs::remove_file(path)
            .with_context(|| format!("could not erase local identity {}", path.display()))?;
    }
    Ok(())
}

#[cfg(target_arch = "wasm32")]
fn remove_state(path: &Path) -> anyhow::Result<()> {
    WEB_STATE.with(|states| {
        states
            .borrow_mut()
            .remove(&path.to_string_lossy().into_owned())
    });
    Ok(())
}

#[cfg(all(not(target_arch = "wasm32"), test))]
fn temporary_path(path: &Path) -> PathBuf {
    path.with_extension("tmp")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adult_birth_date_gate_uses_the_birthday_boundary() {
        let today = Date::from_calendar_date(2026, Month::July, 26).unwrap();
        assert!(validate_adult_birth_date_on("2008-07-26", today).is_ok());
        assert!(validate_adult_birth_date_on("2008-07-27", today).is_err());
        assert!(validate_adult_birth_date_on("not-a-date", today).is_err());
    }

    #[test]
    fn legacy_accounts_migrate_as_adults_with_explicit_groups_hidden() {
        let identity = Identity::generate();
        let credentials = AccountCredentials {
            noise_id: "123456789012".to_owned(),
            locator: "ab".repeat(32),
            vault_key_base64: STANDARD_NO_PAD.encode([7_u8; 32]),
        };
        let state = account_state(&identity, &credentials, test_profile(None), 1, 1);
        let mut encoded = serde_json::to_value(&state).unwrap();
        encoded
            .as_object_mut()
            .unwrap()
            .remove("adult_access")
            .unwrap();

        let migrated: ClientState = serde_json::from_value(encoded).unwrap();
        assert!(migrated.adult_access.age_attested);
        assert!(!migrated.adult_access.explicit_content_enabled);

        let mut renamed_preference = serde_json::to_value(state).unwrap();
        let access = renamed_preference
            .get_mut("adult_access")
            .unwrap()
            .as_object_mut()
            .unwrap();
        access.remove("explicit_content_enabled").unwrap();
        access.insert("adult_content_enabled".to_owned(), serde_json::json!(true));
        let migrated_preference: ClientState = serde_json::from_value(renamed_preference).unwrap();
        assert!(migrated_preference.adult_access.explicit_content_enabled);
    }

    #[test]
    fn message_search_only_matches_literal_message_text() {
        assert_eq!(search_message_score("image", ""), None);
        assert_eq!(search_message_score("photo", ""), None);
        assert_eq!(search_message_score("video", ""), None);
        assert!(search_message_score("ma", "marketing").is_some());
        assert!(search_message_score("photo", "Here is a photo from today").is_some());
    }

    #[test]
    fn search_profile_resolution_uses_the_newest_avatar() {
        let identity = Identity::generate();
        let credentials = AccountCredentials {
            noise_id: "123456789012".to_owned(),
            locator: "ab".repeat(32),
            vault_key_base64: STANDARD_NO_PAD.encode([7_u8; 32]),
        };
        let mut state = account_state(&identity, &credentials, test_profile(None), 1, 1);
        let public_key = "search-result-person".to_owned();
        state.direct_contacts.push(DirectContact {
            public_key: public_key.clone(),
            username: "old name".to_owned(),
            bio: String::new(),
            avatar: None,
            album: None,
            accepts_direct_messages: true,
            direct_message_policy: DirectMessagePolicy::Everyone,
            profile_sequence: 10,
        });
        let avatar = ProfileImage {
            blob_id: "11".repeat(32),
            key_base64: STANDARD.encode([8_u8; 32]),
            mime_type: "image/png".to_owned(),
            byte_length: 128,
            storage: None,
        };
        state.known_people.push(DirectContact {
            public_key: public_key.clone(),
            username: "current name".to_owned(),
            bio: String::new(),
            avatar: Some(avatar.clone()),
            album: None,
            accepts_direct_messages: true,
            direct_message_policy: DirectMessagePolicy::Everyone,
            profile_sequence: 20,
        });

        let profiles =
            current_profiles_by_public_key(&state, identity.public_key_base64().as_str());
        let (_, resolved) = profiles.get(&public_key).unwrap();
        assert_eq!(resolved.username, "current name");
        assert_eq!(resolved.avatar, Some(avatar));
    }

    fn account_state(
        identity: &Identity,
        credentials: &AccountCredentials,
        profile: Profile,
        profile_sequence: u64,
        revision: u64,
    ) -> ClientState {
        ClientState {
            version: 3,
            profile,
            adult_access: AdultAccessSettings::default(),
            profile_sequence,
            contact_signal_published_sequence: 0,
            identity_secret_base64: identity.secret_base64(),
            local_device_id: String::new(),
            devices: HashMap::new(),
            mls_device: None,
            mls_group_states: HashMap::new(),
            mls_recovery_dirty: false,
            mls_join_requests: HashMap::new(),
            mls_local_geneses: HashMap::new(),
            mls_control_logs: HashMap::new(),
            group_event_caches: HashMap::new(),
            direct_event_cache: DirectEventCache::default(),
            groups: Vec::new(),
            group_memberships: HashMap::new(),
            active_group_id: None,
            direct_contacts: Vec::new(),
            known_people: Vec::new(),
            known_profile_versions_hydrated: true,
            block_states: HashMap::new(),
            blocked_by_states: HashMap::new(),
            active_direct_public_key: None,
            direct_deleted_before: HashMap::new(),
            direct_policy_rejected_through: HashMap::new(),
            direct_closed_periods: Vec::new(),
            direct_latest_incoming: HashMap::new(),
            direct_latest_activity: HashMap::new(),
            direct_read_through: HashMap::new(),
            group_latest_incoming: HashMap::new(),
            group_latest_activity: HashMap::new(),
            group_read_through: HashMap::new(),
            group_unread_messages: HashMap::new(),
            topic_read_through: HashMap::new(),
            topic_unread_messages: HashMap::new(),
            topic_latest_incoming: HashMap::new(),
            topic_activity_initialized: HashSet::new(),
            group_activity_initialized: HashSet::new(),
            group_conversation_cache: HashMap::new(),
            group_frequencies: HashMap::new(),
            next_author_sequence: profile_sequence.saturating_add(1),
            account: Some(AccountSession {
                noise_id: credentials.noise_id.clone(),
                locator: credentials.locator.clone(),
                vault_key_base64: credentials.vault_key_base64.clone(),
                revision,
                read_state_revision: 0,
            }),
        }
    }

    fn test_profile(album: Option<ProfileAlbum>) -> Profile {
        Profile {
            username: "alice".to_owned(),
            bio: "testing cross-device sync".to_owned(),
            avatar: None,
            album,
            accepts_direct_messages: true,
            direct_message_policy: DirectMessagePolicy::Everyone,
        }
    }

    fn marker(created_at_millis: u64, event_id: &str) -> DirectMessageMarker {
        DirectMessageMarker {
            created_at_millis,
            event_id: event_id.to_owned(),
        }
    }

    #[test]
    fn revoked_device_record_cannot_be_resurrected_by_stale_activity() {
        let device_id = "a".repeat(64);
        let active = DeviceRecord {
            device_id: device_id.clone(),
            name: "Mac desktop".to_owned(),
            platform: "macOS · Noise desktop".to_owned(),
            created_at_millis: 100,
            last_seen_at_millis: 200,
            revoked_at_millis: None,
            sequence: 10,
        };
        let mut revoked = active.clone();
        revoked.revoked_at_millis = Some(300);
        revoked.sequence = 11;

        let mut records = HashMap::from([(device_id.clone(), revoked.clone())]);
        merge_device_records(&mut records, HashMap::from([(device_id.clone(), active)]));

        assert_eq!(records.get(&device_id), Some(&revoked));
    }

    #[test]
    fn read_cursor_merge_only_moves_forward() {
        let mut current = HashMap::from([("alice".to_owned(), marker(200, "event-b"))]);
        let incoming = HashMap::from([
            ("alice".to_owned(), marker(100, "event-a")),
            ("bob".to_owned(), marker(150, "event-c")),
        ]);

        assert!(merge_read_markers(&mut current, &incoming));
        assert_eq!(current.get("alice"), Some(&marker(200, "event-b")));
        assert_eq!(current.get("bob"), Some(&marker(150, "event-c")));

        let newer = HashMap::from([("alice".to_owned(), marker(300, "event-d"))]);
        assert!(merge_read_markers(&mut current, &newer));
        assert_eq!(current.get("alice"), Some(&marker(300, "event-d")));
        assert!(!merge_read_markers(&mut current, &newer));
    }

    #[test]
    fn encrypted_account_read_state_merges_monotonically() {
        let identity = Identity::generate();
        let credentials = AccountCredentials {
            noise_id: "123456789012".to_owned(),
            locator: "ab".repeat(32),
            vault_key_base64: STANDARD_NO_PAD.encode([7_u8; 32]),
        };
        let mut state = account_state(&identity, &credentials, test_profile(None), 1, 4);
        state
            .direct_read_through
            .insert("alice".to_owned(), marker(200, "event-b"));
        let read_credentials = state.account_read_state_credentials().unwrap();
        assert_ne!(read_credentials.locator, credentials.locator);
        let contents = AccountReadStateContents {
            version: 1,
            direct_read_through: HashMap::from([
                ("alice".to_owned(), marker(100, "event-a")),
                ("bob".to_owned(), marker(300, "event-c")),
            ]),
            group_read_through: HashMap::new(),
            topic_read_through: HashMap::new(),
        };
        let vault = AccountVault::seal(
            &identity,
            &read_credentials,
            7,
            &serde_json::to_vec(&contents).unwrap(),
        )
        .unwrap();

        assert!(
            NoiseClient::merge_remote_read_state(&mut state, &read_credentials, &vault).unwrap()
        );
        assert_eq!(
            state.direct_read_through.get("alice"),
            Some(&marker(200, "event-b"))
        );
        assert_eq!(
            state.direct_read_through.get("bob"),
            Some(&marker(300, "event-c"))
        );
        assert_eq!(
            state.account.as_ref().unwrap().read_state_revision,
            vault.revision
        );
    }

    #[test]
    fn latest_message_marker_cannot_regress_to_an_older_relay_page() {
        let mut latest = HashMap::from([("group:topic".to_owned(), marker(300, "event-c"))]);

        assert!(!advance_message_marker(
            &mut latest,
            "group:topic",
            marker(100, "event-a"),
        ));
        assert_eq!(latest.get("group:topic"), Some(&marker(300, "event-c")));
        assert!(advance_message_marker(
            &mut latest,
            "group:topic",
            marker(400, "event-d"),
        ));
        assert_eq!(latest.get("group:topic"), Some(&marker(400, "event-d")));
    }

    #[test]
    fn repeated_sign_in_for_the_restored_account_is_idempotent() {
        let noise_id = "482109376144";
        let password = "correct horse battery staple";
        let credentials = derive_account_credentials(noise_id, password).unwrap();
        let identity = Identity::generate();
        let state = account_state(&identity, &credentials, test_profile(None), 1, 1);
        let path = std::env::temp_dir().join(format!(
            "noise-idempotent-sign-in-{}-{}.json",
            std::process::id(),
            current_nanos(),
        ));
        save_state(&path, &state).unwrap();

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let summary = runtime
            .block_on(NoiseClient::default().sign_in(&path, noise_id, password, Vec::new()))
            .unwrap();

        assert_eq!(summary.identity.public_key, identity.public_key_base64());
        remove_state(&path).unwrap();
    }

    #[test]
    fn group_memberships_union_and_newest_lifecycle_record_wins() {
        let first = GroupMembership::create("first");
        let second = GroupMembership::create("second");
        let first_id = first.group_id.clone();
        let second_id = second.group_id.clone();
        let mut current = HashMap::from([(
            first_id.clone(),
            GroupMembershipRecord {
                sequence: 0,
                group: Some(first.clone()),
            },
        )]);

        assert!(merge_group_membership_records(
            &mut current,
            HashMap::from([(
                second_id.clone(),
                GroupMembershipRecord {
                    sequence: 0,
                    group: Some(second),
                },
            )]),
        ));
        assert_eq!(current.len(), 2);

        assert!(merge_group_membership_records(
            &mut current,
            HashMap::from([(
                first_id.clone(),
                GroupMembershipRecord {
                    sequence: 20,
                    group: None,
                },
            )]),
        ));
        assert!(
            current
                .get(&first_id)
                .is_some_and(|record| record.group.is_none())
        );

        assert!(!merge_group_membership_records(
            &mut current,
            HashMap::from([(
                first_id.clone(),
                GroupMembershipRecord {
                    sequence: 0,
                    group: Some(first.clone()),
                },
            )]),
        ));
        assert!(
            current
                .get(&first_id)
                .is_some_and(|record| record.group.is_none())
        );

        assert!(merge_group_membership_records(
            &mut current,
            HashMap::from([(
                first_id.clone(),
                GroupMembershipRecord {
                    sequence: 30,
                    group: Some(first),
                },
            )]),
        ));
        assert!(
            current
                .get(&first_id)
                .is_some_and(|record| record.group.is_some())
        );
    }

    #[test]
    fn first_join_activity_persists_current_group_media_everywhere() {
        let identity = Identity::generate();
        let credentials = AccountCredentials {
            noise_id: "123456789012".to_owned(),
            locator: "ab".repeat(32),
            vault_key_base64: STANDARD_NO_PAD.encode([7_u8; 32]),
        };
        let mut state = account_state(&identity, &credentials, test_profile(None), 1, 1);
        let invited_group =
            GroupMembership::create_owned("stale invitation", identity.public_key_base64());
        let group_id = invited_group.group_id.clone();
        state.add_group(invited_group.clone());
        let avatar = ProfileImage {
            blob_id: "11".repeat(32),
            key_base64: STANDARD.encode([8_u8; 32]),
            mime_type: "image/png".to_owned(),
            byte_length: 128,
            storage: None,
        };
        let background = ProfileImage {
            blob_id: "22".repeat(32),
            key_base64: STANDARD.encode([9_u8; 32]),
            mime_type: "image/jpeg".to_owned(),
            byte_length: 256,
            storage: None,
        };
        let current_profile = GroupProfile {
            name: "current group".to_owned(),
            description: "current description".to_owned(),
            rules: String::new(),
            content_rating: GroupContentRating::General,
            avatar: Some(avatar.clone()),
            background: Some(background.clone()),
            mobile_background: None,
            mobile_background_updated: true,
            accent_color: "#7758ED".to_owned(),
            members_can_send_messages: true,
            members_can_send_media: true,
        };
        let events = vec![
            SignedEvent::member_joined(&identity, &invited_group, &state.profile, 1).unwrap(),
            SignedEvent::group_profile_updated(&identity, &invited_group, &current_profile, 2)
                .unwrap(),
        ];
        let path = std::env::temp_dir().join(format!(
            "noise-first-join-profile-{}-{}.json",
            std::process::id(),
            current_nanos(),
        ));
        save_state(&path, &state).unwrap();

        let result = NoiseClient::default()
            .apply_group_activity(
                &path,
                GroupActivityUpdate {
                    group_id: group_id.clone(),
                    events,
                    full_snapshot: true,
                    older_cursor: None,
                    has_older_messages: false,
                    topic_update: None,
                },
            )
            .unwrap();
        let summary_group = result
            .summary
            .groups
            .iter()
            .find(|group| group.group_id == group_id)
            .unwrap();
        let conversation_group = &result.conversation.as_ref().unwrap().group;
        assert_eq!(summary_group.avatar, Some(avatar.clone()));
        assert_eq!(summary_group.background, Some(background.clone()));
        assert_eq!(conversation_group.avatar, Some(avatar.clone()));
        assert_eq!(conversation_group.background, Some(background.clone()));

        let reloaded = load_state(&path).unwrap();
        let persisted_group = reloaded
            .groups
            .iter()
            .find(|group| group.group_id == group_id)
            .unwrap();
        let membership_group = reloaded
            .group_memberships
            .get(&group_id)
            .and_then(|record| record.group.as_ref())
            .unwrap();
        assert_eq!(persisted_group.avatar, Some(avatar.clone()));
        assert_eq!(persisted_group.background, Some(background.clone()));
        assert_eq!(membership_group.avatar, Some(avatar));
        assert_eq!(membership_group.background, Some(background));
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn two_clients_preserve_newer_profile_album_across_the_next_sync() {
        let identity = Identity::generate();
        let credentials = AccountCredentials {
            noise_id: "123456789012".to_owned(),
            locator: "ab".repeat(32),
            vault_key_base64: base64::engine::general_purpose::STANDARD_NO_PAD.encode([7_u8; 32]),
        };
        let album = ProfileAlbum {
            blob_id: "cd".repeat(32),
            key_base64: base64::engine::general_purpose::STANDARD_NO_PAD.encode([9_u8; 32]),
            byte_length: 1_024,
            item_count: 11,
            storage: None,
        };
        let client_a = account_state(&identity, &credentials, test_profile(Some(album)), 200, 1);
        let remote_from_a = AccountVault::seal(
            &identity,
            &credentials,
            2,
            &serde_json::to_vec(&client_a.vault_contents()).unwrap(),
        )
        .unwrap();
        let mut client_b = account_state(&identity, &credentials, test_profile(None), 100, 1);

        assert!(
            NoiseClient::merge_remote_account_state(&mut client_b, &credentials, &remote_from_a,)
                .unwrap()
        );
        assert_eq!(
            client_b
                .profile
                .album
                .as_ref()
                .map(|album| album.item_count),
            Some(11)
        );

        let republished_by_b = AccountVault::seal(
            &identity,
            &credentials,
            3,
            &serde_json::to_vec(&client_b.vault_contents()).unwrap(),
        )
        .unwrap();
        let republished: AccountVaultContents =
            serde_json::from_slice(&republished_by_b.open(&credentials).unwrap()).unwrap();
        assert_eq!(
            republished
                .profile
                .album
                .as_ref()
                .map(|album| album.item_count),
            Some(11)
        );
        assert_eq!(republished.profile_sequence, 200);
    }

    #[test]
    fn account_vault_recovery_shell_does_not_re_poison_hydrated_group_cache() {
        let identity = Identity::generate();
        let credentials = AccountCredentials {
            noise_id: "123456789012".to_owned(),
            locator: "ab".repeat(32),
            vault_key_base64: STANDARD_NO_PAD.encode([7_u8; 32]),
        };
        let mut local = account_state(&identity, &credentials, test_profile(None), 1, 1);
        let group = GroupMembership::create_owned("hydrated group", identity.public_key_base64());
        let group_id = group.group_id.clone();
        local.add_group(group.clone());
        local.group_event_caches.insert(
            group_id.clone(),
            GroupEventCache {
                events: vec![
                    SignedEvent::member_joined(&identity, &group, &local.profile, 1).unwrap(),
                ],
                latest_cursor: None,
                older_cursor: None,
                message_limit: INITIAL_GROUP_MESSAGE_WINDOW,
                has_older_messages: false,
                needs_recent_hydration: false,
                needs_control_hydration: false,
                topic_streams: HashMap::new(),
                portable_conversation: None,
            },
        );

        let mut remote_contents = local.vault_contents();
        remote_contents.group_event_caches.insert(
            group_id.clone(),
            GroupEventCache {
                events: Vec::new(),
                latest_cursor: None,
                older_cursor: None,
                message_limit: INITIAL_GROUP_MESSAGE_WINDOW,
                has_older_messages: true,
                needs_recent_hydration: true,
                needs_control_hydration: true,
                topic_streams: HashMap::new(),
                portable_conversation: None,
            },
        );
        let remote = AccountVault::seal(
            &identity,
            &credentials,
            2,
            &serde_json::to_vec(&remote_contents).unwrap(),
        )
        .unwrap();

        NoiseClient::merge_remote_account_state(&mut local, &credentials, &remote).unwrap();
        let cache = local.group_event_caches.get(&group_id).unwrap();
        assert!(!cache.needs_control_hydration);
        assert_eq!(cache.events.len(), 1);

        local
            .group_event_caches
            .get_mut(&group_id)
            .unwrap()
            .needs_control_hydration = true;
        let path = std::env::temp_dir().join(format!(
            "noise-hydration-repair-{}-{}.json",
            std::process::id(),
            current_nanos(),
        ));
        save_state(&path, &local).unwrap();
        NoiseClient::default()
            .apply_group_activity(
                &path,
                GroupActivityUpdate {
                    group_id: group_id.clone(),
                    events: Vec::new(),
                    full_snapshot: false,
                    older_cursor: None,
                    has_older_messages: false,
                    topic_update: None,
                },
            )
            .unwrap();
        assert!(
            !load_state(&path)
                .unwrap()
                .group_event_caches
                .get(&group_id)
                .unwrap()
                .needs_control_hydration
        );
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn account_vault_restores_group_encryption_without_device_admission() {
        let identity = Identity::generate();
        let credentials = AccountCredentials {
            noise_id: "123456789012".to_owned(),
            locator: "ab".repeat(32),
            vault_key_base64: base64::engine::general_purpose::STANDARD_NO_PAD.encode([7_u8; 32]),
        };
        let mut state = account_state(&identity, &credentials, test_profile(None), 1, 1);
        let group =
            GroupMembership::create_owned("recoverable group", identity.public_key_base64());
        let group_id = group.group_id.clone();
        state.add_group(group.clone());

        state.mls_group_states.insert(
            group_id.clone(),
            MlsAccountState::create(&identity).unwrap(),
        );
        assert!(
            !state
                .vault_contents()
                .mls_group_states
                .contains_key(&group_id),
            "pending KeyPackage state must not make the recovery vault unrestorable"
        );
        state.mls_group_states.clear();

        let mut legacy_mls = MlsAccountState::create(&identity).unwrap();
        let genesis = legacy_mls.create_group_genesis(&identity, &group).unwrap();
        let expected_epoch = legacy_mls.epoch(&group_id).unwrap();
        let expected_device_id = legacy_mls.device_credential().device_id_base64.clone();
        state.mls_device = Some(legacy_mls);

        assert!(state.migrate_recoverable_mls_groups().unwrap());
        assert!(state.mls_recovery_dirty);
        let vault_contents = state.vault_contents();
        assert!(vault_contents.mls_group_states.contains_key(&group_id));
        let vault = AccountVault::seal(
            &identity,
            &credentials,
            2,
            &serde_json::to_vec(&vault_contents).unwrap(),
        )
        .unwrap();
        let restored_contents = serde_json::from_slice(&vault.open(&credentials).unwrap()).unwrap();

        let mut restored = ClientState::from_vault(
            restored_contents,
            state.account.clone().expect("test account session"),
        )
        .unwrap();
        assert!(!restored.mls_recovery_dirty);
        let restored_mls = restored
            .mls_group_state(&group_id)
            .expect("restored group encryption");

        assert_eq!(restored_mls.epoch(&group_id).unwrap(), expected_epoch);
        assert_eq!(
            restored_mls.device_credential().device_id_base64,
            expected_device_id
        );
        assert!(restored.mls_join_requests.is_empty());
        assert_eq!(
            sync_mls_state_from_log(
                &mut restored,
                &MlsControlLog {
                    genesis,
                    epochs: Vec::new(),
                },
            )
            .unwrap(),
            Some(0),
        );
        assert!(restored.mls_join_requests.is_empty());
    }

    fn test_attachment(chunk_lengths: &[u32]) -> MediaAttachment {
        MediaAttachment {
            file_name: "clip.mp4".to_owned(),
            mime_type: "video/mp4".to_owned(),
            byte_length: chunk_lengths.iter().map(|length| u64::from(*length)).sum(),
            chunks: chunk_lengths
                .iter()
                .enumerate()
                .map(|(index, byte_length)| MediaChunk {
                    blob_id: format!("blob-{index}"),
                    key_base64: String::new(),
                    byte_length: *byte_length,
                    storage: None,
                })
                .collect(),
            preview_data_base64: None,
            preview_mime_type: None,
            pixel_width: None,
            pixel_height: None,
        }
    }

    #[test]
    fn select_media_chunks_covers_exactly_the_requested_range() {
        let attachment = test_attachment(&[100, 100, 100, 100]);

        let selected = select_media_chunks(&attachment, 0, 100);
        assert_eq!(
            selected
                .iter()
                .map(|(index, start, _)| (*index, *start))
                .collect::<Vec<_>>(),
            vec![(0, 0)]
        );

        let selected = select_media_chunks(&attachment, 150, 350);
        assert_eq!(
            selected
                .iter()
                .map(|(index, start, _)| (*index, *start))
                .collect::<Vec<_>>(),
            vec![(1, 100), (2, 200), (3, 300)]
        );

        let selected = select_media_chunks(&attachment, 400, 400);
        assert!(selected.is_empty());
    }

    #[test]
    fn media_chunk_lru_serves_hits_and_evicts_oldest() {
        let mut cache = MediaChunkLru::new(3);
        cache.insert("a".to_owned(), Rc::new(vec![1]));
        cache.insert("b".to_owned(), Rc::new(vec![2]));
        cache.insert("c".to_owned(), Rc::new(vec![3]));

        // Touch "a" so "b" becomes the oldest entry.
        assert_eq!(
            cache.get("a", 1).as_deref().map(Vec::as_slice),
            Some(&[1][..])
        );

        cache.insert("d".to_owned(), Rc::new(vec![4]));
        assert!(cache.get("b", 1).is_none(), "oldest chunk must be evicted");
        assert_eq!(
            cache.get("a", 1).as_deref().map(Vec::as_slice),
            Some(&[1][..])
        );
        assert_eq!(
            cache.get("c", 1).as_deref().map(Vec::as_slice),
            Some(&[3][..])
        );
        assert_eq!(
            cache.get("d", 1).as_deref().map(Vec::as_slice),
            Some(&[4][..])
        );

        // Length mismatches are cache misses (stale or corrupt entries).
        assert!(cache.get("a", 2).is_none());
    }

    fn test_credentials() -> AccountCredentials {
        AccountCredentials {
            noise_id: "123456789012".to_owned(),
            locator: "ab".repeat(32),
            vault_key_base64: STANDARD_NO_PAD.encode([7_u8; 32]),
        }
    }

    #[test]
    fn admission_turns_are_deterministic_and_unique_per_control_head() {
        let members = (0..5)
            .map(|index| format!("member-{index}"))
            .collect::<HashSet<_>>();
        let head = "a".repeat(64);
        let mut ranks = members
            .iter()
            .map(|member| admission_author_rank(&head, &members, member).expect("member rank"))
            .collect::<Vec<_>>();
        ranks.sort_unstable();
        assert_eq!(ranks, (0..members.len()).collect::<Vec<_>>());
        assert_eq!(admission_author_rank(&head, &members, "outsider"), None);

        // Turns are drawn from the head record, so each epoch reshuffles them.
        let next_head = "b".repeat(64);
        assert_eq!(
            members
                .iter()
                .filter(|member| admission_author_rank(&next_head, &members, member) == Some(0))
                .count(),
            1
        );

        // The first turn admits immediately; the rest hold back for it.
        assert!(admission_turn_reached("group-turns", "digest", 0));
        assert!(!admission_turn_reached("group-turns", "digest", 1));
    }

    #[test]
    fn a_member_can_author_the_epoch_that_admits_a_joiner() {
        let founder_identity = Identity::generate();
        let member_identity = Identity::generate();
        let joiner_identity = Identity::generate();
        let group =
            GroupMembership::create_owned("member admission", founder_identity.public_key_base64());
        let group_id = group.group_id.clone();
        let mut founder_mls = MlsAccountState::create(&founder_identity).unwrap();
        let mut member_mls = MlsAccountState::create(&member_identity).unwrap();
        let mut joiner_mls = MlsAccountState::create(&joiner_identity).unwrap();
        let member_request =
            MlsJoinRequest::create(&member_identity, &mut member_mls, group_id.clone()).unwrap();
        let joiner_request = MlsJoinRequest::create_with_membership_proof(
            &joiner_identity,
            &mut joiner_mls,
            group_id.clone(),
            SignedEvent::member_joined(&joiner_identity, &group, &test_profile(None), 1).unwrap(),
        )
        .unwrap();
        let genesis = founder_mls
            .create_group_genesis(&founder_identity, &group)
            .unwrap();
        let add_member = founder_mls
            .add_member(&group_id, &member_request.key_package_base64)
            .unwrap();
        let add_member_record = founder_mls
            .create_epoch_record(&founder_identity, &genesis.record_id, add_member.clone())
            .unwrap();
        member_mls
            .join_group(&group_id, add_member.welcome_base64.as_deref().unwrap())
            .unwrap();
        let log = MlsControlLog {
            genesis,
            epochs: vec![add_member_record],
        };
        log.verify().unwrap();

        let mut state = account_state(
            &member_identity,
            &test_credentials(),
            test_profile(None),
            1,
            1,
        );
        state.add_group(group.clone());
        state.set_mls_group_state(&group_id, member_mls);
        state.mls_control_logs.insert(group_id.clone(), log.clone());
        let (head_epoch, _) = log.head();
        let members = log
            .member_accounts_at(head_epoch)
            .unwrap()
            .iter()
            .cloned()
            .collect::<HashSet<_>>();

        let pending = pending_admission_requests(
            &state,
            &group,
            &members,
            &HashMap::new(),
            vec![joiner_request.clone()],
        )
        .unwrap();
        assert_eq!(pending.len(), 1);
        let (_, record) =
            prepare_member_add_epoch(&state, &member_identity, &group, &members, &log, &pending)
                .unwrap();
        assert_eq!(record.owner_public_key, member_identity.public_key_base64());
        let mut admitted = log.clone();
        admitted.epochs.push(record);
        admitted.verify().unwrap();
        assert!(
            admitted
                .member_accounts_at(head_epoch + 1)
                .unwrap()
                .contains(&joiner_identity.public_key_base64())
        );

        // A signed removal keeps that account out until it asks again.
        let removals = HashMap::from([(
            joiner_identity.public_key_base64(),
            joiner_request.created_at_millis,
        )]);
        assert!(
            pending_admission_requests(&state, &group, &members, &removals, vec![joiner_request])
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn initial_mls_admission_does_not_wait_for_the_whole_legacy_roster() {
        let founder_identity = Identity::generate();
        let available_identity = Identity::generate();
        let offline_identity = Identity::generate();
        let group =
            GroupMembership::create_owned("partial cutover", founder_identity.public_key_base64());
        let group_id = group.group_id.clone();
        let mut founder_mls = MlsAccountState::create(&founder_identity).unwrap();
        let mut available_mls = MlsAccountState::create(&available_identity).unwrap();
        let available_request = MlsJoinRequest::create_with_membership_proof(
            &available_identity,
            &mut available_mls,
            group_id.clone(),
            SignedEvent::member_joined(&available_identity, &group, &test_profile(None), 1)
                .unwrap(),
        )
        .unwrap();
        let genesis = founder_mls
            .create_group_genesis(&founder_identity, &group)
            .unwrap();
        let log = MlsControlLog {
            genesis: genesis.clone(),
            epochs: Vec::new(),
        };
        let mut state = account_state(
            &founder_identity,
            &test_credentials(),
            test_profile(None),
            1,
            1,
        );
        state.add_group(group.clone());
        state.set_mls_group_state(&group_id, founder_mls);
        state.mls_control_logs.insert(group_id.clone(), log.clone());

        let current_members = log
            .member_accounts_at(0)
            .unwrap()
            .iter()
            .cloned()
            .collect::<HashSet<_>>();
        let eligible_legacy_members = HashSet::from([
            founder_identity.public_key_base64(),
            available_identity.public_key_base64(),
            offline_identity.public_key_base64(),
        ]);
        let pending = pending_admission_requests(
            &state,
            &group,
            &eligible_legacy_members,
            &HashMap::new(),
            vec![available_request],
        )
        .unwrap();
        let (_, record) = prepare_member_add_epoch(
            &state,
            &founder_identity,
            &group,
            &current_members,
            &log,
            &pending,
        )
        .unwrap();
        assert!(
            record
                .member_accounts
                .contains(&available_identity.public_key_base64())
        );
        assert!(
            !record
                .member_accounts
                .contains(&offline_identity.public_key_base64())
        );

        let joined_epoch = available_mls
            .join_group(&group_id, record.bundle.welcome_base64.as_deref().unwrap())
            .unwrap();
        let history = HistoryKeyLink::unlock_history(
            &group_id,
            joined_epoch.epoch,
            &joined_epoch.archive_key_base64,
            &[record.bundle.history_link],
        )
        .unwrap();
        let epoch_zero_key = history.get(&0).unwrap();
        assert_eq!(
            genesis.legacy_history_bridge.open(epoch_zero_key).unwrap(),
            group.secret_base64
        );
    }

    #[test]
    fn a_replica_that_is_behind_is_not_a_fork_but_a_competing_epoch_is() {
        let founder_identity = Identity::generate();
        let group =
            GroupMembership::create_owned("replica repair", founder_identity.public_key_base64());
        let group_id = group.group_id.clone();
        let mut founder_mls = MlsAccountState::create(&founder_identity).unwrap();
        let mut first_joiner = MlsAccountState::create(&Identity::generate()).unwrap();
        let mut second_joiner = MlsAccountState::create(&Identity::generate()).unwrap();
        let first_package = first_joiner.key_package().unwrap();
        let second_package = second_joiner.key_package().unwrap();
        let genesis = founder_mls
            .create_group_genesis(&founder_identity, &group)
            .unwrap();
        let mut competing_mls = founder_mls.clone();
        let accepted = founder_mls.add_member(&group_id, &first_package).unwrap();
        let accepted_record = founder_mls
            .create_epoch_record(&founder_identity, &genesis.record_id, accepted)
            .unwrap();
        let competing = competing_mls
            .add_member(&group_id, &second_package)
            .unwrap();
        let competing_record = competing_mls
            .create_epoch_record(&founder_identity, &genesis.record_id, competing)
            .unwrap();

        let behind = MlsControlLog {
            genesis: genesis.clone(),
            epochs: Vec::new(),
        };
        let current = MlsControlLog {
            genesis: genesis.clone(),
            epochs: vec![accepted_record],
        };
        let forked = MlsControlLog {
            genesis,
            epochs: vec![competing_record],
        };
        assert!(control_log_leads_to(&behind, &current));
        assert!(control_log_leads_to(&current, &current));
        assert!(!control_log_leads_to(&current, &behind));
        assert!(!control_log_leads_to(&forked, &current));
    }

    #[test]
    fn media_chunk_lru_replacement_does_not_double_count_bytes() {
        let mut cache = MediaChunkLru::new(4);
        cache.insert("a".to_owned(), Rc::new(vec![1, 1]));
        cache.insert("a".to_owned(), Rc::new(vec![2, 2]));
        assert_eq!(cache.total_bytes, 2);
        cache.insert("b".to_owned(), Rc::new(vec![3, 3]));
        assert_eq!(cache.total_bytes, 4);
        assert_eq!(
            cache.get("a", 2).as_deref().map(Vec::as_slice),
            Some(&[2, 2][..])
        );
    }
}
