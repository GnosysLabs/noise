import assert from "node:assert/strict";
import { test } from "node:test";
import {
  compactNoiseSignature,
  noiseSignature,
  normalizeNoiseSignature,
} from "../src/noiseSignature.ts";

function key(byte: number) {
  return Buffer.from(Uint8Array.from({ length: 32 }, () => byte)).toString("base64");
}

test("encodes a public key as a 12-character noise signature", () => {
  const signature = noiseSignature(key(1));
  assert.match(signature, /^[0-9A-HJKMNP-TV-Z]{6}-[0-9A-HJKMNP-TV-Z]{6}$/);
  assert.equal(compactNoiseSignature(key(1)), signature.replace("-", ""));
  assert.equal(normalizeNoiseSignature(signature), compactNoiseSignature(key(1)));
  assert.notEqual(compactNoiseSignature(key(1)), compactNoiseSignature(key(2)));
});
