import type { NoiseRequest } from "./types";

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

export async function noise<T>(request: NoiseRequest): Promise<T | null> {
  if (!isTauri) {
    throw new Error(
      "The browser protocol adapter is not connected yet. The shared interface is ready for its WASM core.",
    );
  }
  const { invoke } = await import("@tauri-apps/api/core");
  const response = await invoke<Envelope<T>>("noise_invoke", { request });
  if (!response.ok) throw new Error(response.error ?? "unknown Noise core error");
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

export async function prepareGroupBackground(file: File): Promise<string> {
  if (!file.type.startsWith("image/")) throw new Error("choose an image file");
  const bitmap = await createImageBitmap(file);
  const scale = Math.min(1, 1920 / bitmap.width, 1080 / bitmap.height);
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
