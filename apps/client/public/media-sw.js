// noise media streams: serves decrypted media byte ranges to <video> elements,
// mirroring the desktop app's noise-media:// protocol. The worker owns no
// decryption keys — every range is fetched and decrypted by the page's WASM
// client and relayed here over postMessage.

const MEDIA_PATH_PREFIX = "/noise-media/";
const MAX_RESPONSE_BYTES = 1024 * 1024;
const MAX_STREAMS = 256;
const RANGE_REQUEST_TIMEOUT_MS = 60_000;
const REGISTRATION_QUERY_TIMEOUT_MS = 3_000;

/** streamId -> { attachment, scopeId, ownerId } */
const streams = new Map();
/** requestId -> { resolve, timer } */
const pendingRanges = new Map();
let rangeRequestCounter = 0;

self.addEventListener("install", (event) => {
  event.waitUntil(self.skipWaiting());
});

self.addEventListener("activate", (event) => {
  event.waitUntil(self.clients.claim());
});

self.addEventListener("message", (event) => {
  const message = event.data;
  if (!message || typeof message !== "object") return;

  if (message.type === "noise-media-register") {
    if (typeof message.streamId !== "string" || !message.attachment) return;
    if (streams.size >= MAX_STREAMS && !streams.has(message.streamId)) {
      streams.delete(streams.keys().next().value);
    }
    streams.set(message.streamId, {
      attachment: message.attachment,
      scopeId: message.scopeId ?? null,
      ownerId: event.source && "id" in event.source ? event.source.id : null,
    });
    return;
  }

  if (message.type === "noise-media-range-response") {
    const pending = pendingRanges.get(message.requestId);
    if (!pending) return;
    pendingRanges.delete(message.requestId);
    clearTimeout(pending.timer);
    pending.resolve(message);
  }
});

self.addEventListener("fetch", (event) => {
  const url = new URL(event.request.url);
  if (url.origin !== self.location.origin || !url.pathname.startsWith(MEDIA_PATH_PREFIX)) {
    return;
  }
  const streamId = url.pathname.slice(MEDIA_PATH_PREFIX.length).split("/")[0];
  if (!streamId) return;
  event.respondWith(serveMedia(event, streamId));
});

async function serveMedia(event, streamId) {
  const registration = await registrationFor(streamId);
  if (!registration) return textResponse(404, "media stream is unavailable");

  const total = registration.attachment.byte_length;
  const baseHeaders = {
    "Content-Type": registration.attachment.mime_type || "application/octet-stream",
    "Accept-Ranges": "bytes",
    // Decrypted plaintext must not persist in the browser HTTP cache.
    "Cache-Control": "no-store",
    "X-Content-Type-Options": "nosniff",
  };

  if (event.request.method === "HEAD") {
    return new Response(null, {
      status: 200,
      headers: { ...baseHeaders, "Content-Length": String(total) },
    });
  }
  if (event.request.method !== "GET" || !total) {
    return textResponse(400, "media stream request is invalid");
  }

  const [start, end] = requestedRange(event.request.headers.get("Range"), total);
  const client = await clientFor(registration, event.clientId);
  if (!client) return textResponse(502, "media stream owner is unavailable");

  const reply = await requestRange(client, streamId, start, end - start + 1);
  if (!reply || !reply.ok || !reply.buffer) {
    return textResponse(502, (reply && reply.error) || "media range request failed");
  }
  const bytes = new Uint8Array(reply.buffer);
  if (bytes.byteLength === 0) return textResponse(502, "media range is empty");
  const actualEnd = Math.min(start + bytes.byteLength - 1, total - 1);
  return new Response(bytes, {
    status: 206,
    headers: {
      ...baseHeaders,
      "Content-Length": String(bytes.byteLength),
      "Content-Range": `bytes ${start}-${actualEnd}/${total}`,
    },
  });
}

// Media element probes are often tiny ("bytes=0-1") but a browser needs about
// a megabyte before the first frame; cap every response there, like the
// desktop protocol, so one range request amortizes several chunk downloads.
function requestedRange(header, total) {
  const last = total - 1;
  let start = 0;
  let end = last;
  const match = header && /^bytes=(\d*)-(\d*)/.exec(header);
  if (match) {
    if (match[1] === "") {
      const suffix = Math.min(Number.parseInt(match[2] || "0", 10) || 0, total);
      start = total - suffix;
    } else {
      start = Number.parseInt(match[1], 10);
      end = match[2] === "" ? last : Number.parseInt(match[2], 10);
    }
  }
  start = Math.min(start, last);
  end = Math.max(start, Math.min(end, start + MAX_RESPONSE_BYTES - 1, last));
  return [start, end];
}

async function registrationFor(streamId) {
  if (streams.has(streamId)) return streams.get(streamId);
  // The worker keeps registrations in memory only; after it is killed and
  // restarted, ask every window to re-publish the one it owns.
  const clients = await windowClients();
  for (const client of clients) {
    client.postMessage({ type: "noise-media-registration-query", streamId });
  }
  const deadline = Date.now() + REGISTRATION_QUERY_TIMEOUT_MS;
  while (Date.now() < deadline) {
    if (streams.has(streamId)) return streams.get(streamId);
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  return null;
}

async function clientFor(registration, fallbackClientId) {
  if (registration.ownerId) {
    const owner = await self.clients.get(registration.ownerId);
    if (owner) return owner;
  }
  if (fallbackClientId) {
    const fallback = await self.clients.get(fallbackClientId);
    if (fallback) return fallback;
  }
  const clients = await windowClients();
  return clients[0] ?? null;
}

async function windowClients() {
  return self.clients.matchAll({ type: "window", includeUncontrolled: true });
}

function requestRange(client, streamId, offset, byteLength) {
  const requestId = `${Date.now().toString(36)}-${(rangeRequestCounter += 1)}`;
  return new Promise((resolve) => {
    const timer = setTimeout(() => {
      pendingRanges.delete(requestId);
      resolve(null);
    }, RANGE_REQUEST_TIMEOUT_MS);
    pendingRanges.set(requestId, { resolve, timer });
    client.postMessage({
      type: "noise-media-range",
      requestId,
      streamId,
      offset,
      byteLength,
    });
  });
}

function textResponse(status, message) {
  return new Response(message, {
    status,
    headers: { "Content-Type": "text/plain", "Cache-Control": "no-store" },
  });
}
