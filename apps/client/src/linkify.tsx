import type { ReactNode } from "react";
import LinkifyIt from "linkify-it";
import { openUrl } from "@tauri-apps/plugin-opener";
import { isTauri } from "./api";
import { MentionChip } from "./MentionChip";
import {
  formatMentionLabel,
  mentionSegments,
  resolveMentionPerson,
  type MentionCandidate,
} from "./mentionSuggestions";

const linkifier = new LinkifyIt().set({ fuzzyEmail: false });

export type MentionRender = {
  onSelect: (person: MentionCandidate) => void;
  renderAvatar: (person: MentionCandidate) => ReactNode;
};

export function openExternalLink(
  event: { preventDefault(): void },
  url: string,
) {
  if (!isTauri) return;
  event.preventDefault();
  void openUrl(url);
}

export function linkify(
  text: string,
  people: MentionCandidate[] = [],
  mention?: MentionRender,
): ReactNode[] {
  const matches = linkifier.match(text);
  if (!matches?.length) return highlightMentions(text, "t", people, mention);

  const parts: ReactNode[] = [];
  let cursor = 0;
  for (const [index, match] of matches.entries()) {
    if (match.index > cursor) {
      parts.push(...highlightMentions(text.slice(cursor, match.index), `t${index}`, people, mention));
    }
    parts.push(
      <a
        className="message-link"
        href={match.url}
        key={`${match.index}-${index}`}
        onClick={(event) => openExternalLink(event, match.url)}
        rel="noopener noreferrer"
        target="_blank"
      >
        {match.text}
      </a>,
    );
    cursor = match.lastIndex;
  }
  if (cursor < text.length) parts.push(...highlightMentions(text.slice(cursor), "end", people, mention));
  return parts;
}

function highlightMentions(
  text: string,
  key: string,
  people: MentionCandidate[],
  mention?: MentionRender,
): ReactNode[] {
  return mentionSegments(text, people).map((part, index) => {
    if (part.kind === "text") return part.value;
    const person = resolveMentionPerson(part.value, people);
    return (
      <MentionChip
        key={`${key}-${index}`}
        label={formatMentionLabel(part.value, people).replace(/^@/, "")}
        title={part.value}
        avatar={person ? mention?.renderAvatar(person) : undefined}
        onClick={person && mention ? () => mention.onSelect(person) : undefined}
      />
    );
  });
}

export function firstLink(text: string): string | null {
  return linkifier.match(text)?.[0]?.url ?? null;
}
