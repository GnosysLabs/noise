export type ProfileImage = {
  blob_id: string;
  key_base64: string;
  mime_type: string;
  byte_length: number;
  storage?: StorageManifest | null;
};

export type DirectMessagePolicy = "everyone" | "shared_groups" | "nobody";
export type GroupContentRating = "general" | "explicit";
export type SafetyReportCategory =
  | "group_rules"
  | "harassment_or_hateful_behavior"
  | "spam_scam_or_impersonation"
  | "threats_or_immediate_danger"
  | "sexual_exploitation_or_non_consensual_sexual_content"
  | "child_safety"
  | "explicit_content_not_properly_labeled"
  | "other";

export type ProfileAlbum = {
  blob_id: string;
  key_base64: string;
  byte_length: number;
  item_count: number;
  storage?: StorageManifest | null;
};

export type ProfileAlbumItem = {
  id: string;
  attachment: MediaAttachment;
  created_at_millis: number;
};

export type ProfileAlbumData = {
  scope_id: string;
  items: ProfileAlbumItem[];
};

export type IdentitySummary = {
  username: string;
  public_key: string;
  noise_id: string | null;
  bio: string;
  avatar: ProfileImage | null;
  album: ProfileAlbum | null;
  accepts_direct_messages: boolean;
  direct_message_policy: DirectMessagePolicy;
  safety_restriction?: SafetyRestrictionSummary | null;
};

export type SafetyRestrictionSummary = {
  expires_at_millis: number | null;
};

export type GroupSummary = {
  group_id: string;
  name: string;
  description: string;
  rules: string;
  content_rating: GroupContentRating;
  avatar: ProfileImage | null;
  background: ProfileImage | null;
  mobile_background: ProfileImage | null;
  accent_color: string;
  members_can_send_messages: boolean;
  members_can_send_media: boolean;
  frequency: string | null;
  owner_public_key: string;
  remote_deletion_supported: boolean;
  is_active: boolean;
  unread_count: number;
  read_state_initialized: boolean;
  safety_restriction?: SafetyRestrictionSummary | null;
};

export type AdultAccessSummary = {
  age_attested: boolean;
  explicit_content_enabled: boolean;
  hidden_explicit_group_count: number;
};

export type LocalSummary = {
  identity: IdentitySummary;
  adult_access: AdultAccessSummary;
  devices: DeviceSummary[];
  groups: GroupSummary[];
  directs: DirectSummary[];
  known_people: DirectSummary[];
  blocked_people: DirectSummary[];
  hidden_public_keys: string[];
};

export type DeviceSummary = {
  device_id: string;
  name: string;
  platform: string;
  created_at_millis: number;
  last_seen_at_millis: number;
  is_current: boolean;
};

export type DirectSummary = {
  public_key: string;
  username: string;
  bio: string;
  avatar: ProfileImage | null;
  album: ProfileAlbum | null;
  accepts_direct_messages: boolean;
  direct_message_policy: DirectMessagePolicy;
  is_active: boolean;
  has_unread: boolean;
};

export type MemberSummary = {
  public_key: string;
  username: string;
  bio: string;
  avatar: ProfileImage | null;
  album: ProfileAlbum | null;
  accepts_direct_messages: boolean;
  direct_message_policy: DirectMessagePolicy;
  is_moderator: boolean;
  moderator_permissions?: ModeratorPermissions | null;
};

export type ModeratorPermissions = {
  edit_group_identity: boolean;
  edit_group_appearance: boolean;
  edit_group_rules: boolean;
  edit_group_general_settings: boolean;
  create_topics: boolean;
  edit_topics: boolean;
  delete_topics: boolean;
  review_reports_and_remove_messages: boolean;
  ban_members: boolean;
  unban_members: boolean;
};

export type MediaChunk = {
  blob_id: string;
  key_base64: string;
  byte_length: number;
  storage?: StorageManifest | null;
};

export type StorageManifest = {
  v: number;
  o: string;
  l: number;
  z: number;
  k: number;
  n: number;
  p: ShardPlacement[];
};

export type ShardPlacement = {
  i: number;
  d: string;
  h: string;
  r: string;
};

export type MediaAttachment = {
  file_name: string;
  mime_type: string;
  byte_length: number;
  chunks: MediaChunk[];
  preview_data_base64?: string | null;
  preview_mime_type?: string | null;
  pixel_width?: number | null;
  pixel_height?: number | null;
  media_album_id?: string | null;
};

export type LinkPreview = {
  url: string;
  title: string;
  description: string | null;
  site_name: string | null;
  image_data_url: string | null;
};

export type MessageSummary = {
  event_id: string;
  message_id: string;
  author_public_key: string;
  username: string;
  bio: string;
  avatar: ProfileImage | null;
  album: ProfileAlbum | null;
  accepts_direct_messages: boolean;
  direct_message_policy: DirectMessagePolicy;
  text: string;
  attachment: MediaAttachment | null;
  reply_to_message_id: string | null;
  forwarded_from?: {
    public_key: string;
    username: string;
  } | null;
  topic_id?: string | null;
  created_at_millis: number;
  reactions?: ReactionSummary[];
  expires_after_read_seconds?: number | null;
  delivered_at_millis?: number | null;
  read_at_millis?: number | null;
  expires_at_millis?: number | null;
  optimistic?: boolean;
  upload_progress?: number;
  upload_error?: string;
  local_attachment?: {
    file_name?: string;
    preview_url: string;
    mime_type: string;
    poster_url?: string;
    pixel_width?: number;
    pixel_height?: number;
    media_album_id?: string | null;
  };
};

export type TopicSummary = {
  topic_id: string;
  name: string;
  icon: string;
  stream_locator: string;
  locked: boolean;
  archived: boolean;
  created_by_public_key: string;
  created_at_millis: number;
  unread_count: number;
  has_older_messages: boolean;
};

export type ReactionSummary = {
  emoji: string;
  count: number;
  reactor_public_keys: string[];
  reacted_by_self: boolean;
};

export type Conversation = {
  group: GroupSummary;
  topics: TopicSummary[];
  general_unread_count: number;
  members: MemberSummary[];
  banned_members: BannedMemberSummary[];
  messages: MessageSummary[];
  reports: ReportSummary[];
  reported_message_event_ids: string[];
  rejected_events: number;
  has_older_messages: boolean;
};

export type GroupActivityResult = {
  summary: LocalSummary;
  conversation: Conversation | null;
  // Set when the sync only hydrated an empty cache, so the real group activity
  // is one more round trip away. Absent on older cores.
  follow_up_recommended?: boolean;
};

export type GroupEncryptionStatus = {
  group_id: string;
  phase:
    | "active"
    | "waiting_for_founder"
    | "waiting_for_members"
    | "waiting_for_admission"
    | "waiting_for_device"
    | "removed";
  epoch: number | null;
  missing_member_public_keys: string[];
};

export type ReportSummary = {
  report_event_id: string;
  reporter_public_key: string;
  reporter_username: string;
  reporter_avatar: ProfileImage | null;
  reason: string;
  created_at_millis: number;
  message: MessageSummary;
};

export type BannedMemberSummary = {
  public_key: string;
  username: string;
  bio: string;
  avatar: ProfileImage | null;
};

export type DirectMessageSummary = {
  event_id: string;
  message_id: string;
  author_public_key: string;
  username: string;
  bio: string;
  avatar: ProfileImage | null;
  album: ProfileAlbum | null;
  accepts_direct_messages: boolean;
  direct_message_policy: DirectMessagePolicy;
  text: string;
  attachment: MediaAttachment | null;
  reply_to_message_id: string | null;
  forwarded_from?: {
    public_key: string;
    username: string;
  } | null;
  created_at_millis: number;
  expires_after_read_seconds?: number | null;
  delivered_at_millis?: number | null;
  read_at_millis?: number | null;
  expires_at_millis?: number | null;
  optimistic?: boolean;
  local_attachment?: {
    preview_url: string;
    mime_type: string;
    poster_url?: string;
    pixel_width?: number;
    pixel_height?: number;
    media_album_id?: string | null;
  };
};

export type SentMessageResult = {
  event_id: string;
  message_id: string;
  created_at_millis: number;
};

export type DirectConversation = {
  contact: DirectSummary;
  media_scope_id: string;
  messages: DirectMessageSummary[];
  disappearing_after_read_seconds?: number | null;
};

export type DirectInbox = {
  summary: LocalSummary;
  conversations: DirectConversation[];
};

export type SearchResults = {
  messages: SearchMessageResult[];
  locations: SearchLocationResult[];
  people: SearchPersonResult[];
  has_more_history: boolean;
  older_scopes: SearchHistoryScope[];
};

export type SearchHistoryScope = {
  group_id: string | null;
  topic_id: string | null;
};

export type SearchMessageResult = {
  event_id: string;
  author_public_key: string;
  username: string;
  avatar: ProfileImage | null;
  text: string;
  attachment: MediaAttachment | null;
  created_at_millis: number;
  group_id: string | null;
  group_name: string | null;
  topic_id: string | null;
  topic_name: string | null;
  direct_public_key: string | null;
};

export type SearchLocationResult = {
  group_id: string;
  group_name: string;
  group_avatar: ProfileImage | null;
  topic_id: string | null;
  topic_name: string | null;
  topic_icon: string | null;
};

export type SearchPersonResult = {
  public_key: string;
  username: string;
  bio: string;
  avatar: ProfileImage | null;
  album: ProfileAlbum | null;
  accepts_direct_messages: boolean;
  direct_message_policy: DirectMessagePolicy;
  has_direct: boolean;
};

export type GroupWatch = {
  revision: number;
  changed: boolean;
  deleted?: boolean;
  online_public_keys: string[];
  recently_active_public_keys: string[];
  changed_stream_locators?: string[];
  control_changed?: boolean;
  change_hints_complete?: boolean;
};

export type ReplyNotificationSummary = {
  event_id: string;
  group_id: string;
  group_name: string;
  username: string;
  text: string;
  attachment_mime_type: string | null;
  created_at_millis: number;
};

export type ReplyNotificationSnapshot = {
  group_id: string;
  replies: ReplyNotificationSummary[];
};

export type MakeResult = {
  group: GroupSummary;
  frequency: string;
  display_frequency: string;
};

export type AvatarData = {
  mime_type: string;
  data_base64: string;
};

export type AttachmentData = {
  mime_type: string;
  file_path: string;
};

export type AttachmentRangeData = {
  mime_type: string;
  data_base64: string;
  offset: number;
  byte_length: number;
  total_byte_length: number;
};

export type NoiseRequest = Record<string, unknown> & { action: string };
