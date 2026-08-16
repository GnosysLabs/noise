import assert from "node:assert/strict";
import { test } from "node:test";
import {
  groupMediaMessages,
  isCollageMediaMime,
  mediaAlbumIdForPending,
  messageMediaAlbumId,
  withMediaAlbumId,
  type AlbumGroupableMessage,
} from "../src/mediaAlbum.ts";

function message(
  id: string,
  overrides: Partial<AlbumGroupableMessage> = {},
): AlbumGroupableMessage {
  return {
    event_id: id,
    author_public_key: "alice",
    attachment: {
      mime_type: "image/jpeg",
      file_name: `${id}.jpg`,
    },
    ...overrides,
  };
}

test("does not group consecutive images that were sent separately", () => {
  const groups = groupMediaMessages([
    message("one"),
    message("two"),
    message("three"),
  ]);
  assert.deepEqual(groups.map((group) => group.messages.map((item) => item.event_id)), [
    ["one"],
    ["two"],
    ["three"],
  ]);
});

test("groups only media that shares a send-batch album id", () => {
  const groups = groupMediaMessages([
    message("one", { attachment: { mime_type: "image/jpeg", media_album_id: "aaa" } }),
    message("two", { attachment: { mime_type: "image/jpeg", media_album_id: "aaa" } }),
    message("three", { attachment: { mime_type: "image/jpeg", media_album_id: "bbb" } }),
    message("four", { attachment: { mime_type: "image/jpeg", media_album_id: "bbb" } }),
    message("five"),
  ]);
  assert.deepEqual(groups.map((group) => group.messages.map((item) => item.event_id)), [
    ["one", "two"],
    ["three", "four"],
    ["five"],
  ]);
});

test("does not merge neighboring albums or forwarded copies", () => {
  const groups = groupMediaMessages([
    message("one", { attachment: { mime_type: "image/jpeg", media_album_id: "aaa" } }),
    message("two", {
      attachment: { mime_type: "image/jpeg", media_album_id: "aaa" },
      forwarded_from: { public_key: "bob", username: "bob" },
    }),
    message("three", { attachment: { mime_type: "image/jpeg", media_album_id: "aaa" } }),
  ]);
  assert.deepEqual(groups.map((group) => group.messages.map((item) => item.event_id)), [
    ["one"],
    ["two"],
    ["three"],
  ]);
});

test("reads the album id from an optimistic local attachment", () => {
  const item = message("local", {
    attachment: null,
    local_attachment: {
      mime_type: "video/mp4",
      file_name: "clip.mp4",
      media_album_id: "abc123",
    },
  });
  assert.equal(messageMediaAlbumId(item), "abc123");
});

test("assigns an album id only when a send has two collage items", () => {
  assert.equal(mediaAlbumIdForPending([{ mimeType: "image/jpeg", name: "a.jpg" }]), null);
  assert.equal(mediaAlbumIdForPending([
    { mimeType: "image/jpeg", name: "a.jpg" },
    { mimeType: "audio/mpeg", name: "a.mp3" },
  ]), null);
  assert.match(
    mediaAlbumIdForPending([
      { mimeType: "image/jpeg", name: "a.jpg" },
      { mimeType: "video/mp4", name: "b.mp4" },
    ]) ?? "",
    /^[0-9a-f]{32}$/,
  );
  assert.equal(isCollageMediaMime("image/png", "klipy-sticker-1.png"), false);
});

test("stamps an album id onto an uploaded attachment", () => {
  const stamped = withMediaAlbumId({ mime_type: "image/jpeg" }, "deadbeef");
  assert.equal(stamped?.media_album_id, "deadbeef");
  assert.equal(withMediaAlbumId({ mime_type: "image/jpeg" }, null)?.media_album_id, undefined);
});
