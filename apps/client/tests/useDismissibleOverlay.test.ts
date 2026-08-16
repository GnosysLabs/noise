import assert from "node:assert/strict";
import { test } from "node:test";
import {
  isSecondaryPointer,
  overlayDismissesOnMouseDown,
} from "../src/useDismissibleOverlay.ts";

test("left-click outside a context menu dismisses it", () => {
  assert.equal(overlayDismissesOnMouseDown(0), true);
});

test("right-click does not dismiss a menu it just opened", () => {
  assert.equal(overlayDismissesOnMouseDown(2), false);
});

test("right mouse button is the message-menu trigger", () => {
  assert.equal(isSecondaryPointer({ button: 2 }), true);
  assert.equal(isSecondaryPointer({ button: 0 }), false);
});
