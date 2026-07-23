#[cfg(target_arch = "wasm32")]
use std::cell::RefCell;
#[cfg(not(target_arch = "wasm32"))]
use std::time::{SystemTime, UNIX_EPOCH};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
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
#[must_use]
pub fn export_web_state(path: &str) -> Option<Vec<u8>> {
    WEB_STATE.with(|states| states.borrow().get(path).cloned())
}

use anyhow::{Context, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use futures_util::{StreamExt, stream::FuturesUnordered};
use noise_core::{
    AcceptedMessage, AccountCredentials, AccountVault, EncryptedBlob, GroupDeletion,
    GroupEventPayload, GroupMembership, GroupPresence, GroupProfile, GroupState, HistoryKeyLink,
    Identity, InviteRecord, InviteRotation, MlsAccountState, MlsControlLog, MlsEpochRecord,
    MlsGroupGenesis, MlsJoinRequest, MlsRemovalReason, MlsRemovalRequest, Profile, SignedEvent,
    StorageManifest, StorageShard, derive_account_credentials, direct_mailbox_id,
    direct_message_id, display_frequency, display_noise_id, encode_blob_for_storage,
    frequency_locator, generate_frequency, generate_noise_id, media_preview_is_valid,
    normalize_frequency, reconstruct_blob_from_storage, valid_reaction_emoji,
};
pub use noise_core::{MediaAttachment, MediaChunk, ProfileImage};
use noise_transport::{
    GATEWAY_HEADER, OHTTP_RELAY_PATH, OHTTP_REQUEST_MEDIA_TYPE, OHTTP_RESPONSE_MEDIA_TYPE,
    PlainResponse, RelayDescriptor, decode_response, encode_request,
};
use ohttp::ClientRequest;
use serde::{Deserialize, Serialize};

#[cfg(not(target_arch = "wasm32"))]
mod relay_pool;

const GROUP_PRESENCE_TTL_MILLIS: u64 = 50_000;
const RECENT_GROUP_PRESENCE_MILLIS: u64 = 5 * 60_000;
const EVENT_REPLICA_SETTLE_MILLIS: u64 = 500;
const RELAY_REQUEST_TIMEOUT_SECS: u64 = 30;

#[derive(Clone)]
pub struct NoiseClient {
    http: reqwest::Client,
    mask_relays: Vec<RelayDescriptor>,
}

pub struct GroupActivityUpdate {
    group_id: String,
    events: Vec<SignedEvent>,
}

impl Default for NoiseClient {
    fn default() -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(40))
            .build()
            .expect("Noise HTTP configuration is valid");
        #[cfg(target_arch = "wasm32")]
        let http = reqwest::Client::builder()
            .build()
            .expect("Noise HTTP configuration is valid");
        Self {
            http,
            mask_relays: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IdentitySummary {
    pub username: String,
    pub public_key: String,
    pub noise_id: Option<String>,
    pub bio: String,
    pub avatar: Option<ProfileImage>,
    pub accepts_direct_messages: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GroupSummary {
    pub group_id: String,
    pub name: String,
    pub description: String,
    pub rules: String,
    pub avatar: Option<ProfileImage>,
    pub background: Option<ProfileImage>,
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LocalSummary {
    pub identity: IdentitySummary,
    pub groups: Vec<GroupSummary>,
    pub directs: Vec<DirectSummary>,
    pub known_people: Vec<DirectSummary>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DirectSummary {
    pub public_key: String,
    pub username: String,
    pub bio: String,
    pub avatar: Option<ProfileImage>,
    pub accepts_direct_messages: bool,
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MemberSummary {
    pub public_key: String,
    pub username: String,
    pub bio: String,
    pub avatar: Option<ProfileImage>,
    pub accepts_direct_messages: bool,
    pub is_moderator: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MessageSummary {
    pub event_id: String,
    pub message_id: String,
    pub author_public_key: String,
    pub username: String,
    pub bio: String,
    pub avatar: Option<ProfileImage>,
    pub accepts_direct_messages: bool,
    pub text: String,
    pub attachment: Option<MediaAttachment>,
    pub reply_to_message_id: Option<String>,
    pub created_at_millis: u64,
    pub reactions: Vec<ReactionSummary>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Conversation {
    pub group: GroupSummary,
    pub members: Vec<MemberSummary>,
    pub banned_members: Vec<BannedMemberSummary>,
    pub messages: Vec<MessageSummary>,
    pub reports: Vec<ReportSummary>,
    pub reported_message_event_ids: Vec<String>,
    pub rejected_events: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReportSummary {
    pub report_event_id: String,
    pub reporter_public_key: String,
    pub reporter_username: String,
    pub reporter_avatar: Option<ProfileImage>,
    pub reason: String,
    pub created_at_millis: u64,
    pub message: MessageSummary,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
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
    pub accepts_direct_messages: bool,
    pub text: String,
    pub attachment: Option<MediaAttachment>,
    pub reply_to_message_id: Option<String>,
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
pub struct GroupWatch {
    pub revision: u64,
    pub changed: bool,
    #[serde(default)]
    pub online_public_keys: Vec<String>,
    #[serde(default)]
    pub recently_active_public_keys: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct RelayGroupWatch {
    revision: u64,
    changed: bool,
    #[serde(default)]
    presences: Vec<GroupPresence>,
}

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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ClientState {
    version: u32,
    profile: Profile,
    identity_secret_base64: String,
    #[serde(default)]
    mls_device: Option<MlsAccountState>,
    #[serde(default)]
    mls_join_requests: HashMap<String, MlsJoinRequest>,
    #[serde(default)]
    mls_local_geneses: HashMap<String, MlsGroupGenesis>,
    #[serde(default)]
    mls_control_logs: HashMap<String, MlsControlLog>,
    groups: Vec<GroupMembership>,
    active_group_id: Option<String>,
    #[serde(default)]
    direct_contacts: Vec<DirectContact>,
    #[serde(default)]
    known_people: Vec<DirectContact>,
    #[serde(default)]
    active_direct_public_key: Option<String>,
    #[serde(default)]
    direct_deleted_before: HashMap<String, u64>,
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
    group_activity_initialized: HashSet<String>,
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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct AccountVaultContents {
    version: u32,
    profile: Profile,
    identity_secret_base64: String,
    groups: Vec<GroupMembership>,
    active_group_id: Option<String>,
    direct_contacts: Vec<DirectContact>,
    known_people: Vec<DirectContact>,
    active_direct_public_key: Option<String>,
    direct_deleted_before: HashMap<String, u64>,
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
    group_activity_initialized: HashSet<String>,
    group_frequencies: HashMap<String, String>,
    next_author_sequence: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct DirectContact {
    public_key: String,
    username: String,
    #[serde(default)]
    bio: String,
    #[serde(default)]
    avatar: Option<ProfileImage>,
    #[serde(default = "default_true")]
    accepts_direct_messages: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct DirectClosedPeriod {
    closed_at_millis: u64,
    reopened_at_millis: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct MessageMarker {
    created_at_millis: u64,
    event_id: String,
}

type DirectMessageMarker = MessageMarker;

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

fn default_true() -> bool {
    true
}

impl ClientState {
    fn identity(&self) -> anyhow::Result<Identity> {
        Identity::from_secret_base64(&self.identity_secret_base64)
            .context("stored identity is invalid")
    }

    fn active_group(&self) -> anyhow::Result<&GroupMembership> {
        let Some(group_id) = self.active_group_id.as_deref() else {
            bail!("no active group; make a group or join with a frequency first")
        };
        self.groups
            .iter()
            .find(|group| group.group_id == group_id)
            .context("active group is missing from local state")
    }

    fn ensure_mls_device(&mut self) -> anyhow::Result<&mut MlsAccountState> {
        if self.mls_device.is_none() {
            let identity = self.identity()?;
            self.mls_device = Some(
                MlsAccountState::create(&identity)
                    .context("could not create this device's MLS identity")?,
            );
        }
        self.mls_device
            .as_mut()
            .context("this device has no MLS identity")
    }

    fn add_group(&mut self, group: GroupMembership) {
        if !self
            .groups
            .iter()
            .any(|existing| existing.group_id == group.group_id)
        {
            self.groups.push(group.clone());
        }
        self.active_group_id = Some(group.group_id);
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
            .context("this identity has no Noise ID")?;
        Ok(AccountCredentials {
            noise_id: account.noise_id.clone(),
            locator: account.locator.clone(),
            vault_key_base64: account.vault_key_base64.clone(),
        })
    }

    fn vault_contents(&self) -> AccountVaultContents {
        AccountVaultContents {
            version: 1,
            profile: self.profile.clone(),
            identity_secret_base64: self.identity_secret_base64.clone(),
            groups: self.groups.clone(),
            active_group_id: self.active_group_id.clone(),
            direct_contacts: self.direct_contacts.clone(),
            known_people: self.known_people.clone(),
            active_direct_public_key: self.active_direct_public_key.clone(),
            direct_deleted_before: self.direct_deleted_before.clone(),
            direct_closed_periods: self.direct_closed_periods.clone(),
            direct_read_through: self.direct_read_through.clone(),
            direct_latest_activity: self.direct_latest_activity.clone(),
            group_read_through: self.group_read_through.clone(),
            group_latest_activity: self.group_latest_activity.clone(),
            group_activity_initialized: self.group_activity_initialized.clone(),
            group_frequencies: self.group_frequencies.clone(),
            next_author_sequence: self.next_author_sequence,
        }
    }

    fn from_vault(contents: AccountVaultContents, account: AccountSession) -> anyhow::Result<Self> {
        if contents.version != 1 {
            bail!("this account vault was created by an unsupported Noise version")
        }
        let identity = Identity::from_secret_base64(&contents.identity_secret_base64)
            .context("stored identity is invalid")?;
        let state = Self {
            version: 3,
            profile: contents.profile,
            identity_secret_base64: contents.identity_secret_base64,
            mls_device: Some(
                MlsAccountState::create(&identity)
                    .context("could not create this device's MLS identity")?,
            ),
            mls_join_requests: HashMap::new(),
            mls_local_geneses: HashMap::new(),
            mls_control_logs: HashMap::new(),
            groups: contents.groups,
            active_group_id: contents.active_group_id,
            direct_contacts: contents.direct_contacts,
            known_people: contents.known_people,
            active_direct_public_key: contents.active_direct_public_key,
            direct_deleted_before: contents.direct_deleted_before,
            direct_closed_periods: contents.direct_closed_periods,
            direct_latest_incoming: HashMap::new(),
            direct_latest_activity: contents.direct_latest_activity,
            direct_read_through: contents.direct_read_through,
            group_latest_incoming: HashMap::new(),
            group_latest_activity: contents.group_latest_activity,
            group_read_through: contents.group_read_through,
            group_unread_messages: HashMap::new(),
            group_activity_initialized: contents.group_activity_initialized,
            group_frequencies: contents.group_frequencies,
            next_author_sequence: contents.next_author_sequence,
            account: Some(account),
        };
        state.identity()?;
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
        self.group_unread_messages
            .get(group_id)
            .map(Vec::len)
            .unwrap_or_default()
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
        let before = (
            self.group_latest_activity.get(group_id).cloned(),
            self.group_latest_incoming.get(group_id).cloned(),
            self.group_read_through.get(group_id).cloned(),
            self.group_unread_messages.get(group_id).cloned(),
            self.group_activity_initialized.contains(group_id),
        );
        let mut activity = messages
            .iter()
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
            .map(|message| MessageMarker {
                created_at_millis: message.created_at_millis,
                event_id: message.event_id.clone(),
            })
            .collect::<Vec<_>>();
        let mut incoming = incoming;
        incoming.sort();
        incoming.dedup();

        if let Some(latest) = activity.last().cloned() {
            self.group_latest_activity
                .insert(group_id.to_owned(), latest);
        } else {
            self.group_latest_activity.remove(group_id);
        }
        if let Some(latest) = incoming.last().cloned() {
            self.group_latest_incoming
                .insert(group_id.to_owned(), latest);
        } else {
            self.group_latest_incoming.remove(group_id);
        }

        if self.group_activity_initialized.insert(group_id.to_owned()) {
            if let Some(latest) = incoming.last().cloned() {
                self.group_read_through.insert(group_id.to_owned(), latest);
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

        before
            != (
                self.group_latest_activity.get(group_id).cloned(),
                self.group_latest_incoming.get(group_id).cloned(),
                self.group_read_through.get(group_id).cloned(),
                self.group_unread_messages.get(group_id).cloned(),
                self.group_activity_initialized.contains(group_id),
            )
    }

    fn forget_group_activity(&mut self, group_id: &str) {
        self.group_latest_incoming.remove(group_id);
        self.group_latest_activity.remove(group_id);
        self.group_read_through.remove(group_id);
        self.group_unread_messages.remove(group_id);
        self.group_activity_initialized.remove(group_id);
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
        if let Some(existing) = self
            .known_people
            .iter_mut()
            .find(|person| person.public_key == contact.public_key)
        {
            *existing = contact.clone();
        } else {
            self.known_people.push(contact.clone());
        }
        if let Some(existing) = self
            .direct_contacts
            .iter_mut()
            .find(|person| person.public_key == contact.public_key)
        {
            *existing = contact;
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
            *existing = contact;
        } else {
            self.direct_contacts.push(contact);
        }
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
                accepts_direct_messages: self.profile.accepts_direct_messages,
            },
            groups: {
                let mut groups = self.groups.iter().collect::<Vec<_>>();
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
                        avatar: group.avatar.clone(),
                        background: group.background.clone(),
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
                let mut contacts = self.direct_contacts.iter().collect::<Vec<_>>();
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
                        accepts_direct_messages: contact.accepts_direct_messages,
                        is_active: self.active_direct_public_key.as_deref()
                            == Some(&contact.public_key),
                        has_unread: self.direct_has_unread(&contact.public_key),
                    })
                    .collect()
            },
            known_people: self
                .known_people
                .iter()
                .map(|contact| DirectSummary {
                    public_key: contact.public_key.clone(),
                    username: contact.username.clone(),
                    bio: contact.bio.clone(),
                    avatar: contact.avatar.clone(),
                    accepts_direct_messages: contact.accepts_direct_messages,
                    is_active: false,
                    has_unread: false,
                })
                .collect(),
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
        avatar_data_base64: Option<String>,
        avatar_mime_type: Option<String>,
        relays: Vec<String>,
    ) -> anyhow::Result<LocalSummary> {
        let path = path.as_ref();
        if state_exists(path) {
            bail!("{} already exists", path.display());
        }
        let username = username.into().trim().to_owned();
        validate_username(&username)?;
        let password = password.into();
        validate_password(&password)?;
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
                accepts_direct_messages: true,
            },
            identity_secret_base64: identity.secret_base64(),
            mls_device: Some(
                MlsAccountState::create(&identity)
                    .context("could not create this device's MLS identity")?,
            ),
            mls_join_requests: HashMap::new(),
            mls_local_geneses: HashMap::new(),
            mls_control_logs: HashMap::new(),
            groups: Vec::new(),
            active_group_id: None,
            direct_contacts: Vec::new(),
            known_people: Vec::new(),
            active_direct_public_key: None,
            direct_deleted_before: HashMap::new(),
            direct_closed_periods: Vec::new(),
            direct_latest_incoming: HashMap::new(),
            direct_latest_activity: HashMap::new(),
            direct_read_through: HashMap::new(),
            group_latest_incoming: HashMap::new(),
            group_latest_activity: HashMap::new(),
            group_read_through: HashMap::new(),
            group_unread_messages: HashMap::new(),
            group_activity_initialized: HashSet::new(),
            group_frequencies: HashMap::new(),
            next_author_sequence: 0,
            account: Some(AccountSession {
                noise_id: credentials.noise_id,
                locator: credentials.locator,
                vault_key_base64: credentials.vault_key_base64,
                revision: 0,
            }),
        };
        self.publish_account_state(&mut state, &relays).await?;
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
        if state_exists(path) {
            bail!("an identity is already active on this device")
        }
        let password = password.into();
        if password.is_empty() || password.chars().count() > 256 {
            bail!("Noise ID or password is incorrect")
        }
        let credentials = derive_account_credentials(noise_id, &password)?;
        let vault = self
            .fetch_account_vault(&relay_list(relays)?, &credentials.locator)
            .await
            .context("Noise ID or password is incorrect")?;
        let plaintext = vault
            .open(&credentials)
            .map_err(|_| anyhow::anyhow!("Noise ID or password is incorrect"))?;
        let contents: AccountVaultContents = serde_json::from_slice(&plaintext)
            .map_err(|_| anyhow::anyhow!("Noise ID or password is incorrect"))?;
        let account = AccountSession {
            noise_id: credentials.noise_id,
            locator: credentials.locator,
            vault_key_base64: credentials.vault_key_base64,
            revision: vault.revision,
        };
        let state = ClientState::from_vault(contents, account)
            .map_err(|_| anyhow::anyhow!("Noise ID or password is incorrect"))?;
        if state.identity()?.public_key_base64() != vault.identity_public_key {
            bail!("Noise ID or password is incorrect")
        }
        save_state(path, &state)?;
        state.summary()
    }

    pub async fn sync_account(
        &self,
        path: impl AsRef<Path>,
        relays: Vec<String>,
    ) -> anyhow::Result<LocalSummary> {
        let path = path.as_ref();
        let mut state = load_state(path)?;
        if state.account.is_none() {
            return state.summary();
        }
        self.publish_account_state(&mut state, &relay_list(relays)?)
            .await?;
        save_state(path, &state)?;
        state.summary()
    }

    pub async fn sync_read_state(
        &self,
        path: impl AsRef<Path>,
        relays: Vec<String>,
    ) -> anyhow::Result<LocalSummary> {
        let path = path.as_ref();
        let mut state = load_state(path)?;
        if state.account.is_none() {
            return state.summary();
        }
        let relays = relay_list(relays)?;
        let credentials = state.account_credentials()?;
        let remote = self
            .fetch_account_vault(&relays, &credentials.locator)
            .await?;
        let changed = Self::merge_remote_read_state(&mut state, &credentials, &remote)?;
        if changed {
            save_state(path, &state)?;
        }
        state.summary()
    }

    pub fn logout(
        &self,
        path: impl AsRef<Path>,
        cache_path: impl AsRef<Path>,
    ) -> anyhow::Result<()> {
        let path = path.as_ref();
        let cache_path = cache_path.as_ref();
        let media_directory = cache_path.join("media");
        if media_directory.exists() {
            fs::remove_dir_all(&media_directory)
                .with_context(|| format!("could not erase {}", media_directory.display()))?;
        }
        purge_profile_image_cache(cache_path)?;
        remove_state(path)?;
        Ok(())
    }

    pub fn local_summary(&self, path: impl AsRef<Path>) -> anyhow::Result<LocalSummary> {
        load_state(path.as_ref())?.summary()
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
        if !state.groups.iter().any(|group| group.group_id == group_id) {
            bail!("unknown group")
        }
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
        let state = load_state(path.as_ref())?;
        let group = state.active_group()?;
        let identity = state.identity()?;
        self.watch_id(group, &identity, since, relay_list(relays)?)
            .await
    }

    pub async fn watch_group_id(
        &self,
        path: impl AsRef<Path>,
        group_id: &str,
        since: Option<u64>,
        relays: Vec<String>,
    ) -> anyhow::Result<GroupWatch> {
        let state = load_state(path.as_ref())?;
        let group = state
            .groups
            .iter()
            .find(|group| group.group_id == group_id)
            .context("unknown group")?;
        let identity = state.identity()?;
        self.watch_id(group, &identity, since, relay_list(relays)?)
            .await
    }

    pub async fn heartbeat_presence(
        &self,
        path: impl AsRef<Path>,
        relays: Vec<String>,
    ) -> anyhow::Result<usize> {
        let state = load_state(path.as_ref())?;
        let identity = state.identity()?;
        let relays = relay_list(relays)?;
        let mut accepted = 0usize;
        for group in &state.groups {
            accepted += self.publish_group_presence(group, &identity, &relays).await;
        }
        for contact in &state.direct_contacts {
            let Ok(mailbox) = identity.direct_mailbox(&contact.public_key, &contact.public_key)
            else {
                continue;
            };
            accepted += self
                .publish_group_presence(&mailbox, &identity, &relays)
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
            .filter(|message| message.author_public_key == identity_public_key)
            .map(|message| message.message_id.clone())
            .collect::<HashSet<_>>();
        let group_name = view.profile.name.clone();
        let mut replies = view
            .messages
            .iter()
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
            .context("this identity has no Noise ID")?;
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
                bail!("this Noise account has been deleted")
            }
            if (200..300).contains(&response.status)
                && let Ok(change) = serde_json::from_slice::<GroupWatch>(&response.body)
            {
                return Ok(change);
            }
        }
        bail!("no relay could hold the private account watch")
    }

    async fn watch_id(
        &self,
        group: &GroupMembership,
        identity: &Identity,
        since: Option<u64>,
        relays: Vec<RelayDescriptor>,
    ) -> anyhow::Result<GroupWatch> {
        self.publish_group_presence(group, identity, &relays).await;
        let revision = since
            .map(|revision| revision.to_string())
            .unwrap_or_else(|| "initial".to_owned());
        let endpoint = format!("/v1/groups/{}/watch/{revision}", group.group_id);

        for index in 0..relays.len() {
            let Ok(response) = self
                .relay_request(&relays, index, "GET", &endpoint, &[])
                .await
            else {
                continue;
            };
            if response.status == 410 {
                bail!("group has been deleted")
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
                    if presence.expires_at_millis > now {
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
                    online_public_keys,
                    recently_active_public_keys,
                });
            }
        }
        bail!("no relay could hold the conversation watch")
    }

    async fn publish_group_presence(
        &self,
        group: &GroupMembership,
        identity: &Identity,
        relays: &[RelayDescriptor],
    ) -> usize {
        let Ok(presence) = GroupPresence::create(
            identity,
            group,
            current_millis().saturating_add(GROUP_PRESENCE_TTL_MILLIS),
        ) else {
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
                    if presence.expires_at_millis > now {
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
                    online_public_keys,
                    recently_active_public_keys,
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
        accepts_direct_messages: bool,
        relays: Vec<String>,
    ) -> anyhow::Result<LocalSummary> {
        let path = path.as_ref();
        let mut state = load_state(path)?;
        let username = username.into().trim().to_owned();
        validate_username(&username)?;
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

        if state.profile.accepts_direct_messages && !accepts_direct_messages {
            state.direct_closed_periods.push(DirectClosedPeriod {
                closed_at_millis: current_millis(),
                reopened_at_millis: None,
            });
        } else if !state.profile.accepts_direct_messages
            && accepts_direct_messages
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
        let identity = state.identity()?;
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
            self.publish_event(&relays, &event).await?;
        }
        save_state(path, &state)?;
        state.summary()
    }

    pub async fn update_group_profile(
        &self,
        path: impl AsRef<Path>,
        name: impl Into<String>,
        description: impl Into<String>,
        rules: impl Into<String>,
        avatar_data_base64: Option<String>,
        avatar_mime_type: Option<String>,
        remove_avatar: bool,
        background_data_base64: Option<String>,
        background_mime_type: Option<String>,
        remove_background: bool,
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
        let events = self.fetch_events(&current_group, relays.clone()).await?;
        let view = rebuild_group_state(&state, &current_group, &events)?;
        if view.owner_public_key.as_deref() != Some(identity.public_key_base64().as_str()) {
            bail!("only the group founder can edit its identity right now")
        }
        let members_can_send_messages =
            members_can_send_messages.unwrap_or(view.profile.members_can_send_messages);
        let members_can_send_media =
            members_can_send_media.unwrap_or(view.profile.members_can_send_media);
        let accent_color = normalize_group_accent_color(
            accent_color.unwrap_or_else(|| view.profile.accent_color.clone()),
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
            current_group.avatar.clone()
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
            current_group.background.clone()
        };

        state.groups[group_index].name = name.clone();
        state.groups[group_index].description = description.clone();
        state.groups[group_index].rules = rules.clone();
        state.groups[group_index].avatar = avatar.clone();
        state.groups[group_index].background = background.clone();
        state.groups[group_index].accent_color = accent_color.clone();
        state.groups[group_index].members_can_send_messages = members_can_send_messages;
        state.groups[group_index].members_can_send_media = members_can_send_media;
        if state.groups[group_index].owner_public_key.is_empty()
            && let Some(owner) = view.owner_public_key
        {
            state.groups[group_index].owner_public_key = owner;
        }
        let group = state.groups[group_index].clone();
        let sequence = state.take_sequence();
        let event = create_group_event(
            &state,
            &identity,
            &group,
            GroupEventPayload::GroupProfileUpdated {
                profile: GroupProfile {
                    name,
                    description,
                    rules,
                    avatar,
                    background,
                    accent_color,
                    members_can_send_messages,
                    members_can_send_media,
                },
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
        let identity = state.identity()?;
        let scope_id = if let Some(scope_id) = scope_id {
            let allowed = state.groups.iter().any(|group| group.group_id == scope_id)
                || state.direct_contacts.iter().any(|contact| {
                    identity
                        .direct_scope_id(&contact.public_key)
                        .ok()
                        .as_deref()
                        == Some(scope_id.as_str())
                });
            if !allowed {
                bail!("media does not belong to a known conversation")
            }
            scope_id
        } else {
            state.active_group()?.group_id.clone()
        };
        let _relays = relay_list(relays)?;
        #[cfg(target_arch = "wasm32")]
        {
            let mut output = Vec::with_capacity(attachment.byte_length as usize);
            for chunk in &attachment.chunks {
                let storage = chunk.storage.as_ref().context(
                    "this media predates constellation storage and is no longer available",
                )?;
                let blob = self.reconstruct_blob(storage, &chunk.key_base64).await?;
                if blob.group_id.as_deref() != Some(scope_id.as_str()) {
                    bail!("media chunk belongs to a different conversation")
                }
                let plaintext = blob.open(&chunk.key_base64)?;
                if plaintext.len() != chunk.byte_length as usize {
                    bail!("media chunk does not match its manifest")
                }
                output.extend_from_slice(&plaintext);
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
            for chunk in &attachment.chunks {
                let storage = chunk.storage.as_ref().context(
                    "this media predates constellation storage and is no longer available",
                )?;
                let blob = self.reconstruct_blob(storage, &chunk.key_base64).await?;
                if blob.group_id.as_deref() != Some(scope_id.as_str()) {
                    bail!("media chunk belongs to a different conversation")
                }
                let plaintext = blob.open(&chunk.key_base64)?;
                if plaintext.len() != chunk.byte_length as usize {
                    bail!("media chunk does not match its manifest")
                }
                output
                    .write_all(&plaintext)
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

    pub async fn upload_media_chunk(
        &self,
        path: impl AsRef<Path>,
        data_base64: String,
        relays: Vec<String>,
    ) -> anyhow::Result<MediaChunk> {
        let state = load_state(path.as_ref())?;
        let group_id = state.active_group()?.group_id.clone();
        let data = STANDARD
            .decode(data_base64)
            .context("media chunk encoding is invalid")?;
        if data.is_empty() || data.len() > 1024 * 1024 {
            bail!("media chunks must contain between 1 byte and 1 MiB")
        }
        let (blob, key_base64) = EncryptedBlob::create_for_group(&data, group_id)?;
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
        let path = path.as_ref();
        let cache_path = cache_path.as_ref();
        let mut state = load_state(path)?;
        let relays = relay_list(relays)?;
        let group = state.active_group()?.clone();
        let identity = state.identity()?;
        let self_public_key = identity.public_key_base64();
        let events = self.fetch_events(&group, relays.clone()).await?;
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
            if let Some(mls) = state.mls_device.as_mut() {
                mls.forget_group(&group.group_id)
                    .context("could not replace a losing MLS genesis")?;
            }
            state.mls_local_geneses.remove(&group.group_id);
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
        let has_pending_membership_proof = state
            .mls_join_requests
            .get(&group.group_id)
            .is_some_and(|request| {
                request.account_public_key == self_public_key
                    && join_request_membership_profile(request, &group).is_some()
            });
        if !active_members.contains(&self_public_key) && !has_pending_membership_proof {
            purge_group_cache(cache_path, &group.group_id)?;
            purge_profile_image_cache(cache_path)?;
            if let Some(mls) = state.mls_device.as_mut() {
                mls.forget_group(&group.group_id)
                    .context("could not erase this group's local encryption state")?;
            }
            state.mls_join_requests.remove(&group.group_id);
            state.mls_local_geneses.remove(&group.group_id);
            state.mls_control_logs.remove(&group.group_id);
            state.group_frequencies.remove(&group.group_id);
            state.forget_group_activity(&group.group_id);
            state
                .groups
                .retain(|candidate| candidate.group_id != group.group_id);
            state.active_group_id = state
                .groups
                .first()
                .map(|candidate| candidate.group_id.clone());
            save_state(path, &state)?;
            return Ok(GroupEncryptionStatus {
                group_id: group.group_id,
                phase: "removed".into(),
                epoch: None,
                missing_member_public_keys: Vec::new(),
            });
        }

        let local_has_mls_group = state
            .mls_device
            .as_ref()
            .is_some_and(|mls| mls.epoch(&group.group_id).is_ok());
        if !local_has_mls_group {
            if !state.mls_join_requests.contains_key(&group.group_id) {
                let request = {
                    let mls = state.ensure_mls_device()?;
                    MlsJoinRequest::create(&identity, mls, group.group_id.clone())
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

        let is_owner = group.owner_public_key == self_public_key;
        if control_log.is_none() {
            if !is_owner {
                save_state(path, &state)?;
                return Ok(GroupEncryptionStatus {
                    group_id: group.group_id,
                    phase: "waiting_for_founder".into(),
                    epoch: None,
                    missing_member_public_keys: Vec::new(),
                });
            }
            let requests = self
                .fetch_mls_join_requests(&relays, &group.group_id)
                .await?;
            let requested_accounts = requests
                .iter()
                .map(|request| request.account_public_key.clone())
                .collect::<HashSet<_>>();
            let mut missing_member_public_keys = active_members
                .iter()
                .filter(|member| {
                    member.as_str() != self_public_key && !requested_accounts.contains(*member)
                })
                .cloned()
                .collect::<Vec<_>>();
            missing_member_public_keys.sort();
            if !missing_member_public_keys.is_empty() {
                save_state(path, &state)?;
                return Ok(GroupEncryptionStatus {
                    group_id: group.group_id,
                    phase: "waiting_for_members".into(),
                    epoch: None,
                    missing_member_public_keys,
                });
            }

            if !state.mls_local_geneses.contains_key(&group.group_id) {
                let mut candidate = state.ensure_mls_device()?.clone();
                let genesis = candidate
                    .create_group_genesis(&identity, &group)
                    .context("could not create the group MLS genesis")?;
                state.mls_device = Some(candidate);
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
        if is_owner && local_epoch.is_some() {
            state
                .mls_control_logs
                .insert(group.group_id.clone(), control_log.clone());
            let current_view = rebuild_group_state(&state, &group, &events)?;
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
                self.publish_mls_epoch(&relays, &record).await?;
                state.mls_device = Some(candidate);
                control_log.epochs.push(record);
                state
                    .mls_control_logs
                    .insert(group.group_id.clone(), control_log.clone());
                save_state(path, &state)?;
            }
            let (head_epoch, _) = control_log.head();
            let current_members = if head_epoch == 0 && control_log.epochs.is_empty() {
                legacy_view.members.keys().cloned().collect::<HashSet<_>>()
            } else {
                control_log
                    .member_accounts_at(head_epoch)
                    .unwrap_or_default()
                    .iter()
                    .cloned()
                    .collect::<HashSet<_>>()
            };
            let requests = self
                .fetch_mls_join_requests(&relays, &group.group_id)
                .await?;
            if let Some((candidate, record)) = prepare_pending_member_add(
                &state,
                &identity,
                &group,
                &current_members,
                &current_view.banned_members,
                &latest_removal_by_account,
                &control_log,
                requests,
            )? {
                self.publish_mls_epoch(&relays, &record).await?;
                state.mls_device = Some(candidate);
                control_log.epochs.push(record);
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
        let (head_epoch, _) = control_log.head();
        if local_epoch == Some(head_epoch)
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
                    accepts_direct_messages: state.profile.accepts_direct_messages,
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
        let identity = state.identity()?;
        let relays = relay_list(relays)?;
        let mut group = GroupMembership::create_owned(name, identity.public_key_base64());
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
                avatar: group.avatar,
                background: group.background,
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
        state.add_group(payload.group);
        let group = state.active_group()?.clone();
        let identity = state.identity()?;
        let control_log = self.fetch_mls_control_log(&relays, &group.group_id).await?;
        if let Some(control_log) = control_log {
            let sequence = state.take_sequence();
            let membership_proof =
                SignedEvent::member_joined(&identity, &group, &state.profile, sequence)?;
            let request = {
                let mls = state.ensure_mls_device()?;
                MlsJoinRequest::create_with_membership_proof(
                    &identity,
                    mls,
                    group.group_id.clone(),
                    membership_proof,
                )
                .context("could not create the encrypted-group join request")?
            };
            self.publish_mls_join_request(&relays, &request).await?;
            state
                .mls_join_requests
                .insert(group.group_id.clone(), request);
            state
                .mls_control_logs
                .insert(group.group_id.clone(), control_log);
        } else {
            let sequence = state.take_sequence();
            let joined = SignedEvent::member_joined(&identity, &group, &state.profile, sequence)?;
            self.publish_event(&relays, &joined).await?;
        }
        save_state(path, &state)?;
        Ok(JoinResult {
            group: GroupSummary {
                group_id: group.group_id,
                name: group.name,
                description: group.description,
                rules: group.rules,
                avatar: group.avatar,
                background: group.background,
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
        accepts_direct_messages: bool,
    ) -> anyhow::Result<LocalSummary> {
        let path = path.as_ref();
        let mut state = load_state(path)?;
        let self_public_key = state.identity()?.public_key_base64();
        if public_key == self_public_key {
            bail!("you cannot start a direct message with yourself")
        }
        direct_mailbox_id(public_key).context("that identity has an invalid public key")?;
        let username = username.into();
        let bio = bio.into();
        validate_username(&username)?;
        if bio.chars().count() > 160 {
            bail!("bios can contain at most 160 characters")
        }
        if !accepts_direct_messages {
            bail!("this person is not accepting direct messages")
        }
        state.add_direct(DirectContact {
            public_key: public_key.to_owned(),
            username,
            bio,
            avatar,
            accepts_direct_messages,
        });
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
        let identity = state.identity()?;
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
                Ok(DirectConversation {
                    contact: direct_summary(
                        contact,
                        state.active_direct_public_key.as_deref() == Some(&contact.public_key),
                        state.direct_has_unread(&contact.public_key),
                    ),
                    media_scope_id: identity.direct_scope_id(&contact.public_key)?,
                    messages: messages_by_contact
                        .remove(&contact.public_key)
                        .unwrap_or_default(),
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(DirectInbox {
            summary: state.summary()?,
            conversations,
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
            .sync_direct_inbox(path, cache_path.as_ref(), relays.clone())
            .await?;
        let public_key = state
            .active_direct_public_key
            .clone()
            .context("choose a direct conversation first")?;
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
        Ok(DirectConversation {
            contact: direct_summary(&contact, true, false),
            media_scope_id: state.identity()?.direct_scope_id(&contact.public_key)?,
            messages: messages
                .into_iter()
                .filter(|message| message.counterparty_public_key == public_key)
                .map(|message| message.message)
                .collect(),
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
        validate_reply_reference(reply_to_message_id.as_deref())?;
        let contact = state
            .active_direct_public_key
            .as_deref()
            .and_then(|public_key| {
                state
                    .direct_contacts
                    .iter()
                    .find(|contact| contact.public_key == public_key)
            })
            .cloned()
            .context("choose a direct conversation first")?;
        if !contact.accepts_direct_messages {
            bail!("this person is not accepting direct messages")
        }
        let identity = state.identity()?;
        let self_public_key = identity.public_key_base64();
        let recipient_mailbox =
            identity.direct_mailbox(&contact.public_key, &contact.public_key)?;
        let sender_mailbox = identity.direct_mailbox(&contact.public_key, &self_public_key)?;
        let sequence = state.take_sequence();
        let recipient_event = SignedEvent::direct_message(
            &identity,
            &recipient_mailbox,
            &contact.public_key,
            &state.profile,
            text.clone(),
            attachment.clone(),
            reply_to_message_id.clone(),
            sequence,
        )?;
        let sender_event = SignedEvent::direct_message(
            &identity,
            &sender_mailbox,
            &contact.public_key,
            &state.profile,
            text,
            attachment,
            reply_to_message_id,
            sequence,
        )?;
        let sent = SentMessageResult {
            event_id: sender_event.event_id.clone(),
            message_id: direct_message_id(&self_public_key, sequence),
            created_at_millis: sender_event.created_at_millis,
        };
        let relays = relay_list(relays)?;
        self.publish_event(&relays, &recipient_event).await?;
        self.publish_event(&relays, &sender_event).await?;
        let marker = DirectMessageMarker {
            created_at_millis: sender_event.created_at_millis,
            event_id: sender_event.event_id,
        };
        state
            .direct_latest_activity
            .entry(contact.public_key)
            .and_modify(|latest| *latest = latest.clone().max(marker.clone()))
            .or_insert(marker);
        save_state(path, &state)?;
        Ok(sent)
    }

    pub async fn upload_direct_media_chunk(
        &self,
        path: impl AsRef<Path>,
        data_base64: String,
        relays: Vec<String>,
    ) -> anyhow::Result<MediaChunk> {
        let state = load_state(path.as_ref())?;
        let contact = state
            .active_direct_public_key
            .as_deref()
            .and_then(|public_key| {
                state
                    .direct_contacts
                    .iter()
                    .find(|contact| contact.public_key == public_key)
            })
            .context("choose a direct conversation first")?;
        let scope_id = state.identity()?.direct_scope_id(&contact.public_key)?;
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
        if text.trim().is_empty() && attachment.is_none() {
            bail!("message cannot be empty")
        }
        validate_reply_reference(reply_to_message_id.as_deref())?;
        let relays = relay_list(relays)?;
        let mut state = load_state(path)?;
        let group = state.active_group()?.clone();
        let sequence = state.take_sequence();
        let event = create_group_event(
            &state,
            &state.identity()?,
            &group,
            GroupEventPayload::Message {
                text,
                attachment,
                reply_to_message_id,
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
        let view = rebuild_group_state(
            &state,
            &group,
            &self.fetch_events(&group, relays.clone()).await?,
        )?;
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
        let view = rebuild_group_state(
            &state,
            &group,
            &self.fetch_events(&group, relays.clone()).await?,
        )?;
        let is_owner = view.owner_public_key.as_deref() == Some(actor_public_key.as_str());
        let target = view
            .messages
            .iter()
            .find(|message| message.event_id == message_event_id)
            .context("that message no longer exists")?;
        let is_moderator = view.moderators.contains(&actor_public_key);
        let is_active = view.members.contains_key(&actor_public_key);
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
        let _ = self.erase_storage_references(storage, false).await;
        save_state(path, &state)?;
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
        let view = rebuild_group_state(
            &state,
            &group,
            &self.fetch_events(&group, relays.clone()).await?,
        )?;
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
        let view = rebuild_group_state(
            &state,
            &group,
            &self.fetch_events(&group, relays.clone()).await?,
        )?;
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
        let view = rebuild_group_state(
            &state,
            &group,
            &self.fetch_events(&group, relays.clone()).await?,
        )?;
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
        save_state(path, &state)?;
        self.publish_event(&relays, &event).await?;
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
        let view = rebuild_group_state(
            &state,
            &group,
            &self.fetch_events(&group, relays.clone()).await?,
        )?;
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
        if let Some(mls) = state.mls_device.as_mut() {
            mls.forget_group(&group.group_id)
                .context("could not erase this group's local encryption state")?;
        }
        state.mls_join_requests.remove(&group.group_id);
        state.mls_local_geneses.remove(&group.group_id);
        state.mls_control_logs.remove(&group.group_id);
        state.group_frequencies.remove(&group.group_id);
        state.forget_group_activity(&group.group_id);
        state
            .groups
            .retain(|candidate| candidate.group_id != group.group_id);
        state.active_group_id = state
            .groups
            .first()
            .map(|candidate| candidate.group_id.clone());
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
            let events = self.fetch_events(&group, relays.clone()).await?;
            self.erase_storage_references(group_storage_references(&group, &events), true)
                .await?;
            self.publish_group_deletion(&relays, &deletion).await?;
        }

        purge_group_cache(cache_path.as_ref(), group_id)?;
        purge_profile_image_cache(cache_path.as_ref())?;
        if let Some(mls) = state.mls_device.as_mut() {
            mls.forget_group(group_id)
                .context("could not erase this group's local encryption state")?;
        }
        state.groups.remove(group_index);
        state.group_frequencies.remove(group_id);
        state.forget_group_activity(group_id);
        state.mls_join_requests.remove(group_id);
        state.mls_local_geneses.remove(group_id);
        state.mls_control_logs.remove(group_id);
        if state.active_group_id.as_deref() == Some(group_id) {
            state.active_group_id = state.groups.first().map(|group| group.group_id.clone());
        }
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
    ) -> anyhow::Result<LocalSummary> {
        let update = self
            .fetch_group_activity(path.as_ref(), group_id, relays)
            .await?;
        self.apply_group_activity(path, update)
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
        let events = self.fetch_events(&group, relay_list(relays)?).await?;
        Ok(GroupActivityUpdate {
            group_id: group_id.to_owned(),
            events,
        })
    }

    pub fn apply_group_activity(
        &self,
        path: impl AsRef<Path>,
        update: GroupActivityUpdate,
    ) -> anyhow::Result<LocalSummary> {
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
        let view = rebuild_group_state(&state, &group, &update.events)?;
        if view.members.contains_key(&identity_public_key)
            && state.record_group_activity(&group_id, &view.messages, &identity_public_key)
        {
            save_state(path, &state)?;
        }
        state.summary()
    }

    pub fn mark_group_read(
        &self,
        path: impl AsRef<Path>,
        group_id: &str,
    ) -> anyhow::Result<LocalSummary> {
        let path = path.as_ref();
        let mut state = load_state(path)?;
        if !state.groups.iter().any(|group| group.group_id == group_id) {
            bail!("unknown group")
        }
        let was_initialized = state.group_activity_initialized.insert(group_id.to_owned());
        let latest = state.group_latest_incoming.get(group_id).cloned();
        let marker_changed = latest
            .as_ref()
            .is_some_and(|marker| state.group_read_through.get(group_id) != Some(marker));
        if let Some(marker) = latest {
            state.group_read_through.insert(group_id.to_owned(), marker);
        }
        let had_unread = state.group_unread_messages.remove(group_id).is_some();
        if was_initialized || marker_changed || had_unread {
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
                    accepts_direct_messages: state.profile.accepts_direct_messages,
                },
                sequence,
            )?;
            save_state(path, &state)?;
            self.publish_event(&relays, &joined).await?;
            events.push(joined);
        }
        let view = rebuild_group_state(&state, &group, &events)?;
        let mut state_changed =
            state.record_group_activity(&group.group_id, &view.messages, &identity_public_key);
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
                accepts_direct_messages: member.accepts_direct_messages,
            });
        }
        if state.groups[group_index].name != resolved_profile.name
            || state.groups[group_index].description != resolved_profile.description
            || state.groups[group_index].rules != resolved_profile.rules
            || state.groups[group_index].avatar != resolved_profile.avatar
            || state.groups[group_index].background != resolved_profile.background
            || state.groups[group_index].accent_color != resolved_profile.accent_color
            || state.groups[group_index].members_can_send_messages
                != resolved_profile.members_can_send_messages
            || state.groups[group_index].members_can_send_media
                != resolved_profile.members_can_send_media
            || state.groups[group_index].owner_public_key != resolved_owner
        {
            state.groups[group_index].name = resolved_profile.name.clone();
            state.groups[group_index].description = resolved_profile.description.clone();
            state.groups[group_index].rules = resolved_profile.rules.clone();
            state.groups[group_index].avatar = resolved_profile.avatar.clone();
            state.groups[group_index].background = resolved_profile.background.clone();
            state.groups[group_index].accent_color = resolved_profile.accent_color.clone();
            state.groups[group_index].members_can_send_messages =
                resolved_profile.members_can_send_messages;
            state.groups[group_index].members_can_send_media =
                resolved_profile.members_can_send_media;
            state.groups[group_index].owner_public_key = resolved_owner.clone();
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
                        accepts_direct_messages: message.accepts_direct_messages,
                        text: message.text.clone(),
                        attachment: message.attachment.clone(),
                        reply_to_message_id: message.reply_to_message_id.clone(),
                        created_at_millis: message.created_at_millis,
                        reactions: reactions_by_message
                            .get(&message.event_id)
                            .cloned()
                            .unwrap_or_default(),
                    },
                })
            })
            .collect::<Vec<_>>();
        let mut members = view
            .members
            .into_values()
            .map(|member| MemberSummary {
                is_moderator: moderators.contains(&member.public_key),
                public_key: member.public_key,
                username: member.username,
                bio: member.bio,
                avatar: member.avatar,
                accepts_direct_messages: member.accepts_direct_messages,
            })
            .collect::<Vec<_>>();
        members.sort_by(|left, right| left.username.cmp(&right.username));
        Ok(Conversation {
            group: GroupSummary {
                group_id: group.group_id.clone(),
                name: resolved_profile.name,
                description: resolved_profile.description,
                rules: resolved_profile.rules,
                avatar: resolved_profile.avatar,
                background: resolved_profile.background,
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
                read_state_initialized: state.group_activity_initialized.contains(&group.group_id),
            },
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
                        accepts_direct_messages: message.accepts_direct_messages,
                        text: message.text,
                        attachment: message.attachment,
                        reply_to_message_id: message.reply_to_message_id,
                        created_at_millis: message.created_at_millis,
                        reactions,
                    }
                })
                .collect(),
            reports,
            reported_message_event_ids,
            rejected_events: view.rejected_events,
        })
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
        let events = self.fetch_events_for_id(&mailbox_id, relays).await?;
        let contacts_before = state.direct_contacts.clone();
        let active_before = state.active_direct_public_key.clone();
        let deletions_before = state.direct_deleted_before.clone();
        let latest_incoming_before = state.direct_latest_incoming.clone();
        let latest_activity_before = state.direct_latest_activity.clone();
        let decoded = events
            .iter()
            .filter_map(|event| decrypt_direct_event(&identity, &state, event))
            .collect::<Vec<_>>();
        for event in &decoded {
            if let DecryptedDirectEvent::ThreadDeleted {
                counterparty_public_key,
                deleted_at_millis,
            } = event
            {
                state
                    .direct_deleted_before
                    .entry(counterparty_public_key.clone())
                    .and_modify(|cutoff| *cutoff = (*cutoff).max(*deleted_at_millis))
                    .or_insert(*deleted_at_millis);
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
        for message in &messages {
            state.remember_direct(message.contact.clone());
            let marker = DirectMessageMarker {
                created_at_millis: message.message.created_at_millis,
                event_id: message.message.event_id.clone(),
            };
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
        if state.direct_contacts != contacts_before
            || state.active_direct_public_key != active_before
            || state.direct_deleted_before != deletions_before
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
        self.publish_mls_control_object(relays, "/v2/mls/genesis", genesis)
            .await
    }

    async fn publish_mls_epoch(
        &self,
        relays: &[RelayDescriptor],
        epoch: &MlsEpochRecord,
    ) -> anyhow::Result<()> {
        self.publish_mls_control_object(relays, "/v2/mls/epochs", epoch)
            .await
    }

    async fn publish_mls_control_object<T: Serialize>(
        &self,
        relays: &[RelayDescriptor],
        endpoint: &str,
        object: &T,
    ) -> anyhow::Result<()> {
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
        if accepted != relays.len() {
            if conflicts > 0 {
                bail!("the group encryption head changed on another device; sync and retry")
            }
            bail!("every configured relay must confirm a group encryption update")
        }
        Ok(())
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
        if observations.iter().all(Option::is_none) {
            return Ok(None);
        }
        let expected = observations
            .iter()
            .find_map(Option::as_ref)
            .context("group encryption observations are empty")?;
        if observations
            .iter()
            .any(|observation| observation.as_ref() != Some(expected))
        {
            bail!("group encryption relays are still converging; retry in a moment")
        }
        Ok(Some(expected.clone()))
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
        let credentials = state.account_credentials()?;
        let identity = state.identity()?;
        if let Ok(remote) = self.fetch_account_vault(relays, &credentials.locator).await {
            Self::merge_remote_read_state(state, &credentials, &remote)?;
        }

        let mut last_error = None;
        for _ in 0..4 {
            let revision = state
                .account
                .as_ref()
                .context("this identity has no Noise ID")?
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
                    .context("this identity has no Noise ID")?
                    .revision = revision;
                return Ok(());
            }

            Self::merge_remote_read_state(state, &credentials, &remote)?;
            last_error = Some(publish_error.unwrap_or_else(|| {
                anyhow::anyhow!("another device updated the encrypted account vault")
            }));
        }

        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("account sync did not complete")))
    }

    fn merge_remote_read_state(
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
            bail!("this account vault was created by an unsupported Noise version")
        }
        let direct_reads_changed = state.merge_direct_read_through(&contents.direct_read_through);
        let group_reads_changed = state.merge_group_read_through(&contents.group_read_through);
        let group_activity_changed = merge_read_markers(
            &mut state.group_latest_activity,
            &contents.group_latest_activity,
        );
        let initialized_before = state.group_activity_initialized.len();
        state
            .group_activity_initialized
            .extend(contents.group_activity_initialized);
        let initialized_changed = state.group_activity_initialized.len() != initialized_before;
        let account = state
            .account
            .as_mut()
            .context("this identity has no Noise ID")?;
        let revision_changed = remote.revision > account.revision;
        account.revision = account.revision.max(remote.revision);
        Ok(direct_reads_changed
            || group_reads_changed
            || group_activity_changed
            || initialized_changed
            || revision_changed)
    }

    async fn publish_account_vault(
        &self,
        relays: &[RelayDescriptor],
        vault: &AccountVault,
    ) -> anyhow::Result<()> {
        let body = serde_json::to_vec(vault)?;
        let mut accepted = 0usize;
        for index in 0..relays.len() {
            if let Ok(response) = self
                .relay_request(relays, index, "POST", "/v1/accounts", &body)
                .await
                && (200..300).contains(&response.status)
            {
                accepted += 1;
            }
        }
        if accepted == 0 {
            bail!("no relay accepted the encrypted account vault")
        }
        Ok(())
    }

    async fn fetch_account_vault(
        &self,
        relays: &[RelayDescriptor],
        locator: &str,
    ) -> anyhow::Result<AccountVault> {
        let mut newest: Option<AccountVault> = None;
        for index in 0..relays.len() {
            let Ok(response) = self
                .relay_request(
                    relays,
                    index,
                    "GET",
                    &format!("/v1/accounts/{locator}"),
                    &[],
                )
                .await
            else {
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
        let body = serde_json::to_vec(event)?;
        let mut publications = FuturesUnordered::new();
        for index in 0..relays.len() {
            let body = body.as_slice();
            publications.push(async move {
                self.relay_request(relays, index, "POST", "/v1/events", body)
                    .await
            });
        }
        while let Some(result) = publications.next().await {
            if result.is_ok_and(|response| (200..300).contains(&response.status)) {
                // Events are idempotent and relays replicate accepted events to
                // their peers. A lagging replica must not hold the sender UI.
                return Ok(());
            }
        }
        bail!("no relay accepted the event")
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
                let deletion = noise_core::shard_deletion(&key_base64, &placement.shard_id)?;
                let client = self.clone();
                deletions.push(async move {
                    let relay = RelayDescriptor::parse(&placement.relay)?;
                    let body = serde_json::to_vec(&deletion)?;
                    let response = client
                        .relay_request(
                            std::slice::from_ref(&relay),
                            0,
                            "DELETE",
                            &format!("/v3/shards/{}", placement.shard_id),
                            &body,
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
                let body = serde_json::to_vec(&shard)?;
                let response = client
                    .relay_request(std::slice::from_ref(&relay), 0, "POST", "/v3/shards", &body)
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
                        &format!("/v3/shards/{}", placement.shard_id),
                        &[],
                    )
                    .await?;
                anyhow::ensure!(
                    (200..300).contains(&response.status),
                    "storage relay does not have shard"
                );
                let shard = serde_json::from_slice::<StorageShard>(&response.body)?;
                shard.verify()?;
                anyhow::ensure!(
                    shard.shard_id == placement.shard_id,
                    "storage relay returned the wrong shard"
                );
                anyhow::ensure!(
                    shard.payload_hash == placement.payload_hash,
                    "storage relay returned a corrupt shard"
                );
                Ok::<StorageShard, anyhow::Error>(shard)
            });
        }
        let mut shards = Vec::with_capacity(required);
        let mut healthy_shard_ids = HashSet::new();
        while let Some(result) = downloads.next().await {
            if let Ok(shard) = result {
                healthy_shard_ids.insert(shard.shard_id.clone());
                shards.push(shard);
                if shards.len() >= required {
                    break;
                }
            }
        }
        let blob = reconstruct_blob_from_storage(manifest, &shards)
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
                    &format!("/v3/shards/{}", placement.shard_id),
                    &[],
                )
                .await
                .ok()
                .filter(|response| (200..300).contains(&response.status))
                .and_then(|response| serde_json::from_slice::<StorageShard>(&response.body).ok())
                .is_some_and(|existing| {
                    existing.verify().is_ok()
                        && existing.shard_id == placement.shard_id
                        && existing.payload_hash == placement.payload_hash
                });
            if already_healthy {
                continue;
            }
            let Ok(body) = serde_json::to_vec(&shard) else {
                continue;
            };
            let _ = self
                .relay_request(std::slice::from_ref(&relay), 0, "POST", "/v3/shards", &body)
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

    async fn fetch_events_for_id(
        &self,
        id: &str,
        relays: Vec<RelayDescriptor>,
    ) -> anyhow::Result<Vec<SignedEvent>> {
        let endpoint = format!("/v1/groups/{id}/events");
        let mut requests = FuturesUnordered::new();
        for index in 0..relays.len() {
            let endpoint = endpoint.clone();
            let relays = &relays;
            requests.push(async move {
                self.relay_request(&relays, index, "GET", &endpoint, &[])
                    .await
            });
        }

        let mut merged = HashMap::<String, SignedEvent>::new();
        let mut reachable = 0usize;
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
            let Some(Ok(response)) = response else {
                continue;
            };
            if !(200..300).contains(&response.status) {
                continue;
            }
            let Ok(events) = serde_json::from_slice::<Vec<SignedEvent>>(&response.body) else {
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
            bail!("no relay was reachable")
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
        let mask = self
            .mask_relays
            .iter()
            .find(|candidate| candidate.base_url != storage.base_url)
            .or_else(|| {
                (1..relays.len())
                    .map(|offset| &relays[(storage_index + offset) % relays.len()])
                    .find(|candidate| candidate.base_url != storage.base_url)
            });
        if let (Some(config), Some(mask)) = (storage.ohttp_config.as_deref(), mask) {
            return self
                .oblivious_request(storage, mask, config, method, path, body)
                .await;
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
            request = request
                .header(reqwest::header::CONTENT_TYPE, "application/json")
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
                }) if recipient_public_key == contact.public_key
                    && valid_direct_profile(&sender_profile)
                    && valid_direct_content(&text, attachment.as_ref())
                    && validate_reply_reference(reply_to_message_id.as_deref()).is_ok() =>
                {
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
                            accepts_direct_messages: sender_profile.accepts_direct_messages,
                            text,
                            attachment,
                            reply_to_message_id,
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
        } if recipient_public_key == self_public_key
            && valid_direct_profile(&sender_profile)
            && valid_direct_content(&text, attachment.as_ref())
            && validate_reply_reference(reply_to_message_id.as_deref()).is_ok() =>
        {
            let contact = DirectContact {
                public_key: event.author_public_key.clone(),
                username: sender_profile.username.clone(),
                bio: sender_profile.bio.clone(),
                avatar: sender_profile.avatar.clone(),
                accepts_direct_messages: sender_profile.accepts_direct_messages,
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
                    accepts_direct_messages: sender_profile.accepts_direct_messages,
                    text,
                    attachment,
                    reply_to_message_id,
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
        _ => None,
    }
}

fn sync_mls_state_from_log(
    state: &mut ClientState,
    log: &MlsControlLog,
) -> anyhow::Result<Option<u64>> {
    log.verify().context("group MLS control log is invalid")?;
    let mut candidate = state.ensure_mls_device()?.clone();
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
        state.mls_device = Some(candidate);
        return Ok(Some(
            log.epochs
                .last()
                .map(|record| record.bundle.epoch)
                .unwrap_or(joined_epoch),
        ));
    };

    for record in &log.epochs {
        if record.bundle.epoch <= current_epoch {
            continue;
        }
        candidate
            .process_commit(&record.bundle)
            .context("could not advance this device to the current MLS epoch")?;
        validate_mls_member_accounts(&candidate, &log.genesis.group_id, &record.member_accounts)?;
        current_epoch = record.bundle.epoch;
    }
    state.mls_device = Some(candidate);
    Ok(Some(current_epoch))
}

fn rebuild_group_state(
    state: &ClientState,
    group: &GroupMembership,
    events: &[SignedEvent],
) -> anyhow::Result<GroupState> {
    let Some(log) = state.mls_control_logs.get(&group.group_id) else {
        return Ok(GroupState::rebuild(group, events));
    };
    log.verify().context("cached MLS control log is invalid")?;
    let mls = state
        .mls_device
        .as_ref()
        .context("this device has no MLS identity")?;
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

fn active_group_epoch(
    state: &ClientState,
    group_id: &str,
) -> anyhow::Result<Option<noise_core::MlsEpochSummary>> {
    if !state.mls_control_logs.contains_key(group_id) {
        return Ok(None);
    }
    Ok(Some(
        state
            .mls_device
            .as_ref()
            .context("this device has no MLS identity")?
            .epoch(group_id)
            .context("this device is not in the current encrypted group")?,
    ))
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

fn prepare_pending_member_add(
    state: &ClientState,
    identity: &Identity,
    group: &GroupMembership,
    active_members: &HashSet<String>,
    banned_members: &HashSet<String>,
    latest_removal_by_account: &HashMap<String, u64>,
    log: &MlsControlLog,
    requests: Vec<MlsJoinRequest>,
) -> anyhow::Result<Option<(MlsAccountState, MlsEpochRecord)>> {
    let mls = state
        .mls_device
        .as_ref()
        .context("this device has no MLS identity")?;
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
            || banned_members.contains(&request.account_public_key)
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
    if newest_by_device.is_empty() {
        return Ok(None);
    }
    let mut requests = newest_by_device.into_values().collect::<Vec<_>>();
    requests.sort_by(|left, right| left.request_id.cmp(&right.request_id));
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
    let mut expected = active_members.iter().cloned().collect::<Vec<_>>();
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
    Ok(Some((candidate, record)))
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
        .mls_device
        .as_ref()
        .context("this device has no MLS identity")?;
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
        accepts_direct_messages,
    } = proof.decrypt(group).ok()?
    else {
        return None;
    };
    let profile = Profile {
        username,
        bio,
        avatar,
        accepts_direct_messages,
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
        accepts_direct_messages: contact.accepts_direct_messages,
        is_active,
        has_unread,
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

fn group_storage_references(
    group: &GroupMembership,
    events: &[SignedEvent],
) -> Vec<(StorageManifest, String)> {
    let mut references = Vec::new();
    references.extend(group.avatar.as_ref().and_then(image_storage_reference));
    references.extend(group.background.as_ref().and_then(image_storage_reference));
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
    let directory = cache_path.join("media").join(scope_id);
    if directory.exists() {
        fs::remove_dir_all(&directory)
            .with_context(|| format!("could not erase {}", directory.display()))?;
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

#[cfg(not(target_arch = "wasm32"))]
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

#[cfg(not(target_arch = "wasm32"))]
fn load_state(path: &Path) -> anyhow::Result<ClientState> {
    let bytes = fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
    serde_json::from_slice(&bytes).context("local state is invalid")
}

#[cfg(target_arch = "wasm32")]
fn load_state(path: &Path) -> anyhow::Result<ClientState> {
    let key = path.to_string_lossy().into_owned();
    let bytes = WEB_STATE
        .with(|states| states.borrow().get(&key).cloned())
        .with_context(|| format!("could not read {}", path.display()))?;
    serde_json::from_slice(&bytes).context("local state is invalid")
}

#[cfg(not(target_arch = "wasm32"))]
fn save_state(path: &Path, state: &ClientState) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    let temporary = temporary_path(path);
    fs::write(&temporary, serde_json::to_vec_pretty(state)?)
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

#[cfg(not(target_arch = "wasm32"))]
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

#[cfg(not(target_arch = "wasm32"))]
fn temporary_path(path: &Path) -> PathBuf {
    path.with_extension("tmp")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marker(created_at_millis: u64, event_id: &str) -> DirectMessageMarker {
        DirectMessageMarker {
            created_at_millis,
            event_id: event_id.to_owned(),
        }
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
}
