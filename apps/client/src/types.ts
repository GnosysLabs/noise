export type ProfileImage = {
  blob_id: string;
  key_base64: string;
  mime_type: string;
  byte_length: number;
};

export type IdentitySummary = {
  username: string;
  public_key: string;
  noise_id: string | null;
  bio: string;
  avatar: ProfileImage | null;
  accepts_direct_messages: boolean;
};

export type GroupSummary = {
  group_id: string;
  name: string;
  description: string;
  rules: string;
  avatar: ProfileImage | null;
  background: ProfileImage | null;
  accent_color: string;
  members_can_send_messages: boolean;
  members_can_send_media: boolean;
  frequency: string | null;
  owner_public_key: string;
  remote_deletion_supported: boolean;
  is_active: boolean;
};

export type LocalSummary = {
  identity: IdentitySummary;
  groups: GroupSummary[];
  directs: DirectSummary[];
  known_people: DirectSummary[];
};

export type DirectSummary = {
  public_key: string;
  username: string;
  bio: string;
  avatar: ProfileImage | null;
  accepts_direct_messages: boolean;
  is_active: boolean;
  has_unread: boolean;
};

export type MemberSummary = {
  public_key: string;
  username: string;
  bio: string;
  avatar: ProfileImage | null;
  accepts_direct_messages: boolean;
  is_moderator: boolean;
};

export type MediaChunk = {
  blob_id: string;
  key_base64: string;
  byte_length: number;
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
};

export type MessageSummary = {
  event_id: string;
  message_id: string;
  author_public_key: string;
  username: string;
  bio: string;
  avatar: ProfileImage | null;
  accepts_direct_messages: boolean;
  text: string;
  attachment: MediaAttachment | null;
  reply_to_message_id: string | null;
  created_at_millis: number;
  reactions?: ReactionSummary[];
  optimistic?: boolean;
  local_attachment?: {
    preview_url: string;
    mime_type: string;
  };
};

export type ReactionSummary = {
  emoji: string;
  count: number;
  reactor_public_keys: string[];
  reacted_by_self: boolean;
};

export type Conversation = {
  group: GroupSummary;
  members: MemberSummary[];
  banned_members: BannedMemberSummary[];
  messages: MessageSummary[];
  reports: ReportSummary[];
  reported_message_event_ids: string[];
  rejected_events: number;
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
  accepts_direct_messages: boolean;
  text: string;
  attachment: MediaAttachment | null;
  reply_to_message_id: string | null;
  created_at_millis: number;
  optimistic?: boolean;
  local_attachment?: {
    preview_url: string;
    mime_type: string;
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
};

export type DirectInbox = {
  summary: LocalSummary;
  conversations: DirectConversation[];
};

export type GroupWatch = {
  revision: number;
  changed: boolean;
  online_public_keys: string[];
  recently_active_public_keys: string[];
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

export type NoiseRequest = Record<string, unknown> & { action: string };
