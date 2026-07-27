type WasmSession = {
  clear_session(): void;
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
const ACCOUNT_REGISTRY_KEY = "account-registry-v1";
const ADDITIONAL_DATA = new TextEncoder().encode("makenoise.chat browser vault v1");

export type BrowserAccountRecord = {
  id: string;
  public_key: string;
  username: string;
  bio: string;
  avatar: unknown | null;
};

export type BrowserAccountList = {
  active_account_id: string | null;
  adding_account: boolean;
  accounts: BrowserAccountRecord[];
};

type BrowserAccountRegistry = {
  version: 1;
  activeAccountID: string;
  addingFromAccountID: string | null;
  accounts: BrowserAccountRecord[];
};

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

async function accountRegistry(): Promise<BrowserAccountRegistry> {
  const existing = await readValue<BrowserAccountRegistry>(ACCOUNT_REGISTRY_KEY);
  if (existing?.version === 1 && existing.activeAccountID) return existing;
  return {
    version: 1,
    activeAccountID: "legacy",
    addingFromAccountID: null,
    accounts: [],
  };
}

function accountStateKey(accountID: string) {
  return accountID === "legacy" ? STATE_KEY : `${STATE_KEY}:${accountID}`;
}

async function restoreAccount(wasm: WasmSession, accountID: string) {
  const encrypted = await readValue<EncryptedVault>(accountStateKey(accountID));
  if (!encrypted) {
    wasm.clear_session();
    return;
  }
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

export async function restoreBrowserVault(wasm: WasmSession) {
  const registry = await accountRegistry();
  await restoreAccount(wasm, registry.activeAccountID);
}

export async function persistBrowserVault(wasm: WasmSession) {
  const registry = await accountRegistry();
  const stateKey = accountStateKey(registry.activeAccountID);
  const state = wasm.session_state();
  if (!state.byteLength) {
    await deleteValue(stateKey);
    return;
  }
  const key = await deviceKey();
  const iv = crypto.getRandomValues(new Uint8Array(12));
  const ciphertext = await crypto.subtle.encrypt(
    { name: "AES-GCM", iv: ownedBuffer(iv), additionalData: ownedBuffer(ADDITIONAL_DATA) },
    key,
    ownedBuffer(state),
  );
  await writeValue(stateKey, { version: 1, iv, ciphertext } satisfies EncryptedVault);
}

export async function browserAccountList(): Promise<BrowserAccountList> {
  const registry = await accountRegistry();
  return {
    active_account_id: registry.activeAccountID,
    adding_account: registry.addingFromAccountID !== null,
    accounts: registry.accounts,
  };
}

export async function updateBrowserAccount(summary: {
  identity: {
    public_key: string;
    username: string;
    bio: string;
    avatar: unknown | null;
  };
}, accountID: string) {
  const registry = await accountRegistry();
  if (registry.activeAccountID !== accountID) return;
  const record: BrowserAccountRecord = {
    id: accountID,
    public_key: summary.identity.public_key,
    username: summary.identity.username,
    bio: summary.identity.bio,
    avatar: summary.identity.avatar,
  };
  const index = registry.accounts.findIndex((account) => account.id === record.id);
  if (index >= 0) registry.accounts[index] = record;
  else registry.accounts.push(record);
  registry.addingFromAccountID = null;
  await writeValue(ACCOUNT_REGISTRY_KEY, registry);
}

export async function startAddingBrowserAccount(wasm: WasmSession) {
  await persistBrowserVault(wasm);
  const registry = await accountRegistry();
  if (registry.addingFromAccountID !== null) return;
  registry.addingFromAccountID = registry.activeAccountID;
  registry.activeAccountID = `account-${Date.now().toString(16)}${crypto.randomUUID().replaceAll("-", "")}`;
  await writeValue(ACCOUNT_REGISTRY_KEY, registry);
  wasm.clear_session();
}

export async function cancelAddingBrowserAccount(_wasm: WasmSession) {
  const registry = await accountRegistry();
  if (!registry.addingFromAccountID) return;
  const pendingID = registry.activeAccountID;
  const previousID = registry.addingFromAccountID;
  await deleteValue(accountStateKey(pendingID));
  registry.activeAccountID = previousID;
  registry.addingFromAccountID = null;
  await writeValue(ACCOUNT_REGISTRY_KEY, registry);
}

export async function switchBrowserAccount(wasm: WasmSession, accountID: string) {
  await persistBrowserVault(wasm);
  const registry = await accountRegistry();
  if (!registry.accounts.some((account) => account.id === accountID)) {
    throw new Error("that account is no longer signed in on this browser");
  }
  registry.activeAccountID = accountID;
  registry.addingFromAccountID = null;
  await writeValue(ACCOUNT_REGISTRY_KEY, registry);
}

export async function removeActiveBrowserAccount(wasm: WasmSession) {
  const registry = await accountRegistry();
  const removedID = registry.activeAccountID;
  await deleteValue(accountStateKey(removedID));
  registry.accounts = registry.accounts.filter((account) => account.id !== removedID);
  registry.addingFromAccountID = null;
  registry.activeAccountID = registry.accounts[0]?.id ?? "legacy";
  await writeValue(ACCOUNT_REGISTRY_KEY, registry);
  await restoreAccount(wasm, registry.activeAccountID);
}
