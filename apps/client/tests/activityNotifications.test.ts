import assert from "node:assert/strict";
import { test } from "node:test";
import {
  activityPreview,
  emptyActivityInbox,
  loadActivityInbox,
  markActivityInboxRead,
  mergeActivityNotifications,
  messageMentionsIdentity,
  notificationsFromMessages,
  saveActivityInbox,
  unreadActivityCount,
  withoutDirectActivity,
} from "../src/activityNotifications.ts";

const self = {
  public_key: "self",
  username: "chris",
  noise_id: "abc123",
};

function message(id: string, overrides: Record<string, unknown> = {}) {
  return {
    event_id: `event-${id}`,
    message_id: id,
    author_public_key: "other",
    username: "sam",
    avatar: null,
    text: "",
    attachment: null,
    reply_to_message_id: null,
    topic_id: null,
    created_at_millis: 1000,
    reactions: [],
    ...overrides,
  };
}

test("detects @username and @noise-id mentions", () => {
  assert.equal(messageMentionsIdentity("hey @chris look", "chris", "abc123"), true);
  assert.equal(messageMentionsIdentity("hey @Chris!", "chris", null), true);
  assert.equal(messageMentionsIdentity("see @abc123", "chris", "abc123"), true);
  assert.equal(messageMentionsIdentity("christmas @christina", "chris", null), false);
  assert.equal(messageMentionsIdentity("email chris@site.com", "chris", null), false);
});

test("detects a display name that contains a space", () => {
  assert.equal(messageMentionsIdentity("hi @kurby dog", "kurby dog", null), true);
  assert.equal(messageMentionsIdentity("hi @kurby", "kurby dog", null), false);
});

test("builds mention, reply, and reaction notifications", () => {
  const items = notificationsFromMessages(
    [
      message("own", { author_public_key: "self", username: "chris", text: "question" }),
      message("mention", { text: "hey @chris", created_at_millis: 20 }),
      message("reply", { text: "here", reply_to_message_id: "own", created_at_millis: 30 }),
      message("own-reacted", {
        author_public_key: "self",
        username: "chris",
        text: "photo",
        reactions: [{
          emoji: "🔥",
          count: 1,
          reactor_public_keys: ["other"],
          reacted_by_self: false,
        }],
      }),
    ],
    self,
    [],
    new Map([["other", { public_key: "other", username: "sam", avatar: null }]]),
    { groupId: "g1", groupName: "general" },
  );
  assert.deepEqual(items.map((item) => item.kind), ["mention", "reply", "reaction"]);
  assert.equal(items[2]?.emoji, "🔥");
});

test("ignores own mentions, hidden authors, and optimistic rows", () => {
  const items = notificationsFromMessages(
    [
      message("self-mention", { author_public_key: "self", text: "note to @chris" }),
      message("hidden", { author_public_key: "blocked", text: "hey @chris" }),
      message("pending", { text: "hey @chris", optimistic: true }),
    ],
    self,
    ["blocked"],
    new Map(),
    { groupId: "g1", groupName: "general" },
  );
  assert.equal(items.length, 0);
});

test("first harvest shows existing activity as read; later items are unread", () => {
  const first = mergeActivityNotifications(
    { ...emptyActivityInbox(), baselineAt: 100 },
    "group:g1",
    [{
      id: "mention:old",
      kind: "mention",
      eventId: "old",
      createdAtMillis: 1,
      actor: { public_key: "other", username: "sam", avatar: null },
      preview: "hey",
    }],
  );
  assert.equal(first.items.length, 1);
  assert.equal(unreadActivityCount(first), 0);
  const second = mergeActivityNotifications(first, "group:g1", [
    {
      id: "mention:old",
      kind: "mention",
      eventId: "old",
      createdAtMillis: 1,
      actor: { public_key: "other", username: "sam", avatar: null },
      preview: "hey",
    },
    {
      id: "reply:new",
      kind: "reply",
      eventId: "new",
      createdAtMillis: 2,
      actor: { public_key: "other", username: "sam", avatar: null },
      preview: "later",
    },
  ]);
  assert.equal(unreadActivityCount(second), 1);
  assert.equal(second.items[0]?.id, "reply:new");
  assert.equal(markActivityInboxRead(second).readIds.includes("reply:new"), true);
});

test("first harvest keeps activity newer than the inbox baseline unread", () => {
  const merged = mergeActivityNotifications(
    { ...emptyActivityInbox(), baselineAt: 100 },
    "group:g1",
    [
      {
        id: "mention:old",
        kind: "mention",
        eventId: "old",
        createdAtMillis: 50,
        actor: { public_key: "other", username: "sam", avatar: null },
        preview: "earlier",
      },
      {
        id: "mention:new",
        kind: "mention",
        eventId: "new",
        createdAtMillis: 200,
        actor: { public_key: "other", username: "sam", avatar: null },
        preview: "just now",
      },
    ],
  );
  assert.equal(merged.items.length, 2);
  assert.equal(unreadActivityCount(merged), 1);
  assert.equal(merged.items[0]?.id, "mention:new");
});

test("previously baselined ids reappear in the list as read", () => {
  const merged = mergeActivityNotifications(
    {
      items: [],
      readIds: ["mention:old"],
      seenScopes: ["group:g1"],
      baselineAt: 1,
    },
    "group:g1",
    [{
      id: "mention:old",
      kind: "mention",
      eventId: "old",
      createdAtMillis: 1,
      actor: { public_key: "other", username: "sam", avatar: null },
      preview: "hey",
    }],
  );
  assert.equal(merged.items.length, 1);
  assert.equal(unreadActivityCount(merged), 0);
});

test("preview prefers text and falls back to media", () => {
  assert.equal(activityPreview("  hello   there  "), "hello there");
  assert.equal(activityPreview("", "image/jpeg"), "sent a photo");
});

test("empty harvest does not mark a chat as seen", () => {
  const first = mergeActivityNotifications(emptyActivityInbox(), "group:g1", []);
  assert.deepEqual(first.seenScopes, []);
  const later = mergeActivityNotifications(first, "group:g1", [{
    id: "mention:1",
    kind: "mention",
    eventId: "1",
    createdAtMillis: 1,
    actor: { public_key: "other", username: "sam", avatar: null },
    preview: "hey",
  }]);
  assert.deepEqual(later.seenScopes, ["group:g1"]);
  assert.equal(later.items.length, 1);
});

test("drops direct-message activity from the inbox", () => {
  const merged = mergeActivityNotifications(emptyActivityInbox(), "direct:sam", [{
    id: "reply:1",
    kind: "reply",
    eventId: "1",
    createdAtMillis: 9,
    actor: { public_key: "sam", username: "sam", avatar: null },
    preview: "yo",
    directPublicKey: "sam",
    directUsername: "sam",
  }]);
  assert.equal(merged.items.length, 0);
  assert.deepEqual(withoutDirectActivity({
    ...emptyActivityInbox(),
    items: [{
      id: "mention:1",
      kind: "mention",
      eventId: "1",
      createdAtMillis: 1,
      actor: { public_key: "sam", username: "sam", avatar: null },
      preview: "hey",
      groupId: "g1",
      groupName: "general",
    }, {
      id: "reply:dm",
      kind: "reply",
      eventId: "2",
      createdAtMillis: 2,
      actor: { public_key: "sam", username: "sam", avatar: null },
      preview: "yo",
      directPublicKey: "sam",
    }],
  }).items.map((item) => item.id), ["mention:1"]);
});

test("persists an inbox per identity", () => {
  const storage = new Map<string, string>();
  const adapter = {
    getItem: (key: string) => storage.get(key) ?? null,
    setItem: (key: string, value: string) => {
      storage.set(key, value);
    },
  };
  const state = mergeActivityNotifications(emptyActivityInbox(), "group:g1", [{
    id: "reply:1",
    kind: "reply",
    eventId: "1",
    createdAtMillis: 9,
    actor: { public_key: "sam", username: "sam", avatar: null },
    preview: "yo",
    groupId: "g1",
    groupName: "general",
  }]);
  saveActivityInbox("self", state, adapter);
  const loaded = loadActivityInbox("self", adapter);
  assert.deepEqual(loaded.seenScopes, ["group:g1"]);
  assert.equal(loaded.items.length, 1);
});
