import { noise } from "./api";
import type { Conversation } from "./types";

export async function loadLocalActivityConversations(groupIds: string[]) {
  try {
    const all = await noise<Conversation[]>({ action: "cached_conversations" });
    if (Array.isArray(all)) return all;
  } catch {
    // Older desktop binaries only expose one group at a time.
  }
  const results = await Promise.all(
    groupIds.map(async (group_id) => {
      try {
        return await noise<Conversation | null>({
          action: "cached_conversation",
          group_id,
        });
      } catch {
        return null;
      }
    }),
  );
  return results.filter((item): item is Conversation => item != null);
}
