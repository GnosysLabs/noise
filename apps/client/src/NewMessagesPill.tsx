import { ChevronDown } from "lucide-react";
import { formatNewMessagesLabel } from "./conversationScroll";

export function NewMessagesPill({
  count,
  onClick,
}: {
  count: number;
  onClick: () => void;
}) {
  if (count <= 0) return null;
  return (
    <button
      type="button"
      className="new-messages-pill"
      aria-label={formatNewMessagesLabel(count)}
      onClick={onClick}
    >
      <span>{formatNewMessagesLabel(count)}</span>
      <ChevronDown size={14} />
    </button>
  );
}
