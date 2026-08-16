import { useCallback, useEffect, useState } from "react";
import {
  emptyActivityInbox,
  loadActivityInbox,
  markActivityInboxRead,
  mergeActivityNotifications,
  saveActivityInbox,
  unreadActivityCount,
  withoutDirectActivity,
  type ActivityInboxState,
  type ActivityNotification,
} from "./activityNotifications";

export function useActivityInbox(identityPublicKey: string | null) {
  const [state, setState] = useState<ActivityInboxState>(emptyActivityInbox);

  useEffect(() => {
    setState(identityPublicKey ? withoutDirectActivity(loadActivityInbox(identityPublicKey)) : emptyActivityInbox());
  }, [identityPublicKey]);

  const harvest = useCallback((scopeId: string, incoming: ActivityNotification[]) => {
    if (!identityPublicKey) return;
    setState((current) => {
      const next = mergeActivityNotifications(current, scopeId, incoming);
      if (next === current) return current;
      saveActivityInbox(identityPublicKey, next);
      return next;
    });
  }, [identityPublicKey]);

  const markAllRead = useCallback(() => {
    if (!identityPublicKey) return;
    setState((current) => {
      const next = markActivityInboxRead(current);
      if (next === current) return current;
      saveActivityInbox(identityPublicKey, next);
      return next;
    });
  }, [identityPublicKey]);

  const visible = withoutDirectActivity(state);
  return {
    items: visible.items,
    unreadCount: unreadActivityCount(visible),
    harvest,
    markAllRead,
  };
}
