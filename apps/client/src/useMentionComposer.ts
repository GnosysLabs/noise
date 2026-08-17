import { useCallback, useState, type KeyboardEvent, type RefObject } from "react";
import {
  activeMentionQuery,
  filterMentionCandidates,
  insertMention,
  type MentionCandidate,
} from "./mentionSuggestions";

export function useMentionComposer(
  people: MentionCandidate[],
  selfPublicKey: string,
  draft: string,
  setDraft: (text: string) => void,
  inputRef: RefObject<HTMLTextAreaElement | null>,
) {
  const [cursor, setCursor] = useState(0);
  const [selectedIndex, setSelectedIndex] = useState(0);
  const [dismissedStart, setDismissedStart] = useState<number | null>(null);
  const rawMention = activeMentionQuery(draft, cursor, people);
  const mention = rawMention && rawMention.start !== dismissedStart ? rawMention : null;
  const matches = mention
    ? filterMentionCandidates(people, mention.query, selfPublicKey)
    : [];
  const index = matches.length === 0 ? 0 : Math.min(selectedIndex, matches.length - 1);

  const syncCursor = useCallback((element: HTMLTextAreaElement) => {
    setCursor(element.selectionStart);
  }, []);

  const pick = useCallback((person: MentionCandidate) => {
    if (!mention) return;
    const next = insertMention(draft, cursor, mention, person);
    setDismissedStart(null);
    setDraft(next.text);
    setCursor(next.cursor);
    window.requestAnimationFrame(() => {
      const input = inputRef.current;
      if (!input) return;
      input.focus();
      input.setSelectionRange(next.cursor, next.cursor);
    });
  }, [cursor, draft, inputRef, mention, setDraft]);

  const onKeyDown = useCallback((event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (matches.length === 0 || !mention) return false;
    if (event.key === "ArrowDown") {
      event.preventDefault();
      setSelectedIndex((current) => (current + 1) % matches.length);
      return true;
    }
    if (event.key === "ArrowUp") {
      event.preventDefault();
      setSelectedIndex((current) => (current - 1 + matches.length) % matches.length);
      return true;
    }
    if (event.key === "Enter" || event.key === "Tab") {
      const person = matches[index];
      if (!person) return false;
      event.preventDefault();
      pick(person);
      return true;
    }
    if (event.key === "Escape") {
      event.preventDefault();
      setDismissedStart(mention.start);
      return true;
    }
    return false;
  }, [index, matches, mention, pick]);

  return {
    matches,
    selectedIndex: index,
    setSelectedIndex,
    pick,
    onKeyDown,
    onDraftChange: (text: string, element: HTMLTextAreaElement) => {
      const nextCursor = element.selectionStart;
      const nextMention = activeMentionQuery(text, nextCursor, people);
      if (!nextMention) setDismissedStart(null);
      setDraft(text);
      setCursor(nextCursor);
      setSelectedIndex(0);
    },
    syncCursor,
  };
}
