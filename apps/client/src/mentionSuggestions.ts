import {
  compactNoiseSignature,
  noiseSignature,
  NOISE_SIGNATURE_PATTERN,
  normalizeNoiseSignature,
} from "./noiseSignature.ts";
import type { ProfileImage } from "./types";

export type MentionCandidate = {
  public_key: string;
  username: string;
  avatar?: ProfileImage | null;
};

export type MentionQuery = {
  start: number;
  query: string;
};

const MAX_SUGGESTIONS = 8;

export function mentionToken(person: MentionCandidate) {
  const signature = noiseSignature(person.public_key);
  return signature === "UNAVAILABLE" ? `@${person.username.trim()}` : `@${signature}`;
}

export function activeMentionQuery(
  text: string,
  cursor: number,
  people: MentionCandidate[] = [],
): MentionQuery | null {
  if (cursor < 1 || cursor > text.length) return null;
  const before = text.slice(0, cursor);
  const match = before.match(/(^|[^A-Za-z0-9_])@([^\n@]*)$/);
  if (!match || match.index == null) return null;
  const query = match[2] ?? "";
  if (query.length > 80) return null;
  const start = match.index + match[1].length;
  if (query.endsWith(" ") && mentionIsComplete(query.trimEnd(), people)) return null;
  return { start, query };
}

export function filterMentionCandidates(
  people: MentionCandidate[],
  query: string,
  selfPublicKey?: string,
) {
  const ranked = people
    .filter((person) => person.public_key !== selfPublicKey && person.username.trim())
    .map((person) => ({ person, rank: mentionRank(person, query) }))
    .filter((item) => item.rank >= 0)
    .sort((left, right) =>
      left.rank - right.rank || left.person.username.localeCompare(right.person.username)
    );
  return ranked.slice(0, MAX_SUGGESTIONS).map((item) => item.person);
}

export function insertMention(
  text: string,
  cursor: number,
  mention: MentionQuery,
  person: MentionCandidate,
) {
  const inserted = `${mentionToken(person)} `;
  return {
    text: `${text.slice(0, mention.start)}${inserted}${text.slice(cursor)}`,
    cursor: mention.start + inserted.length,
  };
}

export function mentionedPublicKeys(
  text: string,
  people: MentionCandidate[],
) {
  const mentioned = new Set<string>();
  const roster = uniquePeople(people);
  const bySignature = new Map<string, string>();
  const byName = new Map<string, MentionCandidate[]>();
  for (const person of roster) {
    const signature = compactNoiseSignature(person.public_key);
    if (signature) bySignature.set(signature, person.public_key);
    const name = person.username.trim().toLowerCase();
    const current = byName.get(name) ?? [];
    current.push(person);
    byName.set(name, current);
  }

  for (const match of text.matchAll(signedMentionPattern())) {
    const publicKey = bySignature.get(normalizeNoiseSignature(match[3] ?? ""));
    if (publicKey) mentioned.add(publicKey);
  }
  for (const match of text.matchAll(signatureMentionPattern())) {
    const publicKey = bySignature.get(normalizeNoiseSignature(match[2] ?? ""));
    if (publicKey) mentioned.add(publicKey);
  }
  for (const [name, matches] of byName) {
    if (matches.length !== 1) continue;
    const person = matches[0];
    if (person && mentionPattern(person.username).test(text)) mentioned.add(person.public_key);
  }
  return mentioned;
}

export function resolveMentionPerson(token: string, people: MentionCandidate[]) {
  const roster = uniquePeople(people);
  const signature = mentionSignature(token);
  if (signature) {
    return roster.find((item) => compactNoiseSignature(item.public_key) === signature) ?? null;
  }
  const name = token.replace(/^@/, "").trim().toLowerCase();
  if (!name) return null;
  const matches = roster.filter((item) => item.username.trim().toLowerCase() === name);
  return matches.length === 1 ? matches[0] ?? null : null;
}

export function formatMentionLabel(token: string, people: MentionCandidate[] = []) {
  const person = resolveMentionPerson(token, people);
  if (person?.username.trim()) return `@${person.username.trim()}`;
  const signature = mentionSignature(token);
  return signature ? `@${dashedSignature(signature)}` : token;
}

export function prettyMentionText(text: string, people: MentionCandidate[] = []) {
  return mentionSegments(text, people)
    .map((part) => part.kind === "mention" ? formatMentionLabel(part.value, people) : part.value)
    .join("");
}

export function mentionSegments(text: string, people: MentionCandidate[] = []) {
  const parts: Array<{ kind: "text" | "mention"; value: string }> = [];
  let cursor = 0;
  for (const range of mentionRanges(text, people)) {
    if (range.start > cursor) parts.push({ kind: "text", value: text.slice(cursor, range.start) });
    parts.push({ kind: "mention", value: range.value });
    cursor = range.end;
  }
  if (cursor < text.length) parts.push({ kind: "text", value: text.slice(cursor) });
  return parts;
}

function mentionRank(person: MentionCandidate, query: string) {
  const name = person.username.trim().toLowerCase();
  const needle = query.trim().toLowerCase();
  const compact = compactNoiseSignature(person.public_key).toLowerCase();
  const dashed = noiseSignature(person.public_key).toLowerCase();
  const compactNeedle = needle.replace(/-/g, "");
  if (!needle) return 1;
  if (name.startsWith(needle) || compact.startsWith(compactNeedle) || dashed.startsWith(needle)) {
    return 0;
  }
  const words = name.split(/\s+/);
  const parts = needle.split(/\s+/);
  if (parts.every((part, index) => words[index]?.startsWith(part))) return 1;
  if (name.includes(needle) || compact.includes(compactNeedle)) return 2;
  return -1;
}

function mentionIsComplete(query: string, people: MentionCandidate[]) {
  const lowered = query.toLowerCase();
  const roster = uniquePeople(people);
  const nameCounts = new Map<string, number>();
  for (const person of roster) {
    const name = person.username.trim().toLowerCase();
    nameCounts.set(name, (nameCounts.get(name) ?? 0) + 1);
  }
  return roster.some((person) => {
    const token = mentionToken(person).slice(1).toLowerCase();
    const name = person.username.trim().toLowerCase();
    return lowered === token || ((nameCounts.get(name) ?? 0) === 1 && lowered === name);
  });
}

function mentionRanges(text: string, people: MentionCandidate[]) {
  const ranges: Array<{ start: number; end: number; value: string }> = [];
  const add = (start: number, value: string) => {
    const end = start + value.length;
    if (ranges.some((range) => start < range.end && end > range.start)) return;
    ranges.push({ start, end, value });
  };
  const signed = new RegExp(
    `(^|[^A-Za-z0-9_@])(@[^\\n@#]+?#(?:${NOISE_SIGNATURE_PATTERN})|@(?:${NOISE_SIGNATURE_PATTERN}))`,
    "gi",
  );
  for (const match of text.matchAll(signed)) {
    const token = match[2] ?? "";
    add((match.index ?? 0) + (match[1]?.length ?? 0), token);
  }
  const roster = uniquePeople(people);
  const nameCounts = new Map<string, number>();
  for (const person of roster) {
    const name = person.username.trim().toLowerCase();
    if (name) nameCounts.set(name, (nameCounts.get(name) ?? 0) + 1);
  }
  for (const person of roster) {
    const name = person.username.trim();
    if (!name || (nameCounts.get(name.toLowerCase()) ?? 0) !== 1) continue;
    for (const match of text.matchAll(mentionPattern(name, "gi"))) {
      const start = (match.index ?? 0) + (match[1]?.length ?? 0);
      add(start, text.slice(start, start + 1 + name.length));
    }
  }
  return ranges.sort((left, right) => left.start - right.start);
}

function mentionPattern(name: string, flags = "i") {
  const escaped = name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return new RegExp(`(^|[^\\w@])@${escaped}(?=$|[^\\w#])`, flags);
}

function uniquePeople(people: MentionCandidate[]) {
  const unique = new Map<string, MentionCandidate>();
  for (const person of people) unique.set(person.public_key, person);
  return [...unique.values()];
}

function signedMentionPattern() {
  return new RegExp(
    `(^|[^A-Za-z0-9_@])@([^\\n@#]+?)#(${NOISE_SIGNATURE_PATTERN})(?=$|[^A-Za-z0-9_-])`,
    "gi",
  );
}

function signatureMentionPattern() {
  return new RegExp(
    `(^|[^A-Za-z0-9_@])@(${NOISE_SIGNATURE_PATTERN})(?=$|[^A-Za-z0-9_-])`,
    "gi",
  );
}

function mentionSignature(token: string) {
  const signed = token.match(new RegExp(`^@([^\\n@#]+?)#(${NOISE_SIGNATURE_PATTERN})$`, "i"));
  if (signed?.[2]) return normalizeNoiseSignature(signed[2]);
  const signatureOnly = token.match(new RegExp(`^@(${NOISE_SIGNATURE_PATTERN})$`, "i"));
  return signatureOnly?.[1] ? normalizeNoiseSignature(signatureOnly[1]) : "";
}

function dashedSignature(value: string) {
  const compact = normalizeNoiseSignature(value);
  return compact.length === 12 ? `${compact.slice(0, 6)}-${compact.slice(6)}` : compact;
}
