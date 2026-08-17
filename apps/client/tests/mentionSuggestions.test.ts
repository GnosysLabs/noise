import assert from "node:assert/strict";
import { test } from "node:test";
import {
  activeMentionQuery,
  filterMentionCandidates,
  formatMentionLabel,
  insertMention,
  mentionedPublicKeys,
  mentionSegments,
  mentionToken,
  prettyMentionText,
  resolveMentionPerson,
} from "../src/mentionSuggestions.ts";
import { compactNoiseSignature, noiseSignature } from "../src/noiseSignature.ts";

function key(byte: number) {
  return Buffer.from(Uint8Array.from({ length: 32 }, () => byte)).toString("base64");
}

const self = { public_key: key(9), username: "chris" };
const sam = { public_key: key(1), username: "sam" };
const otherSam = { public_key: key(2), username: "sam" };
const sandy = { public_key: key(3), username: "sandy" };
const kurby = { public_key: key(4), username: "kurby dog" };
const people = [self, sam, sandy, kurby];

test("opens a mention after @ and ignores emails", () => {
  assert.deepEqual(activeMentionQuery("@", 1), { start: 0, query: "" });
  assert.deepEqual(activeMentionQuery("hi @sa", 6), { start: 3, query: "sa" });
  assert.equal(activeMentionQuery("chris@site", 10), null);
  assert.equal(activeMentionQuery("hello", 5), null);
});

test("keeps multi-word queries open until a name is completed", () => {
  assert.deepEqual(activeMentionQuery("hey @kurby d", 12, [kurby]), {
    start: 4,
    query: "kurby d",
  });
  assert.equal(activeMentionQuery("hey @kurby dog ", 15, [kurby]), null);
  assert.equal(activeMentionQuery("hey @sam ", 9, [sam]), null);
});

test("keeps an ambiguous shared name open until a signature is chosen", () => {
  assert.deepEqual(activeMentionQuery("hey @sam ", 9, [sam, otherSam]), {
    start: 4,
    query: "sam ",
  });
  assert.equal(activeMentionQuery(`hey ${mentionToken(sam)} `, 4 + mentionToken(sam).length + 1, [sam, otherSam]), null);
});

test("narrows members as the query grows", () => {
  const afterAt = filterMentionCandidates(people, "", self.public_key);
  assert.deepEqual(afterAt.map((person) => person.username), ["kurby dog", "sam", "sandy"]);
  assert.deepEqual(
    filterMentionCandidates(people, "sa", self.public_key).map((person) => person.username),
    ["sam", "sandy"],
  );
  assert.deepEqual(
    filterMentionCandidates(people, "sam", self.public_key).map((person) => person.username),
    ["sam"],
  );
  assert.deepEqual(
    filterMentionCandidates(people, "ku d", self.public_key).map((person) => person.username),
    ["kurby dog"],
  );
});

test("can narrow two people with the same name by signature", () => {
  const compact = compactNoiseSignature(otherSam.public_key);
  const matches = filterMentionCandidates([sam, otherSam], compact);
  assert.deepEqual(matches.map((person) => person.public_key), [otherSam.public_key]);
});

test("inserts a signature-keyed mention and leaves a trailing space", () => {
  const next = insertMention("hi @sa", 6, { start: 3, query: "sa" }, sam);
  assert.equal(next.text, `hi ${mentionToken(sam)} `);
  assert.equal(next.cursor, 3 + mentionToken(sam).length + 1);
});

test("notifies only the signed identity when two members share a name", () => {
  const roster = [sam, otherSam];
  assert.deepEqual([...mentionedPublicKeys("hey @sam", roster)], []);
  assert.deepEqual([...mentionedPublicKeys(mentionToken(sam), roster)], [sam.public_key]);
  assert.deepEqual([...mentionedPublicKeys(`@${noiseSignature(sam.public_key)}`, roster)], [sam.public_key]);
  assert.deepEqual([...mentionedPublicKeys(mentionToken(otherSam), roster)], [otherSam.public_key]);
});

test("still matches a unique display name without a signature", () => {
  assert.deepEqual([...mentionedPublicKeys("hey @sandy", people)], [sandy.public_key]);
  assert.deepEqual([...mentionedPublicKeys("hey @kurby dog", people)], [kurby.public_key]);
  assert.deepEqual([...mentionedPublicKeys("hey @kurby", people)], []);
});

test("stores the noise signature, not the display name", () => {
  assert.equal(mentionToken(sam), `@${noiseSignature(sam.public_key)}`);
  assert.equal(formatMentionLabel(mentionToken(sam), people), "@sam");
  assert.equal(prettyMentionText(`hey ${mentionToken(sam)} later`, people), "hey @sam later");
  assert.equal(
    formatMentionLabel(mentionToken(sam), [{ ...sam, username: "samuel" }]),
    "@samuel",
  );
});

test("resolves a mention token to one person", () => {
  assert.equal(resolveMentionPerson(mentionToken(sam), [sam, otherSam])?.public_key, sam.public_key);
  assert.equal(resolveMentionPerson("@sandy", people)?.public_key, sandy.public_key);
  assert.equal(resolveMentionPerson("@sam", [sam, otherSam]), null);
});

test("segments signature and unique-name mentions for rendering", () => {
  const parts = mentionSegments(`hey ${mentionToken(sam)} and @sandy`, people);
  assert.deepEqual(
    parts.filter((part) => part.kind === "mention").map((part) => part.value),
    [mentionToken(sam), "@sandy"],
  );
  assert.deepEqual(
    mentionSegments("hey @sam", [sam, otherSam]).filter((part) => part.kind === "mention"),
    [],
  );
});
