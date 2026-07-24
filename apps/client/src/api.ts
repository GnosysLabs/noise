import type { NoiseRequest } from "./types";
import { persistBrowserVault, restoreBrowserVault } from "./webVault";

type Envelope<T> = {
  ok: boolean;
  data?: T | null;
  error?: string;
};

const defaultRelays = [
  "https://noiserelay.gnosyslabs.xyz#ohttp=AQAgZEyD0P-eAYiQd9F8r4_4ah2EoI_nWvs4QtUSTbVse1sABAABAAM",
  "https://noiserelay.irisirc.chat#ohttp=AQAggzUeerBJnmwbryX5FUuHI5N7DLozSUnf2kYKnfMmkl8ABAABAAM",
];

const configuredRelays = import.meta.env.VITE_NOISE_RELAYS
  ?.split(",")
  .map((relay: string) => relay.trim().replace(/\/$/, ""))
  .filter(Boolean);

export const relays = configuredRelays?.length ? configuredRelays : defaultRelays;

export const isTauri = "__TAURI_INTERNALS__" in window;
document.documentElement.dataset.runtime = isTauri ? "tauri" : "browser";

let relayDiscoveryStarted = false;
let maskRelays: string[] = [];
let maskRelayOffset = 0;
type BrowserAdapter = {
  default(): Promise<unknown>;
  noise_invoke(request: unknown): Promise<unknown>;
  restore_session(bytes: Uint8Array): void;
  session_state(): Uint8Array;
};
let browserAdapterPromise: Promise<BrowserAdapter> | null = null;
let browserMutationQueue = Promise.resolve();

const browserConcurrentActions = new Set([
  "discover_relay_masks",
  "cached_conversation",
  "fetch_avatar",
  "fetch_attachment",
  "fetch_profile_album",
  "heartbeat_presence",
  "reply_notification_snapshot",
  "status",
  "upload_direct_media_chunk",
  "upload_media_chunk",
  "upload_profile_media_chunk",
  "watch_account",
  "watch_direct",
  "watch_group",
  "watch_group_id",
]);

async function browserAdapter() {
  if (!browserAdapterPromise) {
    const wasmVersion = import.meta.env.VITE_NOISE_WASM_VERSION;
    if (!wasmVersion) throw new Error("this noise web build is missing its WASM version");
    const adapterUrl = `/wasm/noise_web-${wasmVersion}.js`;
    browserAdapterPromise = import(/* @vite-ignore */ adapterUrl).then(async (adapter: BrowserAdapter) => {
      await adapter.default();
      await restoreBrowserVault(adapter);
      return adapter;
    });
  }
  return browserAdapterPromise;
}

async function invokeBrowser<T>(request: NoiseRequest): Promise<T | null> {
  const operation = async () => {
    const adapter = await browserAdapter();
    const response = await adapter.noise_invoke({
      ...request,
      mask_relays: rotateMaskRelays(),
    }) as Envelope<T>;
    if (!response.ok) throw new Error(response.error ?? "unknown noise core error");
    if (!browserConcurrentActions.has(request.action)) {
      await persistBrowserVault(adapter);
    }
    return response.data ?? null;
  };

  if (browserConcurrentActions.has(request.action)) return operation();
  const queued = browserMutationQueue.then(operation, operation);
  browserMutationQueue = queued.then(() => undefined, () => undefined);
  return queued;
}

function rotateMaskRelays() {
  if (maskRelays.length < 2) return maskRelays;
  const offset = maskRelayOffset++ % maskRelays.length;
  return [...maskRelays.slice(offset), ...maskRelays.slice(0, offset)];
}

function startRelayDiscovery() {
  if (!isTauri || relayDiscoveryStarted) return;
  relayDiscoveryStarted = true;
  void (async () => {
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      const response = await invoke<Envelope<string[]>>("noise_invoke", {
        request: { action: "discover_relay_masks", relays },
      });
      if (response.ok && response.data) maskRelays = response.data;
    } catch {
      // The pinned seed relays remain the privacy fallback.
    }
  })();
}

export async function noise<T>(request: NoiseRequest): Promise<T | null> {
  if (!isTauri) {
    return invokeBrowser<T>(request);
  }
  startRelayDiscovery();
  const { invoke } = await import("@tauri-apps/api/core");
  const response = await invoke<Envelope<T>>("noise_invoke", {
    request: { ...request, mask_relays: rotateMaskRelays() },
  });
  if (!response.ok) throw new Error(response.error ?? "unknown noise core error");
  return response.data ?? null;
}

export async function prepareImage(file: File): Promise<string> {
  const bitmap = await createImageBitmap(file);
  const size = 256;
  const canvas = document.createElement("canvas");
  canvas.width = size;
  canvas.height = size;
  const context = canvas.getContext("2d");
  if (!context) throw new Error("this browser cannot prepare images");
  const scale = Math.max(size / bitmap.width, size / bitmap.height);
  const width = bitmap.width * scale;
  const height = bitmap.height * scale;
  context.fillStyle = "#000";
  context.fillRect(0, 0, size, size);
  context.drawImage(bitmap, (size - width) / 2, (size - height) / 2, width, height);
  bitmap.close();
  const blob = await new Promise<Blob>((resolve, reject) =>
    canvas.toBlob(
      (value) => (value ? resolve(value) : reject(new Error("image encoding failed"))),
      "image/jpeg",
      0.78,
    ),
  );
  const bytes = new Uint8Array(await blob.arrayBuffer());
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary);
}

export async function prepareGroupBackground(file: File, variant: "desktop" | "mobile" = "desktop"): Promise<string> {
  if (!file.type.startsWith("image/")) throw new Error("choose an image file");
  const bitmap = await createImageBitmap(file);
  const maximumWidth = variant === "mobile" ? 1290 : 1920;
  const maximumHeight = variant === "mobile" ? 2796 : 1080;
  const scale = Math.min(1, maximumWidth / bitmap.width, maximumHeight / bitmap.height);
  const canvas = document.createElement("canvas");
  canvas.width = Math.max(1, Math.round(bitmap.width * scale));
  canvas.height = Math.max(1, Math.round(bitmap.height * scale));
  const context = canvas.getContext("2d");
  if (!context) {
    bitmap.close();
    throw new Error("this browser cannot prepare images");
  }
  context.drawImage(bitmap, 0, 0, canvas.width, canvas.height);
  bitmap.close();

  let blob: Blob | null = null;
  for (const quality of [0.82, 0.72, 0.62]) {
    blob = await new Promise<Blob | null>((resolve) => canvas.toBlob(resolve, "image/jpeg", quality));
    if (blob && blob.size <= 1536 * 1024) break;
  }
  if (!blob || !blob.size || blob.size > 1536 * 1024) {
    throw new Error("this image could not be prepared under the 1.5 MB encrypted background limit");
  }
  const bytes = new Uint8Array(await blob.arrayBuffer());
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary);
}
