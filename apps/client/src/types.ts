export type ProfileImage = {
  blob_id: string;
  key_base64: string;
  mime_type: string;
  byte_length: number;
};

export type IdentitySummary = {
  username: string;
  public_key: string;
  bio: string;
  avatar: ProfileImage | null;
};

export type GroupSummary = {
  group_id: string;
  name: string;
  description: string;
  avatar: ProfileImage | null;
  owner_public_key: string;
  remote_deletion_supported: boolean;
  is_active: boolean;
};

export type LocalSummary = {
  identity: IdentitySummary;
  groups: GroupSummary[];
};

export type MemberSummary = {
  public_key: string;
  username: string;
  bio: string;
  avatar: ProfileImage | null;
};

export type MessageSummary = {
  event_id: string;
  author_public_key: string;
  username: string;
  bio: string;
  avatar: ProfileImage | null;
  text: string;
  created_at_millis: number;
};

export type Conversation = {
  group: GroupSummary;
  members: MemberSummary[];
  messages: MessageSummary[];
  rejected_events: number;
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

export type NoiseRequest = Record<string, unknown> & { action: string };
