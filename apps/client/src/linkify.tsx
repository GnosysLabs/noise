import type { ReactNode } from "react";
import LinkifyIt from "linkify-it";
import { openUrl } from "@tauri-apps/plugin-opener";
import { isTauri } from "./api";

const linkifier = new LinkifyIt().set({ fuzzyEmail: false });

export function openExternalLink(
  event: { preventDefault(): void },
  url: string,
) {
  if (!isTauri) return;
  event.preventDefault();
  void openUrl(url);
}

export function linkify(text: string): ReactNode[] {
  const matches = linkifier.match(text);
  if (!matches?.length) return [text];

  const parts: ReactNode[] = [];
  let cursor = 0;
  for (const [index, match] of matches.entries()) {
    if (match.index > cursor) parts.push(text.slice(cursor, match.index));
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
  if (cursor < text.length) parts.push(text.slice(cursor));
  return parts;
}

export function firstLink(text: string): string | null {
  return linkifier.match(text)?.[0]?.url ?? null;
}
