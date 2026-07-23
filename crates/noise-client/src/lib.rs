use std::{
    collections::HashMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use noise_core::{
    AccountCredentials, AccountVault, EncryptedBlob, GroupDeletion, GroupEventPayload,
    GroupMembership, GroupProfile, GroupState, Identity, InviteRecord, InviteRotation, Profile,
    SignedEvent, derive_account_credentials, direct_mailbox_id, direct_message_id,
    display_frequency, display_noise_id, frequency_locator, generate_frequency, generate_noise_id,
    normalize_frequency,
};
pub use noise_core::{MediaAttachment, MediaChunk, ProfileImage};
use noise_transport::{
    GATEWAY_HEADER, OHTTP_RELAY_PATH, OHTTP_REQUEST_MEDIA_TYPE, OHTTP_RESPONSE_MEDIA_TYPE,
    PlainResponse, RelayDescriptor, decode_response, encode_request,
};
use ohttp::ClientRequest;
use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub struct NoiseClient {
    http: reqwest::Client,
}

impl Default for NoiseClient {
    fn default() -> Self {
        Self {
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(std::time::Duration::from_secs(40))
                .build()
                .expect("Noise HTTP configuration is valid"),
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

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct GroupWatch {
    pub revision: u64,
    pub changed: bool,
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
    direct_read_through: HashMap<String, DirectMessageMarker>,
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
struct DirectMessageMarker {
    created_at_millis: u64,
    event_id: String,
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
        let sequence = self.next_author_sequence;
        self.next_author_sequence += 1;
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
            group_frequencies: self.group_frequencies.clone(),
            next_author_sequence: self.next_author_sequence,
        }
    }

    fn from_vault(contents: AccountVaultContents, account: AccountSession) -> anyhow::Result<Self> {
        if contents.version != 1 {
            bail!("this account vault was created by an unsupported Noise version")
        }
        let state = Self {
            version: 3,
            profile: contents.profile,
            identity_secret_base64: contents.identity_secret_base64,
            groups: contents.groups,
            active_group_id: contents.active_group_id,
            direct_contacts: contents.direct_contacts,
            known_people: contents.known_people,
            active_direct_public_key: contents.active_direct_public_key,
            direct_deleted_before: contents.direct_deleted_before,
            direct_closed_periods: contents.direct_closed_periods,
            direct_latest_incoming: HashMap::new(),
            direct_read_through: HashMap::new(),
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
            groups: self
                .groups
                .iter()
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
                })
                .collect(),
            directs: self
                .direct_contacts
                .iter()
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
                .collect(),
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
        if path.exists() {
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
            self.publish_blob(&relays, &blob).await?;
            Some(ProfileImage {
                blob_id: blob.blob_id,
                key_base64,
                mime_type,
                byte_length: data.len() as u32,
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
            groups: Vec::new(),
            active_group_id: None,
            direct_contacts: Vec::new(),
            known_people: Vec::new(),
            active_direct_public_key: None,
            direct_deleted_before: HashMap::new(),
            direct_closed_periods: Vec::new(),
            direct_latest_incoming: HashMap::new(),
            direct_read_through: HashMap::new(),
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
        if path.exists() {
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
        if path.exists() {
            fs::remove_file(path)
                .with_context(|| format!("could not erase local identity {}", path.display()))?;
        }
        Ok(())
    }

    pub fn local_summary(&self, path: impl AsRef<Path>) -> anyhow::Result<LocalSummary> {
        load_state(path.as_ref())?.summary()
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
        self.watch_id(&group.group_id, since, relay_list(relays)?)
            .await
    }

    async fn watch_id(
        &self,
        id: &str,
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
            if response.status == 410 {
                bail!("group has been deleted")
            }
            if (200..300).contains(&response.status)
                && let Ok(change) = serde_json::from_slice::<GroupWatch>(&response.body)
            {
                return Ok(change);
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
            self.publish_blob(&relays, &blob).await?;
            Some(ProfileImage {
                blob_id: blob.blob_id,
                key_base64,
                mime_type,
                byte_length: data.len() as u32,
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
            let event = SignedEvent::profile_updated(&identity, &group, &state.profile, sequence)?;
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
        let view = GroupState::rebuild(&current_group, &events);
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
            self.publish_blob(&relays, &blob).await?;
            Some(ProfileImage {
                blob_id: blob.blob_id,
                key_base64,
                mime_type,
                byte_length: data.len() as u32,
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
            self.publish_blob(&relays, &blob).await?;
            Some(ProfileImage {
                blob_id: blob.blob_id,
                key_base64,
                mime_type,
                byte_length: data.len() as u32,
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
        let event = SignedEvent::group_profile_updated(
            &identity,
            &group,
            &GroupProfile {
                name,
                description,
                rules,
                avatar,
                background,
                accent_color,
                members_can_send_messages,
                members_can_send_media,
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
        if image.byte_length == 0 || image.byte_length > 1536 * 1024 {
            bail!("image reference has an invalid size")
        }
        if image.blob_id.len() != 64 || !image.blob_id.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            bail!("image reference has an invalid blob identifier")
        }
        let cache_directory = cache_path.as_ref().join("profile-blobs");
        let file_path = cache_directory.join(format!("{}.json", image.blob_id));
        let cached_blob = fs::read(&file_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<EncryptedBlob>(&bytes).ok())
            .filter(|blob| blob.blob_id == image.blob_id && blob.verify().is_ok());
        let blob = if let Some(blob) = cached_blob {
            blob
        } else {
            let blob = self
                .fetch_blob(&relay_list(relays)?, &image.blob_id)
                .await?;
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
        let relays = relay_list(relays)?;
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
            let blob = self.fetch_blob(&relays, &chunk.blob_id).await?;
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
        self.publish_blob(&relay_list(relays)?, &blob).await?;
        Ok(MediaChunk {
            blob_id: blob.blob_id,
            key_base64,
            byte_length: data.len() as u32,
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
            self.publish_blob(&relays, &blob).await?;
            group.avatar = Some(ProfileImage {
                blob_id: blob.blob_id,
                key_base64,
                mime_type,
                byte_length: data.len() as u32,
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
        let sequence = state.take_sequence();
        let group = state.active_group()?.clone();
        let joined =
            SignedEvent::member_joined(&state.identity()?, &group, &state.profile, sequence)?;
        self.publish_event(&relays, &joined).await?;
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

    pub async fn direct_conversation(
        &self,
        path: impl AsRef<Path>,
        cache_path: impl AsRef<Path>,
        relays: Vec<String>,
    ) -> anyhow::Result<DirectConversation> {
        let path = path.as_ref();
        let (mut state, messages) = self
            .sync_direct_inbox(path, cache_path.as_ref(), relay_list(relays)?)
            .await?;
        let public_key = state
            .active_direct_public_key
            .as_deref()
            .context("choose a direct conversation first")?;
        let contact = state
            .direct_contacts
            .iter()
            .find(|contact| contact.public_key == public_key)
            .cloned()
            .context("active direct conversation is missing")?;
        if let Some(latest) = state.direct_latest_incoming.get(public_key).cloned()
            && state.direct_read_through.get(public_key) != Some(&latest)
        {
            state
                .direct_read_through
                .insert(public_key.to_owned(), latest);
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
        let mailbox_id = direct_mailbox_id(&state.identity()?.public_key_base64())?;
        self.watch_id(&mailbox_id, since, relay_list(relays)?).await
    }

    pub async fn say_direct(
        &self,
        path: impl AsRef<Path>,
        text: impl Into<String>,
        attachment: Option<MediaAttachment>,
        reply_to_message_id: Option<String>,
        relays: Vec<String>,
    ) -> anyhow::Result<()> {
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
        let relays = relay_list(relays)?;
        self.publish_event(&relays, &recipient_event).await?;
        self.publish_event(&relays, &sender_event).await?;
        save_state(path, &state)?;
        Ok(())
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
        self.publish_blob(&relay_list(relays)?, &blob).await?;
        Ok(MediaChunk {
            blob_id: blob.blob_id,
            key_base64,
            byte_length: data.len() as u32,
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
            self.publish_event(&relays, &recipient_event).await?;
            self.publish_event(&relays, &sender_event).await?;
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
    ) -> anyhow::Result<()> {
        self.send_message(path.as_ref(), text.into(), None, None, relays)
            .await
    }

    pub async fn say_reply(
        &self,
        path: impl AsRef<Path>,
        text: impl Into<String>,
        reply_to_message_id: Option<String>,
        relays: Vec<String>,
    ) -> anyhow::Result<()> {
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
    ) -> anyhow::Result<()> {
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
    ) -> anyhow::Result<()> {
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
    ) -> anyhow::Result<()> {
        if text.trim().is_empty() && attachment.is_none() {
            bail!("message cannot be empty")
        }
        validate_reply_reference(reply_to_message_id.as_deref())?;
        let relays = relay_list(relays)?;
        let mut state = load_state(path)?;
        let group = state.active_group()?.clone();
        let sequence = state.take_sequence();
        let event = if let Some(attachment) = attachment {
            SignedEvent::chat_with_attachment_reply(
                &state.identity()?,
                &group,
                text,
                attachment,
                reply_to_message_id,
                sequence,
            )?
        } else {
            SignedEvent::chat_reply(
                &state.identity()?,
                &group,
                text,
                reply_to_message_id,
                sequence,
            )?
        };
        self.publish_event(&relays, &event).await?;
        save_state(path, &state)?;
        Ok(())
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
        let view = GroupState::rebuild(&group, &self.fetch_events(&group, relays.clone()).await?);
        if view.owner_public_key.as_deref() != Some(actor_public_key.as_str()) {
            bail!("only the group founder can designate moderators")
        }
        if member_public_key == actor_public_key || !view.members.contains_key(member_public_key) {
            bail!("choose an active group member")
        }
        let sequence = state.take_sequence();
        let event =
            SignedEvent::moderator_set(&identity, &group, member_public_key, enabled, sequence)?;
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
        let view = GroupState::rebuild(&group, &self.fetch_events(&group, relays.clone()).await?);
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
        let sequence = state.take_sequence();
        let event = SignedEvent::message_deleted(&identity, &group, message_event_id, sequence)?;
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
        let view = GroupState::rebuild(&group, &self.fetch_events(&group, relays.clone()).await?);
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
        let event =
            SignedEvent::message_reported(&identity, &group, message_event_id, reason, sequence)?;
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
        let view = GroupState::rebuild(&group, &self.fetch_events(&group, relays.clone()).await?);
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
        let event = SignedEvent::report_resolved(&identity, &group, report_event_id, sequence)?;
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
        let view = GroupState::rebuild(&group, &self.fetch_events(&group, relays.clone()).await?);
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
        let sequence = state.take_sequence();
        let event = SignedEvent::member_banned(
            &identity,
            &group,
            member_public_key,
            delete_messages,
            sequence,
        )?;
        self.publish_event(&relays, &event).await?;
        save_state(path, &state)?;
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
        let view = GroupState::rebuild(&group, &self.fetch_events(&group, relays.clone()).await?);
        if view.owner_public_key.as_deref() != Some(actor_public_key.as_str())
            || !view.members.contains_key(&actor_public_key)
        {
            bail!("only the group founder can unban members")
        }
        if !view.banned_members.contains(member_public_key) {
            bail!("that identity is not banned")
        }
        let sequence = state.take_sequence();
        let event = SignedEvent::member_unbanned(&identity, &group, member_public_key, sequence)?;
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
        let sequence = state.take_sequence();
        let group = state.active_group()?.clone();
        if group.owner_public_key == state.identity()?.public_key_base64() {
            bail!("the founder must delete the group instead of leaving it")
        }
        let event = SignedEvent::member_left(&state.identity()?, &group, sequence)?;
        self.publish_event(&relay_list(relays)?, &event).await?;
        purge_group_cache(cache_path.as_ref(), &group.group_id)?;
        purge_profile_image_cache(cache_path.as_ref())?;
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
            self.publish_group_deletion(&relays, &deletion).await?;
        }

        purge_group_cache(cache_path.as_ref(), group_id)?;
        purge_profile_image_cache(cache_path.as_ref())?;
        state.groups.remove(group_index);
        state.group_frequencies.remove(group_id);
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
                self.publish_group_deletion(&relays, &deletion).await?;
                continue;
            }
            if delete_group_messages {
                let sequence = state.take_sequence();
                let event = SignedEvent::own_messages_deleted(&identity, &group, sequence)?;
                self.publish_event(&relays, &event).await?;
            }
            let sequence = state.take_sequence();
            let event = SignedEvent::member_left(&identity, &group, sequence)?;
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
                self.publish_event(&relays, &recipient_event).await?;
                self.publish_event(&relays, &sender_event).await?;
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
        fs::remove_file(path)
            .with_context(|| format!("could not erase local identity {}", path.display()))?;
        Ok(())
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
        let founder_join_exists = events.iter().any(|event| {
            event.author_public_key == identity_public_key
                && matches!(
                    event.decrypt(&group),
                    Ok(GroupEventPayload::MemberJoined { .. })
                )
        });
        if group.owner_public_key == identity_public_key && !founder_join_exists {
            let sequence = state.take_sequence();
            let joined = SignedEvent::member_joined(&identity, &group, &state.profile, sequence)?;
            self.publish_event(&relays, &joined).await?;
            events.push(joined);
            save_state(path, &state)?;
        }
        let view = GroupState::rebuild(&group, &events);
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
            save_state(path, &state)?;
        }
        if state.known_people != known_people_before {
            save_state(path, &state)?;
        }
        let can_view_reports =
            resolved_owner == identity_public_key || moderators.contains(&identity_public_key);
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
            },
            members,
            banned_members,
            messages: view
                .messages
                .into_iter()
                .map(|message| MessageSummary {
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
            if message.message.author_public_key != self_public_key {
                let marker = DirectMessageMarker {
                    created_at_millis: message.message.created_at_millis,
                    event_id: message.message.event_id.clone(),
                };
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

    async fn publish_account_state(
        &self,
        state: &mut ClientState,
        relays: &[RelayDescriptor],
    ) -> anyhow::Result<()> {
        let credentials = state.account_credentials()?;
        let identity = state.identity()?;
        let plaintext = serde_json::to_vec(&state.vault_contents())?;
        let mut revision = state
            .account
            .as_ref()
            .context("this identity has no Noise ID")?
            .revision
            .checked_add(1)
            .context("account vault revision is exhausted")?;
        let mut vault = AccountVault::seal(&identity, &credentials, revision, &plaintext)?;
        if let Err(first_error) = self.publish_account_vault(relays, &vault).await {
            let Ok(remote) = self.fetch_account_vault(relays, &credentials.locator).await else {
                return Err(first_error);
            };
            if remote.identity_public_key != identity.public_key_base64()
                || remote.revision < revision
            {
                return Err(first_error);
            }
            revision = remote
                .revision
                .checked_add(1)
                .context("account vault revision is exhausted")?;
            vault = AccountVault::seal(&identity, &credentials, revision, &plaintext)?;
            self.publish_account_vault(relays, &vault).await?;
        }
        let remote = self
            .fetch_account_vault(relays, &credentials.locator)
            .await
            .context("no relay accepted the encrypted account vault")?;
        if remote.revision < revision || remote.identity_public_key != identity.public_key_base64()
        {
            bail!("no relay accepted the encrypted account vault")
        }
        revision = revision.max(remote.revision);
        state
            .account
            .as_mut()
            .context("this identity has no Noise ID")?
            .revision = revision;
        Ok(())
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
        let mut accepted = 0usize;
        for index in 0..relays.len() {
            if let Ok(response) = self
                .relay_request(relays, index, "POST", "/v1/events", &body)
                .await
                && (200..300).contains(&response.status)
            {
                accepted += 1;
            }
        }
        if accepted == 0 {
            bail!("no relay accepted the event")
        }
        Ok(())
    }

    async fn publish_blob(
        &self,
        relays: &[RelayDescriptor],
        blob: &EncryptedBlob,
    ) -> anyhow::Result<()> {
        let body = serde_json::to_vec(blob)?;
        let mut accepted = 0usize;
        for index in 0..relays.len() {
            if let Ok(response) = self
                .relay_request(relays, index, "POST", "/v1/blobs", &body)
                .await
                && (200..300).contains(&response.status)
            {
                accepted += 1;
            }
        }
        if accepted == 0 {
            bail!("no relay accepted the encrypted media")
        }
        Ok(())
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

    async fn fetch_blob(
        &self,
        relays: &[RelayDescriptor],
        blob_id: &str,
    ) -> anyhow::Result<EncryptedBlob> {
        for index in 0..relays.len() {
            let Ok(response) = self
                .relay_request(relays, index, "GET", &format!("/v1/blobs/{blob_id}"), &[])
                .await
            else {
                continue;
            };
            if (200..300).contains(&response.status)
                && let Ok(blob) = serde_json::from_slice::<EncryptedBlob>(&response.body)
                && blob.verify().is_ok()
            {
                return Ok(blob);
            }
        }
        bail!("encrypted media is not available from any relay")
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
        let mut merged = HashMap::<String, SignedEvent>::new();
        let mut reachable = 0usize;
        for index in 0..relays.len() {
            let Ok(response) = self
                .relay_request(
                    &relays,
                    index,
                    "GET",
                    &format!("/v1/groups/{id}/events"),
                    &[],
                )
                .await
            else {
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
        let mask = (1..relays.len())
            .map(|offset| &relays[(storage_index + offset) % relays.len()])
            .find(|candidate| candidate.base_url != storage.base_url);
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
        let response = self
            .http
            .post(endpoint)
            .header(reqwest::header::CONTENT_TYPE, OHTTP_REQUEST_MEDIA_TYPE)
            .header(GATEWAY_HEADER, &storage.base_url)
            .body(encrypted_request)
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
        let response = request.send().await?;
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
    let lowered = password.to_lowercase();
    if ["password", "qwerty", "letmein", "123456"]
        .iter()
        .any(|weak| lowered.contains(weak))
    {
        bail!("choose a less predictable password")
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

fn purge_profile_image_cache(cache_path: &Path) -> anyhow::Result<()> {
    let directory = cache_path.join("profile-blobs");
    if directory.exists() {
        fs::remove_dir_all(&directory)
            .with_context(|| format!("could not erase {}", directory.display()))?;
    }
    Ok(())
}

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

fn current_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock is before Unix epoch")
        .as_millis() as u64
}

fn load_state(path: &Path) -> anyhow::Result<ClientState> {
    let bytes = fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
    serde_json::from_slice(&bytes).context("local state is invalid")
}

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

fn temporary_path(path: &Path) -> PathBuf {
    path.with_extension("tmp")
}
