export const CONVERSATION_SCROLL_STORAGE_KEY = "noise.conversation-scroll.v1";
const MAX_STORED_CONVERSATIONS = 200;

export type ConversationScrollAnchor = {
  stuckAtBottom: boolean;
  trackedMessageId: string;
  pixelOffset: number;
  lastSeenNewestId: string;
};

export type ConversationScrollRestore =
  | { mode: "bottom" }
  | {
    mode: "anchor";
    trackedMessageId: string;
    pixelOffset: number;
    lastSeenNewestId: string;
    pinLastSeenToBottom: boolean;
  };

type StoredConversationScroll = ConversationScrollAnchor & { updatedAt: number };

type ScrollStorage = {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
};

const memory = new Map<string, StoredConversationScroll>();
let memoryHydrated = false;

export function conversationScrollStorageKey(
  identityPublicKey: string,
  conversationKey: string,
) {
  return `${identityPublicKey}:${conversationKey}`;
}

export function countMessagesAfter(eventIds: string[], lastSeenNewestId: string) {
  const index = eventIds.lastIndexOf(lastSeenNewestId);
  if (index < 0) return 0;
  return eventIds.length - index - 1;
}

export function formatNewMessagesLabel(count: number) {
  if (count <= 0) return "new messages";
  if (count > 99) return "99+ new messages";
  return `${count} new message${count === 1 ? "" : "s"}`;
}

export function resolveConversationRestore(
  eventIds: string[],
  stored: ConversationScrollAnchor | null,
  preferredMessageId?: string | null,
  unreadHint = 0,
): ConversationScrollRestore {
  if (preferredMessageId && eventIds.includes(preferredMessageId)) {
    return {
      mode: "anchor",
      trackedMessageId: preferredMessageId,
      pixelOffset: 0,
      lastSeenNewestId: stored?.lastSeenNewestId ?? preferredMessageId,
      pinLastSeenToBottom: false,
    };
  }
  if (!stored || eventIds.length === 0) return { mode: "bottom" };

  const trackedExists = eventIds.includes(stored.trackedMessageId);
  const lastSeenExists = eventIds.includes(stored.lastSeenNewestId);
  const newerCount = lastSeenExists
    ? countMessagesAfter(eventIds, stored.lastSeenNewestId)
    : 0;

  if (stored.stuckAtBottom) {
    if (lastSeenExists && newerCount > 0) {
      return {
        mode: "anchor",
        trackedMessageId: stored.lastSeenNewestId,
        pixelOffset: stored.pixelOffset,
        lastSeenNewestId: stored.lastSeenNewestId,
        pinLastSeenToBottom: true,
      };
    }
    if (!lastSeenExists && unreadHint > 0) {
      return {
        mode: "anchor",
        trackedMessageId: eventIds[0],
        pixelOffset: 0,
        lastSeenNewestId: stored.lastSeenNewestId,
        pinLastSeenToBottom: false,
      };
    }
    return { mode: "bottom" };
  }

  if (trackedExists) {
    return {
      mode: "anchor",
      trackedMessageId: stored.trackedMessageId,
      pixelOffset: stored.pixelOffset,
      lastSeenNewestId: lastSeenExists ? stored.lastSeenNewestId : stored.trackedMessageId,
      pinLastSeenToBottom: false,
    };
  }

  if (lastSeenExists && newerCount > 0) {
    return {
      mode: "anchor",
      trackedMessageId: stored.lastSeenNewestId,
      pixelOffset: 0,
      lastSeenNewestId: stored.lastSeenNewestId,
      pinLastSeenToBottom: true,
    };
  }

  return { mode: "bottom" };
}

export function visibleCountForRestore(
  messageCount: number,
  restoreIndex: number,
  minimumWindow: number,
) {
  if (restoreIndex < 0) return Math.min(messageCount, minimumWindow);
  return Math.min(messageCount, messageCount - restoreIndex + minimumWindow);
}

export function resetConversationScrollMemory() {
  memory.clear();
  memoryHydrated = false;
}

export function readConversationScrollAnchor(
  storageKey: string,
  storage?: ScrollStorage | null,
) {
  hydrateConversationScrollMemory(storage);
  const stored = memory.get(storageKey);
  return stored ? publicAnchor(stored) : null;
}

export function writeConversationScrollAnchor(
  storageKey: string,
  anchor: ConversationScrollAnchor,
  storage?: ScrollStorage | null,
) {
  if (!storageKey || (!anchor.lastSeenNewestId && !anchor.trackedMessageId)) return;
  hydrateConversationScrollMemory(storage);
  memory.set(storageKey, { ...anchor, updatedAt: Date.now() });
  pruneConversationScrollMemory();
  persistConversationScrollMemory(storage);
}

function publicAnchor(stored: StoredConversationScroll): ConversationScrollAnchor {
  return {
    stuckAtBottom: stored.stuckAtBottom,
    trackedMessageId: stored.trackedMessageId,
    pixelOffset: stored.pixelOffset,
    lastSeenNewestId: stored.lastSeenNewestId,
  };
}

function hydrateConversationScrollMemory(storage?: ScrollStorage | null) {
  if (memoryHydrated) return;
  memoryHydrated = true;
  const raw = readStorage(storage);
  if (!raw) return;
  try {
    const parsed = JSON.parse(raw) as unknown;
    if (!Array.isArray(parsed)) return;
    for (const entry of parsed) {
      const key = typeof entry?.[0] === "string" ? entry[0] : "";
      const value = parseStoredAnchor(entry?.[1]);
      if (!key || !value) continue;
      memory.set(key, value);
    }
  } catch {
    // A corrupt cache is rebuilt the next time a conversation is opened.
  }
}

function persistConversationScrollMemory(storage?: ScrollStorage | null) {
  const target = storage ?? defaultStorage();
  if (!target) return;
  try {
    target.setItem(
      CONVERSATION_SCROLL_STORAGE_KEY,
      JSON.stringify([...memory.entries()]),
    );
  } catch {
    // The in-memory map still keeps the current session's positions.
  }
}

function pruneConversationScrollMemory() {
  if (memory.size <= MAX_STORED_CONVERSATIONS) return;
  const oldest = [...memory.entries()]
    .sort((left, right) => left[1].updatedAt - right[1].updatedAt);
  for (const [key] of oldest.slice(0, memory.size - MAX_STORED_CONVERSATIONS)) {
    memory.delete(key);
  }
}

function parseStoredAnchor(value: unknown): StoredConversationScroll | null {
  if (!value || typeof value !== "object") return null;
  const record = value as Partial<StoredConversationScroll>;
  if (
    typeof record.trackedMessageId !== "string"
    || typeof record.lastSeenNewestId !== "string"
    || typeof record.pixelOffset !== "number"
    || !Number.isFinite(record.pixelOffset)
    || typeof record.stuckAtBottom !== "boolean"
  ) {
    return null;
  }
  return {
    stuckAtBottom: record.stuckAtBottom,
    trackedMessageId: record.trackedMessageId,
    pixelOffset: record.pixelOffset,
    lastSeenNewestId: record.lastSeenNewestId,
    updatedAt: typeof record.updatedAt === "number" && Number.isFinite(record.updatedAt)
      ? record.updatedAt
      : 0,
  };
}

function readStorage(storage?: ScrollStorage | null) {
  const target = storage ?? defaultStorage();
  if (!target) return null;
  try {
    return target.getItem(CONVERSATION_SCROLL_STORAGE_KEY);
  } catch {
    return null;
  }
}

function defaultStorage(): ScrollStorage | null {
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}
