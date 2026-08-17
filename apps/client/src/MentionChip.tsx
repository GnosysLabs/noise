import type { ReactNode } from "react";

export function MentionChip({
  label,
  title,
  avatar,
  onClick,
}: {
  label: string;
  title?: string;
  avatar?: ReactNode;
  onClick?: () => void;
}) {
  const body = (
    <>
      {avatar}
      <span>{label}</span>
    </>
  );
  if (!onClick) {
    return <span className="message-mention" title={title}>{body}</span>;
  }
  return (
    <button
      type="button"
      className="message-mention"
      title={title}
      onClick={(event) => {
        event.stopPropagation();
        onClick();
      }}
    >
      {body}
    </button>
  );
}
