import type { LocalSummary, NoiseRequest, ProfileImage } from "./types";
import {
  browserAccountList,
  cancelAddingBrowserAccount,
  persistBrowserVault,
  removeActiveBrowserAccount,
  restoreBrowserVault,
  startAddingBrowserAccount,
  switchBrowserAccount,
  updateBrowserAccount,
} from "./webVault";

type Envelope<T> = {
  ok: boolean;
  data?: T | null;
  error?: string;
};

export const centralUrl =
  import.meta.env.VITE_NOISE_CENTRAL_URL?.trim()
  || (import.meta.env.DEV ? "http://127.0.0.1:4302" : "https://api.makenoise.chat");

// Retain the field name until the cross-platform request schema is renamed,
// but there is exactly one transport endpoint and no runtime override or
// fallback to the retired relay network.
export const relays = [centralUrl];
export const noiseSafetyUrl = import.meta.env.VITE_NOISE_SAFETY_URL?.trim()
  || (import.meta.env.DEV ? "http://127.0.0.1:4310" : null);
export const noiseSafetyPublicKey =
  import.meta.env.VITE_NOISE_SAFETY_PUBLIC_KEY?.trim() || null;
export const noiseSafetyDirectiveSigningPublicKey =
  import.meta.env.VITE_NOISE_SAFETY_DIRECTIVE_SIGNING_PUBLIC_KEY?.trim() || null;

export const isTauri = "__TAURI_INTERNALS__" in window;
document.documentElement.dataset.runtime = isTauri ? "tauri" : "browser";

type BrowserAdapter = {
  default(): Promise<unknown>;
  clear_session(): void;
  noise_invoke(request: unknown): Promise<unknown>;
  restore_session(bytes: Uint8Array): void;
  session_state(): Uint8Array;
};
let browserAdapterPromise: Promise<BrowserAdapter> | null = null;
let browserMutationQueue = Promise.resolve();
let browserAccountGeneration = 0;
let localAccountTransitioning = false;

const browserConcurrentActions = new Set([
  "discover_relay_masks",
  "cached_conversation",
  "fetch_avatar",
  "fetch_attachment",
  "fetch_attachment_range",
  "fetch_klipy_media",
  "fetch_link_preview",
  "fetch_profile_album",
  "group_has_pending_admissions",
  "heartbeat_presence",
  "reply_notification_snapshot",
  "resolve_contact_signal",
  "search_local",
  "status",
  "upload_direct_media_chunk",
  "upload_direct_media_chunk_to",
  "upload_media_chunk",
  "upload_media_chunk_to_group",
  "upload_profile_media_chunk",
  "watch_account",
  "watch_read_state",
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
  const accountGeneration = browserAccountGeneration;
  const operation = async () => {
    const adapter = await browserAdapter();
    const response = await adapter.noise_invoke({
      ...request,
      central_url: centralUrl,
    }) as Envelope<T>;
    if (!response.ok) throw new Error(response.error ?? "unknown noise core error");
    if (accountGeneration !== browserAccountGeneration) return null;
    if (!browserConcurrentActions.has(request.action)) {
      await persistBrowserVault(adapter);
    }
    const data = response.data ?? null;
    if (
      data
      && typeof data === "object"
      && "identity" in data
    ) {
      const accountId = (await browserAccountList()).active_account_id;
      if (accountGeneration !== browserAccountGeneration || !accountId) return null;
      await updateBrowserAccount(data as unknown as LocalSummary, accountId);
    }
    if (request.action === "logout" || request.action === "delete_account") {
      await removeActiveBrowserAccount(adapter);
    }
    return data;
  };

  if (browserConcurrentActions.has(request.action)) return operation();
  return enqueueBrowserMutation(operation);
}

function enqueueBrowserMutation<T>(operation: () => Promise<T>): Promise<T> {
  const queued = browserMutationQueue.then(operation, operation);
  browserMutationQueue = queued.then(() => undefined, () => undefined);
  return queued;
}

export async function noise<T>(request: NoiseRequest): Promise<T | null> {
  if (localAccountTransitioning) {
    throw new Error("local account transition in progress");
  }
  if (!isTauri) {
    return invokeBrowser<T>(request);
  }
  const { invoke } = await import("@tauri-apps/api/core");
  const response = await invoke<Envelope<T>>("noise_invoke", {
    request: { ...request, central_url: centralUrl },
  });
  if (!response.ok) throw new Error(response.error ?? "unknown noise core error");
  return response.data ?? null;
}

export type LocalAccount = {
  id: string;
  public_key: string;
  username: string;
  bio: string;
  avatar: ProfileImage | null;
};

export type LocalAccountList = {
  active_account_id: string | null;
  adding_account: boolean;
  accounts: LocalAccount[];
};

export async function listLocalAccounts(): Promise<LocalAccountList> {
  if (!isTauri) {
    return browserAccountList() as Promise<LocalAccountList>;
  }
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<LocalAccountList>("list_local_accounts");
}

export async function startAddingLocalAccount() {
  await runLocalAccountTransition(async () => {
    if (!isTauri) {
      const adapter = await browserAdapter();
      browserAccountGeneration += 1;
      await enqueueBrowserMutation(() => startAddingBrowserAccount(adapter));
      return;
    }
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("start_adding_local_account");
  });
}

export async function cancelAddingLocalAccount() {
  await runLocalAccountTransition(async () => {
    if (!isTauri) {
      const adapter = await browserAdapter();
      browserAccountGeneration += 1;
      await enqueueBrowserMutation(() => cancelAddingBrowserAccount(adapter));
      return;
    }
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("cancel_adding_local_account");
  });
}

export async function switchLocalAccount(accountId: string) {
  await runLocalAccountTransition(async () => {
    if (!isTauri) {
      const adapter = await browserAdapter();
      browserAccountGeneration += 1;
      await enqueueBrowserMutation(() => switchBrowserAccount(adapter, accountId));
      return;
    }
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("switch_local_account", { accountId });
  });
}

async function runLocalAccountTransition(operation: () => Promise<void>) {
  if (localAccountTransitioning) return;
  localAccountTransitioning = true;
  try {
    await operation();
  } catch (cause) {
    localAccountTransitioning = false;
    throw cause;
  }
}

export async function registerMediaStream(request: NoiseRequest) {
  if (!isTauri) return null;
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<string>("register_media_stream", {
    request: {
      ...request,
      relays,
      central_url: centralUrl,
    },
  });
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
