import { AtSign, Bell, Reply, SmilePlus } from "lucide-react";
import { useEffect, useRef, useState, type ReactNode } from "react";
import { overlayDismissesOnMouseDown } from "./useDismissibleOverlay";
import {
  formatActivityTime,
  type ActivityActor,
  type ActivityNotification,
  type ActivityNotificationKind,
} from "./activityNotifications";

export function ActivityInbox({
  items,
  unreadCount,
  onOpen,
  onOpened,
  renderActor,
}: {
  items: ActivityNotification[];
  unreadCount: number;
  onOpen: (item: ActivityNotification) => void;
  onOpened: () => void;
  renderActor: (actor: ActivityActor) => ReactNode;
}) {
  const [open, setOpen] = useState(false);
  const root = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    onOpened();
    const close = (event: MouseEvent) => {
      if (!overlayDismissesOnMouseDown(event.button)) return;
      if (root.current?.contains(event.target as Node)) return;
      setOpen(false);
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") setOpen(false);
    };
    window.addEventListener("mousedown", close);
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("mousedown", close);
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [open, onOpened]);

  return (
    <div className="activity-inbox" ref={root}>
      <button
        type="button"
        className={`activity-bell ${open ? "open" : ""}`}
        aria-label={unreadCount > 0
          ? `${unreadCount} new notification${unreadCount === 1 ? "" : "s"}`
          : "notifications"}
        aria-expanded={open}
        title="notifications"
        onClick={() => setOpen((current) => !current)}
      >
        <Bell size={16} />
        {unreadCount > 0 && (
          <i>{unreadCount > 99 ? "99+" : unreadCount}</i>
        )}
      </button>
      {open && (
        <div className="activity-panel" role="dialog" aria-label="notifications">
          <header>
            <strong>notifications</strong>
            <small>mentions, replies, and reactions</small>
          </header>
          {items.length === 0 ? (
            <div className="activity-empty">
              you will see @mentions, replies to you, and reactions here
            </div>
          ) : (
            <div className="activity-list">
              {items.map((item) => (
                <button
                  key={item.id}
                  type="button"
                  className="activity-row"
                  onClick={() => {
                    setOpen(false);
                    onOpen(item);
                  }}
                >
                  <span className="activity-avatar">
                    {renderActor(item.actor)}
                    <b aria-hidden="true">{kindIcon(item.kind)}</b>
                  </span>
                  <span className="activity-copy">
                    <strong>{headline(item)}</strong>
                    <span>{item.preview}</span>
                    <small>{contextLine(item)}</small>
                  </span>
                </button>
              ))}
            </div>
          )}
        </div>
      )}
    </div>
  );
}

function kindIcon(kind: ActivityNotificationKind) {
  if (kind === "mention") return <AtSign size={9} />;
  if (kind === "reply") return <Reply size={9} />;
  return <SmilePlus size={9} />;
}

function headline(item: ActivityNotification) {
  if (item.kind === "mention") return `${item.actor.username} mentioned you`;
  if (item.kind === "reply") return `${item.actor.username} replied`;
  return `${item.actor.username} reacted ${item.emoji ?? ""}`.trim();
}

function contextLine(item: ActivityNotification) {
  const place = [item.groupName, item.topicName].filter(Boolean).join(" · ");
  return [place, formatActivityTime(item.createdAtMillis)].filter(Boolean).join(" · ");
}
