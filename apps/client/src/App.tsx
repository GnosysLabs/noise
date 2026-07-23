import {
  ArrowLeft,
  ArrowUp,
  AudioWaveform,
  Camera,
  Check,
  ChevronLeft,
  ChevronRight,
  Copy,
  Crown,
  Download,
  Images,
  Info,
  LoaderCircle,
  LogOut,
  MessageCircle,
  MessagesSquare,
  MoreHorizontal,
  Paperclip,
  Pause,
  Play,
  Plus,
  Radio,
  Reply,
  ScrollText,
  Settings2,
  Shield,
  ShieldOff,
  SmilePlus,
  Trash2,
  TriangleAlert,
  UserRoundX,
  UsersRound,
  Volume2,
  VolumeX,
  X,
} from "lucide-react";
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import type { CSSProperties, RefObject } from "react";
import { relaunch } from "@tauri-apps/plugin-process";
import { check } from "@tauri-apps/plugin-updater";
import { isTauri, noise, prepareGroupBackground, prepareImage, relays } from "./api";
import { generateGroupAvatar, generateUserAvatar } from "./groupAvatar";
import { ReactionPicker } from "./ReactionPicker";
import type {
  AttachmentData,
  AvatarData,
  BannedMemberSummary,
  Conversation,
  DirectConversation,
  DirectInbox,
  DirectSummary,
  GroupActivityResult,
  GroupEncryptionStatus,
  GroupSummary,
  GroupWatch,
  IdentitySummary,
  LocalSummary,
  MakeResult,
  MediaAttachment,
  MediaChunk,
  MemberSummary,
  MessageSummary,
  ProfileImage,
  ReactionSummary,
  ReportSummary,
  SentMessageResult,
} from "./types";

type Dialog =
  | { type: "make" }
  | { type: "join" }
  | { type: "frequency"; group: string; frequency: string }
  | { type: "noise_id"; noiseId: string }
  | { type: "profile"; profile: IdentitySummary }
  | { type: "group"; group: GroupSummary }
  | { type: "rules"; group: GroupSummary }
  | { type: "media" }
  | { type: "reports" }
  | { type: "report_message"; message: MessageSummary }
  | { type: "delete_message"; message: MessageSummary; scopeId: string }
  | { type: "ban_member"; member: MemberSummary }
  | { type: "leave_group"; group: GroupSummary }
  | { type: "delete_group"; group: GroupSummary }
  | { type: "delete_direct"; direct: DirectSummary }
  | { type: "delete_account" }
  | { type: "logout" }
  | { type: "person"; person: PersonSummary };

type PersonSummary = Pick<MemberSummary, "public_key" | "username" | "bio" | "avatar" | "accepts_direct_messages"> & {
  presence_status?: PresenceStatus;
};
type SidebarMode = "groups" | "directs";
type PresenceStatus = "online" | "recently-active" | "offline";
const PRESENCE_IDLE_MILLIS = 5 * 60_000;
const PRESENCE_HEARTBEAT_MILLIS = 20_000;
const PRESENCE_OBSERVATION_STALE_MILLIS = 70_000;
const DEFAULT_ACCENT_COLOR = "#7758ED";
const ACCENT_PRESETS = ["#7758ED", "#E84D8A", "#F06A3C", "#E0A82E", "#43B581", "#24A6A6", "#4D82F0", "#A45EE5"];

function presenceStatusesFromWatch(change: GroupWatch) {
  const statuses = new Map<string, PresenceStatus>();
  for (const publicKey of change.recently_active_public_keys ?? []) {
    statuses.set(publicKey, "recently-active");
  }
  for (const publicKey of change.online_public_keys ?? []) {
    statuses.set(publicKey, "online");
  }
  return statuses;
}
const UPDATE_CHECK_INTERVAL_MS = 30 * 60 * 1000;
const UPDATE_CHECK_HEARTBEAT_MS = 60 * 1000;
let cachedAppVersion: string | null = null;
let notificationPermissionPromise: Promise<boolean> | null = null;

async function ensureNotificationPermission() {
  if (!isTauri) return false;
  if (!notificationPermissionPromise) {
    notificationPermissionPromise = (async () => {
      const { invoke } = await import("@tauri-apps/api/core");
      return invoke<boolean>("ensure_native_notification_permission", { relays });
    })().catch(() => false);
  }
  return notificationPermissionPromise;
}

function accentStyle(value?: string | null): CSSProperties {
  const accent = /^#[0-9a-f]{6}$/i.test(value ?? "") ? value!.toUpperCase() : DEFAULT_ACCENT_COLOR;
  const red = Number.parseInt(accent.slice(1, 3), 16);
  const green = Number.parseInt(accent.slice(3, 5), 16);
  const blue = Number.parseInt(accent.slice(5, 7), 16);
  const contrast = (red * 299 + green * 587 + blue * 114) / 1000 > 158 ? "#171519" : "#FFFFFF";
  const light = [red, green, blue].map((channel) => Math.round(channel * 0.64 + 255 * 0.36));
  const dark = [red, green, blue].map((channel) => Math.round(channel * 0.78));
  return {
    "--accent": accent,
    "--accent-rgb": `${red}, ${green}, ${blue}`,
    "--accent-contrast": contrast,
    "--accent-light": `rgb(${light.join(", ")})`,
    "--accent-dark": `rgb(${dark.join(", ")})`,
  } as CSSProperties;
}

function NoiseMark({ size, className }: { size: number; className?: string }) {
  return (
    <svg
      aria-hidden="true"
      className={className}
      width={size}
      height={size}
      viewBox="160 220 704 584"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
    >
      <defs>
        <linearGradient id="noise-mark-gradient" x1="214" y1="512" x2="810" y2="512" gradientUnits="userSpaceOnUse">
          <stop stopColor="var(--accent-light)" />
          <stop offset="1" stopColor="var(--accent-dark)" />
        </linearGradient>
      </defs>
      <path
        d="M206 512h72l55-144 94 296 91-390 91 476 86-382 53 144h70"
        stroke="url(#noise-mark-gradient)"
        strokeWidth="64"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

function CopyButton({
  value,
  label,
  iconOnly = false,
  disabled = false,
  className = "",
}: {
  value: string;
  label: string;
  iconOnly?: boolean;
  disabled?: boolean;
  className?: string;
}) {
  const [copied, setCopied] = useState(false);
  const resetTimer = useRef<number | null>(null);

  useEffect(() => () => {
    if (resetTimer.current !== null) window.clearTimeout(resetTimer.current);
  }, []);

  const copy = async () => {
    await navigator.clipboard.writeText(value);
    setCopied(true);
    if (resetTimer.current !== null) window.clearTimeout(resetTimer.current);
    resetTimer.current = window.setTimeout(() => setCopied(false), 1600);
  };

  const accessibleLabel = copied ? "copied" : label;
  return (
    <button
      type="button"
      className={`copy-action ${copied ? "copied" : ""} ${className}`.trim()}
      disabled={disabled}
      onClick={() => void copy()}
      aria-label={iconOnly ? accessibleLabel : undefined}
      title={iconOnly ? accessibleLabel : undefined}
    >
      {copied ? <Check size={14} /> : <Copy size={14} />}
      {!iconOnly && (copied ? "copied" : label)}
    </button>
  );
}

const avatarCache = new Map<string, string>();
const profileImageRequests = new Map<string, Promise<string>>();
let profileImageCacheGeneration = 0;
const mediaCache = new Map<string, string>();
const mediaLoadPromises = new Map<string, Promise<string>>();
const mediaPreparationPromises = new Map<string, Promise<void>>();
const decodedImageCache = new Set<string>();
const MEDIA_DIMENSIONS_STORAGE_KEY = "noise.media-dimensions.v1";
const mediaDimensionCache = loadStoredMediaDimensions();
const sentMediaPreviewCache = new Map<string, NonNullable<MessageSummary["local_attachment"]>>();
const imagePosterCache = new Map<string, string>();
const videoPosterCache = new Map<string, string>();
const renderedMessageCounts = new Map<string, number>();
let mediaCacheGeneration = 0;

const INITIAL_MESSAGE_COUNT = 24;
const MESSAGE_PAGE_SIZE = 40;

function mediaCacheKey(attachment: MediaAttachment) {
  return attachment.chunks.map((chunk) => chunk.blob_id).join(":");
}

function clearMediaMemoryCache() {
  mediaCacheGeneration += 1;
  const previews = new Set(
    [...sentMediaPreviewCache.values()].map((attachment) => attachment.preview_url),
  );
  for (const preview of previews) URL.revokeObjectURL(preview);
  sentMediaPreviewCache.clear();
  imagePosterCache.clear();
  videoPosterCache.clear();
  decodedImageCache.clear();
  mediaDimensionCache.clear();
  try {
    window.localStorage.removeItem(MEDIA_DIMENSIONS_STORAGE_KEY);
  } catch {
    // The in-memory cache is still cleared when storage is unavailable.
  }
  mediaLoadPromises.clear();
  mediaPreparationPromises.clear();
  mediaCache.clear();
}

function clearProfileImageMemoryCache() {
  profileImageCacheGeneration += 1;
  avatarCache.clear();
  profileImageRequests.clear();
}

function loadProfileImageSource(image: ProfileImage) {
  const cached = avatarCache.get(image.blob_id);
  if (cached) return Promise.resolve(cached);
  const pending = profileImageRequests.get(image.blob_id);
  if (pending) return pending;

  const generation = profileImageCacheGeneration;
  let request: Promise<string>;
  request = noise<AvatarData>({ action: "fetch_avatar", image, relays })
    .then((data) => {
      if (!data) throw new Error("the image could not be loaded");
      const source = `data:${data.mime_type};base64,${data.data_base64}`;
      if (generation === profileImageCacheGeneration) avatarCache.set(image.blob_id, source);
      return source;
    })
    .finally(() => {
      if (profileImageRequests.get(image.blob_id) === request) {
        profileImageRequests.delete(image.blob_id);
      }
    });
  profileImageRequests.set(image.blob_id, request);
  return request;
}

type PendingMedia = {
  name: string;
  mimeType: string;
  byteLength: number;
  file: File;
  previewUrl: string;
  mediaPreview: Promise<MediaPreview | null> | null;
};

type MediaPreview = {
  dataBase64: string;
  mimeType: "image/jpeg";
  pixelWidth: number;
  pixelHeight: number;
};

function prepareImagePreview(file: File): Promise<MediaPreview | null> {
  const source = URL.createObjectURL(file);
  const image = new Image();
  return new Promise((resolve) => {
    const finish = (value: MediaPreview | null) => {
      URL.revokeObjectURL(source);
      resolve(value);
    };
    image.onload = async () => {
      try {
        const pixelWidth = image.naturalWidth;
        const pixelHeight = image.naturalHeight;
        if (!pixelWidth || !pixelHeight) return finish(null);
        const scale = Math.min(1, 360 / Math.max(pixelWidth, pixelHeight));
        const canvas = document.createElement("canvas");
        canvas.width = Math.max(1, Math.round(pixelWidth * scale));
        canvas.height = Math.max(1, Math.round(pixelHeight * scale));
        const context = canvas.getContext("2d");
        if (!context) return finish(null);
        context.fillStyle = "#17161a";
        context.fillRect(0, 0, canvas.width, canvas.height);
        context.drawImage(image, 0, 0, canvas.width, canvas.height);
        let preview = await new Promise<Blob | null>((done) =>
          canvas.toBlob(done, "image/jpeg", 0.62)
        );
        if (preview && preview.size > 58_000) {
          preview = await new Promise<Blob | null>((done) =>
            canvas.toBlob(done, "image/jpeg", 0.42)
          );
        }
        if (!preview) return finish(null);
        const dataBase64 = await fileBase64(preview);
        if (dataBase64.length > 80_000) return finish(null);
        finish({
          dataBase64,
          mimeType: "image/jpeg",
          pixelWidth,
          pixelHeight,
        });
      } catch {
        finish(null);
      }
    };
    image.onerror = () => finish(null);
    image.src = source;
  });
}

function prepareVideoPreview(file: File): Promise<MediaPreview | null> {
  const source = URL.createObjectURL(file);
  const video = document.createElement("video");
  video.muted = true;
  video.playsInline = true;
  video.preload = "auto";
  return new Promise((resolve) => {
    let settled = false;
    let capturing = false;
    let previewTimes: number[] = [];
    let previewIndex = 0;
    const timeout = window.setTimeout(() => finish(null), 15_000);
    const finish = (value: MediaPreview | null) => {
      if (settled) return;
      settled = true;
      window.clearTimeout(timeout);
      video.removeAttribute("src");
      video.load();
      URL.revokeObjectURL(source);
      resolve(value);
    };
    const capture = async () => {
      if (capturing || settled || !video.videoWidth || !video.videoHeight) return;
      capturing = true;
      try {
        if (videoFrameIsNearBlack(video) && previewIndex < previewTimes.length - 1) {
          previewIndex += 1;
          capturing = false;
          video.currentTime = previewTimes[previewIndex];
          return;
        }
        const profiles = [
          { edge: 840, quality: 0.82 },
          { edge: 840, quality: 0.72 },
          { edge: 720, quality: 0.8 },
          { edge: 720, quality: 0.68 },
          { edge: 600, quality: 0.76 },
          { edge: 600, quality: 0.62 },
          { edge: 480, quality: 0.68 },
        ];
        let dataBase64: string | null = null;
        for (const profile of profiles) {
          const scale = Math.min(1, profile.edge / Math.max(video.videoWidth, video.videoHeight));
          const canvas = document.createElement("canvas");
          canvas.width = Math.max(1, Math.round(video.videoWidth * scale));
          canvas.height = Math.max(1, Math.round(video.videoHeight * scale));
          const context = canvas.getContext("2d");
          if (!context) return finish(null);
          context.drawImage(video, 0, 0, canvas.width, canvas.height);
          const preview = await new Promise<Blob | null>((done) =>
            canvas.toBlob(done, "image/jpeg", profile.quality)
          );
          if (!preview || preview.size > 58_000) continue;
          const encoded = await fileBase64(preview);
          if (encoded.length <= 80_000) {
            dataBase64 = encoded;
            break;
          }
        }
        if (!dataBase64) return finish(null);
        finish({
          dataBase64,
          mimeType: "image/jpeg",
          pixelWidth: video.videoWidth,
          pixelHeight: video.videoHeight,
        });
      } catch {
        finish(null);
      }
    };
    video.addEventListener("loadedmetadata", () => {
      previewTimes = videoPreviewTimes(video.duration);
      if (previewTimes.length) video.currentTime = previewTimes[0];
    }, { once: true });
    video.addEventListener("loadeddata", () => {
      if (!previewTimes.length) void capture();
    }, { once: true });
    video.addEventListener("seeked", () => void capture());
    video.addEventListener("error", () => finish(null), { once: true });
    video.src = source;
    video.load();
  });
}

function optimisticMessage(
  identity: IdentitySummary,
  text: string,
  attachment: PendingMedia | null,
  replyToMessageId: string | null,
): MessageSummary {
  const localId = `local:${crypto.randomUUID()}`;
  return {
    event_id: localId,
    message_id: localId,
    author_public_key: identity.public_key,
    username: identity.username,
    bio: identity.bio,
    avatar: identity.avatar,
    accepts_direct_messages: identity.accepts_direct_messages,
    text,
    attachment: null,
    reply_to_message_id: replyToMessageId,
    created_at_millis: Date.now(),
    optimistic: true,
    local_attachment: attachment ? {
      preview_url: URL.createObjectURL(attachment.file),
      mime_type: attachment.mimeType,
    } : undefined,
  };
}

function withReaction(
  message: MessageSummary,
  emoji: string,
  selfPublicKey: string,
  enabled: boolean,
): MessageSummary {
  const reactions = message.reactions ?? [];
  const existing = reactions.find((reaction) => reaction.emoji === emoji);
  if (enabled) {
    if (existing?.reacted_by_self) return message;
    const reactorPublicKeys = existing
      ? [...new Set([...existing.reactor_public_keys, selfPublicKey])]
      : [selfPublicKey];
    const next: ReactionSummary = {
      emoji,
      count: reactorPublicKeys.length,
      reactor_public_keys: reactorPublicKeys,
      reacted_by_self: true,
    };
    return {
      ...message,
      reactions: existing
        ? reactions.map((reaction) => reaction.emoji === emoji ? next : reaction)
        : [...reactions, next],
    };
  }
  if (!existing?.reacted_by_self) return message;
  const reactorPublicKeys = existing.reactor_public_keys.filter(
    (publicKey) => publicKey !== selfPublicKey,
  );
  return {
    ...message,
    reactions: reactorPublicKeys.length
      ? reactions.map((reaction) => reaction.emoji === emoji ? {
        ...reaction,
        count: reactorPublicKeys.length,
        reactor_public_keys: reactorPublicKeys,
        reacted_by_self: false,
      } : reaction)
      : reactions.filter((reaction) => reaction.emoji !== emoji),
  };
}

function releaseOptimisticPreview(item: MessageSummary) {
  const source = item.local_attachment?.preview_url;
  if (source && ![...mediaCache.values()].includes(source)) URL.revokeObjectURL(source);
}

type UpdateStatus =
  | { phase: "ready"; version: string; restartFailed?: boolean }
  | { phase: "failed" };

function useAutoUpdater() {
  const [status, setStatus] = useState<UpdateStatus | null>(null);
  const checkingRef = useRef(false);
  const readyRef = useRef(false);
  const lastCheckAtRef = useRef(0);

  const checkForUpdate = useCallback(async (force = false) => {
    const now = Date.now();
    if (
      checkingRef.current
      || readyRef.current
      || (!force && now - lastCheckAtRef.current < UPDATE_CHECK_INTERVAL_MS)
    ) return;
    checkingRef.current = true;
    lastCheckAtRef.current = now;
    let updateFound = false;
    try {
      const update = await check();
      if (!update) return;
      updateFound = true;
      await update.downloadAndInstall();
      readyRef.current = true;
      setStatus({ phase: "ready", version: update.version });
    } catch (cause) {
      console.error("Noise update failed", cause);
      if (updateFound) setStatus({ phase: "failed" });
    } finally {
      checkingRef.current = false;
    }
  }, []);

  useEffect(() => {
    if (!isTauri || import.meta.env.DEV) return;
    const timer = window.setTimeout(() => void checkForUpdate(), 4000);
    const interval = window.setInterval(() => void checkForUpdate(), UPDATE_CHECK_HEARTBEAT_MS);
    const checkWhenVisible = () => {
      if (document.visibilityState === "visible") void checkForUpdate();
    };
    document.addEventListener("visibilitychange", checkWhenVisible);
    window.addEventListener("focus", checkWhenVisible);
    return () => {
      window.clearTimeout(timer);
      window.clearInterval(interval);
      document.removeEventListener("visibilitychange", checkWhenVisible);
      window.removeEventListener("focus", checkWhenVisible);
    };
  }, [checkForUpdate]);

  const restart = async () => {
    try {
      await relaunch();
    } catch (cause) {
      console.error("Noise could not restart after updating", cause);
      setStatus((current) => current?.phase === "ready" ? { ...current, restartFailed: true } : current);
    }
  };

  return {
    status,
    retry: () => void checkForUpdate(true),
    restart: () => void restart(),
    dismiss: () => setStatus(null),
  };
}

export default function App() {
  const [summary, setSummary] = useState<LocalSummary | null>(null);
  const [conversation, setConversation] = useState<Conversation | null>(null);
  const [groupEncryption, setGroupEncryption] = useState<GroupEncryptionStatus | null>(null);
  const [directConversation, setDirectConversation] = useState<DirectConversation | null>(null);
  const [sidebarMode, setSidebarMode] = useState<SidebarMode>("groups");
  const [pendingGroupId, setPendingGroupId] = useState<string | null>(null);
  const [dialog, setDialog] = useState<Dialog | null>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [presenceStatuses, setPresenceStatuses] = useState<Map<string, PresenceStatus>>(
    () => new Map(),
  );
  const updater = useAutoUpdater();
  const refreshGeneration = useRef(0);
  const groupConversationCache = useRef(new Map<string, Conversation>());
  const presenceScopes = useRef(new Map<string, {
    observedAt: number;
    statuses: Map<string, PresenceStatus>;
  }>());
  const dirtyGroupIds = useRef(new Set<string>());
  const groupWatchRevisions = useRef(new Map<string, number>());
  const directConversationCache = useRef(new Map<string, DirectConversation>());
  const groupReadInFlight = useRef(new Set<string>());
  const groupSelectionInFlight = useRef(false);
  const desiredGroupIdRef = useRef<string | null>(null);
  const sidebarModeRef = useRef(sidebarMode);
  const desiredDirectPublicKeyRef = useRef<string | null>(null);
  sidebarModeRef.current = sidebarMode;
  const summaryActiveDirectPublicKey = summary?.directs.find((direct) => direct.is_active)?.public_key ?? null;
  const summaryActiveGroupId = summary?.groups.find((group) => group.is_active)?.group_id ?? null;
  if (
    (!desiredGroupIdRef.current
      || !summary?.groups.some((group) => group.group_id === desiredGroupIdRef.current))
    && summaryActiveGroupId
  ) {
    desiredGroupIdRef.current = summaryActiveGroupId;
  }
  if (!desiredDirectPublicKeyRef.current && summaryActiveDirectPublicKey) {
    desiredDirectPublicKeyRef.current = summaryActiveDirectPublicKey;
  }
  const [optimisticGroupMessages, setOptimisticGroupMessages] = useState(
    () => new Map<string, MessageSummary[]>(),
  );
  const [optimisticDirectMessages, setOptimisticDirectMessages] = useState(
    () => new Map<string, MessageSummary[]>(),
  );
  const [groupMenu, setGroupMenu] = useState<{
    group: GroupSummary;
    x: number;
    y: number;
  } | null>(null);
  const [directMenu, setDirectMenu] = useState<{ direct: DirectSummary; x: number; y: number } | null>(null);
  const identityPublicKey = summary?.identity.public_key ?? null;
  const lastPresenceActivityAt = useRef(Date.now());
  const selfPresenceActive = useRef(true);
  const [selfPresenceStatus, setSelfPresenceStatus] = useState<PresenceStatus>("online");

  useEffect(() => {
    const suppressNativeContextMenu = (event: MouseEvent) => {
      event.preventDefault();
    };
    document.addEventListener("contextmenu", suppressNativeContextMenu, true);
    return () => document.removeEventListener("contextmenu", suppressNativeContextMenu, true);
  }, []);

  const updatePresenceScope = useCallback((
    scopeId: string,
    statuses: Map<string, PresenceStatus>,
  ) => {
    const now = Date.now();
    presenceScopes.current.set(scopeId, { observedAt: now, statuses });
    const merged = new Map<string, PresenceStatus>();
    for (const [knownScopeId, observation] of presenceScopes.current) {
      if (now - observation.observedAt > PRESENCE_OBSERVATION_STALE_MILLIS) {
        presenceScopes.current.delete(knownScopeId);
        continue;
      }
      for (const [publicKey, status] of observation.statuses) {
        const existing = merged.get(publicKey);
        if (status === "online" || !existing) {
          merged.set(publicKey, status);
        }
      }
    }
    setPresenceStatuses(merged);
  }, []);

  useEffect(() => {
    if (identityPublicKey) void ensureNotificationPermission();
  }, [identityPublicKey]);

  useEffect(() => {
    presenceScopes.current.clear();
    setPresenceStatuses(new Map());
  }, [identityPublicKey]);

  useEffect(() => {
    if (!identityPublicKey) return;
    let stopped = false;
    let timer: number | null = null;
    let heartbeatQueue = Promise.resolve();
    lastPresenceActivityAt.current = Date.now();
    selfPresenceActive.current = true;
    setSelfPresenceStatus("online");

    const publish = (active: boolean) => {
      heartbeatQueue = heartbeatQueue.then(async () => {
        if (stopped) return;
        try {
          await noise({ action: "heartbeat_presence", active, relays });
        } catch {
          // Presence is best-effort and retries without interrupting chat.
        }
      });
      return heartbeatQueue;
    };
    const heartbeat = async () => {
      const active = Date.now() - lastPresenceActivityAt.current < PRESENCE_IDLE_MILLIS;
      if (selfPresenceActive.current !== active) {
        selfPresenceActive.current = active;
        setSelfPresenceStatus(active ? "online" : "recently-active");
      }
      await publish(active);
      if (!stopped) {
        timer = window.setTimeout(() => void heartbeat(), PRESENCE_HEARTBEAT_MILLIS);
      }
    };
    const markActive = () => {
      lastPresenceActivityAt.current = Date.now();
      if (!selfPresenceActive.current) {
        selfPresenceActive.current = true;
        setSelfPresenceStatus("online");
        void publish(true);
      }
    };
    const activityEvents: (keyof WindowEventMap)[] = [
      "keydown",
      "pointerdown",
      "pointermove",
      "wheel",
    ];
    for (const eventName of activityEvents) {
      window.addEventListener(eventName, markActive, { passive: true });
    }
    void heartbeat();
    return () => {
      stopped = true;
      for (const eventName of activityEvents) {
        window.removeEventListener(eventName, markActive);
      }
      if (timer !== null) window.clearTimeout(timer);
    };
  }, [identityPublicKey]);

  function addOptimisticGroupMessage(groupId: string, item: MessageSummary) {
    setOptimisticGroupMessages((current) => {
      const next = new Map(current);
      next.set(groupId, [...(current.get(groupId) ?? []), item]);
      return next;
    });
  }

  function updateVisibleGroupReaction(
    groupId: string,
    messageEventId: string,
    emoji: string,
    enabled: boolean,
    selfPublicKey: string,
  ) {
    setConversation((current) => {
      if (current?.group.group_id !== groupId) return current;
      const next = {
        ...current,
        messages: current.messages.map((item) =>
          item.event_id === messageEventId
            ? withReaction(item, emoji, selfPublicKey, enabled)
            : item
        ),
      };
      groupConversationCache.current.set(groupId, next);
      return next;
    });
  }

  function addOptimisticDirectMessage(publicKey: string, item: MessageSummary) {
    setOptimisticDirectMessages((current) => {
      const next = new Map(current);
      next.set(publicKey, [...(current.get(publicKey) ?? []), item]);
      return next;
    });
  }

  function confirmOptimisticGroupMessage(
    groupId: string,
    localId: string,
    sent: SentMessageResult,
    attachment: MediaAttachment | null,
  ) {
    setOptimisticGroupMessages((current) => {
      const pending = current.get(groupId);
      if (!pending) return current;
      const next = new Map(current);
      next.set(groupId, pending.map((item) => item.event_id === localId ? {
        ...item,
        event_id: sent.event_id,
        message_id: sent.message_id,
        created_at_millis: sent.created_at_millis,
        attachment,
      } : item));
      return next;
    });
  }

  function confirmOptimisticDirectMessage(
    publicKey: string,
    localId: string,
    sent: SentMessageResult,
    attachment: MediaAttachment | null,
  ) {
    setOptimisticDirectMessages((current) => {
      const pending = current.get(publicKey);
      if (!pending) return current;
      const next = new Map(current);
      next.set(publicKey, pending.map((item) => item.event_id === localId ? {
        ...item,
        event_id: sent.event_id,
        message_id: sent.message_id,
        created_at_millis: sent.created_at_millis,
        attachment,
      } : item));
      return next;
    });
  }

  function removeOptimisticGroupMessage(groupId: string, eventId: string) {
    setOptimisticGroupMessages((current) => {
      const pending = current.get(groupId);
      if (!pending) return current;
      const removed = pending.find((item) => item.event_id === eventId);
      const remaining = pending.filter((item) => item.event_id !== eventId);
      if (removed) releaseOptimisticPreview(removed);
      const next = new Map(current);
      if (remaining.length) next.set(groupId, remaining);
      else next.delete(groupId);
      return next;
    });
  }

  function removeOptimisticDirectMessage(publicKey: string, eventId: string) {
    setOptimisticDirectMessages((current) => {
      const pending = current.get(publicKey);
      if (!pending) return current;
      const removed = pending.find((item) => item.event_id === eventId);
      const remaining = pending.filter((item) => item.event_id !== eventId);
      if (removed) releaseOptimisticPreview(removed);
      const next = new Map(current);
      if (remaining.length) next.set(publicKey, remaining);
      else next.delete(publicKey);
      return next;
    });
  }

  useEffect(() => {
    if (!conversation) return;
    const confirmedIds = new Set(conversation.messages.map((item) => item.event_id));
    setOptimisticGroupMessages((current) => {
      const pending = current.get(conversation.group.group_id);
      if (!pending?.some((item) => confirmedIds.has(item.event_id))) return current;
      const remaining = pending.filter((item) => {
        const confirmed = confirmedIds.has(item.event_id);
        if (confirmed) releaseOptimisticPreview(item);
        return !confirmed;
      });
      const next = new Map(current);
      if (remaining.length) next.set(conversation.group.group_id, remaining);
      else next.delete(conversation.group.group_id);
      return next;
    });
  }, [conversation]);

  useEffect(() => {
    if (!directConversation) return;
    const confirmedIds = new Set(directConversation.messages.map((item) => item.event_id));
    setOptimisticDirectMessages((current) => {
      const pending = current.get(directConversation.contact.public_key);
      if (!pending?.some((item) => confirmedIds.has(item.event_id))) return current;
      const remaining = pending.filter((item) => {
        const confirmed = confirmedIds.has(item.event_id);
        if (confirmed) releaseOptimisticPreview(item);
        return !confirmed;
      });
      const next = new Map(current);
      if (remaining.length) next.set(directConversation.contact.public_key, remaining);
      else next.delete(directConversation.contact.public_key);
      return next;
    });
  }, [directConversation]);

  const applyDirectInbox = useCallback((inbox: DirectInbox) => {
    for (const item of inbox.conversations) {
      directConversationCache.current.set(item.contact.public_key, item);
    }
    const reportedActivePublicKey = inbox.summary.directs.find((direct) => direct.is_active)?.public_key;
    const desiredPublicKey = desiredDirectPublicKeyRef.current;
    const activePublicKey = desiredPublicKey
      && inbox.summary.directs.some((direct) => direct.public_key === desiredPublicKey)
      ? desiredPublicKey
      : reportedActivePublicKey;
    desiredDirectPublicKeyRef.current = activePublicKey ?? null;
    const activeConversation = inbox.conversations.find(
      (item) => item.contact.public_key === activePublicKey,
    );
    setSummary({
      ...inbox.summary,
      directs: inbox.summary.directs.map((direct) => ({
        ...direct,
        is_active: direct.public_key === activePublicKey,
      })),
    });
    setDirectConversation((current) =>
      activeConversation
      ?? (current?.contact.public_key === activePublicKey ? current : null)
    );
  }, []);

  const markDirectRead = useCallback(async (publicKey: string) => {
    const marked = await noise<LocalSummary>({
      action: "mark_direct_read",
      public_key: publicKey,
    });
    if (marked) setSummary(marked);
    void noise({ action: "sync_account", relays }).catch(() => {
      // The local read marker is immediate; cross-device sync retries normally.
    });
  }, []);

  const markActiveGroupRead = useCallback(async (groupId: string) => {
    if (groupReadInFlight.current.has(groupId)) return;
    groupReadInFlight.current.add(groupId);
    try {
      const marked = await markGroupRead(groupId);
      if (!marked) return;
      setSummary(marked);
      void noise({ action: "sync_account", relays }).catch(() => {
        // The local group read marker is immediate; cross-device sync retries normally.
      });
    } finally {
      groupReadInFlight.current.delete(groupId);
    }
  }, []);

  const syncDirectInbox = useCallback(async (markActiveRead: boolean) => {
    const generation = refreshGeneration.current;
    const inbox = await noise<DirectInbox>({ action: "direct_inbox", relays });
    if (!inbox) return;
    if (generation !== refreshGeneration.current) return;
    applyDirectInbox(inbox);
    const activePublicKey = desiredDirectPublicKeyRef.current
      ?? inbox.summary.directs.find((direct) => direct.is_active)?.public_key;
    const active = inbox.summary.directs.find((direct) => direct.public_key === activePublicKey);
    if (markActiveRead && active?.has_unread) await markDirectRead(active.public_key);
  }, [applyDirectInbox, markDirectRead]);

  const refresh = useCallback(async () => {
    if (groupSelectionInFlight.current) return;
    const generation = ++refreshGeneration.current;
    const local = await noise<LocalSummary>({ action: "status" });
    if (generation !== refreshGeneration.current) return;
    setSummary(local);
    if (!local) return;

    if (sidebarMode === "groups") {
      const activeGroup = local.groups.find((group) => group.is_active);
      if (!activeGroup) {
        setConversation(null);
        setGroupEncryption(null);
        return;
      }
      const needsReadBaseline = !activeGroup.read_state_initialized;
      let cached = groupConversationCache.current.get(activeGroup.group_id);
      if (!cached) {
        cached = await noise<Conversation>({
          action: "cached_conversation",
          group_id: activeGroup.group_id,
        }) ?? undefined;
        if (generation !== refreshGeneration.current) return;
        if (cached) groupConversationCache.current.set(activeGroup.group_id, cached);
      }
      if (cached) setConversation(cached);
      const encryption = await syncGroupEncryption();
      if (generation !== refreshGeneration.current) return;
      setGroupEncryption(encryption);
      if (encryption?.phase === "removed") {
        const reconciled = await noise<LocalSummary>({ action: "status" });
        if (generation !== refreshGeneration.current) return;
        setConversation(null);
        setGroupEncryption(null);
        setSummary(reconciled);
        return;
      }
      if (
        encryption?.phase === "waiting_for_admission"
        || encryption?.phase === "waiting_for_device"
      ) {
        setConversation(null);
        return;
      }
      const nextConversation = await noise<Conversation>({ action: "conversation", relays });
      const reconciled = await noise<LocalSummary>({ action: "status" });
      if (generation !== refreshGeneration.current) return;
      if (nextConversation) {
        groupConversationCache.current.set(nextConversation.group.group_id, nextConversation);
        dirtyGroupIds.current.delete(nextConversation.group.group_id);
        setConversation(nextConversation);
      }
      setSummary(reconciled);
      if (needsReadBaseline) {
        void noise<LocalSummary>({ action: "sync_account", relays })
          .then((synced) => {
            if (synced) setSummary(synced);
          })
          .catch(() => {
            // The local baseline is durable; encrypted cross-device sync retries normally.
          });
      }
      return;
    }

    await syncDirectInbox(true);
  }, [sidebarMode, syncDirectInbox]);

  useEffect(() => {
    void refresh()
      .catch((cause) => setError(message(cause)))
      .finally(() => setLoading(false));
  }, [refresh]);

  useEffect(() => {
    if (!isTauri || !identityPublicKey) return;
    void ensureNotificationPermission();
  }, [identityPublicKey]);

  const activeGroup = summary?.groups.find(
    (group) => group.group_id === desiredGroupIdRef.current,
  ) ?? summary?.groups.find((group) => group.is_active) ?? null;
  const activeGroupId = activeGroup?.group_id ?? null;
  const activeDirectPublicKey = summary?.directs.find((direct) => direct.is_active)?.public_key ?? null;
  const markCurrentGroupRead = useCallback(() => {
    if (activeGroupId) void markActiveGroupRead(activeGroupId);
  }, [activeGroupId, markActiveGroupRead]);
  const activeGroupBackground = sidebarMode === "groups" ? activeGroup?.background ?? null : null;
  const activeAccentStyle = accentStyle(sidebarMode === "groups" ? activeGroup?.accent_color : null);
  const appBackgroundSource = useProfileImageSource(activeGroupBackground);
  const groupWatchKey = summary?.groups
    .map((group) => group.group_id)
    .sort()
    .join("|") ?? "";

  useEffect(() => {
    if (!identityPublicKey || !summary) return;
    const groups = summary.groups
      .filter((group) => sidebarMode !== "groups" || group.group_id !== activeGroupId);
    let stopped = false;
    const watch = async (group: GroupSummary) => {
      let revision: number | null = groupWatchRevisions.current.get(group.group_id) ?? null;
      while (!stopped) {
        try {
          const initial = revision === null;
          const change: GroupWatch | null = await noise<GroupWatch>({
            action: "watch_group_id",
            group_id: group.group_id,
            since: revision,
            relays,
          });
          if (stopped || !change) return;
          revision = change.revision;
          groupWatchRevisions.current.set(group.group_id, change.revision);
          updatePresenceScope(
            `group:${group.group_id}`,
            presenceStatusesFromWatch(change),
          );
          if (initial || change.changed) {
            if (!initial && change.changed) dirtyGroupIds.current.add(group.group_id);
            const activity = await syncGroupActivity(group.group_id);
            if (!stopped && activity) {
              if (activity.conversation) {
                groupConversationCache.current.set(group.group_id, activity.conversation);
                dirtyGroupIds.current.delete(group.group_id);
              }
              setSummary(activity.summary);
            }
            if (initial && !group.read_state_initialized) {
              void noise<LocalSummary>({ action: "sync_account", relays })
                .then((synced) => {
                  if (!stopped && synced) setSummary(synced);
                })
                .catch(() => {
                  // The local baseline is durable; encrypted cross-device sync retries normally.
                });
            }
          }
        } catch {
          await new Promise((resolve) => window.setTimeout(resolve, 1500));
        }
      }
    };
    for (const group of groups) void watch(group);
    return () => {
      stopped = true;
    };
  }, [activeGroupId, groupWatchKey, identityPublicKey, sidebarMode, updatePresenceScope]);

  useEffect(() => {
    if (sidebarMode !== "groups" || !activeGroupId) return;
    let stopped = false;
    const watch = async () => {
      let revision: number | null = groupWatchRevisions.current.get(activeGroupId) ?? null;
      while (!stopped) {
        try {
          const initial = revision === null;
          const change: GroupWatch | null = await noise<GroupWatch>({
            action: "watch_group_id",
            group_id: activeGroupId,
            since: revision,
            relays,
          });
          if (stopped || !change) return;
          revision = change.revision;
          groupWatchRevisions.current.set(activeGroupId, change.revision);
          updatePresenceScope(
            `group:${activeGroupId}`,
            presenceStatusesFromWatch(change),
          );
          if (!initial && change.changed) {
            dirtyGroupIds.current.add(activeGroupId);
            await refresh();
          }
        } catch {
          await new Promise((resolve) => window.setTimeout(resolve, 1500));
        }
      }
    };
    void watch();
    return () => {
      stopped = true;
    };
  }, [activeGroupId, identityPublicKey, refresh, sidebarMode, updatePresenceScope]);

  useEffect(() => {
    if (!identityPublicKey) return;
    let stopped = false;
    const watch = async () => {
      let revision: number | null = null;
      while (!stopped) {
        try {
          const initial = revision === null;
          const change: GroupWatch | null = await noise<GroupWatch>({ action: "watch_direct", since: revision, relays });
          if (stopped || !change) return;
          revision = change.revision;
          updatePresenceScope("directs", presenceStatusesFromWatch(change));
          if (initial) {
            await syncDirectInbox(sidebarModeRef.current === "directs");
          } else if (change.changed) {
            await syncDirectInbox(sidebarModeRef.current === "directs");
          }
        } catch {
          await new Promise((resolve) => window.setTimeout(resolve, 1500));
        }
      }
    };
    void watch();
    return () => { stopped = true; };
  }, [identityPublicKey, syncDirectInbox, updatePresenceScope]);

  useEffect(() => {
    if (!identityPublicKey || !summary?.identity.noise_id) return;
    let stopped = false;
    const watch = async () => {
      let revision: number | null = null;
      while (!stopped) {
        try {
          const initial = revision === null;
          const change: GroupWatch | null = await noise<GroupWatch>({ action: "watch_account", since: revision, relays });
          if (stopped || !change) return;
          revision = change.revision;
          if (initial || change.changed) {
            const reconciled = await noise<LocalSummary>({ action: "sync_read_state", relays });
            if (!stopped && reconciled) setSummary(reconciled);
          }
        } catch {
          await new Promise((resolve) => window.setTimeout(resolve, 1500));
        }
      }
    };
    void watch();
    return () => { stopped = true; };
  }, [identityPublicKey, summary?.identity.noise_id]);

  async function perform(operation: () => Promise<void>, syncAccount = true) {
    if (busy) return false;
    setBusy(true);
    setError(null);
    try {
      await operation();
      if (syncAccount) await noise({ action: "sync_account", relays });
      return true;
    } catch (cause) {
      if (message(cause) !== "media upload cancelled") setError(message(cause));
      return false;
    } finally {
      setBusy(false);
    }
  }

  async function selectGroup(group: GroupSummary) {
    if (group.group_id === activeGroupId && !pendingGroupId) return;
    const previousGroupId = activeGroupId;
    const needsReadBaseline = !group.read_state_initialized;
    const generation = ++refreshGeneration.current;
    groupSelectionInFlight.current = true;
    setPendingGroupId(group.group_id);
    setError(null);
    setGroupEncryption(null);

    try {
      let cached = groupConversationCache.current.get(group.group_id);
      if (!cached) {
        cached = await noise<Conversation>({
          action: "cached_conversation",
          group_id: group.group_id,
        }) ?? undefined;
        if (generation !== refreshGeneration.current) return;
        if (cached) {
          groupConversationCache.current.set(group.group_id, cached);
          dirtyGroupIds.current.add(group.group_id);
        }
      }
      if (cached) {
        desiredGroupIdRef.current = group.group_id;
        setConversation(cached);
        setSummary((current) => current ? {
          ...current,
          groups: current.groups.map((candidate) => ({
            ...candidate,
            is_active: candidate.group_id === group.group_id,
          })),
        } : current);
      }

      const local = await noise<LocalSummary>({ action: "select_group", group_id: group.group_id });
      if (generation !== refreshGeneration.current) return;
      if (cached) {
        setSummary(local);
        if (!dirtyGroupIds.current.has(group.group_id)) {
          if (needsReadBaseline) {
            void noise<LocalSummary>({ action: "sync_account", relays })
              .then((synced) => {
                if (synced) setSummary(synced);
              })
              .catch(() => {
                // The cached conversation is already usable; account sync retries normally.
              });
          }
          return;
        }
      }
      const encryption = await syncGroupEncryption();
      if (generation !== refreshGeneration.current) return;
      if (encryption?.phase === "removed") {
        const reconciled = await noise<LocalSummary>({ action: "status" });
        if (generation !== refreshGeneration.current) return;
        desiredGroupIdRef.current = reconciled?.groups.find((candidate) => candidate.is_active)?.group_id
          ?? previousGroupId;
        setConversation(null);
        setGroupEncryption(null);
        setSummary(reconciled);
        return;
      }
      if (
        encryption?.phase === "waiting_for_admission"
        || encryption?.phase === "waiting_for_device"
      ) {
        desiredGroupIdRef.current = group.group_id;
        setSummary(local);
        setGroupEncryption(encryption);
        setConversation(null);
        return;
      }
      const fresh = await noise<Conversation>({ action: "conversation", relays });
      const reconciled = await noise<LocalSummary>({ action: "status" });
      if (generation !== refreshGeneration.current) return;
      if (!fresh || fresh.group.group_id !== group.group_id) {
        throw new Error("the selected group did not return its conversation");
      }
      groupConversationCache.current.set(fresh.group.group_id, fresh);
      dirtyGroupIds.current.delete(fresh.group.group_id);
      desiredGroupIdRef.current = group.group_id;
      setConversation(fresh);
      setGroupEncryption(encryption);
      setSummary(reconciled);
      if (needsReadBaseline) {
        void noise<LocalSummary>({ action: "sync_account", relays })
          .then((synced) => {
            if (synced) setSummary(synced);
          })
          .catch(() => {
            // The local baseline is durable; encrypted cross-device sync retries normally.
          });
      }
    } catch (cause) {
      if (generation === refreshGeneration.current) {
        desiredGroupIdRef.current = previousGroupId;
        if (previousGroupId) {
          const previous = groupConversationCache.current.get(previousGroupId);
          if (previous) setConversation(previous);
        }
        setError(message(cause));
      }
    } finally {
      if (generation === refreshGeneration.current) {
        groupSelectionInFlight.current = false;
        setPendingGroupId(null);
      }
    }
  }

  async function selectDirect(direct: DirectSummary) {
    if (desiredDirectPublicKeyRef.current === direct.public_key && direct.is_active) return;
    const generation = ++refreshGeneration.current;
    desiredDirectPublicKeyRef.current = direct.public_key;
    setError(null);
    const cached = directConversationCache.current.get(direct.public_key);
    if (cached) setDirectConversation(cached);
    setSummary((current) => current ? {
      ...current,
      directs: current.directs.map((candidate) => ({
        ...candidate,
        is_active: candidate.public_key === direct.public_key,
        has_unread: candidate.public_key === direct.public_key ? false : candidate.has_unread,
      })),
    } : current);

    try {
      const local = await noise<LocalSummary>({ action: "select_direct", public_key: direct.public_key });
      if (generation !== refreshGeneration.current) return;
      const marked = direct.has_unread
        ? await noise<LocalSummary>({ action: "mark_direct_read", public_key: direct.public_key })
        : local;
      if (generation !== refreshGeneration.current) return;
      setSummary(marked);
      const fresh = await noise<DirectConversation>({ action: "direct_conversation", relays });
      const reconciled = await noise<LocalSummary>({ action: "status" });
      if (generation !== refreshGeneration.current) return;
      if (fresh) {
        directConversationCache.current.set(fresh.contact.public_key, fresh);
        setDirectConversation(fresh);
      }
      setSummary(reconciled);
      void noise({ action: "sync_account", relays }).catch(() => {
        // The thread is already read locally; cross-device sync retries normally.
      });
    } catch (cause) {
      if (generation === refreshGeneration.current) setError(message(cause));
    }
  }

  async function startDirect(person: PersonSummary) {
    const started = await perform(async () => {
      const local = await noise<LocalSummary>({
        action: "start_direct",
        public_key: person.public_key,
        username: person.username,
        bio: person.bio,
        avatar: person.avatar,
        accepts_direct_messages: person.accepts_direct_messages,
      });
      if (!local) throw new Error("the direct conversation could not be started");
      const contact = local.directs.find((direct) => direct.public_key === person.public_key);
      if (!contact) throw new Error("the direct conversation is missing");
      const immediateConversation: DirectConversation = {
        contact,
        media_scope_id: "",
        messages: [],
      };
      directConversationCache.current.set(person.public_key, immediateConversation);
      desiredDirectPublicKeyRef.current = person.public_key;
      setDirectConversation(immediateConversation);
      setSummary(local);
      setDialog(null);
      setSidebarMode("directs");
    }, false);
    if (!started) return;

    void (async () => {
      try {
        const fresh = await noise<DirectConversation>({ action: "direct_conversation", relays });
        if (fresh) {
          directConversationCache.current.set(fresh.contact.public_key, fresh);
          if (desiredDirectPublicKeyRef.current === fresh.contact.public_key) {
            setDirectConversation(fresh);
          }
        }
      } catch (cause) {
        setError(message(cause));
      }
    })();
    void noise({ action: "sync_account", relays }).catch(() => {
      // The DM is already available locally; encrypted account sync retries normally.
    });
  }

  async function switchSidebarMode(nextMode: SidebarMode) {
    if (nextMode === sidebarMode) return;
    if (nextMode === "groups") {
      setSidebarMode("groups");
      return;
    }

    const newestUnread = summary?.directs.find((direct) => direct.has_unread);
    if (newestUnread && !newestUnread.is_active) {
      const generation = ++refreshGeneration.current;
      desiredDirectPublicKeyRef.current = newestUnread.public_key;
      const cached = directConversationCache.current.get(newestUnread.public_key);
      if (cached) setDirectConversation(cached);
      setSummary((current) => current ? {
        ...current,
        directs: current.directs.map((candidate) => ({
          ...candidate,
          is_active: candidate.public_key === newestUnread.public_key,
          has_unread: candidate.public_key === newestUnread.public_key
            ? false
            : candidate.has_unread,
        })),
      } : current);
      try {
        const local = await noise<LocalSummary>({
          action: "select_direct",
          public_key: newestUnread.public_key,
        });
        if (generation !== refreshGeneration.current) return;
        if (local) setSummary(local);
        await markDirectRead(newestUnread.public_key);
      } catch (cause) {
        if (generation === refreshGeneration.current) setError(message(cause));
      }
    }
    setSidebarMode("directs");
  }

  if (loading) return <><Loading /><UpdateBanner {...updater} /></>;
  if (!summary) {
    return (
      <>
      <Onboarding
        busy={busy}
        onCreate={(username, password) =>
          perform(async () => {
            const avatar = await generateUserAvatar(`${username}:${crypto.randomUUID()}`);
            const local = await noise<LocalSummary>({
              action: "initialize",
              username,
              password,
              avatar_data_base64: avatar,
              avatar_mime_type: "image/png",
              relays,
            });
            setSummary(local);
            if (local?.identity.noise_id) setDialog({ type: "noise_id", noiseId: local.identity.noise_id });
          })
        }
        onSignIn={(noiseId, password) =>
          perform(async () => {
            const local = await noise<LocalSummary>({ action: "sign_in", noise_id: noiseId, password, relays });
            setSummary(local);
          })
        }
      />
      {error && <ErrorToast error={error} onClose={() => setError(null)} />}
      <UpdateBanner {...updater} />
      </>
    );
  }

  const selectedConversationState = conversation?.group.group_id === activeGroupId ? conversation : null;
  const selectedGroupPending = activeGroupId ? optimisticGroupMessages.get(activeGroupId) ?? [] : [];
  const selectedConversation = selectedConversationState ? {
    ...selectedConversationState,
    messages: [
      ...selectedConversationState.messages,
      ...selectedGroupPending.filter((pending) =>
        !selectedConversationState.messages.some((item) => item.event_id === pending.event_id)
      ),
    ],
  } : null;
  const selectedDirectConversationState = directConversation?.contact.public_key === activeDirectPublicKey
    ? directConversation
    : null;
  const selectedDirectPending = activeDirectPublicKey
    ? optimisticDirectMessages.get(activeDirectPublicKey) ?? []
    : [];
  const selectedDirectConversation = selectedDirectConversationState ? {
    ...selectedDirectConversationState,
    messages: [
      ...selectedDirectConversationState.messages,
      ...selectedDirectPending.filter((pending) =>
        !selectedDirectConversationState.messages.some((item) => item.event_id === pending.event_id)
      ),
    ],
  } : null;
  const selectedPresenceStatuses = new Map(presenceStatuses);
  selectedPresenceStatuses.set(summary.identity.public_key, selfPresenceStatus);
  const visibleSummary = activeGroupId ? {
    ...summary,
    groups: summary.groups.map((group) => ({
      ...group,
      is_active: group.group_id === activeGroupId,
    })),
  } : summary;

  return (
    <div className={`app-shell ${appBackgroundSource ? "group-background-active" : ""}`} style={activeAccentStyle}>
      {appBackgroundSource && <div className="group-app-background" style={{ backgroundImage: `url(${JSON.stringify(appBackgroundSource)})` }} aria-hidden="true" />}
        <Sidebar
        summary={visibleSummary}
        mode={sidebarMode}
        pendingGroupId={pendingGroupId}
        directPresenceStatuses={presenceStatuses}
        selfPresenceStatus={selfPresenceStatus}
        onMode={(mode) => void switchSidebarMode(mode)}
        onMake={() => setDialog({ type: "make" })}
        onJoin={() => setDialog({ type: "join" })}
        onProfile={() => setDialog({ type: "profile", profile: summary.identity })}
        onContextMenu={(group, x, y) => {
          setGroupMenu({ group, x, y });
        }}
        onDirectContextMenu={(direct, x, y) => setDirectMenu({ direct, x, y })}
        onSelect={(group) => void selectGroup(group)}
        onSelectDirect={(direct) => void selectDirect(direct)}
      />

      <main className="conversation-pane">
        <section className={`mode-pane ${sidebarMode === "groups" ? "active" : "inactive"}`} aria-hidden={sidebarMode !== "groups"}>
          {selectedConversation ? (
            <ConversationPanel
              key={selectedConversation.group.group_id}
              conversation={selectedConversation}
              busy={busy || pendingGroupId === selectedConversation.group.group_id}
              hasBackground={Boolean(appBackgroundSource)}
              canEditGroup={selectedConversation.group.owner_public_key === summary.identity.public_key}
              unreadCount={activeGroup?.unread_count ?? 0}
              selfPublicKey={summary.identity.public_key}
              presenceStatuses={selectedPresenceStatuses}
              onGroupSettings={() => setDialog({ type: "group", group: selectedConversation.group })}
              onReports={() => setDialog({ type: "reports" })}
              onMedia={() => setDialog({ type: "media" })}
              onRules={() => setDialog({ type: "rules", group: selectedConversation.group })}
              onPerson={(person) => setDialog({ type: "person", person })}
              onMessage={(person) => void startDirect(person)}
              onDeleteMessage={(item) => setDialog({
                type: "delete_message",
                message: item,
                scopeId: selectedConversation.group.group_id,
              })}
              onDownload={(item) => perform(async () => {
                if (!item.attachment) throw new Error("this message has no media");
                await downloadAttachment(item.attachment, selectedConversation.group.group_id);
              }, false)}
              onReaction={async (item, emoji) => {
                const groupId = selectedConversation.group.group_id;
                const enabled = !item.reactions?.some(
                  (reaction) => reaction.emoji === emoji && reaction.reacted_by_self,
                );
                updateVisibleGroupReaction(
                  groupId,
                  item.event_id,
                  emoji,
                  enabled,
                  summary.identity.public_key,
                );
                try {
                  await noise({
                    action: "set_reaction",
                    message_event_id: item.event_id,
                    emoji,
                    enabled,
                    relays,
                  });
                  await refresh();
                } catch (cause) {
                  updateVisibleGroupReaction(
                    groupId,
                    item.event_id,
                    emoji,
                    !enabled,
                    summary.identity.public_key,
                  );
                  setError(message(cause));
                }
              }}
              onSetModerator={(member, enabled) =>
                perform(async () => {
                  await noise({ action: "set_moderator", member_public_key: member.public_key, enabled, relays });
                  await refresh();
                })
              }
              onBan={(member) => setDialog({ type: "ban_member", member })}
              onReport={(message) => setDialog({ type: "report_message", message })}
              onReachedBottom={markCurrentGroupRead}
              onSend={async (text, pending, onProgress, replyToMessageId, signal) => {
                const groupId = selectedConversation.group.group_id;
                const optimistic = optimisticMessage(summary.identity, text, pending, replyToMessageId);
                if (!pending) addOptimisticGroupMessage(groupId, optimistic);
                let attachment: MediaAttachment | null = null;
                let result: SentMessageResult | null = null;
                const sent = await perform(async () => {
                  attachment = await uploadPendingMedia(pending, "upload_media_chunk", onProgress, signal);
                  if (signal.aborted) throw new Error("media upload cancelled");
                  result = await noise<SentMessageResult>({
                    action: "say",
                    text,
                    attachment,
                    reply_to_message_id: replyToMessageId,
                    relays,
                  });
                  if (!result) throw new Error("the relay did not confirm the message");
                }, false);
                if (!sent || !result) {
                  if (pending) releaseOptimisticPreview(optimistic);
                  else removeOptimisticGroupMessage(groupId, optimistic.event_id);
                  return false;
                }
                const confirmed = result as SentMessageResult;
                if (pending) addOptimisticGroupMessage(groupId, optimistic);
                if (attachment && optimistic.local_attachment) {
                  mediaCache.set(mediaCacheKey(attachment), optimistic.local_attachment.preview_url);
                  sentMediaPreviewCache.set(confirmed.event_id, optimistic.local_attachment);
                }
                confirmOptimisticGroupMessage(groupId, optimistic.event_id, confirmed, attachment);
                void refresh().catch((cause) => setError(message(cause)));
                void noise({ action: "sync_account", relays }).catch(() => undefined);
                return true;
              }}
            />
          ) : activeGroupId && groupEncryption?.group_id === activeGroupId
            && (
              groupEncryption.phase === "waiting_for_admission"
              || groupEncryption.phase === "waiting_for_device"
            ) ? (
              <EncryptionPending phase={groupEncryption.phase} />
            ) : activeGroupId ? <Loading /> : (
            <EmptyGroup
              onMake={() => setDialog({ type: "make" })}
              onJoin={() => setDialog({ type: "join" })}
            />
          )}
        </section>
        <section className={`mode-pane ${sidebarMode === "directs" ? "active" : "inactive"}`} aria-hidden={sidebarMode !== "directs"}>
          {selectedDirectConversation ? (
            <DirectConversationPanel
              key={selectedDirectConversation.contact.public_key}
              conversation={selectedDirectConversation}
              busy={busy}
              selfPublicKey={summary.identity.public_key}
              selfPresence={selfPresenceStatus}
              contactPresence={presenceStatuses.get(selectedDirectConversation.contact.public_key) ?? "offline"}
              onPerson={(person) => setDialog({ type: "person", person })}
              onDelete={() => setDialog({ type: "delete_direct", direct: selectedDirectConversation.contact })}
              onDownload={(item) => perform(async () => {
                if (!item.attachment) throw new Error("this message has no media");
                await downloadAttachment(item.attachment, selectedDirectConversation.media_scope_id);
              }, false)}
              onSend={async (text, pending, onProgress, replyToMessageId, signal) => {
                const publicKey = selectedDirectConversation.contact.public_key;
                const optimistic = optimisticMessage(summary.identity, text, pending, replyToMessageId);
                if (!pending) addOptimisticDirectMessage(publicKey, optimistic);
                let attachment: MediaAttachment | null = null;
                let result: SentMessageResult | null = null;
                const sent = await perform(async () => {
                  attachment = await uploadPendingMedia(pending, "upload_direct_media_chunk", onProgress, signal);
                  if (signal.aborted) throw new Error("media upload cancelled");
                  result = await noise<SentMessageResult>({
                    action: "say_direct",
                    text,
                    attachment,
                    reply_to_message_id: replyToMessageId,
                    relays,
                  });
                  if (!result) throw new Error("the relay did not confirm the message");
                }, false);
                if (!sent || !result) {
                  if (pending) releaseOptimisticPreview(optimistic);
                  else removeOptimisticDirectMessage(publicKey, optimistic.event_id);
                  return false;
                }
                const confirmed = result as SentMessageResult;
                if (pending) addOptimisticDirectMessage(publicKey, optimistic);
                if (attachment && optimistic.local_attachment) {
                  mediaCache.set(mediaCacheKey(attachment), optimistic.local_attachment.preview_url);
                  sentMediaPreviewCache.set(confirmed.event_id, optimistic.local_attachment);
                }
                confirmOptimisticDirectMessage(publicKey, optimistic.event_id, confirmed, attachment);
                void refresh().catch((cause) => setError(message(cause)));
                void noise({ action: "sync_account", relays }).catch(() => undefined);
                return true;
              }}
            />
          ) : activeDirectPublicKey ? <Loading /> : <EmptyDirects />}
        </section>
      </main>

      {dialog?.type === "make" && (
        <MakeDialog
          busy={busy}
          onClose={() => setDialog(null)}
          onSubmit={(name) =>
            perform(async () => {
              const avatar = await generateGroupAvatar(`${name}:${crypto.randomUUID()}`);
              const result = await noise<MakeResult>({
                action: "make",
                name,
                avatar_data_base64: avatar,
                avatar_mime_type: "image/png",
                relays,
              });
              if (!result) throw new Error("the group was not created");
              await refresh();
              setDialog({
                type: "frequency",
                group: result.group.name,
                frequency: result.display_frequency,
              });
            })
          }
        />
      )}
      {dialog?.type === "join" && (
        <JoinDialog
          busy={busy}
          onClose={() => setDialog(null)}
          onSubmit={(frequency) =>
            perform(async () => {
              await noise({ action: "join", frequency, relays });
              await refresh();
              setDialog(null);
            })
          }
        />
      )}
      {dialog?.type === "frequency" && (
        <FrequencyDialog
          group={dialog.group}
          frequency={dialog.frequency}
          onClose={() => setDialog(null)}
        />
      )}
      {dialog?.type === "noise_id" && <NoiseIdDialog noiseId={dialog.noiseId} onClose={() => setDialog(null)} />}
      {dialog?.type === "profile" && (
        <SettingsDialog
          profile={dialog.profile}
          busy={busy}
          onClose={() => setDialog(null)}
          onDeleteAccount={() => setDialog({ type: "delete_account" })}
          onLogout={() => setDialog({ type: "logout" })}
          onSave={(username, bio, avatar, removeAvatar, acceptsDirectMessages) =>
            perform(async () => {
              const local = await noise<LocalSummary>({
                action: "update_profile",
                username,
                bio,
                avatar_data_base64: avatar,
                avatar_mime_type: avatar ? "image/jpeg" : null,
                remove_avatar: removeAvatar,
                accepts_direct_messages: acceptsDirectMessages,
                relays,
              });
              setSummary(local);
              await refresh();
              setDialog(null);
            })
          }
        />
      )}
      {dialog?.type === "group" && (
        <GroupSettingsDialog
          group={dialog.group}
          bannedMembers={conversation?.group.group_id === dialog.group.group_id ? conversation.banned_members : []}
          presenceStatuses={selectedPresenceStatuses}
          busy={busy}
          onClose={() => setDialog(null)}
          onUnban={(member) => perform(async () => {
            await noise({ action: "unban_member", member_public_key: member.public_key, relays });
            await refresh();
          })}
          onRotateFrequency={(revokeOnly) => perform(async () => {
            const local = await noise<LocalSummary>({ action: "rotate_frequency", revoke_only: revokeOnly, relays });
            if (!local) throw new Error("the relay did not return the updated frequency");
            setSummary(local);
            await refresh();
            const updatedGroup = local.groups.find((group) => group.group_id === dialog.group.group_id);
            if (updatedGroup) setDialog({ type: "group", group: updatedGroup });
          })}
          onSave={(name, description, accentColor, avatar, removeAvatar, background, removeBackground, mobileBackground, removeMobileBackground, membersCanSendMessages, membersCanSendMedia) =>
            perform(async () => {
              const local = await noise<LocalSummary>({
                action: "update_group_profile",
                name,
                description,
                rules: dialog.group.rules,
                accent_color: accentColor,
                avatar_data_base64: avatar,
                avatar_mime_type: avatar ? "image/jpeg" : null,
                remove_avatar: removeAvatar,
                background_data_base64: background,
                background_mime_type: background ? "image/jpeg" : null,
                remove_background: removeBackground,
                mobile_background_data_base64: mobileBackground,
                mobile_background_mime_type: mobileBackground ? "image/jpeg" : null,
                remove_mobile_background: removeMobileBackground,
                members_can_send_messages: membersCanSendMessages,
                members_can_send_media: membersCanSendMedia,
                relays,
              });
              setSummary(local);
              await refresh();
              setDialog(null);
            })
          }
        />
      )}
      {dialog?.type === "rules" && (
        <RulesDialog
          group={dialog.group}
          canEdit={dialog.group.owner_public_key === summary.identity.public_key}
          busy={busy}
          onClose={() => setDialog(null)}
          onSave={(rules) =>
            perform(async () => {
              const local = await noise<LocalSummary>({
                action: "update_group_profile",
                name: dialog.group.name,
                description: dialog.group.description,
                rules,
                avatar_data_base64: null,
                avatar_mime_type: null,
                remove_avatar: false,
                relays,
              });
              setSummary(local);
              await refresh();
              setDialog(null);
            })
          }
        />
      )}
      {dialog?.type === "media" && conversation && (
        <MediaGalleryDialog
          group={conversation.group}
          messages={conversation.messages}
          onClose={() => setDialog(null)}
        />
      )}
      {dialog?.type === "report_message" && (
        <ReportMessageDialog
          message={dialog.message}
          busy={busy}
          onClose={() => setDialog(null)}
          onReport={(reason) => perform(async () => {
            await noise({ action: "report_message", message_event_id: dialog.message.event_id, reason, relays });
            await refresh();
            setDialog(null);
          })}
        />
      )}
      {dialog?.type === "delete_message" && (
        <DeleteMessageDialog
          message={dialog.message}
          scopeId={dialog.scopeId}
          busy={busy}
          onClose={() => setDialog(null)}
          onDelete={() => perform(async () => {
            await noise({
              action: "delete_message",
              message_event_id: dialog.message.event_id,
              relays,
            });
            await refresh();
            setDialog(null);
          })}
        />
      )}
      {dialog?.type === "reports" && conversation && (
        <ReportsDialog
          reports={conversation.reports}
          presenceStatuses={selectedPresenceStatuses}
          busy={busy}
          onClose={() => setDialog(null)}
          onDismiss={(report) => perform(async () => {
            await noise({ action: "resolve_report", report_event_id: report.report_event_id, relays });
            await refresh();
          })}
          onDelete={async (report) => {
            setDialog({
              type: "delete_message",
              message: report.message,
              scopeId: conversation.group.group_id,
            });
            return true;
          }}
        />
      )}
      {dialog?.type === "ban_member" && (
        <BanMemberDialog
          member={dialog.member}
          busy={busy}
          onClose={() => setDialog(null)}
          onBan={(deleteMessages) =>
            perform(async () => {
              await noise({
                action: "ban_member",
                member_public_key: dialog.member.public_key,
                delete_messages: deleteMessages,
                relays,
              });
              await refresh();
              setDialog(null);
            })
          }
        />
      )}
      {dialog?.type === "leave_group" && (
        <LeaveGroupDialog
          group={dialog.group}
          busy={busy}
          onClose={() => setDialog(null)}
          onLeave={() =>
            perform(async () => {
              const local = await noise<LocalSummary>({ action: "leave", relays });
              setSummary(local);
              setConversation(null);
              groupConversationCache.current.delete(dialog.group.group_id);
              clearMediaMemoryCache();
              clearProfileImageMemoryCache();
              await refresh();
              setDialog(null);
            })
          }
        />
      )}
      {dialog?.type === "delete_group" && (
        <DeleteGroupDialog
          group={dialog.group}
          busy={busy}
          onClose={() => setDialog(null)}
          onDelete={() =>
            perform(async () => {
              const local = await noise<LocalSummary>({
                action: "delete_group",
                group_id: dialog.group.group_id,
                relays,
              });
              setSummary(local);
              groupConversationCache.current.delete(dialog.group.group_id);
              clearMediaMemoryCache();
              clearProfileImageMemoryCache();
              await refresh();
              setDialog(null);
            })
          }
        />
      )}
      {dialog?.type === "delete_direct" && (
        <DeleteDirectDialog
          direct={dialog.direct}
          busy={busy}
          onClose={() => setDialog(null)}
          onDelete={(forBoth) => perform(async () => {
            const local = await noise<LocalSummary>({ action: "delete_direct", public_key: dialog.direct.public_key, for_both: forBoth, relays });
            setSummary(local);
            setDirectConversation(null);
            directConversationCache.current.delete(dialog.direct.public_key);
            clearMediaMemoryCache();
            await refresh();
            setDialog(null);
          })}
        />
      )}
      {dialog?.type === "delete_account" && (
        <DeleteAccountDialog
          busy={busy}
          ownedGroupCount={summary.groups.filter((group) => group.owner_public_key === summary.identity.public_key).length}
          onClose={() => setDialog({ type: "profile", profile: summary.identity })}
          onDelete={(deleteGroupMessages, deleteDirectThreads) => perform(async () => {
            await noise({
              action: "delete_account",
              delete_group_messages: deleteGroupMessages,
              delete_direct_threads: deleteDirectThreads,
              relays,
            });
            refreshGeneration.current += 1;
            clearMediaMemoryCache();
            clearProfileImageMemoryCache();
            groupConversationCache.current.clear();
            directConversationCache.current.clear();
            setConversation(null);
            setDirectConversation(null);
            setDialog(null);
            setSummary(null);
          }, false)}
        />
      )}
      {dialog?.type === "logout" && (
        <LogoutDialog
          busy={busy}
          onClose={() => setDialog({ type: "profile", profile: summary.identity })}
          onLogout={() => perform(async () => {
            await noise({ action: "logout" });
            refreshGeneration.current += 1;
            clearMediaMemoryCache();
            clearProfileImageMemoryCache();
            groupConversationCache.current.clear();
            directConversationCache.current.clear();
            setConversation(null);
            setDirectConversation(null);
            setDialog(null);
            setSummary(null);
          }, false)}
        />
      )}
      {dialog?.type === "person" && (
        <PersonDialog person={dialog.person} canMessage={dialog.person.public_key !== summary.identity.public_key && dialog.person.accepts_direct_messages} onMessage={() => void startDirect(dialog.person)} onClose={() => setDialog(null)} />
      )}
      {groupMenu && (
        <GroupContextMenu
          x={groupMenu.x}
          y={groupMenu.y}
          onClose={() => setGroupMenu(null)}
          onDelete={() => {
            setDialog({ type: "delete_group", group: groupMenu.group });
            setGroupMenu(null);
          }}
          onLeave={() => {
            setDialog({ type: "leave_group", group: groupMenu.group });
            setGroupMenu(null);
          }}
          isFounder={groupMenu.group.owner_public_key === summary.identity.public_key}
        />
      )}
      {directMenu && <DirectContextMenu
        x={directMenu.x}
        y={directMenu.y}
        onClose={() => setDirectMenu(null)}
        onDelete={() => { setDialog({ type: "delete_direct", direct: directMenu.direct }); setDirectMenu(null); }}
      />}
      {error && <ErrorToast error={error} onClose={() => setError(null)} />}
      <UpdateBanner {...updater} />
    </div>
  );
}

function Sidebar({
  summary,
  mode,
  pendingGroupId,
  directPresenceStatuses,
  selfPresenceStatus,
  onMode,
  onMake,
  onJoin,
  onProfile,
  onContextMenu,
  onDirectContextMenu,
  onSelect,
  onSelectDirect,
}: {
  summary: LocalSummary;
  mode: SidebarMode;
  pendingGroupId: string | null;
  directPresenceStatuses: Map<string, PresenceStatus>;
  selfPresenceStatus: PresenceStatus;
  onMode: (mode: SidebarMode) => void;
  onMake: () => void;
  onJoin: () => void;
  onProfile: () => void;
  onContextMenu: (group: GroupSummary, x: number, y: number) => void;
  onDirectContextMenu: (direct: DirectSummary, x: number, y: number) => void;
  onSelect: (group: GroupSummary) => void;
  onSelectDirect: (direct: DirectSummary) => void;
}) {
  const hasUnreadDirects = summary.directs.some((direct) => direct.has_unread);
  return (
    <aside className="sidebar">
      <div className="sidebar-drag" data-tauri-drag-region>
        <div className="brand" data-tauri-drag-region><NoiseMark size={22} /><strong>noise</strong></div>
      </div>
      <div className="sidebar-tabs">
        <button className={mode === "groups" ? "active" : ""} onClick={() => onMode("groups")}><UsersRound size={14} /> groups</button>
        <button className={mode === "directs" ? "active" : ""} onClick={() => onMode("directs")}><MessagesSquare size={14} /> dms{hasUnreadDirects && <span className="tab-unread-dot" aria-label="unread direct messages" />}</button>
      </div>
      {mode === "groups" && <div className="sidebar-actions">
        <button className="wide-button" onClick={onMake}><Plus size={15} /> create group</button>
        <button className="square-button" onClick={onJoin} title="join group" aria-label="join group"><Radio size={16} /></button>
      </div>}
      <div className="group-list">
        {mode === "groups" ? summary.groups.map((group) => (
          <button
            className={`group-row ${group.is_active ? "active" : ""} ${pendingGroupId === group.group_id ? "pending" : ""}`}
            key={group.group_id}
            onClick={() => onSelect(group)}
            onContextMenu={(event) => {
              event.preventDefault();
              onContextMenu(group, event.clientX, event.clientY);
            }}
          >
            <Avatar name={group.name} image={group.avatar} size={27} square />
            <span>{group.name}</span>
            {group.unread_count > 0 && (
              <span
                className="group-unread-count"
                aria-label={`${group.unread_count} unread ${group.unread_count === 1 ? "message" : "messages"}`}
              >
                {group.unread_count > 99 ? "99+" : group.unread_count}
              </span>
            )}
          </button>
        )) : summary.directs.map((direct) => (
          <button
            className={`group-row direct-row ${direct.is_active ? "active" : ""}`}
            key={direct.public_key}
            onClick={() => onSelectDirect(direct)}
            onContextMenu={(event) => { event.preventDefault(); onDirectContextMenu(direct, event.clientX, event.clientY); }}
          >
            <PresenceAvatar name={direct.username} image={direct.avatar} size={27} status={directPresenceStatuses.get(direct.public_key) ?? "offline"} />
            <span>{direct.username}</span>
            {direct.has_unread && <span className="direct-unread-dot" aria-label={`unread messages from ${direct.username}`} />}
          </button>
        ))}
        {mode === "directs" && summary.directs.length === 0 && <div className="empty-direct-list">message someone from a shared group</div>}
      </div>
      <button className="self-profile" onClick={onProfile}>
        <PresenceAvatar name={summary.identity.username} image={summary.identity.avatar} size={32} status={selfPresenceStatus} />
        <span><strong>{summary.identity.username}</strong><small>{summary.identity.bio || "build your identity"}</small></span>
        <Settings2 size={13} />
      </button>
    </aside>
  );
}

function GroupContextMenu({
  x,
  y,
  isFounder,
  onClose,
  onDelete,
  onLeave,
}: {
  x: number;
  y: number;
  isFounder: boolean;
  onClose: () => void;
  onDelete: () => void;
  onLeave: () => void;
}) {
  useEffect(() => {
    const close = () => onClose();
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("mousedown", close);
    window.addEventListener("blur", close);
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("mousedown", close);
      window.removeEventListener("blur", close);
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [onClose]);
  return (
    <div
      className="group-context-menu"
      style={{ left: Math.min(x, window.innerWidth - 190), top: Math.min(y, window.innerHeight - 58) }}
      onMouseDown={(event) => event.stopPropagation()}
    >
      {isFounder
        ? <button onClick={onDelete}><Trash2 size={14} /> delete group</button>
        : <button onClick={onLeave}><LogOut size={14} /> leave group</button>}
    </div>
  );
}

function DirectContextMenu({ x, y, onClose, onDelete }: { x: number; y: number; onClose: () => void; onDelete: () => void }) {
  useEffect(() => {
    const close = () => onClose();
    const onKeyDown = (event: KeyboardEvent) => { if (event.key === "Escape") onClose(); };
    window.addEventListener("mousedown", close);
    window.addEventListener("blur", close);
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("mousedown", close);
      window.removeEventListener("blur", close);
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [onClose]);
  return <div className="group-context-menu" style={{ left: Math.min(x, window.innerWidth - 190), top: Math.min(y, window.innerHeight - 58) }} onMouseDown={(event) => event.stopPropagation()}><button onClick={onDelete}><Trash2 size={14} /> delete conversation</button></div>;
}

function MessageContextMenu({ x, y, busy, onClose, onReact, onReply, onDownload, onReport, onDelete, onBan }: { x: number; y: number; busy: boolean; onClose: () => void; onReact?: () => void; onReply: () => void; onDownload?: () => Promise<boolean>; onReport?: () => void; onDelete?: () => void; onBan?: () => void }) {
  const [downloading, setDownloading] = useState(false);
  const [downloaded, setDownloaded] = useState(false);
  useEffect(() => {
    const close = () => onClose();
    const onKeyDown = (event: KeyboardEvent) => { if (event.key === "Escape") onClose(); };
    window.addEventListener("mousedown", close);
    window.addEventListener("blur", close);
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("mousedown", close);
      window.removeEventListener("blur", close);
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [onClose]);
  const menuHeight = 50 + (onReact ? 42 : 0) + (onDownload ? 42 : 0) + (onReport ? 42 : 0) + (onDelete ? 42 : 0) + (onBan ? 42 : 0);
  return <div className="member-context-menu" style={{ left: Math.min(x, window.innerWidth - 200), top: Math.min(y, window.innerHeight - menuHeight) }} onMouseDown={(event) => event.stopPropagation()}>{onReact && <button disabled={busy || downloading} onClick={onReact}><SmilePlus size={14} /> react</button>}<button disabled={busy || downloading} onClick={onReply}><Reply size={14} /> reply</button>{onDownload && <button disabled={busy || downloading || downloaded} onClick={() => { setDownloading(true); void onDownload().then((success) => { setDownloading(false); if (success) { setDownloaded(true); window.setTimeout(onClose, 650); } else { onClose(); } }); }}>{downloaded ? <Check size={14} /> : downloading ? <LoaderCircle className="spinner" size={14} /> : <Download size={14} />}{downloaded ? "downloaded" : downloading ? "downloading" : "download media"}</button>}{onReport && <button className="report-action" disabled={busy || downloading} onClick={onReport}><TriangleAlert size={14} /> report message</button>}{onDelete && <button className="danger" disabled={busy || downloading} onClick={onDelete}><Trash2 size={14} /> delete message</button>}{onBan && <button className="danger" disabled={busy || downloading} onClick={onBan}><UserRoundX size={14} /> ban member</button>}</div>;
}

function MemberContextMenu({ member, x, y, canDesignate, canBan, onClose, onMessage, onSetModerator, onBan }: { member: MemberSummary; x: number; y: number; canDesignate: boolean; canBan: boolean; onClose: () => void; onMessage: () => void; onSetModerator: (enabled: boolean) => void; onBan: () => void }) {
  useEffect(() => {
    const close = () => onClose();
    const onKeyDown = (event: KeyboardEvent) => { if (event.key === "Escape") onClose(); };
    window.addEventListener("mousedown", close);
    window.addEventListener("blur", close);
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("mousedown", close);
      window.removeEventListener("blur", close);
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [onClose]);
  return (
    <div className="member-context-menu" style={{ left: Math.min(x - 188, window.innerWidth - 196), top: Math.min(y, window.innerHeight - (48 + (canDesignate ? 42 : 0) + (canBan ? 42 : 0))) }} onMouseDown={(event) => event.stopPropagation()}>
      {member.accepts_direct_messages
        ? <button onClick={onMessage}><MessageCircle size={14} /> message</button>
        : <button disabled><MessageCircle size={14} /> DMs closed</button>}
      {canDesignate && <button onClick={() => onSetModerator(!member.is_moderator)}>{member.is_moderator ? <ShieldOff size={14} /> : <Shield size={14} />}{member.is_moderator ? "remove moderator" : "make moderator"}</button>}
      {canBan && <button className="danger" onClick={onBan}><UserRoundX size={14} /> ban member</button>}
    </div>
  );
}

type SavedMessageScroll =
  | { stuckAtBottom: true }
  | { stuckAtBottom: false; trackedMessageId: string; pixelOffset: number };

function useChunkedMessageList<T extends { event_id: string }>(
  conversationKey: string,
  messages: T[],
) {
  const ref = useRef<HTMLDivElement>(null);
  const positionedConversation = useRef<string | null>(null);
  const previousMessageCount = useRef(messages.length);
  const savedScroll = useRef<SavedMessageScroll>({ stuckAtBottom: true });
  const loadingOlder = useRef(false);
  const [atBottom, setAtBottom] = useState(true);
  const [visibleCount, setVisibleCount] = useState(() =>
    Math.min(
      messages.length,
      Math.max(INITIAL_MESSAGE_COUNT, renderedMessageCounts.get(conversationKey) ?? 0),
    )
  );
  const incomingCount = Math.max(0, messages.length - previousMessageCount.current);
  const renderedCount = Math.min(messages.length, visibleCount + incomingCount);
  const visibleMessages = messages.slice(Math.max(0, messages.length - renderedCount));
  const hasOlder = renderedCount < messages.length;

  const saveScrollPosition = useCallback(() => {
    const element = ref.current;
    if (!element) return;
    const bottomDistance = element.scrollHeight - element.scrollTop - element.clientHeight;
    if (bottomDistance < 96) {
      savedScroll.current = { stuckAtBottom: true };
      setAtBottom(true);
      return;
    }
    setAtBottom(false);
    const containerTop = element.getBoundingClientRect().top;
    const rows = element.querySelectorAll<HTMLElement>("[data-message-id]");
    for (const row of rows) {
      const bounds = row.getBoundingClientRect();
      if (bounds.bottom > containerTop + 1) {
        savedScroll.current = {
          stuckAtBottom: false,
          trackedMessageId: row.dataset.messageId ?? "",
          pixelOffset: bounds.top - containerTop,
        };
        return;
      }
    }
  }, []);

  useLayoutEffect(() => {
    const element = ref.current;
    if (!element) return;
    if (positionedConversation.current !== conversationKey) {
      element.scrollTop = element.scrollHeight;
      positionedConversation.current = conversationKey;
      savedScroll.current = { stuckAtBottom: true };
      setAtBottom(true);
    } else if (savedScroll.current.stuckAtBottom) {
      const bottomDistance = element.scrollHeight - element.scrollTop - element.clientHeight;
      if (bottomDistance > 0) element.scrollBy({ top: bottomDistance, behavior: "auto" });
    } else {
      const tracked = element.querySelector<HTMLElement>(
        `[data-message-id="${CSS.escape(savedScroll.current.trackedMessageId)}"]`,
      );
      if (tracked) {
        const containerTop = element.getBoundingClientRect().top;
        const currentOffset = tracked.getBoundingClientRect().top - containerTop;
        const correction = currentOffset - savedScroll.current.pixelOffset;
        if (Math.abs(correction) > 0.5) {
          element.scrollBy({ top: correction, behavior: "auto" });
        }
      }
    }
    if (visibleCount !== renderedCount) {
      setVisibleCount(renderedCount);
    }
    renderedMessageCounts.set(conversationKey, renderedCount);
    previousMessageCount.current = messages.length;
    loadingOlder.current = false;
    if (
      hasOlder
      && element.scrollHeight <= element.clientHeight + 1
      && !loadingOlder.current
    ) {
      loadingOlder.current = true;
      setVisibleCount((current) =>
        Math.min(messages.length, current + MESSAGE_PAGE_SIZE)
      );
    }
  });

  const onScroll = useCallback(() => {
    const element = ref.current;
    if (!element) return;
    saveScrollPosition();
    if (!hasOlder || loadingOlder.current || element.scrollTop > element.clientHeight) return;
    loadingOlder.current = true;
    setVisibleCount((current) => {
      const next = Math.min(messages.length, current + MESSAGE_PAGE_SIZE);
      renderedMessageCounts.set(conversationKey, next);
      return next;
    });
  }, [conversationKey, hasOlder, messages.length, saveScrollPosition]);

  return { ref, onScroll, visibleMessages, renderedCount, atBottom };
}

function useAutosizeComposer(
  ref: RefObject<HTMLTextAreaElement | null>,
  value: string,
) {
  useLayoutEffect(() => {
    const element = ref.current;
    if (!element) return;
    element.style.height = "0px";
    const height = Math.min(element.scrollHeight, 168);
    element.style.height = `${Math.max(height, 42)}px`;
    element.style.overflowY = element.scrollHeight > 168 ? "auto" : "hidden";
  }, [ref, value]);
}

function ConversationPanel({
  conversation,
  busy,
  hasBackground,
  canEditGroup,
  unreadCount,
  selfPublicKey,
  presenceStatuses,
  onGroupSettings,
  onReports,
  onMedia,
  onRules,
  onPerson,
  onMessage,
  onDeleteMessage,
  onDownload,
  onReaction,
  onSetModerator,
  onBan,
  onReport,
  onReachedBottom,
  onSend,
}: {
  conversation: Conversation;
  busy: boolean;
  hasBackground: boolean;
  canEditGroup: boolean;
  unreadCount: number;
  selfPublicKey: string;
  presenceStatuses: Map<string, PresenceStatus>;
  onGroupSettings: () => void;
  onReports: () => void;
  onMedia: () => void;
  onRules: () => void;
  onPerson: (person: PersonSummary) => void;
  onMessage: (person: PersonSummary) => void;
  onDeleteMessage: (message: MessageSummary) => void;
  onDownload: (message: MessageSummary) => Promise<boolean>;
  onReaction: (message: MessageSummary, emoji: string) => Promise<void>;
  onSetModerator: (member: MemberSummary, enabled: boolean) => Promise<boolean>;
  onBan: (member: MemberSummary) => void;
  onReport: (message: MessageSummary) => void;
  onReachedBottom: () => void;
  onSend: (text: string, attachment: PendingMedia | null, onProgress: (progress: number) => void, replyToMessageId: string | null, signal: AbortSignal) => Promise<boolean>;
}) {
  const [draft, setDraft] = useState("");
  const [attachment, setAttachment] = useState<PendingMedia | null>(null);
  const [attachmentError, setAttachmentError] = useState<string | null>(null);
  const [uploadProgress, setUploadProgress] = useState<number | null>(null);
  const [memberMenu, setMemberMenu] = useState<{ member: MemberSummary; x: number; y: number } | null>(null);
  const [messageMenu, setMessageMenu] = useState<{ message: MessageSummary; x: number; y: number } | null>(null);
  const [reactionPicker, setReactionPicker] = useState<{ message: MessageSummary; x: number; y: number } | null>(null);
  const [replyingTo, setReplyingTo] = useState<MessageSummary | null>(null);
  const fileInput = useRef<HTMLInputElement>(null);
  const composerInput = useRef<HTMLTextAreaElement>(null);
  const uploadController = useRef<AbortController | null>(null);
  useAutosizeComposer(composerInput, draft);
  const messageList = useChunkedMessageList(
    conversation.group.group_id,
    conversation.messages,
  );
  useEffect(() => {
    if (messageList.atBottom && unreadCount > 0) onReachedBottom();
  }, [conversation.messages.length, messageList.atBottom, onReachedBottom, unreadCount]);
  useWarmConversationMedia(
    conversation.messages,
    conversation.group.group_id,
    messageList.renderedCount,
  );
  const selfMember = conversation.members.find((member) => member.public_key === selfPublicKey);
  const canModerate = canEditGroup || selfMember?.is_moderator === true;
  const canSendMessages = canModerate || conversation.group.members_can_send_messages;
  const canSendMedia = canModerate || conversation.group.members_can_send_media;
  const sortedMembers = [...conversation.members].sort((left, right) => {
    const roleRank = (member: MemberSummary) =>
      member.public_key === conversation.group.owner_public_key ? 0 : member.is_moderator ? 1 : 2;
    const presenceRank = (member: MemberSummary) => {
      const status = presenceStatuses.get(member.public_key) ?? "offline";
      return status === "online" ? 0 : status === "recently-active" ? 1 : 2;
    };
    return roleRank(left) - roleRank(right)
      || presenceRank(left) - presenceRank(right)
      || left.username.localeCompare(right.username);
  });
  const moderationMembers = sortedMembers.filter((member) =>
    member.public_key === conversation.group.owner_public_key || member.is_moderator
  );
  const regularMembers = sortedMembers.filter((member) =>
    member.public_key !== conversation.group.owner_public_key && !member.is_moderator
  );
  const renderMember = (member: MemberSummary) => (
    <div key={member.public_key} className="member-sidebar-row">
      <button className="member-sidebar-main" onClick={() => onPerson({
        ...member,
        presence_status: presenceStatuses.get(member.public_key) ?? "offline",
      })}>
        <span className="member-avatar-wrap">
          <PresenceAvatar name={member.username} image={member.avatar} size={30} status={presenceStatuses.get(member.public_key) ?? "offline"} />
          {member.public_key === conversation.group.owner_public_key
            ? <span className="member-role-mark founder" aria-label="group founder" title="group founder"><Crown size={9} /></span>
            : member.is_moderator && <span className="member-role-mark moderator" aria-label="group moderator" title="group moderator"><Shield size={8} /></span>}
        </span>
        <span className="member-sidebar-copy">
          <strong>{member.username}</strong>
          <span className="member-sidebar-meta">
            <small>{member.bio || "tuned in"}</small>
          </span>
        </span>
      </button>
      {member.public_key !== selfPublicKey && <button className="member-actions" aria-label={`actions for ${member.username}`} onClick={(event) => { const rect = event.currentTarget.getBoundingClientRect(); setMemberMenu({ member, x: rect.right, y: rect.bottom + 4 }); }}><MoreHorizontal size={15} /></button>}
    </div>
  );
  const attachmentPreview = attachment?.previewUrl;
  useEffect(() => () => {
    if (attachmentPreview) URL.revokeObjectURL(attachmentPreview);
  }, [attachmentPreview]);
  async function chooseMedia(file?: File) {
    if (!file) return;
    setAttachmentError(null);
    if (!/^(image|video|audio)\//.test(file.type)) {
      setAttachmentError("choose an image, video, or audio file");
      return;
    }
    if (!file.size || file.size > 500 * 1024 * 1024) {
      setAttachmentError("media can be up to 500 MB");
      return;
    }
    setAttachment({
      name: file.name,
      mimeType: file.type,
      byteLength: file.size,
      file,
      previewUrl: URL.createObjectURL(file),
      mediaPreview: file.type.startsWith("video/")
        ? prepareVideoPreview(file)
        : file.type.startsWith("image/")
          ? prepareImagePreview(file)
          : null,
    });
    if (fileInput.current) fileInput.current.value = "";
  }
  async function submit() {
    const text = draft.trim();
    if ((!text && !attachment) || busy || (text && !canSendMessages) || (attachment && !canSendMedia)) return;
    const submittedDraft = draft;
    const submittedReply = replyingTo;
    const pendingAttachment = attachment;
    setDraft("");
    setReplyingTo(null);
    if (pendingAttachment) setUploadProgress(0);
    const controller = new AbortController();
    uploadController.current = controller;
    const sent = await onSend(text, pendingAttachment, setUploadProgress, submittedReply?.message_id ?? null, controller.signal);
    if (uploadController.current === controller) uploadController.current = null;
    setUploadProgress(null);
    if (sent) {
      setAttachment(null);
    } else {
      setDraft((current) => current || submittedDraft);
      setReplyingTo((current) => current ?? submittedReply);
    }
  }
  return (
    <div className={`conversation group-conversation ${hasBackground ? "has-background" : ""}`}>
      <header className="chat-header" data-tauri-drag-region>
        <div className="group-identity static" data-tauri-drag-region>
          <Avatar name={conversation.group.name} image={conversation.group.avatar} size={36} square />
          <span><strong>{conversation.group.name}</strong><small>{conversation.group.description || "group"}</small></span>
        </div>
        <div className="chat-header-actions">
          {canModerate && <button className={`icon-button media-button reports-button ${conversation.reports.length ? "has-reports" : ""}`} onClick={onReports} aria-label="moderation reports" title="moderation reports"><TriangleAlert size={17} />{conversation.reports.length > 0 && <i />}</button>}
          {canEditGroup && <button className="icon-button media-button" onClick={onGroupSettings} aria-label="group settings" title="group settings"><Settings2 size={17} /></button>}
          <button className="icon-button media-button" onClick={onMedia} aria-label="group media" title="group media"><Images size={17} /></button>
          <button className="rules-button" onClick={onRules}>Rules</button>
          {busy && <LoaderCircle className="spinner" size={14} />}
        </div>
      </header>
      <div className="messages" ref={messageList.ref} onScroll={messageList.onScroll}>
        {conversation.messages.length === 0 && <div className="quiet">the group is quiet</div>}
        {messageList.visibleMessages.map((item) => (
          <MessageRow key={item.event_id} message={item} own={item.author_public_key === selfPublicKey} presence={presenceStatuses.get(item.author_public_key) ?? "offline"} replyTo={conversation.messages.find((candidate) => candidate.message_id === item.reply_to_message_id)} onContextMenu={item.optimistic ? undefined : (event) => { event.preventDefault(); setMessageMenu({ message: item, x: event.clientX, y: event.clientY }); }} onToggleReaction={(emoji) => void onReaction(item, emoji)} onPerson={onPerson} mediaScopeId={conversation.group.group_id} />
        ))}
      </div>
      {selfMember && (canSendMessages || canSendMedia) ? <div className="composer">
        {replyingTo && <ReplyTarget message={replyingTo} mediaScopeId={conversation.group.group_id} onClose={() => setReplyingTo(null)} />}
        {attachment && <div className={`attachment-draft ${attachment.mimeType.startsWith("audio/") ? "audio" : ""}`}>{attachment.mimeType.startsWith("image/") ? <img src={attachment.previewUrl} alt="" /> : attachment.mimeType.startsWith("video/") ? <video src={attachment.previewUrl} muted playsInline preload="metadata" onLoadedMetadata={(event) => { const video = event.currentTarget; if (Number.isFinite(video.duration) && video.duration > 0) video.currentTime = Math.min(0.25, video.duration / 2); }} /> : <div className="audio-thumbnail"><AudioWaveform size={30} /></div>}{uploadProgress !== null && <div className="attachment-progress"><i style={{ width: `${uploadProgress}%` }} /><span>{uploadProgress}%</span></div>}<button onClick={() => { uploadController.current?.abort(); setAttachment(null); setUploadProgress(null); }} aria-label={uploadProgress !== null ? "cancel upload" : "remove attachment"}><X size={14} /></button></div>}
        {attachmentError && <div className="attachment-error">{attachmentError}</div>}
        <button className="attach-button" disabled={busy || !canSendMedia} onClick={() => fileInput.current?.click()} aria-label="attach media" title={canSendMedia ? "attach media" : "members cannot send media"}><Paperclip size={17} /></button>
        <input ref={fileInput} hidden type="file" accept="image/*,video/*,audio/*" onChange={(event) => void chooseMedia(event.target.files?.[0])} />
        <textarea
          ref={composerInput}
          rows={1}
          value={draft}
          disabled={!canSendMessages}
          placeholder={canSendMessages ? "send noise" : "members cannot send messages"}
          onChange={(event) => setDraft(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Enter" && !event.shiftKey) {
              event.preventDefault();
              void submit();
            }
          }}
        />
        <button className="send-button" disabled={(!draft.trim() && !attachment) || busy || (!!draft.trim() && !canSendMessages) || (!!attachment && !canSendMedia)} onClick={() => void submit()}><ArrowUp size={17} /></button>
      </div> : selfMember ? <div className="membership-revoked"><ShieldOff size={16} /> only moderators can post right now</div> : <div className="membership-revoked"><UserRoundX size={16} /> you no longer have access to this group</div>}
      <aside className="member-sidebar">
        <div className="member-sidebar-list">
          <section className="member-sidebar-section">
            <div className="member-sidebar-heading">
              <strong>moderation</strong>
              <span>{moderationMembers.length}</span>
            </div>
            {moderationMembers.map(renderMember)}
          </section>
          <section className="member-sidebar-section">
            <div className="member-sidebar-heading">
              <strong>members</strong>
              <span>{regularMembers.length}</span>
            </div>
            {regularMembers.map(renderMember)}
          </section>
        </div>
      </aside>
      <AppVersionFooter />
      {memberMenu && <MemberContextMenu
        member={memberMenu.member}
        x={memberMenu.x}
        y={memberMenu.y}
        canDesignate={canEditGroup}
        canBan={canModerate && memberMenu.member.public_key !== conversation.group.owner_public_key && (canEditGroup || !memberMenu.member.is_moderator)}
        onClose={() => setMemberMenu(null)}
        onMessage={() => { onMessage(memberMenu.member); setMemberMenu(null); }}
        onSetModerator={(enabled) => { void onSetModerator(memberMenu.member, enabled); setMemberMenu(null); }}
        onBan={() => { onBan(memberMenu.member); setMemberMenu(null); }}
      />}
      {messageMenu && <MessageContextMenu
        x={messageMenu.x}
        y={messageMenu.y}
        busy={busy}
        onClose={() => setMessageMenu(null)}
        onReact={() => {
          setReactionPicker({
            message: messageMenu.message,
            x: messageMenu.x,
            y: messageMenu.y,
          });
          setMessageMenu(null);
        }}
        onReply={() => { setReplyingTo(messageMenu.message); setMessageMenu(null); window.setTimeout(() => composerInput.current?.focus(), 0); }}
        onDownload={messageMenu.message.attachment ? () => onDownload(messageMenu.message) : undefined}
        onReport={!canModerate && messageMenu.message.author_public_key !== selfPublicKey && !conversation.reported_message_event_ids.includes(messageMenu.message.event_id) ? () => { onReport(messageMenu.message); setMessageMenu(null); } : undefined}
        onDelete={(canModerate || messageMenu.message.author_public_key === selfPublicKey) ? () => { onDeleteMessage(messageMenu.message); setMessageMenu(null); } : undefined}
        onBan={(() => {
          const member = conversation.members.find((candidate) => candidate.public_key === messageMenu.message.author_public_key);
          const canBanAuthor = member
            && member.public_key !== selfPublicKey
            && member.public_key !== conversation.group.owner_public_key
            && (canEditGroup || !member.is_moderator);
          return canBanAuthor ? () => { onBan(member); setMessageMenu(null); } : undefined;
        })()}
      />}
      {reactionPicker && <ReactionPicker
        x={reactionPicker.x}
        y={reactionPicker.y}
        onClose={() => setReactionPicker(null)}
        onPick={(emoji) => {
          const target = reactionPicker.message;
          setReactionPicker(null);
          void onReaction(target, emoji);
        }}
      />}
    </div>
  );
}

function DirectConversationPanel({ conversation, busy, selfPublicKey, selfPresence, contactPresence, onPerson, onDelete, onDownload, onSend }: { conversation: DirectConversation; busy: boolean; selfPublicKey: string; selfPresence: PresenceStatus; contactPresence: PresenceStatus; onPerson: (person: PersonSummary) => void; onDelete: () => void; onDownload: (message: MessageSummary) => Promise<boolean>; onSend: (text: string, attachment: PendingMedia | null, onProgress: (progress: number) => void, replyToMessageId: string | null, signal: AbortSignal) => Promise<boolean> }) {
  const [draft, setDraft] = useState("");
  const [attachment, setAttachment] = useState<PendingMedia | null>(null);
  const [attachmentError, setAttachmentError] = useState<string | null>(null);
  const [uploadProgress, setUploadProgress] = useState<number | null>(null);
  const [messageMenu, setMessageMenu] = useState<{ message: MessageSummary; x: number; y: number } | null>(null);
  const [replyingTo, setReplyingTo] = useState<MessageSummary | null>(null);
  const fileInput = useRef<HTMLInputElement>(null);
  const composerInput = useRef<HTMLTextAreaElement>(null);
  const uploadController = useRef<AbortController | null>(null);
  useAutosizeComposer(composerInput, draft);
  const messageList = useChunkedMessageList(
    conversation.contact.public_key,
    conversation.messages,
  );
  useWarmConversationMedia(
    conversation.messages,
    conversation.media_scope_id,
    messageList.renderedCount,
  );
  const attachmentPreview = attachment?.previewUrl;
  useEffect(() => () => { if (attachmentPreview) URL.revokeObjectURL(attachmentPreview); }, [attachmentPreview]);
  async function chooseMedia(file?: File) {
    if (!file) return;
    setAttachmentError(null);
    if (!/^(image|video|audio)\//.test(file.type)) {
      setAttachmentError("choose an image, video, or audio file");
      return;
    }
    if (!file.size || file.size > 500 * 1024 * 1024) {
      setAttachmentError("media can be up to 500 MB");
      return;
    }
    setAttachment({
      name: file.name,
      mimeType: file.type,
      byteLength: file.size,
      file,
      previewUrl: URL.createObjectURL(file),
      mediaPreview: file.type.startsWith("video/")
        ? prepareVideoPreview(file)
        : file.type.startsWith("image/")
          ? prepareImagePreview(file)
          : null,
    });
    if (fileInput.current) fileInput.current.value = "";
  }
  async function submit() {
    const text = draft.trim();
    if ((!text && !attachment) || busy) return;
    const submittedDraft = draft;
    const submittedReply = replyingTo;
    const pendingAttachment = attachment;
    setDraft("");
    setReplyingTo(null);
    if (pendingAttachment) setUploadProgress(0);
    const controller = new AbortController();
    uploadController.current = controller;
    const sent = await onSend(text, pendingAttachment, setUploadProgress, submittedReply?.message_id ?? null, controller.signal);
    if (uploadController.current === controller) uploadController.current = null;
    setUploadProgress(null);
    if (sent) {
      setAttachment(null);
    } else {
      setDraft((current) => current || submittedDraft);
      setReplyingTo((current) => current ?? submittedReply);
    }
  }
  const person = { public_key: conversation.contact.public_key, username: conversation.contact.username, bio: conversation.contact.bio, avatar: conversation.contact.avatar, accepts_direct_messages: conversation.contact.accepts_direct_messages, presence_status: contactPresence };
  return (
    <div className="conversation direct-conversation">
      <header className="chat-header" data-tauri-drag-region>
        <div className="group-identity static" data-tauri-drag-region>
          <PresenceAvatar name={conversation.contact.username} image={conversation.contact.avatar} size={36} status={contactPresence} />
          <span><strong>{conversation.contact.username}</strong><small>{conversation.contact.bio || "encrypted direct message"}</small></span>
        </div>
        <div className="chat-header-actions"><button className="icon-button media-button delete-direct-button" onClick={onDelete} aria-label="delete conversation" title="delete conversation"><Trash2 size={16} /></button>{busy && <LoaderCircle className="spinner" size={14} />}</div>
      </header>
      <div className="messages" ref={messageList.ref} onScroll={messageList.onScroll}>
        {conversation.messages.length === 0 && <div className="quiet">start the conversation</div>}
        {messageList.visibleMessages.map((item) => <MessageRow key={item.event_id} message={item} own={item.author_public_key === selfPublicKey} presence={item.author_public_key === selfPublicKey ? selfPresence : contactPresence} replyTo={conversation.messages.find((candidate) => candidate.message_id === item.reply_to_message_id)} onContextMenu={item.optimistic ? undefined : (event) => { event.preventDefault(); setMessageMenu({ message: item, x: event.clientX, y: event.clientY }); }} onPerson={onPerson} mediaScopeId={conversation.media_scope_id} />)}
      </div>
      {conversation.contact.accepts_direct_messages ? <div className="composer">
        {replyingTo && <ReplyTarget message={replyingTo} mediaScopeId={conversation.media_scope_id} onClose={() => setReplyingTo(null)} />}
        {attachment && <div className={`attachment-draft ${attachment.mimeType.startsWith("audio/") ? "audio" : ""}`}>{attachment.mimeType.startsWith("image/") ? <img src={attachment.previewUrl} alt="" /> : attachment.mimeType.startsWith("video/") ? <video src={attachment.previewUrl} muted playsInline preload="metadata" onLoadedMetadata={(event) => primeVideoFrame(event.currentTarget)} /> : <div className="audio-thumbnail"><AudioWaveform size={30} /></div>}{uploadProgress !== null && <div className="attachment-progress"><i style={{ width: `${uploadProgress}%` }} /><span>{uploadProgress}%</span></div>}<button onClick={() => { uploadController.current?.abort(); setAttachment(null); setUploadProgress(null); }} aria-label={uploadProgress !== null ? "cancel upload" : "remove attachment"}><X size={14} /></button></div>}
        {attachmentError && <div className="attachment-error">{attachmentError}</div>}
        <button className="attach-button" disabled={busy} onClick={() => fileInput.current?.click()} aria-label="attach media"><Paperclip size={17} /></button>
        <input ref={fileInput} hidden type="file" accept="image/*,video/*,audio/*" onChange={(event) => void chooseMedia(event.target.files?.[0])} />
        <textarea ref={composerInput} rows={1} value={draft} placeholder={`message ${conversation.contact.username}`} onChange={(event) => setDraft(event.target.value)} onKeyDown={(event) => { if (event.key === "Enter" && !event.shiftKey) { event.preventDefault(); void submit(); } }} />
        <button className="send-button" disabled={(!draft.trim() && !attachment) || busy} onClick={() => void submit()}><ArrowUp size={17} /></button>
      </div> : <div className="membership-revoked"><MessageCircle size={16} /> {conversation.contact.username} isn’t accepting DMs</div>}
      <aside className="member-sidebar direct-profile-sidebar">
        <button className="direct-profile-identity" onClick={() => onPerson(person)}>
          <PresenceAvatar name={conversation.contact.username} image={conversation.contact.avatar} size={72} status={contactPresence} />
          <strong>{conversation.contact.username}</strong>
        </button>
        <div className="noise-signature"><small>Noise Signature</small><strong>{noiseSignature(conversation.contact.public_key)}</strong></div>
        <p>{conversation.contact.bio || "no bio yet"}</p>
        <span className={`direct-profile-status ${conversation.contact.accepts_direct_messages ? "open" : "closed"}`}><i />{conversation.contact.accepts_direct_messages ? "accepting DMs" : "DMs closed"}</span>
      </aside>
      <AppVersionFooter />
      {messageMenu && <MessageContextMenu x={messageMenu.x} y={messageMenu.y} busy={busy} onClose={() => setMessageMenu(null)} onReply={() => { setReplyingTo(messageMenu.message); setMessageMenu(null); window.setTimeout(() => composerInput.current?.focus(), 0); }} onDownload={messageMenu.message.attachment ? () => onDownload(messageMenu.message) : undefined} />}
    </div>
  );
}

function ReplyTarget({ message, mediaScopeId, onClose }: { message: MessageSummary; mediaScopeId?: string; onClose: () => void }) {
  return <div className="reply-target"><Reply size={15} />{message.attachment && <ReplyMediaThumbnail message={message as MessageSummary & { attachment: MediaAttachment }} scopeId={mediaScopeId} />}<span><small>replying to {message.username}</small><strong>{replyPreview(message)}</strong></span><button onClick={onClose} aria-label="cancel reply"><X size={14} /></button></div>;
}

function AppVersionFooter() {
  const [version, setVersion] = useState(cachedAppVersion);
  const [showAbout, setShowAbout] = useState(false);
  useEffect(() => {
    if (version || !isTauri) return;
    let active = true;
    void import("@tauri-apps/api/app")
      .then(({ getVersion }) => getVersion())
      .then((currentVersion) => {
        cachedAppVersion = currentVersion;
        if (active) setVersion(currentVersion);
      })
      .catch(() => undefined);
    return () => { active = false; };
  }, [version]);
  const browserPlatform = `${navigator.platform ?? ""} ${navigator.userAgent}`;
  const desktopDownloadLabel = /Windows/i.test(browserPlatform)
    ? "Download Windows app"
    : /Macintosh|Mac OS X|MacIntel/i.test(browserPlatform)
      ? "Download macOS app"
      : "Download desktop app";
  return <>
    <div className="member-sidebar-footer">
      {isTauri
        ? <span>{version ? `Beta V.${version}` : "Beta"}</span>
        : <a href="https://makenoise.chat/#download" target="_blank" rel="noreferrer"><Download size={13} />{desktopDownloadLabel}</a>}
      <button onClick={() => setShowAbout(true)} aria-label="about noise" title="about noise"><Info size={13} /></button>
    </div>
    {showAbout && <AboutNoiseDialog onClose={() => setShowAbout(false)} />}
  </>;
}

function AboutNoiseDialog({ onClose }: { onClose: () => void }) {
  return (
    <Modal onClose={onClose} wide className="about-noise-modal">
      <DialogHeading icon={<NoiseMark size={30} />} title="how noise works" detail="private groups without phone numbers, email addresses, or a central owner" />
      <div className="about-noise">
        <section>
          <strong>your account is yours</strong>
          <p>You sign in with a noise ID and password instead of a phone number or email. Your display name and photo can change without changing who you are to the people and groups that know you.</p>
        </section>
        <section>
          <strong>locked before it leaves</strong>
          <p>noise locks messages, DMs, profiles, and uploads on your device before sending them anywhere. Relay machines carry the locked data, but they do not receive the readable contents.</p>
        </section>
        <section>
          <strong>group locks change with membership</strong>
          <p>When someone joins, leaves, or is banned, the group gets a new lock for future activity. New members can receive the group’s earlier history, while removed members cannot open anything posted afterward.</p>
        </section>
        <section>
          <strong>frequencies are invitations</strong>
          <p>A group’s 12-digit frequency helps someone find the group and ask to join. It is not the key that unlocks the chat, and the founder can revoke it or replace it with a new one.</p>
        </section>
        <section>
          <strong>relays keep noise available</strong>
          <p>Relays hold locked group activity so people can catch up after being offline. One relay can also pass a request to another, helping prevent the machine storing the data from seeing where the request began.</p>
        </section>
        <section>
          <strong>media is spread out</strong>
          <p>Photos and videos are locked, split into pieces, and spread across several relays with recovery pieces added. No relay needs the whole file, and noise can rebuild it when enough pieces are available.</p>
        </section>
        <section className="about-boundary">
          <strong>fyi</strong>
          <p>noise cannot stop someone from taking a screenshot, exporting content, or reading an unlocked or compromised device. Its security design has not yet received an independent audit.</p>
        </section>
      </div>
      <DialogButtons><button className="primary" onClick={onClose}>done</button></DialogButtons>
    </Modal>
  );
}

function MessageRow({
  message,
  own,
  presence,
  replyTo,
  onContextMenu,
  onToggleReaction,
  onPerson,
  mediaScopeId,
}: {
  message: MessageSummary;
  own: boolean;
  presence?: PresenceStatus;
  replyTo?: MessageSummary;
  onContextMenu?: (event: React.MouseEvent<HTMLElement>) => void;
  onToggleReaction?: (emoji: string) => void;
  onPerson: (person: PersonSummary) => void;
  mediaScopeId?: string;
}) {
  const person = { public_key: message.author_public_key, username: message.username, bio: message.bio, avatar: message.avatar, accepts_direct_messages: message.accepts_direct_messages, presence_status: presence };
  const localAttachment = message.local_attachment ?? sentMediaPreviewCache.get(message.event_id);
  const jumboEmojiCount = !localAttachment && !message.attachment
    ? emojiOnlyCount(message.text)
    : null;
  return (
    <article
      className={`message-row ${own ? "own" : ""} ${message.optimistic ? "optimistic" : ""}`}
      data-message-id={message.event_id}
      onMouseDown={onContextMenu ? (event) => { if (event.button === 2) event.preventDefault(); } : undefined}
      onContextMenu={onContextMenu ? (event) => {
        event.preventDefault();
        window.getSelection()?.removeAllRanges();
        onContextMenu?.(event);
      } : undefined}
    >
      <button onClick={() => onPerson(person)}><PresenceAvatar name={message.username} image={message.avatar} size={34} status={presence ?? "offline"} /></button>
      <div className="message-body"><div className="message-meta"><button onClick={() => onPerson(person)}>{message.username}</button></div>{message.reply_to_message_id && <div className="message-reply-reference">{replyTo ? <>{replyTo.attachment && <ReplyMediaThumbnail message={replyTo as MessageSummary & { attachment: MediaAttachment }} scopeId={mediaScopeId} />}<span className="message-reply-copy"><strong>{replyTo.username}</strong><span>{replyPreview(replyTo)}</span></span></> : <span>original message unavailable</span>}</div>}{message.text && <p className={jumboEmojiCount ? `emoji-only emoji-only-${jumboEmojiCount}` : undefined}>{message.text}</p>}{localAttachment ? <LocalMessageMedia attachment={localAttachment} manifest={message.attachment} /> : message.attachment && <MessageMedia attachment={message.attachment} scopeId={mediaScopeId} />}<time className="message-time">{formatTime(message.created_at_millis)}</time>{message.reactions && message.reactions.length > 0 && <MessageReactions reactions={message.reactions} onToggle={onToggleReaction} />}</div>
    </article>
  );
}

function MessageReactions({
  reactions,
  onToggle,
}: {
  reactions: ReactionSummary[];
  onToggle?: (emoji: string) => void;
}) {
  return (
    <div className="message-reactions">
      {reactions.map((reaction) => (
        <button
          key={reaction.emoji}
          type="button"
          className={reaction.reacted_by_self ? "mine" : undefined}
          disabled={!onToggle}
          title={reaction.reacted_by_self ? "remove reaction" : `react ${reaction.emoji}`}
          onClick={() => onToggle?.(reaction.emoji)}
        >
          <span>{reaction.emoji}</span>
          <small>{reaction.count}</small>
        </button>
      ))}
    </div>
  );
}

function replyPreview(message: MessageSummary) {
  const text = message.text.trim();
  if (text) return text.length > 96 ? `${text.slice(0, 96)}…` : text;
  if (message.attachment?.mime_type.startsWith("image/")) return "photo";
  if (message.attachment?.mime_type.startsWith("video/")) return "video";
  if (message.attachment?.mime_type.startsWith("audio/")) return "audio";
  return "message";
}

function ReplyMediaThumbnail({ message, scopeId }: { message: MessageSummary & { attachment: MediaAttachment }; scopeId?: string }) {
  const { attachment } = message;
  const localAttachment = message.local_attachment ?? sentMediaPreviewCache.get(message.event_id);
  const embeddedPreview = mediaPoster(attachment);
  const { source } = useMediaSource(
    attachment,
    scopeId,
    !localAttachment && !embeddedPreview,
  );
  const image = attachment.mime_type.startsWith("image/");
  const video = attachment.mime_type.startsWith("video/");
  const posterCacheKey = mediaCacheKey(attachment);
  const poster = embeddedPreview ?? videoPosterCache.get(posterCacheKey);
  if (image) {
    const imageSource = localAttachment?.preview_url ?? poster ?? source;
    return <span className="reply-media-thumbnail">{imageSource ? <img src={imageSource} alt="" /> : <LoaderCircle className="spinner" size={13} />}</span>;
  }
  if (video) {
    const videoSource = localAttachment?.preview_url ?? source;
    return <span className="reply-media-thumbnail video">{poster ? <img src={poster} alt="" /> : videoSource ? <video src={videoSource} muted playsInline preload="metadata" onLoadedMetadata={(event) => primeVideoFrame(event.currentTarget)} /> : <LoaderCircle className="spinner" size={13} />}<i><Play size={9} fill="currentColor" /></i></span>;
  }
  return <span className="reply-media-thumbnail audio"><AudioWaveform size={18} /></span>;
}

function MessageMedia({ attachment, scopeId, autoplayVideo = false }: { attachment: MediaAttachment; scopeId?: string; autoplayVideo?: boolean }) {
  const visibility = useNearViewport<HTMLDivElement>();
  const { source, failed } = useMediaSource(attachment, scopeId, visibility.near);
  const poster = mediaPoster(attachment);
  const image = attachment.mime_type.startsWith("image/");
  const video = attachment.mime_type.startsWith("video/");
  const posterCacheKey = mediaCacheKey(attachment);
  return (
    <div className="message-media" ref={visibility.ref}>
      {image ? (
        <ChatImage
          source={source ?? undefined}
          preview={poster}
          cacheKey={posterCacheKey}
          pixelWidth={attachment.pixel_width}
          pixelHeight={attachment.pixel_height}
          failed={failed}
        />
      ) : video ? (
        <ChatVideo
          source={source ?? undefined}
          poster={poster}
          posterCacheKey={posterCacheKey}
          pixelWidth={attachment.pixel_width}
          pixelHeight={attachment.pixel_height}
          autoPlay={autoplayVideo}
        />
      ) : source ? (
        <audio src={source} controls preload="metadata" />
      ) : (
        <div className="media-loading" aria-label={failed ? "media unavailable" : "loading media"}>
          {failed ? <X size={16} /> : <LoaderCircle className="spinner" size={15} />}
        </div>
      )}
    </div>
  );
}

function LocalMessageMedia({ attachment, manifest }: { attachment: NonNullable<MessageSummary["local_attachment"]>; manifest: MediaAttachment | null }) {
  const poster = manifest ? mediaPoster(manifest) : undefined;
  const posterCacheKey = manifest ? mediaCacheKey(manifest) : undefined;
  return <div className="message-media">{attachment.mime_type.startsWith("image/") ? <ChatImage source={attachment.preview_url} preview={poster ?? attachment.preview_url} cacheKey={posterCacheKey ?? attachment.preview_url} pixelWidth={manifest?.pixel_width} pixelHeight={manifest?.pixel_height} /> : attachment.mime_type.startsWith("video/") ? <ChatVideo source={attachment.preview_url} poster={poster} posterCacheKey={posterCacheKey} pixelWidth={manifest?.pixel_width} pixelHeight={manifest?.pixel_height} /> : <audio src={attachment.preview_url} controls preload="metadata" />}</div>;
}

function mediaPoster(attachment: MediaAttachment) {
  return attachment.preview_data_base64 && attachment.preview_mime_type
    ? `data:${attachment.preview_mime_type};base64,${attachment.preview_data_base64}`
    : undefined;
}

function requestMediaSource(attachment: MediaAttachment, scopeId?: string) {
  const cacheKey = mediaCacheKey(attachment);
  const cached = mediaCache.get(cacheKey);
  if (cached) return Promise.resolve(cached);
  const pending = mediaLoadPromises.get(cacheKey);
  if (pending) return pending;
  const generation = mediaCacheGeneration;
  let request: Promise<string>;
  request = (async () => {
      for (let attempt = 0; attempt < 12; attempt += 1) {
        try {
          const data = await noise<AttachmentData>({
            action: "fetch_attachment",
            attachment,
            scope_id: scopeId,
            relays,
          });
          if (!data) throw new Error("media is not available yet");
          const next = isTauri
            ? (await import("@tauri-apps/api/core")).convertFileSrc(data.file_path)
            : data.file_path;
          if (generation === mediaCacheGeneration) mediaCache.set(cacheKey, next);
          return next;
        } catch {
          if (attempt === 11) throw new Error("media is unavailable");
          const delay = Math.min(400 * 1.6 ** attempt, 3000);
          await new Promise<void>((resolve) => window.setTimeout(resolve, delay));
        }
      }
      throw new Error("media is unavailable");
    })().finally(() => {
      if (mediaLoadPromises.get(cacheKey) === request) mediaLoadPromises.delete(cacheKey);
    });
  mediaLoadPromises.set(cacheKey, request);
  return request;
}

async function downloadAttachment(attachment: MediaAttachment, scopeId?: string) {
  const data = await noise<AttachmentData>({
    action: "fetch_attachment",
    attachment,
    scope_id: scopeId,
    relays,
  });
  if (!data) throw new Error("media is unavailable");
  if (isTauri) {
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke<string>("download_media", {
      sourcePath: data.file_path,
      fileName: attachment.file_name,
    });
    return;
  }
  const link = document.createElement("a");
  link.href = data.file_path;
  link.download = attachment.file_name || "noise-media";
  link.style.display = "none";
  document.body.append(link);
  link.click();
  link.remove();
}

function prepareMediaSource(attachment: MediaAttachment, source: string) {
  const cacheKey = mediaCacheKey(attachment);
  const pending = mediaPreparationPromises.get(cacheKey);
  if (pending) return pending;
  let request: Promise<void>;
  if (attachment.mime_type.startsWith("image/")) {
    request = prepareImageMediaSource(
      source,
      cacheKey,
      !mediaPoster(attachment),
    );
  } else if (
    attachment.mime_type.startsWith("video/")
    && !videoPosterCache.has(cacheKey)
  ) {
    request = prepareVideoMediaSource(source, cacheKey);
  } else {
    request = Promise.resolve();
  }
  request = request.finally(() => {
    if (mediaPreparationPromises.get(cacheKey) === request) {
      mediaPreparationPromises.delete(cacheKey);
    }
  });
  mediaPreparationPromises.set(cacheKey, request);
  return request;
}

async function prepareImageMediaSource(
  source: string,
  cacheKey: string,
  generatePoster: boolean,
) {
  if (
    decodedImageCache.has(cacheKey)
    && mediaDimensionCache.has(cacheKey)
    && (!generatePoster || imagePosterCache.has(cacheKey))
  ) return;
  try {
    const response = await fetch(source);
    if (!response.ok) throw new Error("cached image could not be read");
    const bitmap = await createImageBitmap(await response.blob());
    rememberMediaDimensions(cacheKey, bitmap.width, bitmap.height);
    decodedImageCache.add(cacheKey);
    if (generatePoster && !imagePosterCache.has(cacheKey)) {
      const scale = Math.min(1, 480 / Math.max(bitmap.width, bitmap.height));
      const canvas = document.createElement("canvas");
      canvas.width = Math.max(1, Math.round(bitmap.width * scale));
      canvas.height = Math.max(1, Math.round(bitmap.height * scale));
      const context = canvas.getContext("2d");
      if (context) {
        context.drawImage(bitmap, 0, 0, canvas.width, canvas.height);
        imagePosterCache.set(cacheKey, canvas.toDataURL("image/jpeg", 0.68));
      }
    }
    bitmap.close();
    return;
  } catch {
    // Some WebViews cannot fetch the custom asset URL. The image element
    // fallback still measures it and may be able to capture a poster.
  }
  await new Promise<void>((resolve) => {
    const image = new Image();
    image.decoding = "async";
    image.onload = () => {
      rememberMediaDimensions(cacheKey, image.naturalWidth, image.naturalHeight);
      decodedImageCache.add(cacheKey);
      if (generatePoster && !imagePosterCache.has(cacheKey)) {
        try {
          const scale = Math.min(1, 480 / Math.max(image.naturalWidth, image.naturalHeight));
          const canvas = document.createElement("canvas");
          canvas.width = Math.max(1, Math.round(image.naturalWidth * scale));
          canvas.height = Math.max(1, Math.round(image.naturalHeight * scale));
          const context = canvas.getContext("2d");
          if (context) {
            context.drawImage(image, 0, 0, canvas.width, canvas.height);
            imagePosterCache.set(cacheKey, canvas.toDataURL("image/jpeg", 0.68));
          }
        } catch {
          // The full image can still open even if this format cannot be frozen.
        }
      }
      resolve();
    };
    image.onerror = () => resolve();
    image.src = source;
  });
}

function prepareVideoMediaSource(source: string, cacheKey: string) {
  return new Promise<void>((resolve) => {
    const video = document.createElement("video");
    video.muted = true;
    video.playsInline = true;
    video.preload = "auto";
    let previewIndex = 0;
    let settled = false;
    let previewTimes: number[] = [];
    const timeout = window.setTimeout(finish, 12_000);
    function finish() {
      if (settled) return;
      settled = true;
      window.clearTimeout(timeout);
      video.removeAttribute("src");
      video.load();
      resolve();
    }
    function capture() {
      if (settled || !video.videoWidth || !video.videoHeight) return;
      rememberMediaDimensions(cacheKey, video.videoWidth, video.videoHeight);
      if (videoFrameIsNearBlack(video) && previewIndex < previewTimes.length - 1) {
        previewIndex += 1;
        video.currentTime = previewTimes[previewIndex];
        return;
      }
      try {
        const scale = Math.min(1, 960 / Math.max(video.videoWidth, video.videoHeight));
        const canvas = document.createElement("canvas");
        canvas.width = Math.max(1, Math.round(video.videoWidth * scale));
        canvas.height = Math.max(1, Math.round(video.videoHeight * scale));
        const context = canvas.getContext("2d");
        if (context) {
          context.drawImage(video, 0, 0, canvas.width, canvas.height);
          videoPosterCache.set(cacheKey, canvas.toDataURL("image/jpeg", 0.82));
        }
      } catch {
        // The locally cached video still opens even when this codec blocks canvas capture.
      }
      finish();
    }
    video.onloadedmetadata = () => {
      rememberMediaDimensions(cacheKey, video.videoWidth, video.videoHeight);
      previewTimes = videoPreviewTimes(video.duration);
      if (previewTimes.length) video.currentTime = previewTimes[0];
    };
    video.onseeked = capture;
    video.onloadeddata = capture;
    video.onerror = finish;
    video.src = source;
    video.load();
  });
}

function useWarmConversationMedia(
  messages: Array<{ attachment: MediaAttachment | null }>,
  scopeId: string,
  renderedCount: number,
) {
  const candidates = messages
    .slice(Math.max(0, messages.length - renderedCount - MESSAGE_PAGE_SIZE))
    .reverse()
    .flatMap((message) => message.attachment ? [message.attachment] : []);
  const signature = candidates.map(mediaCacheKey).join("|");
  useEffect(() => {
    if (!candidates.length) return;
    let stopped = false;
    let cursor = 0;
    const warmNext = async () => {
      while (!stopped) {
        const attachment = candidates[cursor];
        cursor += 1;
        if (!attachment) return;
        try {
          const source = await requestMediaSource(attachment, scopeId);
          await prepareMediaSource(attachment, source);
        } catch {
          // The normal renderer can retry media that is still propagating between relays.
        }
      }
    };
    const workerCount = Math.min(4, candidates.length);
    for (let index = 0; index < workerCount; index += 1) void warmNext();
    return () => { stopped = true; };
  }, [scopeId, signature]);
}

function useMediaSource(
  attachment: MediaAttachment,
  scopeId?: string,
  enabled = true,
) {
  const cacheKey = mediaCacheKey(attachment);
  const [loaded, setLoaded] = useState<{ cacheKey: string; source: string } | null>(() => {
    const source = mediaCache.get(cacheKey);
    return source ? { cacheKey, source } : null;
  });
  const [failedKey, setFailedKey] = useState<string | null>(null);
  const source = loaded?.cacheKey === cacheKey
    ? loaded.source
    : mediaCache.get(cacheKey) ?? null;
  useEffect(() => {
    const cached = mediaCache.get(cacheKey);
    if (cached) {
      setLoaded({ cacheKey, source: cached });
      setFailedKey(null);
      return;
    }
    if (!enabled) return;
    let active = true;
    setFailedKey(null);
    void requestMediaSource(attachment, scopeId)
      .then((next) => {
        if (active) setLoaded({ cacheKey, source: next });
      })
      .catch(() => {
        if (active) setFailedKey(cacheKey);
      });
    return () => { active = false; };
  }, [attachment, cacheKey, enabled, scopeId]);
  return { source, failed: failedKey === cacheKey };
}

function useNearViewport<T extends HTMLElement>() {
  const ref = useRef<T>(null);
  const [near, setNear] = useState(false);
  useEffect(() => {
    const element = ref.current;
    if (!element || near) return;
    const observer = new IntersectionObserver((entries) => {
      if (entries.some((entry) => entry.isIntersecting)) {
        setNear(true);
        observer.disconnect();
      }
    }, { rootMargin: "900px 0px" });
    observer.observe(element);
    return () => observer.disconnect();
  }, [near]);
  return { ref, near };
}

function mediaFrameStyle(
  pixelWidth?: number | null,
  pixelHeight?: number | null,
  fallbackWidth = 320,
  fallbackHeight = 200,
): CSSProperties {
  const naturalWidth = pixelWidth || fallbackWidth;
  const naturalHeight = pixelHeight || fallbackHeight;
  const scale = Math.min(1, 420 / naturalWidth, 480 / naturalHeight);
  const displayWidth = Math.max(1, Math.round(naturalWidth * scale));
  const displayHeight = Math.max(1, Math.round(naturalHeight * scale));
  return {
    width: `${displayWidth}px`,
    maxWidth: "100%",
    aspectRatio: `${displayWidth} / ${displayHeight}`,
  };
}

function rememberMediaDimensions(cacheKey: string, width: number, height: number) {
  if (
    !Number.isFinite(width)
    || !Number.isFinite(height)
    || width <= 0
    || height <= 0
  ) return;
  const dimensions = {
    width: Math.round(width),
    height: Math.round(height),
  };
  const current = mediaDimensionCache.get(cacheKey);
  if (current?.width === dimensions.width && current.height === dimensions.height) return;
  mediaDimensionCache.set(cacheKey, dimensions);
  while (mediaDimensionCache.size > 2_000) {
    const oldest = mediaDimensionCache.keys().next().value;
    if (!oldest) break;
    mediaDimensionCache.delete(oldest);
  }
  try {
    window.localStorage.setItem(
      MEDIA_DIMENSIONS_STORAGE_KEY,
      JSON.stringify([...mediaDimensionCache.entries()]),
    );
  } catch {
    // Media still renders correctly for this session if storage is unavailable.
  }
}

function loadStoredMediaDimensions() {
  const dimensions = new Map<string, { width: number; height: number }>();
  try {
    const stored = JSON.parse(
      window.localStorage.getItem(MEDIA_DIMENSIONS_STORAGE_KEY) ?? "[]",
    ) as Array<[string, { width: number; height: number }]>;
    for (const [cacheKey, value] of stored) {
      if (
        typeof cacheKey === "string"
        && Number.isFinite(value?.width)
        && Number.isFinite(value?.height)
        && value.width > 0
        && value.height > 0
      ) {
        dimensions.set(cacheKey, value);
      }
    }
  } catch {
    // A corrupt or unavailable cache simply gets rebuilt from media metadata.
  }
  return dimensions;
}

function ChatImage({
  source,
  preview,
  cacheKey,
  pixelWidth,
  pixelHeight,
  failed = false,
}: {
  source?: string;
  preview?: string;
  cacheKey: string;
  pixelWidth?: number | null;
  pixelHeight?: number | null;
  failed?: boolean;
}) {
  const suppliedDimensions = pixelWidth && pixelHeight
    ? { width: pixelWidth, height: pixelHeight }
    : null;
  const [dimensions, setDimensions] = useState(
    () => suppliedDimensions ?? mediaDimensionCache.get(cacheKey) ?? null,
  );
  const [ready, setReady] = useState(
    () => Boolean(
      source
      && decodedImageCache.has(cacheKey)
      && (suppliedDimensions || mediaDimensionCache.has(cacheKey))
    ),
  );
  useEffect(() => {
    const knownDimensions = suppliedDimensions ?? mediaDimensionCache.get(cacheKey) ?? null;
    setDimensions(knownDimensions);
    setReady(Boolean(source && decodedImageCache.has(cacheKey) && knownDimensions));
  }, [cacheKey, pixelHeight, pixelWidth, source]);
  const style = mediaFrameStyle(dimensions?.width, dimensions?.height);
  return (
    <span className="chat-image" style={style}>
      {source && (
        <img
          className={ready ? "ready" : ""}
          src={source}
          alt="shared media"
          onLoad={(event) => {
            const image = event.currentTarget;
            const measured = {
              width: image.naturalWidth,
              height: image.naturalHeight,
            };
            rememberMediaDimensions(cacheKey, measured.width, measured.height);
            setDimensions(measured);
            decodedImageCache.add(cacheKey);
            setReady(true);
          }}
        />
      )}
      {!ready && preview && <img className="media-preview-cover" src={preview} alt="" aria-hidden="true" />}
      {!ready && !preview && (
        <span className={`media-skeleton ${failed ? "failed" : ""}`}>
          {failed && <X size={16} />}
        </span>
      )}
    </span>
  );
}

function ChatVideo({
  source,
  poster,
  posterCacheKey,
  pixelWidth,
  pixelHeight,
  autoPlay = false,
}: {
  source?: string;
  poster?: string;
  posterCacheKey?: string;
  pixelWidth?: number | null;
  pixelHeight?: number | null;
  autoPlay?: boolean;
}) {
  const [playing, setPlaying] = useState(false);
  const [muted, setMuted] = useState(false);
  const [hasStarted, setHasStarted] = useState(false);
  const [currentTime, setCurrentTime] = useState(0);
  const [duration, setDuration] = useState(0);
  const [playbackFrameReady, setPlaybackFrameReady] = useState(false);
  const [decodedPoster, setDecodedPoster] = useState(
    () => (posterCacheKey ? videoPosterCache.get(posterCacheKey) : undefined) ?? poster,
  );
  const [measuredDimensions, setMeasuredDimensions] = useState(() =>
    posterCacheKey ? mediaDimensionCache.get(posterCacheKey) ?? null : null
  );
  const video = useRef<HTMLVideoElement>(null);
  useEffect(() => {
    setPlaybackFrameReady(false);
    setHasStarted(false);
  }, [source]);
  useEffect(() => {
    const element = video.current;
    if (!autoPlay || !source || !element) return;
    element.currentTime = 0;
    void element.play().catch(() => undefined);
    return () => {
      element.pause();
      element.currentTime = 0;
    };
  }, [autoPlay, source]);
  useEffect(() => {
    setMeasuredDimensions(
      pixelWidth && pixelHeight
        ? { width: pixelWidth, height: pixelHeight }
        : posterCacheKey
          ? mediaDimensionCache.get(posterCacheKey) ?? null
          : null,
    );
  }, [pixelHeight, pixelWidth, posterCacheKey]);
  useEffect(() => {
    if (!poster) return;
    let active = true;
    void imageIsNearBlack(poster).then((nearBlack) => {
      if (!active) return;
      if (nearBlack) {
        setDecodedPoster(
          posterCacheKey ? videoPosterCache.get(posterCacheKey) : undefined,
        );
        const element = video.current;
        if (element && element.readyState >= HTMLMediaElement.HAVE_METADATA) {
          element.dataset.thumbnailPrimed = "false";
          primeVideoFrame(element);
        }
        return;
      }
      setDecodedPoster(
        (posterCacheKey ? videoPosterCache.get(posterCacheKey) : undefined) ?? poster,
      );
    });
    return () => { active = false; };
  }, [poster, posterCacheKey]);
  const capturePoster = (element: HTMLVideoElement) => {
    if (!posterCacheKey || !element.videoWidth || !element.videoHeight) return;
    const cached = videoPosterCache.get(posterCacheKey);
    if (cached) {
      if (decodedPoster !== cached) setDecodedPoster(cached);
      return;
    }
    try {
      if (videoFrameIsNearBlack(element)) {
        const previewTimes = videoPreviewTimes(element.duration);
        const currentIndex = Number(element.dataset.posterAttempt ?? "0");
        if (currentIndex < previewTimes.length - 1) {
          const nextIndex = currentIndex + 1;
          element.dataset.posterAttempt = String(nextIndex);
          element.currentTime = previewTimes[nextIndex];
          return;
        }
      }
      const scale = Math.min(1, 960 / Math.max(element.videoWidth, element.videoHeight));
      const canvas = document.createElement("canvas");
      canvas.width = Math.max(1, Math.round(element.videoWidth * scale));
      canvas.height = Math.max(1, Math.round(element.videoHeight * scale));
      const context = canvas.getContext("2d");
      if (!context) return;
      context.drawImage(element, 0, 0, canvas.width, canvas.height);
      const next = canvas.toDataURL("image/jpeg", 0.82);
      videoPosterCache.set(posterCacheKey, next);
      setDecodedPoster(next);
    } catch {
      // Some platform codecs disallow canvas capture; the cached file still plays normally.
    }
  };
  const frameWidth = pixelWidth ?? measuredDimensions?.width;
  const frameHeight = pixelHeight ?? measuredDimensions?.height;
  const hasDimensions = Boolean(frameWidth && frameHeight);
  const frameStyle = mediaFrameStyle(frameWidth, frameHeight, 288, 176);
  const togglePlayback = () => {
    const element = video.current;
    if (!element || !source) return;
    if (element.paused) void element.play();
    else element.pause();
  };
  const toggleMuted = () => {
    const element = video.current;
    if (element) element.muted = !element.muted;
  };
  const seek = (value: number) => {
    const element = video.current;
    if (!element) return;
    const nextTime = Math.min(Math.max(value, 0), duration || 0);
    element.currentTime = nextTime;
    setCurrentTime(nextTime);
  };
  const revealOnRenderedFrame = (element: HTMLVideoElement) => {
    if (playbackFrameReady) return;
    if ("requestVideoFrameCallback" in element) {
      element.requestVideoFrameCallback(() => setPlaybackFrameReady(true));
      return;
    }
    window.requestAnimationFrame(() => {
      window.requestAnimationFrame(() => setPlaybackFrameReady(true));
    });
  };
  return <div className={`chat-video ${hasDimensions ? "sized" : ""} ${hasStarted ? "started" : ""}`} style={frameStyle}>
    <video
      ref={video}
      src={source}
      poster={decodedPoster}
      width={frameWidth ?? undefined}
      height={frameHeight ?? undefined}
      muted={muted}
      autoPlay={autoPlay}
      playsInline
      preload="auto"
      onCanPlay={(event) => {
        if (autoPlay && event.currentTarget.paused) {
          void event.currentTarget.play().catch(() => undefined);
        }
      }}
      onLoadedMetadata={(event) => {
        const element = event.currentTarget;
        if (element.videoWidth && element.videoHeight) {
          const measured = {
            width: element.videoWidth,
            height: element.videoHeight,
          };
          if (posterCacheKey) {
            rememberMediaDimensions(posterCacheKey, measured.width, measured.height);
          }
          setMeasuredDimensions(measured);
        }
        setDuration(Number.isFinite(element.duration) ? element.duration : 0);
        setCurrentTime(element.currentTime);
        setMuted(element.muted);
        if (!posterCacheKey || !videoPosterCache.has(posterCacheKey)) {
          primeVideoFrame(element);
        }
      }}
      onLoadedData={(event) => capturePoster(event.currentTarget)}
      onSeeked={(event) => capturePoster(event.currentTarget)}
      onPlay={() => {
        setPlaying(true);
        setHasStarted(true);
      }}
      onPlaying={(event) => revealOnRenderedFrame(event.currentTarget)}
      onPause={() => setPlaying(false)}
      onEnded={() => setPlaying(false)}
      onTimeUpdate={(event) => setCurrentTime(event.currentTarget.currentTime)}
      onDurationChange={(event) => setDuration(Number.isFinite(event.currentTarget.duration) ? event.currentTarget.duration : 0)}
      onVolumeChange={(event) => setMuted(event.currentTarget.muted)}
      onClick={togglePlayback}
      onKeyDown={(event) => {
        if (event.key === "Enter" || event.key === " ") {
          event.preventDefault();
          togglePlayback();
        }
      }}
      tabIndex={0}
      aria-label="video"
      title="play or pause video"
    />
    {decodedPoster && !playbackFrameReady && <img className="chat-video-poster-cover" src={decodedPoster} alt="" aria-hidden="true" />}
    {!decodedPoster && !playbackFrameReady && <span className="media-skeleton video" aria-hidden="true" />}
    {!hasStarted && source && <button type="button" className="chat-video-start" onClick={togglePlayback} aria-label="play video" title="play video"><Play size={25} fill="currentColor" /></button>}
    <div className="noise-video-controls" aria-label="video controls">
      <button type="button" className="noise-video-control-button" disabled={!source} onClick={togglePlayback} aria-label={playing ? "pause video" : "play video"} title={playing ? "pause" : "play"}>
        {playing ? <Pause size={16} fill="currentColor" /> : <Play size={16} fill="currentColor" />}
      </button>
      <span className="noise-video-time">{formatVideoTime(currentTime)}</span>
      <input
        className="noise-video-scrubber"
        type="range"
        min="0"
        max={duration || 0}
        step="0.1"
        value={Math.min(currentTime, duration || 0)}
        onChange={(event) => seek(Number(event.currentTarget.value))}
        aria-label="seek video"
        aria-valuetext={`${formatVideoTime(currentTime)} of ${formatVideoTime(duration)}`}
        disabled={!source || !duration}
      />
      <span className="noise-video-time">{formatVideoTime(duration)}</span>
      <button type="button" className="noise-video-control-button" disabled={!source} onClick={toggleMuted} aria-label={muted ? "unmute video" : "mute video"} title={muted ? "unmute" : "mute"}>
        {muted ? <VolumeX size={17} /> : <Volume2 size={17} />}
      </button>
    </div>
  </div>;
}

function formatVideoTime(seconds: number) {
  if (!Number.isFinite(seconds) || seconds < 0) return "0:00";
  const wholeSeconds = Math.floor(seconds);
  const minutes = Math.floor(wholeSeconds / 60);
  return `${minutes}:${String(wholeSeconds % 60).padStart(2, "0")}`;
}

type MediaMessage = MessageSummary & { attachment: MediaAttachment };

function MediaGalleryDialog({ group, messages, onClose }: { group: GroupSummary; messages: MessageSummary[]; onClose: () => void }) {
  const media = messages.filter((item): item is MediaMessage => item.attachment !== null);
  const [selected, setSelected] = useState<MediaMessage | null>(null);
  const selectedIndex = selected
    ? media.findIndex((item) => item.event_id === selected.event_id)
    : -1;
  const showPrevious = selectedIndex > 0;
  const showNext = selectedIndex >= 0 && selectedIndex < media.length - 1;
  useEffect(() => {
    if (!selected) return;
    const navigate = (event: KeyboardEvent) => {
      if (event.key === "ArrowLeft" && showPrevious) {
        event.preventDefault();
        setSelected(media[selectedIndex - 1]);
      } else if (event.key === "ArrowRight" && showNext) {
        event.preventDefault();
        setSelected(media[selectedIndex + 1]);
      }
    };
    window.addEventListener("keydown", navigate);
    return () => window.removeEventListener("keydown", navigate);
  }, [media, selected, selectedIndex, showNext, showPrevious]);
  return (
    <Modal onClose={onClose} wide>
      <DialogHeading icon={<Images />} title="group media" detail={`${media.length} ${media.length === 1 ? "upload" : "uploads"} in ${group.name}`} />
      {selected ? (
        <div className="gallery-view">
          <button className="gallery-back" onClick={() => setSelected(null)}><ArrowLeft size={14} /> all media</button>
          <div className="gallery-viewer">
            <button className="gallery-nav previous" disabled={!showPrevious} onClick={() => showPrevious && setSelected(media[selectedIndex - 1])} aria-label="previous media"><ChevronLeft size={25} /></button>
            <MessageMedia key={selected.event_id} attachment={selected.attachment} scopeId={group.group_id} autoplayVideo />
            <button className="gallery-nav next" disabled={!showNext} onClick={() => showNext && setSelected(media[selectedIndex + 1])} aria-label="next media"><ChevronRight size={25} /></button>
          </div>
          <small>{selectedIndex + 1} of {media.length} · shared by {selected.username} · {formatGalleryDate(selected.created_at_millis)}</small>
        </div>
      ) : media.length ? (
        <div className="media-gallery">
          {media.map((item) => <GalleryTile key={item.event_id} message={item} scopeId={group.group_id} onOpen={() => setSelected(item)} />)}
        </div>
      ) : (
        <div className="empty-gallery"><Images size={27} /><span>no media has been shared yet</span></div>
      )}
    </Modal>
  );
}

function GalleryTile({ message, scopeId, onOpen }: { message: MediaMessage; scopeId: string; onOpen: () => void }) {
  const { attachment } = message;
  const visibility = useNearViewport<HTMLButtonElement>();
  const { source, failed } = useMediaSource(attachment, scopeId, visibility.near);
  const image = attachment.mime_type.startsWith("image/");
  const video = attachment.mime_type.startsWith("video/");
  const thumbnail = useGalleryThumbnail(attachment, source);
  return (
    <button ref={visibility.ref} className={`gallery-tile ${image ? "image" : video ? "video" : "audio"}`} onClick={onOpen} aria-label={`open media shared by ${message.username}`}>
      {image || video
        ? thumbnail
          ? <img src={thumbnail} alt="" />
          : <span className="gallery-loading">{failed ? <X size={16} /> : <LoaderCircle className="spinner" size={16} />}</span>
        : source
          ? <span className="gallery-audio"><AudioWaveform size={30} /><small>audio</small></span>
          : <span className="gallery-loading">{failed ? <X size={16} /> : <LoaderCircle className="spinner" size={16} />}</span>}
      {video && thumbnail && <i className="gallery-play"><Play size={15} fill="currentColor" /></i>}
    </button>
  );
}

function useGalleryThumbnail(attachment: MediaAttachment, source: string | null) {
  const cacheKey = mediaCacheKey(attachment);
  const embedded = mediaPoster(attachment);
  const cachedPoster = () => attachment.mime_type.startsWith("video/")
    ? videoPosterCache.get(cacheKey)
    : imagePosterCache.get(cacheKey);
  const [generated, setGenerated] = useState<string | null>(() => cachedPoster() ?? null);
  useEffect(() => {
    const cached = cachedPoster();
    if (cached) {
      setGenerated(cached);
      return;
    }
    if (!source) return;
    let active = true;
    void prepareMediaSource(attachment, source).then(() => {
      if (active) {
        setGenerated(
          cachedPoster()
          ?? (attachment.mime_type.startsWith("image/") ? source : null),
        );
      }
    });
    return () => { active = false; };
  }, [attachment, cacheKey, embedded, source]);
  return generated ?? embedded;
}

function useProfileImageSource(image: ProfileImage | null) {
  const [loaded, setLoaded] = useState<{ blobId: string; source: string } | null>(() => {
    if (!image) return null;
    const source = avatarCache.get(image.blob_id);
    return source ? { blobId: image.blob_id, source } : null;
  });
  const source = image
    ? loaded?.blobId === image.blob_id
      ? loaded.source
      : avatarCache.get(image.blob_id)
    : undefined;
  useEffect(() => {
    if (!image) {
      setLoaded(null);
      return;
    }
    const target = image;
    const cached = avatarCache.get(target.blob_id);
    if (cached) {
      setLoaded({ blobId: target.blob_id, source: cached });
      return;
    }
    setLoaded(null);
    let active = true;
    void loadProfileImageSource(target)
      .then((source) => {
        if (active) setLoaded({ blobId: target.blob_id, source });
      })
      .catch(() => undefined);
    return () => { active = false; };
  }, [image?.blob_id, image?.key_base64, image?.mime_type, image?.byte_length]);
  return source;
}

function Avatar({ name, image, size, square = false }: { name: string; image: ProfileImage | null; size: number; square?: boolean }) {
  const source = useProfileImageSource(image);
  return (
    <span className={`avatar ${square ? "square" : ""}`} style={{ width: size, height: size }}>
      {source ? <img src={source} alt="" /> : <b>{name.slice(0, 1).toUpperCase()}</b>}
    </span>
  );
}

function PresenceAvatar({
  name,
  image,
  size,
  status,
}: {
  name: string;
  image: ProfileImage | null;
  size: number;
  status: PresenceStatus;
}) {
  return (
    <span className="presence-avatar">
      <Avatar name={name} image={image} size={size} />
      <i className={`presence-status ${status}`} aria-label={status} title={status.replace("-", " ")} />
    </span>
  );
}

function Onboarding({ busy, onCreate, onSignIn }: { busy: boolean; onCreate: (username: string, password: string) => Promise<boolean>; onSignIn: (noiseId: string, password: string) => Promise<boolean> }) {
  const [mode, setMode] = useState<"create" | "signin">("create");
  const [username, setUsername] = useState("");
  const [noiseId, setNoiseId] = useState("");
  const [password, setPassword] = useState("");
  const [confirmation, setConfirmation] = useState("");
  const [createAttempted, setCreateAttempted] = useState(false);
  const displayedNoiseId = noiseId.match(/.{1,4}/g)?.join(" ") ?? "";
  const passwordLength = Array.from(password).length;
  const passwordClasses = [
    /\p{Ll}/u.test(password),
    /\p{Lu}/u.test(password),
    /\p{N}/u.test(password),
    /[^\p{L}\p{N}]/u.test(password),
  ].filter(Boolean).length;
  const passwordRequirements = [
    { label: "16–256 characters", met: passwordLength >= 16 && passwordLength <= 256 },
    { label: `24+ characters or ${passwordClasses}/3 character types`, met: passwordLength >= 24 || passwordClasses >= 3 },
    { label: "passwords match", met: confirmation.length > 0 && password === confirmation },
  ];
  const usernameReady = username.trim().length > 0;
  const passwordReady = passwordRequirements.every((requirement) => requirement.met);
  const createReady = usernameReady && passwordReady;
  const submitCreate = () => {
    setCreateAttempted(true);
    if (busy || !createReady) return;
    void onCreate(username.trim(), password);
  };
  return (
    <div className="onboarding" data-tauri-drag-region>
      <NoiseMark size={54} />
      <h1>noise</h1>
      <p>no phone number. no email. just your Noise ID and password.</p>
      <div className="onboarding-tabs">
        <button className={mode === "create" ? "active" : ""} onClick={() => setMode("create")}>create identity</button>
        <button className={mode === "signin" ? "active" : ""} onClick={() => setMode("signin")}>sign in</button>
      </div>
      {mode === "create" ? <>
        <input autoFocus value={username} maxLength={32} aria-invalid={createAttempted && !usernameReady} onChange={(event) => setUsername(event.target.value)} placeholder="display name" />
        <input type="password" autoComplete="new-password" value={password} aria-describedby="password-requirements" aria-invalid={createAttempted && !passwordReady} onChange={(event) => setPassword(event.target.value)} placeholder="strong password" />
        <input type="password" autoComplete="new-password" value={confirmation} aria-describedby="password-requirements" aria-invalid={createAttempted && password !== confirmation} onChange={(event) => setConfirmation(event.target.value)} placeholder="confirm password" onKeyDown={(event) => { if (event.key === "Enter") submitCreate(); }} />
        <div id="password-requirements" className={`password-requirements${createAttempted && !createReady ? " invalid" : ""}`} aria-live="polite">
          <strong>password requirements</strong>
          <ul>
            {passwordRequirements.map((requirement) => <li key={requirement.label} className={requirement.met ? "met" : ""}><Check size={11} /> {requirement.label}</li>)}
          </ul>
          {createAttempted && !createReady && <span><TriangleAlert size={12} /> {usernameReady ? "complete the requirements above to continue" : "enter a display name to continue"}</span>}
        </div>
        <button disabled={!createReady || busy} onClick={submitCreate}>{busy && <LoaderCircle className="spinner" size={14} />} create identity</button>
        <small>use a password manager or a long, memorable passphrase</small>
      </> : <>
        <input autoFocus className="frequency-input" inputMode="numeric" value={displayedNoiseId} onChange={(event) => setNoiseId(event.target.value.replace(/\D/g, "").slice(0, 12))} placeholder="0000 0000 0000" />
        <input type="password" autoComplete="current-password" value={password} onChange={(event) => setPassword(event.target.value)} placeholder="password" onKeyDown={(event) => { if (event.key === "Enter" && noiseId.length === 12 && password) void onSignIn(noiseId, password); }} />
        <button disabled={noiseId.length !== 12 || !password || busy} onClick={() => void onSignIn(noiseId, password)}>{busy && <LoaderCircle className="spinner" size={14} />} sign in</button>
        <small>your encrypted identity will be restored from the relay network</small>
      </>}
    </div>
  );
}

function EmptyGroup({ onMake, onJoin }: { onMake: () => void; onJoin: () => void }) {
  return <div className="empty-group"><Radio size={38} /><h2>nothing but noise</h2><p>create a group or join one with its frequency</p><div><button onClick={onMake}>create group</button><button onClick={onJoin}>join group</button></div></div>;
}

function EmptyDirects() {
  return <div className="empty-group"><MessagesSquare size={38} /><h2>no direct messages</h2><p>open someone from a shared group to message them privately</p></div>;
}

function MakeDialog({ busy, onClose, onSubmit }: { busy: boolean; onClose: () => void; onSubmit: (name: string) => Promise<boolean> }) {
  const [name, setName] = useState("");
  return <Modal onClose={onClose}><DialogHeading icon={<UsersRound />} title="create group" detail="give the group a name" /><input autoFocus value={name} onChange={(event) => setName(event.target.value)} placeholder="group name" /><DialogButtons onClose={onClose}><button className="primary" disabled={!name.trim() || busy} onClick={() => void onSubmit(name.trim())}>create group</button></DialogButtons></Modal>;
}

function JoinDialog({ busy, onClose, onSubmit }: { busy: boolean; onClose: () => void; onSubmit: (frequency: string) => Promise<boolean> }) {
  const [frequency, setFrequency] = useState("");
  const displayedFrequency = frequency.match(/.{1,4}/g)?.join(" ") ?? "";
  return <Modal onClose={onClose}><DialogHeading icon={<Radio />} title="join group" detail="enter its 12-digit frequency" /><input autoFocus className="frequency-input" value={displayedFrequency} onChange={(event) => setFrequency(event.target.value.replace(/\D/g, "").slice(0, 12))} placeholder="0000 0000 0000" inputMode="numeric" /><DialogButtons onClose={onClose}><button className="primary" disabled={frequency.length !== 12 || busy} onClick={() => void onSubmit(frequency)}>join group</button></DialogButtons></Modal>;
}

function FrequencyDialog({ group, frequency, onClose }: { group: string; frequency: string; onClose: () => void }) {
  return <Modal onClose={onClose}><DialogHeading icon={<Radio />} title="you're live" detail={`share this frequency to invite people to ${group}`} /><div className="frequency-card">{frequency}</div><DialogButtons><CopyButton value={frequency} label="copy frequency" /><button className="primary" onClick={onClose}>done</button></DialogButtons></Modal>;
}

function NoiseIdDialog({ noiseId, onClose }: { noiseId: string; onClose: () => void }) {
  return <Modal onClose={onClose}><DialogHeading icon={<NoiseMark size={28} />} title="this is your Noise ID" detail="you’ll use it with your password to sign in on any device" /><div className="frequency-card">{noiseId}</div><p className="noise-id-warning">Save this somewhere private. Noise cannot recover it for you.</p><DialogButtons><CopyButton value={noiseId} label="copy Noise ID" /><button className="primary" onClick={onClose}>I saved it</button></DialogButtons></Modal>;
}

function SettingsDialog({ profile, busy, onClose, onSave, onLogout, onDeleteAccount }: { profile: IdentitySummary; busy: boolean; onClose: () => void; onSave: (username: string, bio: string, avatar: string | null, remove: boolean, acceptsDirectMessages: boolean) => Promise<boolean>; onLogout: () => void; onDeleteAccount: () => void }) {
  const [tab, setTab] = useState<"identity" | "privacy" | "account">("identity");
  const [username, setUsername] = useState(profile.username);
  const [bio, setBio] = useState(profile.bio);
  const [acceptsDirectMessages, setAcceptsDirectMessages] = useState(profile.accepts_direct_messages);
  const image = useImageSelection();
  const settingsChanged = username.trim() !== profile.username
    || bio !== profile.bio
    || acceptsDirectMessages !== profile.accepts_direct_messages
    || image.base64 !== null
    || image.removed;
  return (
    <Modal onClose={onClose}>
      <DialogHeading icon={<Settings2 />} title="settings" detail="your Noise identity" />
      <div className="group-settings-tabs" role="tablist" aria-label="user settings sections">
        <button className={tab === "identity" ? "active" : ""} role="tab" aria-selected={tab === "identity"} onClick={() => setTab("identity")}>Identity</button>
        <button className={tab === "privacy" ? "active" : ""} role="tab" aria-selected={tab === "privacy"} onClick={() => setTab("privacy")}>Privacy</button>
        <button className={tab === "account" ? "active" : ""} role="tab" aria-selected={tab === "account"} onClick={() => setTab("account")}>Account</button>
      </div>
      <div className="group-settings-panel user-settings-panel" role="tabpanel">
        {tab === "identity" && <div className="group-settings-identity">
          <div className="identity-editor"><ImagePicker name={username} existing={profile.avatar} selection={image} /><small>public identity</small></div>
          <LabeledArea label="display name" count={`${username.length}/32`}><input value={username} maxLength={32} onChange={(event) => setUsername(event.target.value)} /></LabeledArea>
          <LabeledArea label="bio" count={`${bio.length}/160`}><textarea value={bio} onChange={(event) => setBio(event.target.value)} /></LabeledArea>
        </div>}
        {tab === "privacy" && <section className="settings-section user-privacy-settings">
          <h3>direct messages</h3>
          <label className="settings-toggle-row"><span><strong>accept direct messages</strong><small>allow people from shared groups to message you</small></span><input type="checkbox" role="switch" checked={acceptsDirectMessages} onChange={(event) => setAcceptsDirectMessages(event.target.checked)} /></label>
        </section>}
        {tab === "account" && <div className="user-account-settings">
          {profile.noise_id && <section className="settings-section"><h3>Noise ID</h3><div className="noise-id-setting"><strong>{profile.noise_id}</strong><CopyButton value={profile.noise_id} label="copy" /></div><p>Use this with your password to sign in on another device.</p></section>}
          {profile.noise_id && <section className="settings-section account-session"><span><strong>log out on this device</strong><small>Your encrypted identity remains available on the relay network.</small></span><button disabled={busy} onClick={onLogout}>log out</button></section>}
          <section className="settings-danger"><span><strong>delete account</strong><small>erase this identity and its encrypted account vault</small></span><button className="danger" disabled={busy} onClick={onDeleteAccount}>delete account</button></section>
        </div>}
      </div>
      <DialogButtons onClose={onClose} closeLabel={settingsChanged ? "cancel" : "close"}>
        {tab === "identity" && (profile.avatar || image.preview) && <button className="danger" onClick={image.remove}>remove photo</button>}
        {settingsChanged && <button className="primary" disabled={!username.trim() || username.length > 32 || bio.length > 160 || busy} onClick={() => void onSave(username.trim(), bio, image.base64, image.removed, acceptsDirectMessages)}>save settings</button>}
      </DialogButtons>
    </Modal>
  );
}

function GroupSettingsDialog({ group, bannedMembers, presenceStatuses, busy, onClose, onSave, onUnban, onRotateFrequency }: { group: GroupSummary; bannedMembers: BannedMemberSummary[]; presenceStatuses: Map<string, PresenceStatus>; busy: boolean; onClose: () => void; onSave: (name: string, description: string, accentColor: string, avatar: string | null, removeAvatar: boolean, background: string | null, removeBackground: boolean, mobileBackground: string | null, removeMobileBackground: boolean, membersCanSendMessages: boolean, membersCanSendMedia: boolean) => Promise<boolean>; onUnban: (member: BannedMemberSummary) => Promise<boolean>; onRotateFrequency: (revokeOnly: boolean) => Promise<boolean> }) {
  const [tab, setTab] = useState<"identity" | "appearance" | "general" | "banned">("identity");
  const [revokeArmed, setRevokeArmed] = useState(false);
  const [name, setName] = useState(group.name);
  const [description, setDescription] = useState(group.description);
  const [accentColor, setAccentColor] = useState(group.accent_color || DEFAULT_ACCENT_COLOR);
  const [membersCanSendMessages, setMembersCanSendMessages] = useState(group.members_can_send_messages);
  const [membersCanSendMedia, setMembersCanSendMedia] = useState(group.members_can_send_media);
  const image = useImageSelection();
  const background = useBackgroundSelection("desktop");
  const mobileBackground = useBackgroundSelection("mobile");
  const hasGroupIcon = Boolean(image.preview || (!image.removed && group.avatar));
  const settingsChanged = name.trim() !== group.name
    || description !== group.description
    || accentColor !== group.accent_color
    || membersCanSendMessages !== group.members_can_send_messages
    || membersCanSendMedia !== group.members_can_send_media
    || image.base64 !== null
    || image.removed
    || background.base64 !== null
    || background.removed
    || mobileBackground.base64 !== null
    || mobileBackground.removed;
  return (
    <Modal onClose={onClose} className="group-settings-modal">
      <DialogHeading icon={<Settings2 />} title="group settings" detail={group.name} />
      <div className="group-settings-tabs group-tabs" role="tablist" aria-label="group settings sections">
        <button className={tab === "identity" ? "active" : ""} role="tab" aria-selected={tab === "identity"} onClick={() => setTab("identity")}>Identity</button>
        <button className={tab === "appearance" ? "active" : ""} role="tab" aria-selected={tab === "appearance"} onClick={() => setTab("appearance")}>Appearance</button>
        <button className={tab === "general" ? "active" : ""} role="tab" aria-selected={tab === "general"} onClick={() => setTab("general")}>General</button>
        <button className={tab === "banned" ? "active" : ""} role="tab" aria-selected={tab === "banned"} onClick={() => setTab("banned")}>Banned{bannedMembers.length > 0 && <i>{bannedMembers.length}</i>}</button>
      </div>
      <div className="group-settings-panel" role="tabpanel">
        {tab === "identity" && <div className="group-settings-identity">
          <div className="group-identity-images">
            <div className="identity-editor">
              <div className="identity-image-control">
                <ImagePicker name={group.name} existing={group.avatar} selection={image} square />
                {hasGroupIcon && <button className="identity-image-remove" disabled={busy} onClick={image.remove} aria-label="remove group icon" title="remove group icon"><X size={11} /></button>}
              </div>
              <small>group icon</small>
            </div>
          </div>
          <LabeledArea label="name"><input value={name} onChange={(event) => setName(event.target.value)} /></LabeledArea>
          <LabeledArea label="description" count={`${description.length}/200`}><textarea value={description} onChange={(event) => setDescription(event.target.value)} /></LabeledArea>
        </div>}
        {tab === "appearance" && <div className="group-settings-appearance">
          <div className="group-background-pickers">
            <BackgroundPicker existing={group.background} selection={background} disabled={busy} label="chat background · desktop" recommendation="1920 × 1080 recommended" />
            <BackgroundPicker existing={group.mobile_background} selection={mobileBackground} disabled={busy} label="chat background · mobile" recommendation="1290 × 2796 recommended" mobile />
          </div>
          <div className="group-accent-setting">
            <div className="group-accent-heading"><span><strong>accent color</strong><small>group-wide theme</small></span><code>{accentColor}</code></div>
            <div className="accent-color-controls">
              {ACCENT_PRESETS.map((color) => <button key={color} type="button" className={accentColor === color ? "selected" : ""} style={{ backgroundColor: color }} aria-label={`use accent ${color}`} aria-pressed={accentColor === color} onClick={() => setAccentColor(color)} />)}
              <label className="custom-accent-color" title="choose a custom color" style={{ backgroundColor: accentColor }}>
                <input type="color" value={accentColor} onChange={(event) => setAccentColor(event.target.value.toUpperCase())} />
                <span>+</span>
              </label>
            </div>
          </div>
        </div>}
        {tab === "general" && <section className="settings-section group-general-settings">
          <h3>what can members do?</h3>
          <label className="settings-toggle-row"><span><strong>send messages</strong><small>moderators can always send messages</small></span><input type="checkbox" role="switch" checked={membersCanSendMessages} onChange={(event) => setMembersCanSendMessages(event.target.checked)} /></label>
          <label className="settings-toggle-row"><span><strong>send media</strong><small>moderators can always upload media</small></span><input type="checkbox" role="switch" checked={membersCanSendMedia} onChange={(event) => setMembersCanSendMedia(event.target.checked)} /></label>
          <h3 className="frequency-heading">frequency</h3>
          <div className="group-frequency-settings">
            <div className="group-frequency-value">
              <span>{group.frequency ?? "not stored on this device"}</span>
              {group.frequency && <CopyButton value={group.frequency} label="copy frequency" iconOnly disabled={busy} />}
            </div>
            <p>{group.frequency ? "Anyone with this code can join the group." : "Generate one to revoke any older invitation and create a code this device can manage."}</p>
            {group.remote_deletion_supported ? <div className="group-frequency-actions">
              {group.frequency && <button className={revokeArmed ? "confirm" : "danger"} disabled={busy} onClick={() => { if (revokeArmed) { setRevokeArmed(false); void onRotateFrequency(true); } else { setRevokeArmed(true); } }}><Trash2 size={13} /> {revokeArmed ? "confirm revoke" : "revoke"}</button>}
              <button disabled={busy} onClick={() => { setRevokeArmed(false); void onRotateFrequency(false); }}><Radio size={13} /> {group.frequency ? "generate new" : "generate frequency"}</button>
            </div> : <small className="legacy-frequency-note">This legacy group cannot authenticate frequency rotation.</small>}
          </div>
        </section>}
        {tab === "banned" && <section className="settings-section">
          {bannedMembers.length ? <div className="banned-user-list">{bannedMembers.map((member) => <div className="banned-user-row" key={member.public_key}><PresenceAvatar name={member.username} image={member.avatar} size={30} status={presenceStatuses.get(member.public_key) ?? "offline"} /><span><strong>{member.username}</strong><small>{member.bio || "banned from this group"}</small></span><button disabled={busy} onClick={() => void onUnban(member)}>unban</button></div>)}</div> : <p className="empty-banned-users">no one is banned</p>}
        </section>}
      </div>
      <DialogButtons onClose={onClose} closeLabel={settingsChanged ? "cancel" : "close"}>
        {settingsChanged && <button className="primary" disabled={!name.trim() || name.length > 80 || description.length > 200 || background.busy || mobileBackground.busy || busy} onClick={() => void onSave(name.trim(), description, accentColor, image.base64, image.removed, background.base64, background.removed, mobileBackground.base64, mobileBackground.removed, membersCanSendMessages, membersCanSendMedia)}>save settings</button>}
      </DialogButtons>
    </Modal>
  );
}

function RulesDialog({ group, canEdit, busy, onClose, onSave }: { group: GroupSummary; canEdit: boolean; busy: boolean; onClose: () => void; onSave: (rules: string) => Promise<boolean> }) {
  const [rules, setRules] = useState(() => ruleItems(group.rules));
  const [draft, setDraft] = useState("");
  const candidate = draft.trim();
  const duplicate = rules.some((rule) => rule.toLocaleLowerCase() === candidate.toLocaleLowerCase());
  const canAdd = candidate.length > 0 && candidate.length <= 200 && rules.length < 20 && !duplicate;
  const addRule = () => {
    if (!canAdd) return;
    setRules((current) => [...current, candidate]);
    setDraft("");
  };
  const savedRules = canAdd ? [...rules, candidate] : rules;
  return <Modal onClose={onClose}><DialogHeading icon={<ScrollText />} title="group rules" detail={group.name} />{canEdit ? <div className="rule-builder"><div className="rule-entry"><input autoFocus value={draft} maxLength={200} placeholder="add a rule" onChange={(event) => setDraft(event.target.value)} onKeyDown={(event) => { if (event.key === "Enter") { event.preventDefault(); addRule(); } }} /><button disabled={!canAdd} onClick={addRule} aria-label="add rule"><Plus size={16} /></button></div><div className="rule-count"><span>{rules.length}/20 rules</span><span>{draft.length}/200</span></div>{rules.length ? <ol className="rule-list">{rules.map((rule, index) => <li key={`${rule}-${index}`}><span>{rule}</span><button onClick={() => setRules((current) => current.filter((_, itemIndex) => itemIndex !== index))} aria-label={`remove rule ${index + 1}`}><X size={13} /></button></li>)}</ol> : <p className="empty-rules">add the first rule for this group</p>}</div> : rules.length ? <ol className="rules-copy">{rules.map((rule, index) => <li key={`${rule}-${index}`}>{rule}</li>)}</ol> : <p className="empty-rules">no rules have been set for this group</p>}<DialogButtons onClose={onClose} closeLabel={canEdit ? "cancel" : "close"}>{canEdit && <button className="primary" disabled={busy || (!!candidate && !canAdd)} onClick={() => void onSave(savedRules.join("\n"))}>save rules</button>}</DialogButtons></Modal>;
}

function ruleItems(value: string) {
  return value.split(/\r?\n/).map((rule) => rule.trim()).filter(Boolean).slice(0, 20);
}

function emojiOnlyCount(text: string): 1 | 2 | 3 | null {
  const trimmed = text.trim();
  if (!trimmed || /[\p{L}\p{N}]/u.test(trimmed)) return null;
  const Segmenter = (Intl as { Segmenter?: typeof Intl.Segmenter }).Segmenter;
  if (!Segmenter) return null;
  const segments = new Segmenter(undefined, { granularity: "grapheme" });
  let count = 0;
  for (const { segment } of segments.segment(trimmed)) {
    if (/^\s+$/.test(segment)) continue;
    if (!/\p{Extended_Pictographic}/u.test(segment)) return null;
    count += 1;
    if (count > 3) return null;
  }
  return count === 1 || count === 2 || count === 3 ? count : null;
}

function ReportMessageDialog({ message, busy, onClose, onReport }: { message: MessageSummary; busy: boolean; onClose: () => void; onReport: (reason: string) => Promise<boolean> }) {
  const [reason, setReason] = useState("");
  return (
    <Modal onClose={onClose} compact>
      <DialogHeading icon={<TriangleAlert />} title="report message?" detail="send this to the group’s moderation queue" />
      <div className="report-target-preview"><strong>{message.username}</strong><p>{reportMessagePreview(message)}</p></div>
      <LabeledArea label="details (optional)" count={`${reason.length}/280`}><textarea autoFocus maxLength={280} value={reason} placeholder="what should moderators know?" onChange={(event) => setReason(event.target.value)} /></LabeledArea>
      <DialogButtons onClose={onClose}><button className="report-confirm" disabled={busy} onClick={() => void onReport(reason.trim())}>{busy && <LoaderCircle className="spinner" size={13} />} report message</button></DialogButtons>
    </Modal>
  );
}

function ReportsDialog({ reports, presenceStatuses, busy, onClose, onDismiss, onDelete }: { reports: ReportSummary[]; presenceStatuses: Map<string, PresenceStatus>; busy: boolean; onClose: () => void; onDismiss: (report: ReportSummary) => Promise<boolean>; onDelete: (report: ReportSummary) => Promise<boolean> }) {
  return (
    <Modal onClose={onClose} wide>
      <DialogHeading icon={<TriangleAlert />} title="reports" detail={reports.length === 1 ? "1 report needs review" : `${reports.length} reports need review`} />
      {reports.length ? <div className="reports-queue">{reports.map((report) => (
        <article className="report-card" key={report.report_event_id}>
          <div className="reported-message-author"><PresenceAvatar name={report.message.username} image={report.message.avatar} size={34} status={presenceStatuses.get(report.message.author_public_key) ?? "offline"} /><span><strong>{report.message.username}</strong><small>posted {formatGalleryDate(report.message.created_at_millis)}</small></span></div>
          <p className="reported-message-copy">{reportMessagePreview(report.message)}</p>
          <div className="reporter-context"><PresenceAvatar name={report.reporter_username} image={report.reporter_avatar} size={24} status={presenceStatuses.get(report.reporter_public_key) ?? "offline"} /><span><small>reported by {report.reporter_username} · {formatGalleryDate(report.created_at_millis)}</small><strong>{report.reason || "no additional details"}</strong></span></div>
          <div className="report-actions"><button disabled={busy} onClick={() => void onDismiss(report)}>dismiss</button><button className="danger" disabled={busy} onClick={() => void onDelete(report)}><Trash2 size={13} /> delete message</button></div>
        </article>
      ))}</div> : <div className="empty-reports"><Check size={25} /><strong>all clear</strong><span>there are no reports waiting for review</span></div>}
      <DialogButtons onClose={onClose} closeLabel="close">{busy && <LoaderCircle className="spinner" size={14} />}</DialogButtons>
    </Modal>
  );
}

function reportMessagePreview(message: MessageSummary) {
  if (message.text.trim()) return message.text;
  if (message.attachment?.mime_type.startsWith("image/")) return "image attachment";
  if (message.attachment?.mime_type.startsWith("video/")) return "video attachment";
  if (message.attachment?.mime_type.startsWith("audio/")) return "audio attachment";
  return "media attachment";
}

function BanMemberDialog({ member, busy, onClose, onBan }: { member: MemberSummary; busy: boolean; onClose: () => void; onBan: (deleteMessages: boolean) => Promise<boolean> }) {
  const [deleteMessages, setDeleteMessages] = useState(false);
  return <Modal onClose={onClose} compact><DialogHeading icon={<UserRoundX />} title={`ban ${member.username}?`} detail="they will be removed from the group" /><label className="ban-history-option"><input type="checkbox" checked={deleteMessages} onChange={(event) => setDeleteMessages(event.target.checked)} /><span><strong>delete all their messages</strong><small>also removes their media from the group history and gallery</small></span></label><DialogButtons onClose={onClose}><button className="delete-confirm" disabled={busy} onClick={() => void onBan(deleteMessages)}>{busy && <LoaderCircle className="spinner" size={13} />} ban member</button></DialogButtons></Modal>;
}

function LeaveGroupDialog({ group, busy, onClose, onLeave }: { group: GroupSummary; busy: boolean; onClose: () => void; onLeave: () => Promise<boolean> }) {
  return <Modal onClose={onClose} compact><DialogHeading icon={<LogOut />} title="leave group?" detail={group.name} /><p className="deletion-warning">This removes the group, its decrypted media cache, and its local data from this device.</p><DialogButtons onClose={onClose}><button className="delete-confirm" disabled={busy} onClick={() => void onLeave()}>{busy && <LoaderCircle className="spinner" size={13} />} leave group</button></DialogButtons></Modal>;
}

function DeleteDirectDialog({ direct, busy, onClose, onDelete }: { direct: DirectSummary; busy: boolean; onClose: () => void; onDelete: (forBoth: boolean) => Promise<boolean> }) {
  return <Modal onClose={onClose}><DialogHeading icon={<Trash2 />} title="delete conversation?" detail={direct.username} /><p className="deletion-warning">Choose whether Noise should erase this thread only from this device or send a signed erasure to both users’ Noise clients.</p><div className="direct-delete-options"><button disabled={busy} onClick={() => void onDelete(false)}><strong>just for me</strong><small>erase this device’s history and cached media</small></button><button className="danger" disabled={busy} onClick={() => void onDelete(true)}><strong>for both of us</strong><small>ask all synced Noise clients to erase the thread</small></button></div><DialogButtons onClose={onClose} closeLabel="cancel">{busy && <LoaderCircle className="spinner" size={14} />}</DialogButtons></Modal>;
}

function DeleteAccountDialog({ busy, ownedGroupCount, onClose, onDelete }: { busy: boolean; ownedGroupCount: number; onClose: () => void; onDelete: (deleteGroupMessages: boolean, deleteDirectThreads: boolean) => Promise<boolean> }) {
  const [deleteGroupMessages, setDeleteGroupMessages] = useState(false);
  const [deleteDirectThreads, setDeleteDirectThreads] = useState(false);
  return <Modal onClose={onClose}><DialogHeading icon={<UserRoundX />} title="delete your account?" detail="this permanently erases the identity on this device" />{ownedGroupCount > 0 && <p className="deletion-warning">{ownedGroupCount === 1 ? "The group you founded" : `The ${ownedGroupCount} groups you founded`} will also be permanently deleted so no group is left with a missing founder.</p>}<div className="account-delete-options"><label className="ban-history-option"><input type="checkbox" checked={deleteGroupMessages} onChange={(event) => setDeleteGroupMessages(event.target.checked)} /><span><strong>delete all messages I sent in groups</strong><small>send a signed removal to every group before leaving</small></span></label><label className="ban-history-option"><input type="checkbox" checked={deleteDirectThreads} onChange={(event) => setDeleteDirectThreads(event.target.checked)} /><span><strong>delete all DM threads</strong><small>ask both users’ Noise clients to erase every thread and cached media</small></span></label></div><p className="deletion-fine-print">Noise can erase relay data and tell official clients to forget it, but it cannot recall screenshots, exports, backups, or modified clients.</p><DialogButtons onClose={onClose}><button className="delete-confirm" disabled={busy} onClick={() => void onDelete(deleteGroupMessages, deleteDirectThreads)}>{busy && <LoaderCircle className="spinner" size={13} />} delete account</button></DialogButtons></Modal>;
}

function LogoutDialog({ busy, onClose, onLogout }: { busy: boolean; onClose: () => void; onLogout: () => Promise<boolean> }) {
  return <Modal onClose={onClose} compact><DialogHeading icon={<LogOut />} title="log out on this device?" detail="your account stays encrypted on the relay network" /><p className="deletion-warning">Local identity data and cached media will be removed. Sign back in with your Noise ID and password.</p><DialogButtons onClose={onClose}><button className="primary" disabled={busy} onClick={() => void onLogout()}>{busy && <LoaderCircle className="spinner" size={13} />} log out</button></DialogButtons></Modal>;
}

function DeleteGroupDialog({ group, busy, onClose, onDelete }: { group: GroupSummary; busy: boolean; onClose: () => void; onDelete: () => Promise<boolean> }) {
  const warning = group.remote_deletion_supported
    ? "This permanently erases its messages, invitation, and group media from the relays. It cannot be undone."
    : "This older group predates authenticated relay deletion. It will be removed from this device; groups made from this version onward are erased from the relays too.";
  return <Modal onClose={onClose} compact><DialogHeading icon={<Trash2 />} title="delete group?" detail={group.name} /><p className="deletion-warning">{warning}</p><DialogButtons onClose={onClose}><button className="delete-confirm" disabled={busy} onClick={() => void onDelete()}>{busy && <LoaderCircle className="spinner" size={13} />} {group.remote_deletion_supported ? "delete group" : "remove group"}</button></DialogButtons></Modal>;
}

function DeleteMessageDialog({ message, scopeId, busy, onClose, onDelete }: { message: MessageSummary; scopeId: string; busy: boolean; onClose: () => void; onDelete: () => Promise<boolean> }) {
  return (
    <Modal onClose={onClose} compact>
      <DialogHeading icon={<Trash2 />} title="delete message?" detail={`sent by ${message.username}`} />
      <div className="delete-message-preview">
        {message.attachment && <ReplyMediaThumbnail message={message as MessageSummary & { attachment: MediaAttachment }} scopeId={scopeId} />}
        <span>
          <strong>{replyPreview(message)}</strong>
          <small>{formatTime(message.created_at_millis)}</small>
        </span>
      </div>
      <p className="deletion-warning">This removes the message from the group history for everyone. It cannot be undone in Noise.</p>
      <DialogButtons onClose={onClose}>
        <button className="delete-confirm" disabled={busy} onClick={() => void onDelete()}>
          {busy && <LoaderCircle className="spinner" size={13} />} delete message
        </button>
      </DialogButtons>
    </Modal>
  );
}

function PersonDialog({ person, canMessage, onMessage, onClose }: { person: PersonSummary; canMessage: boolean; onMessage: () => void; onClose: () => void }) {
  return <Modal onClose={onClose} compact><div className="person-card"><PresenceAvatar name={person.username} image={person.avatar} size={72} status={person.presence_status ?? "offline"} /><h2>{person.username}</h2><div className="noise-signature"><small>Noise Signature</small><strong>{noiseSignature(person.public_key)}</strong></div><p>{person.bio || "no bio yet"}</p>{canMessage && <button className="profile-message" onClick={onMessage}><MessageCircle size={15} /> message</button>}</div></Modal>;
}

function noiseSignature(publicKey: string) {
  const alphabet = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";
  try {
    const padded = publicKey.padEnd(Math.ceil(publicKey.length / 4) * 4, "=");
    const bytes = Uint8Array.from(atob(padded), (character) => character.charCodeAt(0));
    if (bytes.length < 8) return "UNAVAILABLE";
    let signature = "";
    for (let characterIndex = 0; characterIndex < 12; characterIndex += 1) {
      let value = 0;
      for (let bitIndex = 0; bitIndex < 5; bitIndex += 1) {
        const sourceBit = characterIndex * 5 + bitIndex;
        value = (value << 1) | ((bytes[Math.floor(sourceBit / 8)] >> (7 - (sourceBit % 8))) & 1);
      }
      signature += alphabet[value];
    }
    return `${signature.slice(0, 6)}-${signature.slice(6)}`;
  } catch {
    return "UNAVAILABLE";
  }
}

function Modal({ children, onClose, compact = false, wide = false, className = "" }: { children: React.ReactNode; onClose: () => void; compact?: boolean; wide?: boolean; className?: string }) {
  return <div className="modal-backdrop" onMouseDown={onClose}><section className={`modal ${compact ? "compact" : ""} ${wide ? "wide" : ""} ${className}`.trim()} onMouseDown={(event) => event.stopPropagation()}><button className="modal-close" onClick={onClose}><X size={15} /></button>{children}</section></div>;
}

function DialogHeading({ icon, title, detail }: { icon: React.ReactNode; title: string; detail: string }) {
  return <div className="dialog-heading"><span>{icon}</span><h2>{title}</h2><p>{detail}</p></div>;
}

function DialogButtons({ children, onClose, closeLabel = "cancel" }: { children: React.ReactNode; onClose?: () => void; closeLabel?: string }) {
  return <div className="dialog-buttons">{onClose && <button onClick={onClose}>{closeLabel}</button>}<span />{children}</div>;
}

function LabeledArea({ label, count, children }: { label: string; count?: string; children: React.ReactNode }) {
  return <label className="labeled-area"><span><strong>{label}</strong><small>{count}</small></span>{children}</label>;
}

function ImagePicker({ name, existing, selection, square = false, disabled = false }: { name: string; existing: ProfileImage | null; selection: ReturnType<typeof useImageSelection>; square?: boolean; disabled?: boolean }) {
  const input = useRef<HTMLInputElement>(null);
  return <button className="image-picker" disabled={disabled} onClick={() => input.current?.click()}><span className={`avatar ${square ? "square" : ""}`} style={{ width: 96, height: 96 }}>{selection.preview ? <img src={selection.preview} alt="" /> : <Avatar name={name} image={selection.removed ? null : existing} size={96} square={square} />}</span>{!disabled && <i><Camera size={13} /></i>}<input ref={input} hidden type="file" accept="image/*" onChange={(event) => void selection.choose(event.target.files?.[0])} /></button>;
}

function BackgroundPicker({ existing, selection, label, recommendation, mobile = false, disabled = false }: { existing: ProfileImage | null; selection: ReturnType<typeof useBackgroundSelection>; label: string; recommendation: string; mobile?: boolean; disabled?: boolean }) {
  const input = useRef<HTMLInputElement>(null);
  const existingSource = useProfileImageSource(selection.removed ? null : existing);
  const source = selection.preview ?? existingSource;
  const hasBackground = Boolean(selection.preview || (!selection.removed && existing));
  return (
    <div className={`background-picker ${mobile ? "mobile" : "desktop"}`}>
      <div className="background-picker-control">
        <button className="background-picker-preview" disabled={disabled || selection.busy} onClick={() => input.current?.click()}>
          {source
            ? <img src={source} alt={`selected ${label}`} />
            : hasBackground
              ? <span><LoaderCircle className="spinner" size={16} /></span>
              : <span><Camera size={17} /> add background</span>}
          {source && <i><Camera size={12} /></i>}
        </button>
        {hasBackground && <button className="background-picker-remove" disabled={disabled || selection.busy} onClick={selection.remove} aria-label={`remove ${label}`} title={`remove ${label}`}><X size={11} /></button>}
      </div>
      <input ref={input} hidden type="file" accept="image/*" onChange={(event) => { const target = event.currentTarget; void selection.choose(target.files?.[0]).finally(() => { target.value = ""; }); }} />
      <small>{label}</small>
      <em>{recommendation}</em>
      {selection.error && <p>{selection.error}</p>}
    </div>
  );
}

function useBackgroundSelection(variant: "desktop" | "mobile") {
  const [base64, setBase64] = useState<string | null>(null);
  const [preview, setPreview] = useState<string | null>(null);
  const [removed, setRemoved] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  return {
    base64,
    preview,
    removed,
    busy,
    error,
    async choose(file?: File) {
      if (!file) return;
      setBusy(true);
      setError(null);
      try {
        const data = await prepareGroupBackground(file, variant);
        setBase64(data);
        setPreview(`data:image/jpeg;base64,${data}`);
        setRemoved(false);
      } catch (cause) {
        setError(message(cause));
      } finally {
        setBusy(false);
      }
    },
    remove() {
      setBase64(null);
      setPreview(null);
      setRemoved(true);
      setError(null);
    },
  };
}

function useImageSelection() {
  const [base64, setBase64] = useState<string | null>(null);
  const [preview, setPreview] = useState<string | null>(null);
  const [removed, setRemoved] = useState(false);
  return {
    base64,
    preview,
    removed,
    async choose(file?: File) {
      if (!file) return;
      const data = await prepareImage(file);
      setBase64(data);
      setPreview(`data:image/jpeg;base64,${data}`);
      setRemoved(false);
    },
    remove() { setBase64(null); setPreview(null); setRemoved(true); },
  };
}

function ErrorToast({ error, onClose }: { error: string; onClose: () => void }) {
  return <div className="error-toast"><span><strong>signal lost</strong>{error}</span><button onClick={onClose}><X size={15} /></button></div>;
}

function UpdateBanner({ status, retry, restart, dismiss }: ReturnType<typeof useAutoUpdater>) {
  if (!status) return null;
  if (status.phase === "ready") {
    return <div className="update-banner ready"><span><strong>Noise {status.version} is ready</strong><small>{status.restartFailed ? "restart failed · close and reopen Noise" : "update installed · restart when you're ready"}</small></span><button onClick={restart}>{status.restartFailed ? "try restart" : "restart Noise"}</button></div>;
  }
  return <div className="update-banner failed"><span><strong>update failed</strong><small>your current version is still intact</small></span><button onClick={retry}>try again</button><button className="update-dismiss" onClick={dismiss} aria-label="dismiss update"><X size={14} /></button></div>;
}

function Loading() { return <div className="loading"><LoaderCircle className="spinner" /></div>; }

async function syncGroupEncryption(): Promise<GroupEncryptionStatus | null> {
  try {
    return await noise<GroupEncryptionStatus>({
      action: "sync_group_encryption",
      relays,
    });
  } catch (cause) {
    if (message(cause).includes("unknown variant `sync_group_encryption`")) return null;
    throw cause;
  }
}

async function syncGroupActivity(groupId: string): Promise<GroupActivityResult | null> {
  try {
    const result = await noise<GroupActivityResult | LocalSummary>({
      action: "sync_group_activity",
      group_id: groupId,
      relays,
    });
    if (!result) return null;
    return "summary" in result
      ? result
      : { summary: result, conversation: null };
  } catch (cause) {
    if (message(cause).includes("unknown variant `sync_group_activity`")) return null;
    throw cause;
  }
}

async function markGroupRead(groupId: string): Promise<LocalSummary | null> {
  try {
    return await noise<LocalSummary>({
      action: "mark_group_read",
      group_id: groupId,
    });
  } catch (cause) {
    if (message(cause).includes("unknown variant `mark_group_read`")) return null;
    throw cause;
  }
}

function EncryptionPending({ phase }: { phase: GroupEncryptionStatus["phase"] }) {
  return (
    <div className="encryption-pending">
      <Shield />
      <strong>securing this device</strong>
      <span>
        {phase === "waiting_for_admission"
          ? "the group founder will admit this identity automatically"
          : "another authenticated device must admit this device"}
      </span>
      <small>you can leave Noise open — this screen updates as soon as the group confirms</small>
    </div>
  );
}

function formatTime(millis: number) {
  return new Intl.DateTimeFormat(undefined, { hour: "numeric", minute: "2-digit" }).format(new Date(millis));
}

function formatGalleryDate(millis: number) {
  return new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric", hour: "numeric", minute: "2-digit" }).format(new Date(millis));
}

function primeVideoFrame(video: HTMLVideoElement) {
  if (video.dataset.thumbnailPrimed === "true") return;
  video.dataset.thumbnailPrimed = "true";
  const previewTimes = videoPreviewTimes(video.duration);
  video.dataset.posterAttempt = "0";
  video.currentTime = previewTimes[0] ?? 0.001;
}

function videoPreviewTimes(duration: number) {
  if (!Number.isFinite(duration) || duration <= 0) return [];
  const end = Math.max(0.001, duration - 0.05);
  return [
    Math.min(end, Math.max(0.12, Math.min(0.5, duration * 0.03))),
    Math.min(end, Math.max(0.35, Math.min(1.5, duration * 0.1))),
    Math.min(end, Math.max(0.7, Math.min(3, duration * 0.2))),
  ].filter((time, index, times) => index === 0 || Math.abs(time - times[index - 1]) > 0.02);
}

function videoFrameIsNearBlack(video: HTMLVideoElement) {
  try {
    const canvas = document.createElement("canvas");
    canvas.width = 24;
    canvas.height = 24;
    const context = canvas.getContext("2d", { willReadFrequently: true });
    if (!context) return false;
    context.drawImage(video, 0, 0, canvas.width, canvas.height);
    const pixels = context.getImageData(0, 0, canvas.width, canvas.height).data;
    let luminance = 0;
    let brightest = 0;
    for (let index = 0; index < pixels.length; index += 4) {
      const value = pixels[index] * 0.2126 + pixels[index + 1] * 0.7152 + pixels[index + 2] * 0.0722;
      luminance += value;
      brightest = Math.max(brightest, value);
    }
    return luminance / (pixels.length / 4) < 18 && brightest < 48;
  } catch {
    return false;
  }
}

function imageIsNearBlack(source: string) {
  return new Promise<boolean>((resolve) => {
    const image = new Image();
    image.onload = () => {
      try {
        const canvas = document.createElement("canvas");
        canvas.width = 24;
        canvas.height = 24;
        const context = canvas.getContext("2d", { willReadFrequently: true });
        if (!context) return resolve(false);
        context.drawImage(image, 0, 0, canvas.width, canvas.height);
        const pixels = context.getImageData(0, 0, canvas.width, canvas.height).data;
        let luminance = 0;
        let brightest = 0;
        for (let index = 0; index < pixels.length; index += 4) {
          const value = pixels[index] * 0.2126 + pixels[index + 1] * 0.7152 + pixels[index + 2] * 0.0722;
          luminance += value;
          brightest = Math.max(brightest, value);
        }
        resolve(luminance / (pixels.length / 4) < 18 && brightest < 48);
      } catch {
        resolve(false);
      }
    };
    image.onerror = () => resolve(false);
    image.src = source;
  });
}

async function uploadPendingMedia(pending: PendingMedia | null, action: "upload_media_chunk" | "upload_direct_media_chunk", onProgress: (progress: number) => void, signal: AbortSignal): Promise<MediaAttachment | null> {
  if (!pending) return null;
  const mediaPreview = pending.mediaPreview;
  const chunks: MediaChunk[] = [];
  const chunkSize = 1024 * 1024;
  for (let offset = 0; offset < pending.file.size; offset += chunkSize) {
    if (signal.aborted) throw new Error("media upload cancelled");
    const chunk = await noise<MediaChunk>({
      action,
      data_base64: await fileBase64(pending.file.slice(offset, offset + chunkSize)),
      relays,
    });
    if (signal.aborted) throw new Error("media upload cancelled");
    if (!chunk) throw new Error("relay did not return a media chunk reference");
    chunks.push(chunk);
    onProgress(Math.min(95, Math.round(((offset + chunk.byte_length) / pending.file.size) * 95)));
  }
  if (signal.aborted) throw new Error("media upload cancelled");
  const preview = mediaPreview ? await mediaPreview : null;
  if (signal.aborted) throw new Error("media upload cancelled");
  return {
    file_name: pending.name,
    mime_type: pending.mimeType,
    byte_length: pending.byteLength,
    chunks,
    preview_data_base64: preview?.dataBase64 ?? null,
    preview_mime_type: preview?.mimeType ?? null,
    pixel_width: preview?.pixelWidth ?? null,
    pixel_height: preview?.pixelHeight ?? null,
  };
}

function fileBase64(blob: Blob) {
  return new Promise<string>((resolve, reject) => {
    const reader = new FileReader();
    reader.onerror = () => reject(reader.error ?? new Error("could not read media chunk"));
    reader.onload = () => {
      const value = String(reader.result ?? "");
      const separator = value.indexOf(",");
      separator >= 0 ? resolve(value.slice(separator + 1)) : reject(new Error("invalid media chunk"));
    };
    reader.readAsDataURL(blob);
  });
}

function message(cause: unknown) { return cause instanceof Error ? cause.message : String(cause); }
