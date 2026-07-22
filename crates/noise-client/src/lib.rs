use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
pub use noise_core::ProfileImage;
use noise_core::{
    EncryptedBlob, GroupDeletion, GroupEventPayload, GroupMembership, GroupProfile, GroupState,
    Identity, InviteRecord, Profile, SignedEvent, display_frequency, frequency_locator,
    generate_frequency, normalize_frequency,
};
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
                .timeout(std::time::Duration::from_secs(20))
                .build()
                .expect("Noise HTTP configuration is valid"),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IdentitySummary {
    pub username: String,
    pub public_key: String,
    pub bio: String,
    pub avatar: Option<ProfileImage>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GroupSummary {
    pub group_id: String,
    pub name: String,
    pub description: String,
    pub avatar: Option<ProfileImage>,
    pub owner_public_key: String,
    pub remote_deletion_supported: bool,
    pub is_active: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LocalSummary {
    pub identity: IdentitySummary,
    pub groups: Vec<GroupSummary>,
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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MessageSummary {
    pub event_id: String,
    pub author_public_key: String,
    pub username: String,
    pub bio: String,
    pub avatar: Option<ProfileImage>,
    pub text: String,
    pub created_at_millis: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Conversation {
    pub group: GroupSummary,
    pub members: Vec<MemberSummary>,
    pub messages: Vec<MessageSummary>,
    pub rejected_events: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AvatarData {
    pub mime_type: String,
    pub data_base64: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ClientState {
    version: u32,
    profile: Profile,
    identity_secret_base64: String,
    groups: Vec<GroupMembership>,
    active_group_id: Option<String>,
    #[serde(default)]
    next_author_sequence: u64,
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

    fn summary(&self) -> anyhow::Result<LocalSummary> {
        let public_key = self.identity()?.public_key_base64();
        Ok(LocalSummary {
            identity: IdentitySummary {
                username: self.profile.username.clone(),
                public_key,
                bio: self.profile.bio.clone(),
                avatar: self.profile.avatar.clone(),
            },
            groups: self
                .groups
                .iter()
                .map(|group| GroupSummary {
                    group_id: group.group_id.clone(),
                    name: group.name.clone(),
                    description: group.description.clone(),
                    avatar: group.avatar.clone(),
                    owner_public_key: group.owner_public_key.clone(),
                    remote_deletion_supported: !group.authority_nonce_base64.is_empty(),
                    is_active: self.active_group_id.as_deref() == Some(&group.group_id),
                })
                .collect(),
        })
    }
}

impl NoiseClient {
    pub fn initialize(
        &self,
        path: impl AsRef<Path>,
        username: impl Into<String>,
    ) -> anyhow::Result<LocalSummary> {
        let path = path.as_ref();
        if path.exists() {
            bail!("{} already exists", path.display());
        }
        let username = username.into();
        validate_username(&username)?;
        let identity = Identity::generate();
        let state = ClientState {
            version: 2,
            profile: Profile {
                username,
                bio: String::new(),
                avatar: None,
            },
            identity_secret_base64: identity.secret_base64(),
            groups: Vec::new(),
            active_group_id: None,
            next_author_sequence: 0,
        };
        save_state(path, &state)?;
        state.summary()
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

    pub async fn update_profile(
        &self,
        path: impl AsRef<Path>,
        bio: impl Into<String>,
        avatar_data_base64: Option<String>,
        avatar_mime_type: Option<String>,
        remove_avatar: bool,
        relays: Vec<String>,
    ) -> anyhow::Result<LocalSummary> {
        let path = path.as_ref();
        let mut state = load_state(path)?;
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

        state.profile.bio = bio;
        state.profile.avatar = avatar;
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
        avatar_data_base64: Option<String>,
        avatar_mime_type: Option<String>,
        remove_avatar: bool,
        relays: Vec<String>,
    ) -> anyhow::Result<LocalSummary> {
        let path = path.as_ref();
        let mut state = load_state(path)?;
        let name = name.into().trim().to_owned();
        let description = description.into().trim().to_owned();
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

        state.groups[group_index].name = name.clone();
        state.groups[group_index].description = description.clone();
        state.groups[group_index].avatar = avatar.clone();
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
                avatar,
            },
            sequence,
        )?;
        self.publish_event(&relays, &event).await?;
        save_state(path, &state)?;
        state.summary()
    }

    pub async fn fetch_avatar(
        &self,
        image: &ProfileImage,
        relays: Vec<String>,
    ) -> anyhow::Result<AvatarData> {
        if image.byte_length == 0 || image.byte_length > 256 * 1024 {
            bail!("avatar reference has an invalid size")
        }
        let blob = self
            .fetch_blob(&relay_list(relays)?, &image.blob_id)
            .await?;
        let data = blob.open(&image.key_base64)?;
        if data.len() != image.byte_length as usize {
            bail!("avatar data does not match its profile reference")
        }
        Ok(AvatarData {
            mime_type: image.mime_type.clone(),
            data_base64: STANDARD.encode(data),
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
                avatar: group.avatar,
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
                avatar: group.avatar,
                owner_public_key: group.owner_public_key,
                remote_deletion_supported: !group.authority_nonce_base64.is_empty(),
                is_active: true,
            },
        })
    }

    pub async fn say(
        &self,
        path: impl AsRef<Path>,
        text: impl Into<String>,
        relays: Vec<String>,
    ) -> anyhow::Result<()> {
        let path = path.as_ref();
        let text = text.into();
        if text.is_empty() {
            bail!("message cannot be empty")
        }
        let mut state = load_state(path)?;
        let sequence = state.take_sequence();
        let group = state.active_group()?.clone();
        let event = SignedEvent::chat(&state.identity()?, &group, text, sequence)?;
        self.publish_event(&relay_list(relays)?, &event).await?;
        save_state(path, &state)?;
        Ok(())
    }

    pub async fn leave(&self, path: impl AsRef<Path>, relays: Vec<String>) -> anyhow::Result<()> {
        let path = path.as_ref();
        let mut state = load_state(path)?;
        let sequence = state.take_sequence();
        let group = state.active_group()?.clone();
        let event = SignedEvent::member_left(&state.identity()?, &group, sequence)?;
        self.publish_event(&relay_list(relays)?, &event).await?;
        save_state(path, &state)?;
        Ok(())
    }

    pub async fn delete_group(
        &self,
        path: impl AsRef<Path>,
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

        state.groups.remove(group_index);
        if state.active_group_id.as_deref() == Some(group_id) {
            state.active_group_id = state.groups.first().map(|group| group.group_id.clone());
        }
        save_state(path, &state)?;
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
        if state.groups[group_index].name != resolved_profile.name
            || state.groups[group_index].description != resolved_profile.description
            || state.groups[group_index].avatar != resolved_profile.avatar
            || state.groups[group_index].owner_public_key != resolved_owner
        {
            state.groups[group_index].name = resolved_profile.name.clone();
            state.groups[group_index].description = resolved_profile.description.clone();
            state.groups[group_index].avatar = resolved_profile.avatar.clone();
            state.groups[group_index].owner_public_key = resolved_owner.clone();
            save_state(path, &state)?;
        }
        let mut members = view
            .members
            .into_values()
            .map(|member| MemberSummary {
                public_key: member.public_key,
                username: member.username,
                bio: member.bio,
                avatar: member.avatar,
            })
            .collect::<Vec<_>>();
        members.sort_by(|left, right| left.username.cmp(&right.username));
        Ok(Conversation {
            group: GroupSummary {
                group_id: group.group_id.clone(),
                name: resolved_profile.name,
                description: resolved_profile.description,
                avatar: resolved_profile.avatar,
                owner_public_key: resolved_owner,
                remote_deletion_supported: !group.authority_nonce_base64.is_empty(),
                is_active: true,
            },
            members,
            messages: view
                .messages
                .into_iter()
                .map(|message| MessageSummary {
                    event_id: message.event_id,
                    author_public_key: message.author_public_key,
                    username: message.username,
                    bio: message.bio,
                    avatar: message.avatar,
                    text: message.text,
                    created_at_millis: message.created_at_millis,
                })
                .collect(),
            rejected_events: view.rejected_events,
        })
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
            bail!("no relay accepted the encrypted avatar")
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
        bail!("avatar is not available from any relay")
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
        let mut merged = HashMap::<String, SignedEvent>::new();
        let mut reachable = 0usize;
        for index in 0..relays.len() {
            let Ok(response) = self
                .relay_request(
                    &relays,
                    index,
                    "GET",
                    &format!("/v1/groups/{}/events", group.group_id),
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
    if username.is_empty() || username.chars().count() > 32 {
        bail!("usernames must contain between 1 and 32 characters")
    }
    if username.chars().any(char::is_whitespace) {
        bail!("usernames cannot contain whitespace")
    }
    Ok(())
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
