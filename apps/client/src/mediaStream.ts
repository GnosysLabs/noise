import { isTauri, noise, relays } from "./api";
import type { AttachmentRangeData, MediaAttachment } from "./types";

// Page-side half of the media stream service worker (public/media-sw.js).
// Registering a stream is instant: the video element receives a same-origin
// URL immediately and the worker asks this registry for decrypted ranges as
// the browser requests them.

type StreamRegistration = {
  attachment: MediaAttachment;
  scopeId?: string;
};

const MAX_STREAMS = 256;
const registry = new Map<string, StreamRegistration>();
let streamCounter = 0;
let initialized = false;
let controlled = false;

export function webMediaStreamReady() {
  return controlled;
}

export function initWebMediaStreams() {
  if (initialized || isTauri || !("serviceWorker" in navigator)) return;
  initialized = true;
  controlled = Boolean(navigator.serviceWorker.controller);
  navigator.serviceWorker.addEventListener("controllerchange", () => {
    controlled = Boolean(navigator.serviceWorker.controller);
    publishRegistrations();
  });
  navigator.serviceWorker.addEventListener("message", (event) => {
    void handleWorkerMessage(event);
  });
  void navigator.serviceWorker.register("/media-sw.js").catch(() => {
    controlled = false;
  });
}

export function registerWebMediaStream(
  attachment: MediaAttachment,
  scopeId?: string,
): string {
  const id = `${Date.now().toString(16).padStart(16, "0")}${(streamCounter += 1)
    .toString(16)
    .padStart(16, "0")}`;
  if (registry.size >= MAX_STREAMS && !registry.has(id)) {
    const oldest = registry.keys().next().value;
    if (oldest) registry.delete(oldest);
  }
  const registration: StreamRegistration = { attachment, scopeId };
  registry.set(id, registration);
  publishRegistration(id, registration);
  return `/noise-media/${id}/media.${mediaFileExtension(attachment)}`;
}

function publishRegistrations() {
  for (const [id, registration] of registry) publishRegistration(id, registration);
}

function publishRegistration(id: string, registration: StreamRegistration) {
  navigator.serviceWorker.controller?.postMessage({
    type: "noise-media-register",
    streamId: id,
    attachment: registration.attachment,
    scopeId: registration.scopeId ?? null,
  });
}

async function handleWorkerMessage(event: MessageEvent) {
  const message = event.data;
  if (!message || typeof message !== "object") return;

  if (message.type === "noise-media-registration-query") {
    const registration = registry.get(message.streamId);
    if (registration) publishRegistration(message.streamId, registration);
    return;
  }

  if (message.type !== "noise-media-range") return;
  const { requestId, streamId, offset, byteLength } = message as {
    requestId: string;
    streamId: string;
    offset: number;
    byteLength: number;
  };
  const respond = (payload: Record<string, unknown>, transfer?: Transferable[]) => {
    navigator.serviceWorker.controller?.postMessage(
      { type: "noise-media-range-response", requestId, ...payload },
      { transfer },
    );
  };
  const registration = registry.get(streamId);
  if (!registration) {
    respond({ ok: false, error: "media stream is not registered" });
    return;
  }
  try {
    const data = await noise<AttachmentRangeData>({
      action: "fetch_attachment_range",
      attachment: registration.attachment,
      scope_id: registration.scopeId,
      offset,
      byte_length: byteLength,
      relays,
    });
    if (!data) throw new Error("media range is unavailable");
    const bytes = decodeBase64(data.data_base64);
    respond({ ok: true, buffer: bytes.buffer }, [bytes.buffer]);
  } catch (cause) {
    respond({
      ok: false,
      error: cause instanceof Error ? cause.message : "media range request failed",
    });
  }
}

function decodeBase64(value: string) {
  const binary = atob(value);
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index += 1) {
    bytes[index] = binary.charCodeAt(index);
  }
  return bytes;
}

function mediaFileExtension(attachment: MediaAttachment) {
  const fromName = attachment.file_name.split(".").pop()?.toLowerCase();
  if (
    fromName
    && fromName !== attachment.file_name.toLowerCase()
    && fromName.length <= 8
    && /^[a-z0-9]+$/.test(fromName)
  ) {
    return fromName;
  }
  switch (attachment.mime_type) {
    case "video/quicktime":
      return "mov";
    case "video/x-m4v":
      return "m4v";
    default:
      return "mp4";
  }
}
