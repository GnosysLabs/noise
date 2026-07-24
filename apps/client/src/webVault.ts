type WasmSession = {
  restore_session(bytes: Uint8Array): void;
  session_state(): Uint8Array;
};

type EncryptedVault = {
  version: 1;
  iv: Uint8Array;
  ciphertext: ArrayBuffer;
};

const DATABASE_NAME = "noise-browser";
const STORE_NAME = "private-vault";
const DEVICE_KEY = "device-key";
const STATE_KEY = "encrypted-state";
const ADDITIONAL_DATA = new TextEncoder().encode("makenoise.chat browser vault v1");

let databasePromise: Promise<IDBDatabase> | null = null;

function ownedBuffer(bytes: Uint8Array) {
  return bytes.slice().buffer as ArrayBuffer;
}

function database() {
  if (!databasePromise) {
    databasePromise = new Promise<IDBDatabase>((resolve, reject) => {
      const request = indexedDB.open(DATABASE_NAME, 1);
      request.onupgradeneeded = () => {
        if (!request.result.objectStoreNames.contains(STORE_NAME)) {
          request.result.createObjectStore(STORE_NAME);
        }
      };
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error ?? new Error("the encrypted browser vault could not be opened"));
      request.onblocked = () => reject(new Error("another noise tab is blocking the encrypted browser vault"));
    });
  }
  return databasePromise;
}

async function readValue<T>(key: string): Promise<T | undefined> {
  const db = await database();
  return new Promise<T | undefined>((resolve, reject) => {
    const transaction = db.transaction(STORE_NAME, "readonly");
    const request = transaction.objectStore(STORE_NAME).get(key);
    request.onsuccess = () => resolve(request.result as T | undefined);
    request.onerror = () => reject(request.error ?? new Error("the encrypted browser vault could not be read"));
  });
}

async function writeValue(key: string, value: unknown) {
  const db = await database();
  await new Promise<void>((resolve, reject) => {
    const transaction = db.transaction(STORE_NAME, "readwrite");
    transaction.objectStore(STORE_NAME).put(value, key);
    transaction.oncomplete = () => resolve();
    transaction.onerror = () => reject(transaction.error ?? new Error("the encrypted browser vault could not be saved"));
    transaction.onabort = () => reject(transaction.error ?? new Error("the encrypted browser vault save was aborted"));
  });
}

async function deleteValue(key: string) {
  const db = await database();
  await new Promise<void>((resolve, reject) => {
    const transaction = db.transaction(STORE_NAME, "readwrite");
    transaction.objectStore(STORE_NAME).delete(key);
    transaction.oncomplete = () => resolve();
    transaction.onerror = () => reject(transaction.error ?? new Error("the encrypted browser vault could not be erased"));
    transaction.onabort = () => reject(transaction.error ?? new Error("the encrypted browser vault erase was aborted"));
  });
}

async function deviceKey() {
  const existing = await readValue<CryptoKey>(DEVICE_KEY);
  if (existing) return existing;
  const created = await crypto.subtle.generateKey(
    { name: "AES-GCM", length: 256 },
    false,
    ["encrypt", "decrypt"],
  );
  await writeValue(DEVICE_KEY, created);
  void navigator.storage?.persist?.().catch(() => false);
  return created;
}

export async function restoreBrowserVault(wasm: WasmSession) {
  const encrypted = await readValue<EncryptedVault>(STATE_KEY);
  if (!encrypted) return;
  if (encrypted.version !== 1 || !(encrypted.iv instanceof Uint8Array)) {
    throw new Error("the encrypted browser vault has an unsupported format");
  }
  const key = await readValue<CryptoKey>(DEVICE_KEY);
  if (!key) throw new Error("this browser no longer has the key for its noise vault; sign in again");
  let plaintext: ArrayBuffer;
  try {
    plaintext = await crypto.subtle.decrypt(
      {
        name: "AES-GCM",
        iv: ownedBuffer(encrypted.iv),
        additionalData: ownedBuffer(ADDITIONAL_DATA),
      },
      key,
      encrypted.ciphertext,
    );
  } catch {
    throw new Error("this browser could not unlock its noise vault; sign in again");
  }
  wasm.restore_session(new Uint8Array(plaintext));
}

export async function persistBrowserVault(wasm: WasmSession) {
  const state = wasm.session_state();
  if (!state.byteLength) {
    await deleteValue(STATE_KEY);
    return;
  }
  const key = await deviceKey();
  const iv = crypto.getRandomValues(new Uint8Array(12));
  const ciphertext = await crypto.subtle.encrypt(
    { name: "AES-GCM", iv: ownedBuffer(iv), additionalData: ownedBuffer(ADDITIONAL_DATA) },
    key,
    ownedBuffer(state),
  );
  await writeValue(STATE_KEY, { version: 1, iv, ciphertext } satisfies EncryptedVault);
}
