const MAX_MEDIA_ALBUM_ITEMS = 10;

export type AlbumGroupableMessage = {
  event_id: string;
  author_public_key: string;
  attachment?: {
    mime_type: string;
    file_name?: string | null;
    media_album_id?: string | null;
  } | null;
  local_attachment?: {
    mime_type: string;
    file_name?: string | null;
    media_album_id?: string | null;
  };
  forwarded_from?: { public_key: string; username: string } | null;
};

export type MediaMessageGroup<T extends AlbumGroupableMessage> = {
  messages: T[];
};

export function createMediaAlbumId() {
  const bytes = new Uint8Array(16);
  crypto.getRandomValues(bytes);
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

export function isCollageMediaMime(mimeType: string, fileName?: string | null) {
  return (mimeType.startsWith("image/") || mimeType.startsWith("video/"))
    && !fileName?.toLowerCase().startsWith("klipy-sticker-");
}

export function mediaAlbumIdForPending(
  items: Array<{ mimeType: string; name?: string | null } | null>,
) {
  const collageCount = items.filter((item) => (
    item !== null && isCollageMediaMime(item.mimeType, item.name)
  )).length;
  return collageCount >= 2 ? createMediaAlbumId() : null;
}

export function withMediaAlbumId<T extends {
  mime_type: string;
  file_name?: string | null;
  media_album_id?: string | null;
}>(
  attachment: T | null,
  albumId: string | null,
) {
  if (!attachment || !albumId) return attachment;
  if (!isCollageMediaMime(attachment.mime_type, attachment.file_name)) return attachment;
  return { ...attachment, media_album_id: albumId };
}

export function messageMediaAlbumId(message: AlbumGroupableMessage) {
  return message.attachment?.media_album_id
    ?? message.local_attachment?.media_album_id
    ?? null;
}

function messageMediaMime(message: AlbumGroupableMessage) {
  return message.local_attachment?.mime_type ?? message.attachment?.mime_type ?? "";
}

function messageMediaFileName(message: AlbumGroupableMessage) {
  return message.local_attachment?.file_name ?? message.attachment?.file_name ?? "";
}

export function isCollageMediaMessage(message: AlbumGroupableMessage) {
  return isCollageMediaMime(messageMediaMime(message), messageMediaFileName(message));
}

function canAppendToMediaAlbum(
  album: AlbumGroupableMessage[],
  candidate: AlbumGroupableMessage,
) {
  const previous = album.at(-1);
  if (!previous || album.length >= MAX_MEDIA_ALBUM_ITEMS) return false;
  const albumId = messageMediaAlbumId(previous);
  return Boolean(albumId)
    && albumId === messageMediaAlbumId(candidate)
    && isCollageMediaMessage(candidate)
    && candidate.author_public_key === previous.author_public_key
    && !previous.forwarded_from
    && !candidate.forwarded_from;
}

export function groupMediaMessages<T extends AlbumGroupableMessage>(messages: T[]) {
  const groups: MediaMessageGroup<T>[] = [];
  for (const message of messages) {
    const previousGroup = groups.at(-1);
    if (
      previousGroup
      && isCollageMediaMessage(previousGroup.messages[0])
      && canAppendToMediaAlbum(previousGroup.messages, message)
    ) {
      previousGroup.messages.push(message);
    } else {
      groups.push({ messages: [message] });
    }
  }
  return groups;
}
