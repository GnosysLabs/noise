import assert from "node:assert/strict";
import { test } from "node:test";
import {
  MAX_COMPOSER_MEDIA_ITEMS,
  MAX_MEDIA_BYTES,
  composerMediaError,
  composerMediaSelectionError,
  restoreComposerAfterSend,
  selectComposerMedia,
} from "../src/composerMedia.ts";

function info(item: { mimeType: string; byteLength: number }) {
  return item;
}

test("accepts image, video, and audio files up to the remaining slots", () => {
  const selection = selectComposerMedia(
    [
      { mimeType: "image/jpeg", byteLength: 1_000 },
      { mimeType: "video/mp4", byteLength: 2_000 },
      { mimeType: "audio/mpeg", byteLength: 3_000 },
    ],
    2,
    info,
  );

  assert.equal(selection.accepted.length, 2);
  assert.equal(selection.rejected.length, 1);
  assert.equal(selection.limited, true);
  assert.equal(selection.invalid, false);
  assert.equal(selection.oversized, false);
  assert.equal(
    composerMediaSelectionError(selection),
    `you can attach up to ${MAX_COMPOSER_MEDIA_ITEMS} media items`,
  );
});

test("skips invalid and oversized files and still fills later valid slots", () => {
  const selection = selectComposerMedia(
    [
      { mimeType: "application/pdf", byteLength: 1_000 },
      { mimeType: "image/png", byteLength: MAX_MEDIA_BYTES + 1 },
      { mimeType: "image/webp", byteLength: 4_000 },
      { mimeType: "image/gif", byteLength: 5_000 },
    ],
    2,
    info,
  );

  assert.deepEqual(
    selection.accepted.map((item) => item.mimeType),
    ["image/webp", "image/gif"],
  );
  assert.equal(selection.invalid, true);
  assert.equal(selection.oversized, true);
  assert.equal(selection.limited, false);
  assert.equal(
    composerMediaSelectionError(selection),
    "choose image, video, or audio files; each media item can be up to 500 MB",
  );
});

test("treats empty files as oversized and ignores unknown native inspects", () => {
  const selection = selectComposerMedia(
    [null, { mimeType: "image/jpeg", byteLength: 0 }, { mimeType: "image/jpeg", byteLength: 12 }],
    3,
    (item) => item,
  );

  assert.equal(selection.accepted.length, 1);
  assert.equal(selection.oversized, true);
  assert.equal(selection.invalid, false);
});

test("adds a limit error when a later attach step rejects extra items", () => {
  assert.equal(
    composerMediaError("choose image, video, or audio files", true),
    `you can attach up to ${MAX_COMPOSER_MEDIA_ITEMS} media items; choose image, video, or audio files`,
  );
  assert.equal(
    composerMediaError(`you can attach up to ${MAX_COMPOSER_MEDIA_ITEMS} media items`, true),
    `you can attach up to ${MAX_COMPOSER_MEDIA_ITEMS} media items`,
  );
});

test("restores unsent attachments and the draft only when the first send failed", () => {
  const attachments = ["one", "two", "three"];

  assert.deepEqual(restoreComposerAfterSend({ confirmedCount: 3, attachments }), {
    remainingAttachments: [],
    restoreDraft: false,
  });
  assert.deepEqual(restoreComposerAfterSend({ confirmedCount: 2, attachments }), {
    remainingAttachments: ["three"],
    restoreDraft: false,
  });
  assert.deepEqual(restoreComposerAfterSend({ confirmedCount: 0, attachments }), {
    remainingAttachments: attachments,
    restoreDraft: true,
  });
  assert.deepEqual(restoreComposerAfterSend({ confirmedCount: 0, attachments: [] }), {
    remainingAttachments: [],
    restoreDraft: true,
  });
  assert.deepEqual(restoreComposerAfterSend({ confirmedCount: 1, attachments: [] }), {
    remainingAttachments: [],
    restoreDraft: false,
  });
});
