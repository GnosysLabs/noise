import assert from "node:assert/strict";
import { test } from "node:test";
import {
  CONVERSATION_SCROLL_STORAGE_KEY,
  conversationScrollStorageKey,
  countMessagesAfter,
  formatNewMessagesLabel,
  readConversationScrollAnchor,
  resetConversationScrollMemory,
  resolveConversationRestore,
  visibleCountForRestore,
  writeConversationScrollAnchor,
} from "../src/conversationScroll.ts";

test("counts only messages after the last seen newest", () => {
  assert.equal(countMessagesAfter(["a", "b", "c"], "b"), 1);
  assert.equal(countMessagesAfter(["a", "b", "c"], "c"), 0);
  assert.equal(countMessagesAfter(["a", "b", "c"], "missing"), 0);
});

test("formats the new-messages pill label", () => {
  assert.equal(formatNewMessagesLabel(0), "new messages");
  assert.equal(formatNewMessagesLabel(1), "1 new message");
  assert.equal(formatNewMessagesLabel(12), "12 new messages");
  assert.equal(formatNewMessagesLabel(100), "99+ new messages");
});

test("keeps a first visit at the newest message", () => {
  assert.deepEqual(resolveConversationRestore(["a", "b"], null), { mode: "bottom" });
});

test("parks at the last seen message when newer ones arrived", () => {
  const restore = resolveConversationRestore(
    ["old", "mid", "new"],
    {
      stuckAtBottom: true,
      trackedMessageId: "mid",
      pixelOffset: 12,
      lastSeenNewestId: "mid",
    },
  );
  assert.deepEqual(restore, {
    mode: "anchor",
    trackedMessageId: "mid",
    pixelOffset: 12,
    lastSeenNewestId: "mid",
    pinLastSeenToBottom: true,
  });
});

test("restores a mid-chat position even when newer messages arrived", () => {
  const restore = resolveConversationRestore(
    ["old", "here", "later", "newest"],
    {
      stuckAtBottom: false,
      trackedMessageId: "here",
      pixelOffset: 40,
      lastSeenNewestId: "later",
    },
  );
  assert.deepEqual(restore, {
    mode: "anchor",
    trackedMessageId: "here",
    pixelOffset: 40,
    lastSeenNewestId: "later",
    pinLastSeenToBottom: false,
  });
});

test("stays at the bottom when the last seen message is still newest", () => {
  assert.deepEqual(
    resolveConversationRestore(
      ["a", "b"],
      {
        stuckAtBottom: true,
        trackedMessageId: "b",
        pixelOffset: 0,
        lastSeenNewestId: "b",
      },
    ),
    { mode: "bottom" },
  );
});

test("parks at the oldest loaded message when the leave point fell out of the window", () => {
  const restore = resolveConversationRestore(
    ["n1", "n2", "n3"],
    {
      stuckAtBottom: true,
      trackedMessageId: "old",
      pixelOffset: 0,
      lastSeenNewestId: "old",
    },
    null,
    8,
  );
  assert.deepEqual(restore, {
    mode: "anchor",
    trackedMessageId: "n1",
    pixelOffset: 0,
    lastSeenNewestId: "old",
    pinLastSeenToBottom: false,
  });
});

test("lets a search jump win over a stored position", () => {
  const restore = resolveConversationRestore(
    ["a", "b", "c"],
    {
      stuckAtBottom: true,
      trackedMessageId: "a",
      pixelOffset: 0,
      lastSeenNewestId: "a",
    },
    "c",
  );
  assert.equal(restore.mode, "anchor");
  if (restore.mode === "anchor") {
    assert.equal(restore.trackedMessageId, "c");
    assert.equal(restore.pinLastSeenToBottom, false);
  }
});

test("expands the rendered window to include the restore point", () => {
  assert.equal(visibleCountForRestore(30, 20, 24), 30);
  assert.equal(visibleCountForRestore(80, 10, 24), 80);
  assert.equal(visibleCountForRestore(80, 60, 24), 44);
  assert.equal(visibleCountForRestore(10, -1, 24), 10);
});

test("stores and reads a scroll anchor per identity and conversation", () => {
  resetConversationScrollMemory();
  const storage = new Map<string, string>();
  const adapter = {
    getItem: (key: string) => storage.get(key) ?? null,
    setItem: (key: string, value: string) => {
      storage.set(key, value);
    },
  };
  const key = conversationScrollStorageKey("alice", "group:general");
  writeConversationScrollAnchor(key, {
    stuckAtBottom: false,
    trackedMessageId: "msg-1",
    pixelOffset: 18,
    lastSeenNewestId: "msg-9",
  }, adapter);
  assert.ok(storage.get(CONVERSATION_SCROLL_STORAGE_KEY));
  resetConversationScrollMemory();
  assert.deepEqual(readConversationScrollAnchor(key, adapter), {
    stuckAtBottom: false,
    trackedMessageId: "msg-1",
    pixelOffset: 18,
    lastSeenNewestId: "msg-9",
  });
});
