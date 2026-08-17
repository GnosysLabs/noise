import { mentionedPublicKeys, prettyMentionText } from "./mentionSuggestions.ts";
import type {
  Conversation,
  MessageSummary,
  ProfileImage,
} from "./types";

export const ACTIVITY_INBOX_STORAGE_KEY = "noise.activity-inbox.v1";
const MAX_INBOX_ITEMS = 80;
const MAX_READ_IDS = 400;

export type ActivityNotificationKind = "mention" | "reply" | "reaction";

export type ActivityActor = {
  public_key: string;
  username: string;
  avatar: ProfileImage | null;
};

export type ActivityNotification = {
  id: string;
  kind: ActivityNotificationKind;
  eventId: string;
  createdAtMillis: number;
  actor: ActivityActor;
  preview: string;
  emoji?: string;
  groupId?: string;
  groupName?: string;
  topicId?: string | null;
  topicName?: string | null;
  directPublicKey?: string;
  directUsername?: string;
};

export type ActivityInboxState = {
  items: ActivityNotification[];
  readIds: string[];
  seenScopes: string[];
  baselineAt: number;
};

type InboxStorage = {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
};

export function emptyActivityInbox(): ActivityInboxState {
  return { items: [], readIds: [], seenScopes: [], baselineAt: Date.now() };
}

export function activityInboxStorageKey(identityPublicKey: string) {
  return `${ACTIVITY_INBOX_STORAGE_KEY}:${identityPublicKey}`;
}

export function messageMentionsIdentity(
  text: string,
  self: { public_key: string; username: string; noise_id?: string | null },
  members: Array<{ public_key: string; username: string }> = [],
) {
  const roster = members.length > 0 ? members : [self];
  if (mentionedPublicKeys(text, roster).has(self.public_key)) return true;
  const noiseId = self.noise_id?.trim();
  return Boolean(noiseId) && mentionPattern(noiseId!).test(text);
}

export function activityPreview(
  text: string,
  mimeType?: string | null,
  people: Array<{ public_key: string; username: string }> = [],
) {
  const trimmed = prettyMentionText(text.trim().replace(/\s+/g, " "), people);
  if (trimmed) return trimmed.length > 86 ? `${trimmed.slice(0, 85)}…` : trimmed;
  if (mimeType?.startsWith("image/")) return "sent a photo";
  if (mimeType?.startsWith("video/")) return "sent a video";
  if (mimeType?.startsWith("audio/")) return "sent audio";
  if (mimeType) return "sent an attachment";
  return "sent a message";
}

export function formatActivityTime(millis: number, now = Date.now()) {
  const delta = Math.max(0, now - millis);
  if (delta < 45_000) return "now";
  if (delta < 3_600_000) return `${Math.max(1, Math.round(delta / 60_000))}m`;
  if (delta < 86_400_000) return `${Math.max(1, Math.round(delta / 3_600_000))}h`;
  const date = new Date(millis);
  const today = new Date(now);
  const yesterday = new Date(today.getFullYear(), today.getMonth(), today.getDate() - 1);
  if (sameDay(date, yesterday)) return "yesterday";
  if (date.getFullYear() === today.getFullYear()) {
    return new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric" }).format(date);
  }
  return new Intl.DateTimeFormat(undefined, {
    month: "short",
    day: "numeric",
    year: "numeric",
  }).format(date);
}

export function unreadActivityCount(state: ActivityInboxState) {
  const read = new Set(state.readIds);
  return state.items.reduce((count, item) => count + (read.has(item.id) ? 0 : 1), 0);
}

export function notificationsFromGroupConversation(
  conversation: Conversation,
  self: { public_key: string; username: string; noise_id: string | null },
  hiddenPublicKeys: string[] = [],
) {
  const people = new Map<string, ActivityActor>();
  for (const member of conversation.members) {
    people.set(member.public_key, {
      public_key: member.public_key,
      username: member.username,
      avatar: member.avatar,
    });
  }
  const topics = new Map(conversation.topics.map((topic) => [topic.topic_id, topic.name]));
  return notificationsFromMessages(conversation.messages, self, hiddenPublicKeys, people, {
    groupId: conversation.group.group_id,
    groupName: conversation.group.name,
    topicName: (topicId) => topicId ? topics.get(topicId) ?? null : null,
  });
}

export function withoutDirectActivity(state: ActivityInboxState): ActivityInboxState {
  const items = state.items.filter((item) => !item.directPublicKey);
  if (items.length === state.items.length) return state;
  return { ...state, items };
}

export function notificationsFromMessages(
  messages: Array<Pick<MessageSummary, "event_id" | "message_id" | "author_public_key" | "username" | "avatar" | "text" | "attachment" | "reply_to_message_id" | "topic_id" | "created_at_millis" | "reactions" | "optimistic">>,
  self: { public_key: string; username: string; noise_id: string | null },
  hiddenPublicKeys: string[],
  people: Map<string, ActivityActor>,
  scope: {
    groupId?: string;
    groupName?: string;
    topicName?: (topicId: string | null | undefined) => string | null;
    directPublicKey?: string;
    directUsername?: string;
  },
) {
  const hidden = new Set(hiddenPublicKeys);
  const ownMessageIds = new Set(
    messages
      .filter((message) => message.author_public_key === self.public_key)
      .map((message) => message.message_id),
  );
  const items: ActivityNotification[] = [];
  const roster = [self, ...people.values()];
  for (const message of messages) {
    if (message.optimistic || hidden.has(message.author_public_key)) continue;
    const actor = people.get(message.author_public_key) ?? {
      public_key: message.author_public_key,
      username: message.username,
      avatar: message.avatar,
    };
    const location = {
      groupId: scope.groupId,
      groupName: scope.groupName,
      topicId: message.topic_id ?? null,
      topicName: scope.topicName?.(message.topic_id) ?? null,
      directPublicKey: scope.directPublicKey,
      directUsername: scope.directUsername,
    };
    if (message.author_public_key !== self.public_key) {
      if (messageMentionsIdentity(message.text, self, roster)) {
        items.push({
          id: `mention:${message.event_id}`,
          kind: "mention",
          eventId: message.event_id,
          createdAtMillis: message.created_at_millis,
          actor,
          preview: activityPreview(message.text, message.attachment?.mime_type, roster),
          ...location,
        });
      }
      if (
        message.reply_to_message_id
        && ownMessageIds.has(message.reply_to_message_id)
      ) {
        items.push({
          id: `reply:${message.event_id}`,
          kind: "reply",
          eventId: message.event_id,
          createdAtMillis: message.created_at_millis,
          actor,
          preview: activityPreview(message.text, message.attachment?.mime_type, roster),
          ...location,
        });
      }
    }
    if (message.author_public_key === self.public_key) {
      for (const reaction of message.reactions ?? []) {
        for (const reactorPublicKey of reaction.reactor_public_keys) {
          if (reactorPublicKey === self.public_key || hidden.has(reactorPublicKey)) continue;
          const reactor = people.get(reactorPublicKey) ?? {
            public_key: reactorPublicKey,
            username: "someone",
            avatar: null,
          };
          items.push({
            id: `reaction:${message.event_id}:${reaction.emoji}:${reactorPublicKey}`,
            kind: "reaction",
            eventId: message.event_id,
            createdAtMillis: message.created_at_millis,
            actor: reactor,
            preview: activityPreview(message.text, message.attachment?.mime_type, roster),
            emoji: reaction.emoji,
            ...location,
          });
        }
      }
    }
  }
  return items;
}

export function mergeActivityNotifications(
  state: ActivityInboxState,
  scopeId: string,
  incoming: ActivityNotification[],
) {
  state = withoutDirectActivity(state);
  incoming = incoming.filter((item) => !item.directPublicKey);
  const baselineAt = state.baselineAt ?? Date.now();
  if (incoming.length === 0) {
    return state.baselineAt === baselineAt ? state : { ...state, baselineAt };
  }
  const firstLook = !state.seenScopes.includes(scopeId);
  const itemIds = new Set(state.items.map((item) => item.id));
  const additions = incoming.filter((item) => !itemIds.has(item.id));
  const readIds = new Set(state.readIds);
  if (firstLook) {
    for (const item of incoming) {
      if (item.createdAtMillis <= baselineAt) readIds.add(item.id);
    }
  }
  if (
    additions.length === 0
    && !firstLook
    && readIds.size === state.readIds.length
    && state.baselineAt === baselineAt
  ) {
    return state;
  }

  const items = additions.length === 0
    ? state.items
    : [...additions, ...state.items]
      .sort((left, right) => right.createdAtMillis - left.createdAtMillis || left.id.localeCompare(right.id))
      .slice(0, MAX_INBOX_ITEMS);
  return {
    items,
    readIds: capReadIds([...readIds]),
    seenScopes: firstLook ? [...state.seenScopes, scopeId] : state.seenScopes,
    baselineAt,
  };
}

export function markActivityInboxRead(state: ActivityInboxState) {
  const readIds = capReadIds([...new Set([...state.readIds, ...state.items.map((item) => item.id)])]);
  if (readIds.length === state.readIds.length) return state;
  return { ...state, readIds };
}

function capReadIds(ids: string[]) {
  return ids.length <= MAX_READ_IDS ? ids : ids.slice(ids.length - MAX_READ_IDS);
}

export function loadActivityInbox(
  identityPublicKey: string,
  storage?: InboxStorage | null,
) {
  const raw = readStorage(activityInboxStorageKey(identityPublicKey), storage);
  if (!raw) return emptyActivityInbox();
  try {
    const parsed = JSON.parse(raw) as Partial<ActivityInboxState>;
    if (!Array.isArray(parsed.items) || !Array.isArray(parsed.readIds) || !Array.isArray(parsed.seenScopes)) {
      return emptyActivityInbox();
    }
    const baselineAt = typeof parsed.baselineAt === "number" ? parsed.baselineAt : Date.now();
    const state = withoutDirectActivity({
      items: parsed.items.filter(isStoredNotification),
      readIds: parsed.readIds.filter((id): id is string => typeof id === "string"),
      seenScopes: parsed.seenScopes.filter((id): id is string => typeof id === "string"),
      baselineAt,
    });
    const hasBaseline = typeof parsed.baselineAt === "number";
    if (!hasBaseline || state.items.length !== parsed.items.length) {
      saveActivityInbox(identityPublicKey, state, storage);
    }
    return state;
  } catch {
    return emptyActivityInbox();
  }
}

export function saveActivityInbox(
  identityPublicKey: string,
  state: ActivityInboxState,
  storage?: InboxStorage | null,
) {
  const target = storage ?? defaultStorage();
  if (!target) return;
  try {
    target.setItem(activityInboxStorageKey(identityPublicKey), JSON.stringify(state));
  } catch {
    // The in-memory inbox still works for this session.
  }
}

function mentionPattern(name: string) {
  const escaped = name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return new RegExp(`(^|[^\\w@])@${escaped}(?=$|[^\\w#])`, "i");
}

function sameDay(left: Date, right: Date) {
  return left.getFullYear() === right.getFullYear()
    && left.getMonth() === right.getMonth()
    && left.getDate() === right.getDate();
}

function isStoredNotification(value: unknown): value is ActivityNotification {
  if (!value || typeof value !== "object") return false;
  const item = value as ActivityNotification;
  return typeof item.id === "string"
    && (item.kind === "mention" || item.kind === "reply" || item.kind === "reaction")
    && typeof item.eventId === "string"
    && typeof item.createdAtMillis === "number"
    && typeof item.actor?.public_key === "string"
    && typeof item.actor.username === "string"
    && typeof item.preview === "string";
}

function readStorage(key: string, storage?: InboxStorage | null) {
  const target = storage ?? defaultStorage();
  if (!target) return null;
  try {
    return target.getItem(key);
  } catch {
    return null;
  }
}

function defaultStorage(): InboxStorage | null {
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}
