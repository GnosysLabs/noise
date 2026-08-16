export const MAX_COMPOSER_MEDIA_ITEMS = 10;
export const MAX_MEDIA_BYTES = 500 * 1024 * 1024;

export type ComposerMediaInfo = {
  mimeType: string;
  byteLength: number;
};

export type ComposerMediaSelection<T> = {
  accepted: T[];
  rejected: T[];
  limited: boolean;
  invalid: boolean;
  oversized: boolean;
};

export function isAcceptedComposerMime(mimeType: string) {
  return /^(image|video|audio)\//.test(mimeType);
}

export function selectComposerMedia<T>(
  items: T[],
  available: number,
  info: (item: T) => ComposerMediaInfo | null,
): ComposerMediaSelection<T> {
  const accepted: T[] = [];
  const rejected: T[] = [];
  let limited = false;
  let invalid = false;
  let oversized = false;
  const slots = Math.max(0, available);

  for (const item of items) {
    const details = info(item);
    if (!details) {
      rejected.push(item);
      continue;
    }
    if (!isAcceptedComposerMime(details.mimeType)) {
      invalid = true;
      rejected.push(item);
      continue;
    }
    if (!details.byteLength || details.byteLength > MAX_MEDIA_BYTES) {
      oversized = true;
      rejected.push(item);
      continue;
    }
    if (accepted.length >= slots) {
      limited = true;
      rejected.push(item);
      continue;
    }
    accepted.push(item);
  }

  return { accepted, rejected, limited, invalid, oversized };
}

export function composerMediaSelectionError({
  limited,
  invalid,
  oversized,
}: {
  limited: boolean;
  invalid: boolean;
  oversized: boolean;
}) {
  const details = [
    limited ? `you can attach up to ${MAX_COMPOSER_MEDIA_ITEMS} media items` : null,
    invalid ? "choose image, video, or audio files" : null,
    oversized ? "each media item can be up to 500 MB" : null,
  ].filter((detail): detail is string => Boolean(detail));
  return details.length ? details.join("; ") : null;
}

export function composerMediaError(error: string | null, limited: boolean) {
  const limitError = `you can attach up to ${MAX_COMPOSER_MEDIA_ITEMS} media items`;
  if (!limited || error?.includes(limitError)) return error;
  return error ? `${limitError}; ${error}` : limitError;
}

export function composerSendBatchSize(attachmentCount: number) {
  return attachmentCount > 0 ? attachmentCount : 1;
}

export function restoreComposerAfterSend<T>({
  confirmedCount,
  attachments,
}: {
  confirmedCount: number;
  attachments: T[];
}) {
  const batchSize = composerSendBatchSize(attachments.length);
  return {
    remainingAttachments: confirmedCount >= batchSize
      ? []
      : attachments.slice(confirmedCount),
    restoreDraft: confirmedCount === 0,
  };
}
