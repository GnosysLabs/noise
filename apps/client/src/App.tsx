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
  Forward,
  GripVertical,
  Images,
  Info,
  Laptop,
  LoaderCircle,
  LogOut,
  Maximize,
  MessageCircle,
  MessagesSquare,
  Minimize,
  MoreHorizontal,
  Paperclip,
  Pause,
  Play,
  Plus,
  Radio,
  Reply,
  ScrollText,
  Search,
  Settings2,
  Shield,
  ShieldOff,
  SmilePlus,
  Trash2,
  TriangleAlert,
  UserPlus,
  UserRoundX,
  UsersRound,
  Volume2,
  VolumeX,
  X,
} from "lucide-react";
import { useCallback, useEffect, useId, useLayoutEffect, useRef, useState, useSyncExternalStore } from "react";
import type { CSSProperties, RefObject } from "react";
import { createPortal } from "react-dom";
import { relaunch } from "@tauri-apps/plugin-process";
import { check } from "@tauri-apps/plugin-updater";
import {
  cancelAddingLocalAccount,
  isTauri,
  listLocalAccounts,
  noise,
  prepareGroupBackground,
  prepareImage,
  registerMediaStream,
  relays,
  startAddingLocalAccount,
  switchLocalAccount,
} from "./api";
import type { LocalAccount, LocalAccountList } from "./api";
import { registerWebMediaStream, webMediaStreamReady } from "./mediaStream";
import { generateGroupAvatar, generateUserAvatar } from "./groupAvatar";
import { firstLink, linkify, openExternalLink } from "./linkify";
import { ReactionPicker } from "./ReactionPicker";
import { useLinkPreview } from "./useLinkPreview";
import type {
  AdultAccessSummary,
  AttachmentData,
  AttachmentRangeData,
  AvatarData,
  BannedMemberSummary,
  Conversation,
  DirectConversation,
  DirectMessagePolicy,
  DirectInbox,
  DirectSummary,
  DeviceSummary,
  GroupActivityResult,
  GroupEncryptionStatus,
  GroupContentRating,
  GroupSummary,
  GroupWatch,
  IdentitySummary,
  LocalSummary,
  LinkPreview,
  MakeResult,
  MediaAttachment,
  MediaChunk,
  MemberSummary,
  MessageSummary,
  ProfileAlbumData,
  ProfileAlbumItem,
  ProfileImage,
  ReactionSummary,
  ReportSummary,
  SearchLocationResult,
  SearchHistoryScope,
  SearchMessageResult,
  SearchPersonResult,
  SearchResults,
  SentMessageResult,
  TopicSummary,
} from "./types";

type Dialog =
  | { type: "make" }
  | { type: "join" }
  | { type: "frequency"; group: string; frequency: string }
  | { type: "noise_id"; noiseId: string }
  | { type: "profile"; profile: IdentitySummary }
  | { type: "group"; group: GroupSummary }
  | { type: "create_topic"; group: GroupSummary }
  | { type: "topic"; group: GroupSummary; topic: TopicSummary }
  | { type: "rules"; group: GroupSummary }
  | { type: "media" }
  | { type: "reports" }
  | { type: "report_message"; message: MessageSummary }
  | { type: "forward_message"; message: MessageSummary; sourceScopeId: string }
  | { type: "delete_message"; message: MessageSummary; scopeId: string }
  | { type: "ban_member"; member: MemberSummary }
  | { type: "leave_group"; group: GroupSummary }
  | { type: "delete_group"; group: GroupSummary }
  | { type: "delete_direct"; direct: DirectSummary }
  | { type: "delete_account" }
  | { type: "logout" }
  | { type: "search" }
  | { type: "new_direct" }
  | { type: "block_person"; person: PersonSummary }
  | { type: "album"; person: PersonSummary; editable: boolean }
  | { type: "person"; person: PersonSummary };

type PersonSummary = Pick<MemberSummary, "public_key" | "username" | "bio" | "avatar" | "album" | "accepts_direct_messages" | "direct_message_policy"> & {
  presence_status?: PresenceStatus;
};
type SidebarMode = "groups" | "directs";
type PresenceStatus = "online" | "recently-active" | "offline";
type ForwardDestination =
  | {
      type: "group";
      groupId: string;
      topicId: string | null;
      label: string;
    }
  | {
      type: "direct";
      publicKey: string;
      label: string;
    };

function albumButtonLabel(
  album: { item_count: number } | null | undefined,
  label = "album",
) {
  return album && album.item_count > 0
    ? `${label} (${album.item_count})`
    : label;
}

function hideBlockedPeople(
  conversation: Conversation,
  blockedPublicKeys: Set<string>,
): Conversation {
  if (blockedPublicKeys.size === 0) return conversation;
  const messages = conversation.messages
    .filter((item) => !blockedPublicKeys.has(item.author_public_key))
    .map((item) => ({
      ...item,
      reactions: item.reactions
        ?.map((reaction) => {
          const reactorPublicKeys = reaction.reactor_public_keys.filter(
            (publicKey) => !blockedPublicKeys.has(publicKey),
          );
          return {
            ...reaction,
            reactor_public_keys: reactorPublicKeys,
            count: reactorPublicKeys.length,
          };
        })
        .filter((reaction) => reaction.count > 0),
    }));
  const visibleMessageIds = new Set(messages.map((item) => item.event_id));
  return {
    ...conversation,
    members: conversation.members.filter(
      (member) => !blockedPublicKeys.has(member.public_key),
    ),
    messages,
    reports: conversation.reports.filter(
      (report) =>
        !blockedPublicKeys.has(report.reporter_public_key)
        && !blockedPublicKeys.has(report.message.author_public_key),
    ),
    reported_message_event_ids: conversation.reported_message_event_ids.filter(
      (eventId) => visibleMessageIds.has(eventId),
    ),
  };
}

function withCurrentDirectProfile(
  message: MessageSummary,
  self: IdentitySummary,
  contact: DirectSummary,
): MessageSummary {
  const profile = message.author_public_key === self.public_key
    ? self
    : message.author_public_key === contact.public_key
      ? contact
      : null;
  if (!profile) return message;
  return {
    ...message,
    username: profile.username,
    bio: profile.bio,
    avatar: profile.avatar,
    album: profile.album,
    accepts_direct_messages: profile.accepts_direct_messages,
    direct_message_policy: profile.direct_message_policy,
  };
}

function withoutMessage(conversation: Conversation, messageEventId: string): Conversation {
  return {
    ...conversation,
    messages: conversation.messages.filter((message) => message.event_id !== messageEventId),
    reports: conversation.reports.filter(
      (report) => report.message.event_id !== messageEventId,
    ),
    reported_message_event_ids: conversation.reported_message_event_ids.filter(
      (eventId) => eventId !== messageEventId,
    ),
  };
}

function withoutMessages(
  conversation: Conversation,
  messageEventIds: ReadonlySet<string>,
): Conversation {
  if (messageEventIds.size === 0) return conversation;
  return {
    ...conversation,
    messages: conversation.messages.filter(
      (message) => !messageEventIds.has(message.event_id),
    ),
    reports: conversation.reports.filter(
      (report) => !messageEventIds.has(report.message.event_id),
    ),
    reported_message_event_ids: conversation.reported_message_event_ids.filter(
      (eventId) => !messageEventIds.has(eventId),
    ),
  };
}

function restoreMessage(
  conversation: Conversation,
  snapshot: Conversation,
  messageEventId: string,
): Conversation {
  const message = snapshot.messages.find((item) => item.event_id === messageEventId);
  const reports = snapshot.reports.filter(
    (report) => report.message.event_id === messageEventId,
  );
  return {
    ...conversation,
    messages: message && !conversation.messages.some((item) => item.event_id === messageEventId)
      ? [...conversation.messages, message].sort((left, right) =>
          left.created_at_millis - right.created_at_millis
          || left.event_id.localeCompare(right.event_id)
        )
      : conversation.messages,
    reports: reports.length
      ? [
          ...conversation.reports,
          ...reports.filter((report) =>
            !conversation.reports.some(
              (existing) => existing.report_event_id === report.report_event_id,
            )
          ),
        ].sort((left, right) =>
          left.created_at_millis - right.created_at_millis
          || left.report_event_id.localeCompare(right.report_event_id)
        )
      : conversation.reports,
    reported_message_event_ids:
      snapshot.reported_message_event_ids.includes(messageEventId)
      && !conversation.reported_message_event_ids.includes(messageEventId)
        ? [...conversation.reported_message_event_ids, messageEventId]
        : conversation.reported_message_event_ids,
  };
}

function withCurrentGroupProfile(
  group: GroupSummary,
  current: GroupSummary,
): GroupSummary {
  if (group.group_id !== current.group_id) return group;
  return {
    ...group,
    name: current.name,
    description: current.description,
    rules: current.rules,
    content_rating: current.content_rating,
    avatar: current.avatar,
    background: current.background,
    mobile_background: current.mobile_background,
    accent_color: current.accent_color,
    members_can_send_messages: current.members_can_send_messages,
    members_can_send_media: current.members_can_send_media,
    owner_public_key: current.owner_public_key,
  };
}

const PRESENCE_IDLE_MILLIS = 5 * 60_000;
const PRESENCE_HEARTBEAT_MILLIS = 20_000;
const PRESENCE_OBSERVATION_STALE_MILLIS = 70_000;
const DEFAULT_ACCENT_COLOR = "#7758ED";
const MAX_DISPLAY_NAME_LENGTH = 16;
const ACCENT_PRESETS = ["#7758ED", "#E84D8A", "#F06A3C", "#E0A82E", "#43B581", "#24A6A6", "#4D82F0", "#A45EE5"];
const DEVICE_SESSION_REVOKED_ERROR = "this device was logged out remotely";

function currentDeviceDescriptor() {
  const userAgent = navigator.userAgent;
  const isMac = /Macintosh|Mac OS X/i.test(userAgent);
  const isWindows = /Windows/i.test(userAgent);
  const isLinux = /Linux/i.test(userAgent) && !/Android/i.test(userAgent);
  if (isTauri) {
    const platform = isMac ? "macOS" : isWindows ? "Windows" : isLinux ? "Linux" : "Desktop";
    return { name: `${platform} desktop`, platform: `${platform} · Noise desktop` };
  }
  const platform = /iPhone|iPad/i.test(userAgent)
    ? "iOS"
    : /Android/i.test(userAgent)
      ? "Android"
      : isMac
        ? "macOS"
        : isWindows
          ? "Windows"
          : isLinux
            ? "Linux"
            : "Web";
  return { name: `${platform} browser`, platform: `${platform} · Noise web` };
}

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

function mergeGroupWatchChanges(current: GroupWatch, incoming: GroupWatch): GroupWatch {
  return {
    ...incoming,
    changed: current.changed || incoming.changed,
    deleted: current.deleted || incoming.deleted,
    changed_stream_locators: Array.from(new Set([
      ...(current.changed_stream_locators ?? []),
      ...(incoming.changed_stream_locators ?? []),
    ])),
    control_changed: current.control_changed || incoming.control_changed,
    change_hints_complete:
      current.change_hints_complete === true
      && incoming.change_hints_complete === true,
  };
}

type PendingGroupWatch = { change: GroupWatch; initial: boolean };

function mergePendingGroupWatch(
  current: PendingGroupWatch | null,
  incoming: PendingGroupWatch,
): PendingGroupWatch {
  return current
    ? {
      change: mergeGroupWatchChanges(current.change, incoming.change),
      initial: current.initial || incoming.initial,
    }
    : incoming;
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

function NoiseMark({ size, className, monochrome = false }: { size: number; className?: string; monochrome?: boolean }) {
  const gradientId = useId().replaceAll(":", "");
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
      {!monochrome && (
        <defs>
          <linearGradient id={gradientId} x1="214" y1="512" x2="810" y2="512" gradientUnits="userSpaceOnUse">
            <stop stopColor="var(--accent-light)" />
            <stop offset="1" stopColor="var(--accent-dark)" />
          </linearGradient>
        </defs>
      )}
      <path
        d="M206 512h72l55-144 94 296 91-390 91 476 86-382 53 144h70"
        stroke={monochrome ? "currentColor" : `url(#${gradientId})`}
        strokeWidth="64"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

function MediaLoadStatus({
  failed = false,
  compact = false,
  prominent = false,
}: {
  failed?: boolean;
  compact?: boolean;
  prominent?: boolean;
}) {
  return (
    <span
      className={`media-load-status ${failed ? "failed" : ""} ${compact ? "compact" : ""} ${prominent ? "prominent" : ""}`}
      role="status"
      aria-label={failed ? "media unavailable" : "loading media"}
    >
      <span className="media-load-status-circle">
        {failed
          ? <X size={compact ? 13 : 18} />
          : <NoiseMark
              size={compact ? 15 : prominent ? 38 : 30}
              className="noise-loading-indicator"
              monochrome
            />}
      </span>
    </span>
  );
}

function OlderMessagesSentinel({
  loading,
  sentinel,
}: {
  loading: boolean;
  sentinel: RefObject<HTMLDivElement | null>;
}) {
  return (
    <div className="messages-older" ref={sentinel}>
      {loading && (
        <span
          className="messages-older-loading"
          role="status"
          aria-label="loading older messages"
        >
          <NoiseMark size={26} className="noise-loading-indicator" monochrome />
        </span>
      )}
    </div>
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

function ContactSignalCopyButton({ publicKey }: { publicKey: string }) {
  return <CopyButton value={noiseSignature(publicKey)} label="copy signal" />;
}

const avatarCache = new Map<string, string>();
const profileImageRequests = new Map<string, Promise<string>>();
let profileImageCacheGeneration = 0;
const mediaCache = new Map<string, string>();
const mediaLoadPromises = new Map<string, Promise<string>>();
const mediaBootstrapPromises = new Map<string, Promise<void>>();
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
// How close to the top of the list counts as "asking for older messages".
const OLDER_MESSAGE_TRIGGER_DISTANCE = 320;
// A relay page can be entirely made of messages the current topic filters out,
// so walk back a few pages before giving the scroll gesture back to the user.
const REMOTE_HISTORY_ATTEMPTS = 4;
const GENERAL_TOPIC_LOADING_KEY = "__noise_general_topic__";
type MediaLoadPriority = "visible" | "nearby" | "background";
const MEDIA_PRIORITY_RANK: Record<MediaLoadPriority, number> = {
  visible: 0,
  nearby: 1,
  background: 2,
};

class MediaLoadScheduler {
  private active = 0;
  private sequence = 0;
  private readonly queued = new Map<string, {
    key: string;
    priority: MediaLoadPriority;
    sequence: number;
    run: () => Promise<string>;
    resolve: (value: string) => void;
    reject: (cause: unknown) => void;
  }>();

  constructor(private readonly limit: number) {}

  enqueue(
    key: string,
    priority: MediaLoadPriority,
    run: () => Promise<string>,
  ) {
    const promise = new Promise<string>((resolve, reject) => {
      this.queued.set(key, {
        key,
        priority,
        sequence: this.sequence++,
        run,
        resolve,
        reject,
      });
    });
    this.pump();
    return promise;
  }

  promote(key: string, priority: MediaLoadPriority) {
    const job = this.queued.get(key);
    if (!job || MEDIA_PRIORITY_RANK[priority] >= MEDIA_PRIORITY_RANK[job.priority]) return;
    job.priority = priority;
    this.pump();
  }

  cancelQueued() {
    const jobs = [...this.queued.values()];
    this.queued.clear();
    for (const job of jobs) job.reject(new Error("loading superseded"));
  }

  private pump() {
    while (this.active < this.limit && this.queued.size) {
      const job = [...this.queued.values()].sort((left, right) =>
        MEDIA_PRIORITY_RANK[left.priority] - MEDIA_PRIORITY_RANK[right.priority]
        || left.sequence - right.sequence
      )[0];
      this.queued.delete(job.key);
      this.active += 1;
      void job.run()
        .then(job.resolve, job.reject)
        .finally(() => {
          this.active -= 1;
          this.pump();
        });
    }
  }
}

const mediaLoadScheduler = new MediaLoadScheduler(3);

// Video bootstraps (the first megabyte a browser needs before the first
// frame) get their own lane so a Play click is never queued behind feed
// images.
const mediaBootstrapScheduler = new MediaLoadScheduler(2);

function mediaCacheKey(attachment: MediaAttachment) {
  return attachment.chunks.map((chunk) => chunk.blob_id).join(":");
}

function mediaFailureIsPermanent(cause: unknown) {
  const detail = message(cause).toLowerCase();
  return detail.includes("predates constellation storage")
    || detail.includes("retired alpha media format")
    || detail.includes("invalid blob")
    || detail.includes("invalid size")
    || detail.includes("does not match")
    || detail.includes("belongs to a different conversation")
    || detail.includes("does not belong to a known conversation");
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
  mediaBootstrapPromises.clear();
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
  file: Promise<File>;
  previewUrl: string;
  mediaPreview: Promise<MediaPreview | null> | null;
};

type ComposerUploadState = {
  attachment: PendingMedia | null;
  progress: number | null;
  controller: AbortController | null;
};

const emptyComposerUpload: ComposerUploadState = {
  attachment: null,
  progress: null,
  controller: null,
};
const composerUploads = new Map<string, ComposerUploadState>();
const composerUploadListeners = new Map<string, Set<() => void>>();

function composerUpload(key: string) {
  return composerUploads.get(key) ?? emptyComposerUpload;
}

function updateComposerUpload(
  key: string,
  update: (current: ComposerUploadState) => ComposerUploadState,
) {
  const current = composerUpload(key);
  const next = update(current);
  if (!next.attachment && next.progress === null && !next.controller) {
    composerUploads.delete(key);
  } else {
    composerUploads.set(key, next);
  }
  for (const listener of composerUploadListeners.get(key) ?? []) listener();
}

function useComposerUpload(key: string) {
  const subscribe = useCallback((listener: () => void) => {
    const listeners = composerUploadListeners.get(key) ?? new Set();
    listeners.add(listener);
    composerUploadListeners.set(key, listeners);
    return () => {
      listeners.delete(listener);
      if (!listeners.size) composerUploadListeners.delete(key);
    };
  }, [key]);
  const getSnapshot = useCallback(() => composerUpload(key), [key]);
  const state = useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
  const setAttachment = useCallback((attachment: PendingMedia | null) => {
    updateComposerUpload(key, (current) => {
      if (current.attachment && current.attachment !== attachment) {
        URL.revokeObjectURL(current.attachment.previewUrl);
      }
      return { ...current, attachment };
    });
  }, [key]);
  const setProgress = useCallback((progress: number | null) => {
    updateComposerUpload(key, (current) => ({ ...current, progress }));
  }, [key]);
  const setController = useCallback((controller: AbortController | null) => {
    updateComposerUpload(key, (current) => ({ ...current, controller }));
  }, [key]);
  const takeAttachment = useCallback(() => {
    let attachment: PendingMedia | null = null;
    updateComposerUpload(key, (current) => {
      attachment = current.attachment;
      return { ...current, attachment: null, progress: null, controller: null };
    });
    return attachment;
  }, [key]);
  return { ...state, setAttachment, setProgress, setController, takeAttachment };
}

function hasTransferredFiles(transfer: DataTransfer) {
  return Array.from(transfer.types).includes("Files");
}

function firstTransferredFile(files: FileList | null | undefined) {
  const all = Array.from(files ?? []);
  return all.find((file) => /^(image|video|audio)\//.test(file.type))
    ?? all[0]
    ?? null;
}

function firstClipboardFile(clipboard: DataTransfer | null) {
  const listed = firstTransferredFile(clipboard?.files);
  if (listed) return listed;
  const itemFiles = Array.from(clipboard?.items ?? [])
    .filter((item) => item.kind === "file")
    .map((item) => item.getAsFile())
    .filter((file): file is File => Boolean(file));
  return itemFiles.find((file) => /^(image|video|audio)\//.test(file.type))
    ?? itemFiles[0]
    ?? null;
}

function useComposerMediaIntake(
  active: boolean,
  canAccept: boolean,
  onMedia: (file: File) => void,
  onNativeMedia: (path: string) => void,
) {
  const [dragging, setDragging] = useState(false);
  const onMediaRef = useRef(onMedia);
  const onNativeMediaRef = useRef(onNativeMedia);
  onMediaRef.current = onMedia;
  onNativeMediaRef.current = onNativeMedia;

  useEffect(() => {
    if (!active) {
      setDragging(false);
      return;
    }
    let dragDepth = 0;
    const resetDrag = () => {
      dragDepth = 0;
      setDragging(false);
    };
    const onDragEnter = (event: DragEvent) => {
      if (!event.dataTransfer || !hasTransferredFiles(event.dataTransfer)) return;
      event.preventDefault();
      dragDepth += 1;
      if (canAccept) setDragging(true);
    };
    const onDragOver = (event: DragEvent) => {
      if (!event.dataTransfer || !hasTransferredFiles(event.dataTransfer)) return;
      event.preventDefault();
      event.dataTransfer.dropEffect = canAccept ? "copy" : "none";
    };
    const onDragLeave = (event: DragEvent) => {
      if (!event.dataTransfer || !hasTransferredFiles(event.dataTransfer)) return;
      dragDepth = Math.max(0, dragDepth - 1);
      if (dragDepth === 0) setDragging(false);
    };
    const onDrop = (event: DragEvent) => {
      if (!event.dataTransfer || !hasTransferredFiles(event.dataTransfer)) return;
      event.preventDefault();
      resetDrag();
      if (!canAccept) return;
      const file = firstTransferredFile(event.dataTransfer.files);
      if (file) onMediaRef.current(file);
    };
    const onPaste = (event: ClipboardEvent) => {
      const file = firstClipboardFile(event.clipboardData);
      if (!file) return;
      event.preventDefault();
      if (canAccept) onMediaRef.current(file);
    };
    window.addEventListener("dragenter", onDragEnter);
    window.addEventListener("dragover", onDragOver);
    window.addEventListener("dragleave", onDragLeave);
    window.addEventListener("drop", onDrop);
    window.addEventListener("paste", onPaste);
    window.addEventListener("blur", resetDrag);
    let unlistenNativeDrop: (() => void) | undefined;
    if (isTauri) {
      void import("@tauri-apps/api/window")
        .then(({ getCurrentWindow }) => getCurrentWindow().onDragDropEvent((event) => {
          if (event.payload.type === "enter" || event.payload.type === "over") {
            if (canAccept) setDragging(true);
            return;
          }
          if (event.payload.type === "leave") {
            resetDrag();
            return;
          }
          resetDrag();
          if (!canAccept) return;
          const path = event.payload.paths[0];
          if (path) onNativeMediaRef.current(path);
        }))
        .then((unlisten) => {
          unlistenNativeDrop = unlisten;
        })
        .catch(() => {
          // Browser drag events remain available if the native listener fails.
        });
    }
    return () => {
      unlistenNativeDrop?.();
      window.removeEventListener("dragenter", onDragEnter);
      window.removeEventListener("dragover", onDragOver);
      window.removeEventListener("dragleave", onDragLeave);
      window.removeEventListener("drop", onDrop);
      window.removeEventListener("paste", onPaste);
      window.removeEventListener("blur", resetDrag);
    };
  }, [active, canAccept]);

  return dragging;
}

type MediaPreview = {
  dataBase64: string;
  mimeType: "image/jpeg";
  pixelWidth: number;
  pixelHeight: number;
};

function preparePendingMedia(file: File): PendingMedia {
  const previewUrl = URL.createObjectURL(file);
  const preparedFile = optimizeOutgoingMedia(file);
  const immediatePreview = file.type.startsWith("video/")
    ? prepareVideoPreviewSource(previewUrl)
    : null;
  return {
    name: file.name,
    mimeType: file.type,
    byteLength: file.size,
    file: preparedFile,
    previewUrl,
    mediaPreview: file.type.startsWith("video/")
      ? firstMediaPreview(
          immediatePreview,
          prepareMediaPreviewFromFile(preparedFile, "video"),
        )
      : file.type.startsWith("image/")
        ? prepareImagePreviewSource(previewUrl)
        : null,
  };
}

function firstMediaPreview(
  ...candidates: Array<Promise<MediaPreview | null> | null>
): Promise<MediaPreview | null> {
  const pending = candidates.filter(
    (candidate): candidate is Promise<MediaPreview | null> => Boolean(candidate)
  );
  if (!pending.length) return Promise.resolve(null);
  return new Promise((resolve) => {
    let remaining = pending.length;
    let settled = false;
    for (const candidate of pending) {
      void candidate
        .then((preview) => {
          if (!settled && preview) {
            settled = true;
            resolve(preview);
          }
        })
        .catch(() => undefined)
        .finally(() => {
          remaining -= 1;
          if (!settled && remaining === 0) resolve(null);
        });
    }
  });
}

async function prepareMediaPreviewFromFile(
  file: Promise<File>,
  kind: "video" | "image",
): Promise<MediaPreview | null> {
  const prepared = await file;
  const source = URL.createObjectURL(prepared);
  try {
    return kind === "video"
      ? await prepareVideoPreviewSource(source)
      : await prepareImagePreviewSource(source);
  } finally {
    URL.revokeObjectURL(source);
  }
}

async function optimizeOutgoingMedia(file: File) {
  if (
    file.type.startsWith("image/")
    && file.type !== "image/gif"
  ) {
    return optimizeOutgoingImage(file);
  }
  if (file.type.startsWith("video/") && file.size > 24 * 1024 * 1024) {
    return optimizeOutgoingVideo(file);
  }
  return file;
}

async function optimizeOutgoingImage(file: File) {
  let bitmap: ImageBitmap | null = null;
  try {
    bitmap = await createImageBitmap(file);
    const maximumDimension = Math.max(bitmap.width, bitmap.height);
    if (maximumDimension <= 2_560 && file.size <= 4 * 1024 * 1024) {
      return file;
    }
    const scale = Math.min(1, 2_560 / maximumDimension);
    const canvas = document.createElement("canvas");
    canvas.width = Math.max(1, Math.round(bitmap.width * scale));
    canvas.height = Math.max(1, Math.round(bitmap.height * scale));
    const context = canvas.getContext("2d");
    if (!context) return file;
    context.fillStyle = "#17161a";
    context.fillRect(0, 0, canvas.width, canvas.height);
    context.drawImage(bitmap, 0, 0, canvas.width, canvas.height);
    const profiles = [
      { edge: 2_560, quality: 0.84 },
      { edge: 2_048, quality: 0.8 },
      { edge: 1_600, quality: 0.76 },
    ];
    for (const profile of profiles) {
      const profileScale = Math.min(
        1,
        profile.edge / Math.max(canvas.width, canvas.height),
      );
      const output = document.createElement("canvas");
      output.width = Math.max(1, Math.round(canvas.width * profileScale));
      output.height = Math.max(1, Math.round(canvas.height * profileScale));
      output.getContext("2d")?.drawImage(
        canvas,
        0,
        0,
        output.width,
        output.height,
      );
      const blob = await new Promise<Blob | null>((resolve) =>
        output.toBlob(resolve, "image/jpeg", profile.quality)
      );
      if (!blob) continue;
      if (blob.size > 6 * 1024 * 1024 && profile !== profiles.at(-1)) {
        continue;
      }
      if (blob.size >= file.size) return file;
      const name = file.name.replace(/\.[^.]+$/, "") || "noise-photo";
      return new File([blob], `${name}.jpg`, {
        type: "image/jpeg",
        lastModified: file.lastModified,
      });
    }
  } catch {
    return file;
  } finally {
    bitmap?.close();
  }
  return file;
}

async function optimizeOutgoingVideo(file: File) {
  if (!isTauri) return file;
  const nativePath = (file as File & { path?: string }).path;
  if (!nativePath) return file;
  try {
    const { convertFileSrc, invoke } = await import("@tauri-apps/api/core");
    const optimized = await invoke<{
      file_path: string;
      file_name: string;
      mime_type: string;
      byte_length: number;
    }>("optimize_video_upload", { sourcePath: nativePath });
    if (!optimized.byte_length || optimized.byte_length >= file.size) return file;
    const response = await fetch(convertFileSrc(optimized.file_path));
    if (!response.ok) return file;
    const blob = await response.blob();
    if (blob.size !== optimized.byte_length) return file;
    return new File([blob], optimized.file_name, {
      type: optimized.mime_type,
      lastModified: file.lastModified,
    });
  } catch {
    return file;
  }
}

async function chooseNativePendingMedia() {
  if (!isTauri) return null;
  const { open } = await import("@tauri-apps/plugin-dialog");
  const selected = await open({
    multiple: false,
    directory: false,
    filters: [{
      name: "media",
      extensions: [
        "jpg", "jpeg", "png", "gif", "webp",
        "mov", "mp4", "m4v", "avi", "mpeg", "mpg",
        "mp3", "m4a", "wav", "aac", "ogg",
      ],
    }],
  });
  if (!selected) return null;
  return pendingMediaFromNativePath(selected);
}

async function pendingMediaFromNativePath(sourcePath: string) {
  if (!isTauri) return null;
  const { convertFileSrc, invoke } = await import("@tauri-apps/api/core");
  const inspected = await invoke<{
    file_path: string;
    file_name: string;
    mime_type: string;
    byte_length: number;
  }>("inspect_media_upload", { sourcePath });
  const previewUrl = convertFileSrc(inspected.file_path);
  const file = (async () => {
    const prepared = await invoke<{
      file_path: string;
      file_name: string;
      mime_type: string;
      byte_length: number;
    }>("prepare_media_upload", { sourcePath: inspected.file_path });
    const bytes = await invoke<Uint8Array>("read_prepared_media", {
      sourcePath: prepared.file_path,
    });
    const data = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes);
    if (data.byteLength !== prepared.byte_length) {
      throw new Error("prepared media could not be read");
    }
    const buffer = new ArrayBuffer(data.byteLength);
    new Uint8Array(buffer).set(data);
    return new File([buffer], prepared.file_name, {
      type: prepared.mime_type,
      lastModified: Date.now(),
    });
  })();
  return {
    name: inspected.file_name,
    mimeType: inspected.mime_type,
    byteLength: inspected.byte_length,
    file,
    previewUrl,
    mediaPreview: inspected.mime_type.startsWith("video/")
      ? firstMediaPreview(
          prepareVideoPreviewSource(previewUrl),
          prepareMediaPreviewFromFile(file, "video"),
        )
      : inspected.mime_type.startsWith("image/")
        ? prepareImagePreviewSource(previewUrl)
        : null,
  } satisfies PendingMedia;
}

// The shared core validates the base64 encoding of a media preview rather
// than the JPEG itself, and rejects anything longer than 80,000 characters.
// Base64 expands every 3 bytes into 4 characters, so the JPEG has to stay
// within 60,000 bytes to survive that check.
const MAX_PREVIEW_BASE64_LENGTH = 80_000;
const MAX_PREVIEW_BYTES = 60_000;

// A message without a preview shows a generic placeholder everywhere it is
// referenced, so every profile is attempted before giving up on one.
const IMAGE_PREVIEW_PROFILES = [
  { edge: 360, quality: 0.62 },
  { edge: 360, quality: 0.42 },
  { edge: 288, quality: 0.5 },
  { edge: 224, quality: 0.45 },
];

async function encodePreviewWithinBudget(
  canvas: HTMLCanvasElement,
  quality: number,
): Promise<string | null> {
  const preview = await new Promise<Blob | null>((done) =>
    canvas.toBlob(done, "image/jpeg", quality)
  );
  if (!preview || preview.size > MAX_PREVIEW_BYTES) return null;
  const encoded = await fileBase64(preview);
  return encoded.length <= MAX_PREVIEW_BASE64_LENGTH ? encoded : null;
}

function prepareImagePreviewSource(source: string): Promise<MediaPreview | null> {
  const image = new Image();
  return new Promise((resolve) => {
    const finish = (value: MediaPreview | null) => {
      resolve(value);
    };
    image.onload = async () => {
      try {
        const pixelWidth = image.naturalWidth;
        const pixelHeight = image.naturalHeight;
        if (!pixelWidth || !pixelHeight) return finish(null);
        for (const profile of IMAGE_PREVIEW_PROFILES) {
          const scale = Math.min(1, profile.edge / Math.max(pixelWidth, pixelHeight));
          const canvas = document.createElement("canvas");
          canvas.width = Math.max(1, Math.round(pixelWidth * scale));
          canvas.height = Math.max(1, Math.round(pixelHeight * scale));
          const context = canvas.getContext("2d");
          if (!context) return finish(null);
          context.fillStyle = "#17161a";
          context.fillRect(0, 0, canvas.width, canvas.height);
          context.drawImage(image, 0, 0, canvas.width, canvas.height);
          const dataBase64 = await encodePreviewWithinBudget(canvas, profile.quality);
          if (dataBase64) {
            return finish({
              dataBase64,
              mimeType: "image/jpeg",
              pixelWidth,
              pixelHeight,
            });
          }
        }
        finish(null);
      } catch {
        finish(null);
      }
    };
    image.onerror = () => finish(null);
    image.src = source;
  });
}

function prepareVideoPreviewSource(source: string): Promise<MediaPreview | null> {
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
          { edge: 480, quality: 0.52 },
          { edge: 384, quality: 0.58 },
          { edge: 288, quality: 0.48 },
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
          const encoded = await encodePreviewWithinBudget(canvas, profile.quality);
          if (encoded) {
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

async function optimisticMessage(
  identity: IdentitySummary,
  text: string,
  attachment: PendingMedia | null,
  replyToMessageId: string | null,
  reuseAttachmentPreview = false,
): Promise<MessageSummary> {
  const localId = `local:${crypto.randomUUID()}`;
  const localFile = attachment && !reuseAttachmentPreview ? await attachment.file : null;
  return {
    event_id: localId,
    message_id: localId,
    author_public_key: identity.public_key,
    username: identity.username,
    bio: identity.bio,
    avatar: identity.avatar,
    album: identity.album,
    accepts_direct_messages: identity.accepts_direct_messages,
    direct_message_policy: identity.direct_message_policy,
    text,
    attachment: null,
    reply_to_message_id: replyToMessageId,
    created_at_millis: Date.now(),
    optimistic: true,
    local_attachment: attachment && (localFile || reuseAttachmentPreview) ? {
      preview_url: reuseAttachmentPreview
        ? attachment.previewUrl
        : URL.createObjectURL(localFile as File),
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
      console.error("noise update failed", cause);
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
      console.error("noise could not restart after updating", cause);
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
  const [localAccounts, setLocalAccounts] = useState<LocalAccountList>({
    active_account_id: null,
    adding_account: false,
    accounts: [],
  });
  const [conversation, setConversation] = useState<Conversation | null>(null);
  const [groupEncryption, setGroupEncryption] = useState<GroupEncryptionStatus | null>(null);
  const [directConversation, setDirectConversation] = useState<DirectConversation | null>(null);
  const [sidebarMode, setSidebarMode] = useState<SidebarMode>("groups");
  const [pendingGroupId, setPendingGroupId] = useState<string | null>(null);
  const [activeTopicId, setActiveTopicId] = useState<string | null>(null);
  const [pendingTopicId, setPendingTopicId] = useState<string | null>(null);
  const [loadingTopicId, setLoadingTopicId] = useState<string | null>(null);
  const [dialog, setDialog] = useState<Dialog | null>(null);
  const [messageJump, setMessageJump] = useState<{
    eventId: string;
    groupId?: string;
    topicId?: string | null;
    directPublicKey?: string;
    nonce: number;
  } | null>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const clearRecoveredRelayError = useCallback(() => {
    setError((current) =>
      current && isRelayConnectivityError(current) ? null : current
    );
  }, []);
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
  const admissionQueue = useRef(Promise.resolve());
  const accountCacheSyncTimer = useRef<number | null>(null);
  const groupSelectionInFlight = useRef(false);
  const topicSelectionGeneration = useRef(0);
  const activeTopicIdRef = useRef<string | null>(null);
  const desiredGroupIdRef = useRef<string | null>(null);
  const sidebarModeRef = useRef(sidebarMode);
  const desiredDirectPublicKeyRef = useRef<string | null>(null);
  sidebarModeRef.current = sidebarMode;
  activeTopicIdRef.current = activeTopicId;
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
  const [deletedGroupMessageIds, setDeletedGroupMessageIds] = useState(
    () => new Map<string, Set<string>>(),
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
  const blockedPublicKeyKey = summary?.hidden_public_keys.join("|") ?? "";
  const previousBlockedPublicKeyKey = useRef("");
  const lastPresenceActivityAt = useRef(Date.now());
  const selfPresenceActive = useRef(true);
  const [selfPresenceStatus, setSelfPresenceStatus] = useState<PresenceStatus>("online");

  useEffect(() => {
    const handleSearchShortcut = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") {
        event.preventDefault();
        setDialog({ type: "search" });
      } else if (event.key === "Escape") {
        setDialog((current) => current?.type === "search" ? null : current);
      }
    };
    window.addEventListener("keydown", handleSearchShortcut);
    return () => window.removeEventListener("keydown", handleSearchShortcut);
  }, []);

  const refreshLocalAccountList = useCallback(async () => {
    try {
      setLocalAccounts(await listLocalAccounts());
    } catch (cause) {
      console.error("noise could not read the local account list", cause);
    }
  }, []);

  useEffect(() => {
    void refreshLocalAccountList();
  }, [identityPublicKey, refreshLocalAccountList]);

  useEffect(() => {
    if (!error || !isRelayConnectivityError(error)) return;
    if (summary) {
      setError(null);
      return;
    }
    const timer = window.setTimeout(() => {
      setError((current) => current === error ? null : current);
    }, 6_000);
    return () => window.clearTimeout(timer);
  }, [error, summary]);

  useEffect(() => {
    if (!identityPublicKey || !summary?.identity.noise_id) return;
    const descriptor = currentDeviceDescriptor();
    let stopped = false;
    void noise<LocalSummary>({
      action: "register_device",
      name: descriptor.name,
      platform: descriptor.platform,
      relays,
    })
      .then((local) => {
        if (!stopped && local) setSummary(local);
      })
      .catch((cause) => {
        if (message(cause) !== DEVICE_SESSION_REVOKED_ERROR) return;
        void noise({ action: "logout" }).finally(() => window.location.reload());
      });
    return () => {
      stopped = true;
    };
  }, [identityPublicKey, summary?.identity.noise_id]);

  const scheduleAccountCacheSync = useCallback(() => {
    if (accountCacheSyncTimer.current !== null) {
      window.clearTimeout(accountCacheSyncTimer.current);
    }
    accountCacheSyncTimer.current = window.setTimeout(() => {
      accountCacheSyncTimer.current = null;
      void noise<LocalSummary>({
        action: "sync_account",
        relays,
        interruptible: true,
      })
        .then((synced) => {
          if (synced) setSummary(synced);
        })
        .catch(() => {
          // Cached text remains local and the next normal account sync retries.
        });
    }, 4_000);
  }, []);

  useEffect(() => () => {
    if (accountCacheSyncTimer.current !== null) {
      window.clearTimeout(accountCacheSyncTimer.current);
    }
  }, []);

  useEffect(() => {
    if (identityPublicKey && conversation) scheduleAccountCacheSync();
  }, [
    conversation?.group.group_id,
    identityPublicKey,
    scheduleAccountCacheSync,
  ]);

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

  function updateOptimisticGroupMessage(
    groupId: string,
    eventId: string,
    update: Partial<MessageSummary>,
  ) {
    setOptimisticGroupMessages((current) => {
      const pending = current.get(groupId);
      if (!pending?.some((item) => item.event_id === eventId)) return current;
      const next = new Map(current);
      next.set(groupId, pending.map((item) =>
        item.event_id === eventId ? { ...item, ...update } : item
      ));
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

  function updateVisibleGroupModerator(
    groupId: string,
    memberPublicKey: string,
    enabled: boolean,
  ) {
    setConversation((current) => {
      if (!current || current.group.group_id !== groupId) return current;
      const next = {
        ...current,
        members: current.members.map((member) =>
          member.public_key === memberPublicKey
            ? { ...member, is_moderator: enabled }
            : member
        ),
      };
      groupConversationCache.current.set(groupId, next);
      return next;
    });
  }

  async function refreshCachedGroup(groupId: string) {
    const cached = await noise<Conversation>({
      action: "cached_conversation",
      group_id: groupId,
    });
    if (!cached) return;
    groupConversationCache.current.set(groupId, cached);
    setConversation((current) =>
      current?.group.group_id === groupId ? cached : current
    );
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
        upload_progress: undefined,
        upload_error: undefined,
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
    void noise({ action: "publish_read_state", relays }).catch(() => {
      // The local read marker is immediate; cross-device sync retries normally.
    });
  }, []);

  const markActiveGroupRead = useCallback(async (groupId: string) => {
    if (groupReadInFlight.current.has(groupId)) return;
    groupReadInFlight.current.add(groupId);
    const unreadToClear =
      groupConversationCache.current.get(groupId)?.general_unread_count ?? 0;
    setSummary((current) => current ? {
      ...current,
      groups: current.groups.map((group) =>
        group.group_id === groupId
          ? { ...group, unread_count: Math.max(0, group.unread_count - unreadToClear) }
          : group
      ),
    } : current);
    const clearVisibleUnread = () => {
      setConversation((current) => {
        if (current?.group.group_id !== groupId) return current;
        const updated = {
          ...current,
          group: {
            ...current.group,
            unread_count: Math.max(
              0,
              current.group.unread_count - current.general_unread_count,
            ),
          },
          general_unread_count: 0,
        };
        groupConversationCache.current.set(groupId, updated);
        return updated;
      });
    };
    clearVisibleUnread();
    try {
      const marked = await markGroupRead(groupId);
      if (!marked) return;
      setSummary(marked);
      clearVisibleUnread();
      void noise({ action: "publish_read_state", relays }).catch(() => {
        // The local group read marker is immediate; cross-device sync retries normally.
      });
    } finally {
      groupReadInFlight.current.delete(groupId);
    }
  }, []);

  const markActiveTopicRead = useCallback(async (groupId: string, topicId: string) => {
    const readKey = `${groupId}:${topicId}`;
    if (groupReadInFlight.current.has(readKey)) return;
    groupReadInFlight.current.add(readKey);
    const unreadToClear = groupConversationCache.current
      .get(groupId)
      ?.topics.find((topic) => topic.topic_id === topicId)
      ?.unread_count ?? 0;
    setSummary((current) => current ? {
      ...current,
      groups: current.groups.map((group) =>
        group.group_id === groupId
          ? { ...group, unread_count: Math.max(0, group.unread_count - unreadToClear) }
          : group
      ),
    } : current);
    const clearVisibleUnread = () => {
      setConversation((current) => {
        if (current?.group.group_id !== groupId) return current;
        const topicUnread = current.topics.find(
          (topic) => topic.topic_id === topicId,
        )?.unread_count ?? 0;
        const updated = {
          ...current,
          group: {
            ...current.group,
            unread_count: Math.max(0, current.group.unread_count - topicUnread),
          },
          topics: current.topics.map((topic) =>
            topic.topic_id === topicId ? { ...topic, unread_count: 0 } : topic
          ),
        };
        groupConversationCache.current.set(groupId, updated);
        return updated;
      });
    };
    // Entering a topic is itself the read gesture. Clear the badge before any
    // bridge or network work so navigation never waits on relay activity.
    clearVisibleUnread();
    try {
      const marked = await markTopicRead(groupId, topicId);
      if (!marked) return;
      setSummary(marked);
      clearVisibleUnread();
      void noise({ action: "publish_read_state", relays }).catch(() => {
        // The local topic read marker is immediate; cross-device sync retries normally.
      });
    } finally {
      groupReadInFlight.current.delete(readKey);
    }
  }, []);

  const markEntireGroupRead = useCallback(async (groupId: string) => {
    const readKey = `all:${groupId}`;
    if (groupReadInFlight.current.has(readKey)) return;
    groupReadInFlight.current.add(readKey);
    try {
      const marked = await noise<LocalSummary>({
        action: "mark_entire_group_read",
        group_id: groupId,
      });
      if (!marked) return;
      setSummary(marked);
      const cached = groupConversationCache.current.get(groupId);
      if (cached) {
        groupConversationCache.current.set(groupId, {
          ...cached,
          group: { ...cached.group, unread_count: 0 },
          general_unread_count: 0,
          topics: cached.topics.map((topic) => ({ ...topic, unread_count: 0 })),
        });
      }
      setConversation((current) => {
        if (current?.group.group_id !== groupId) return current;
        return {
          ...current,
          group: { ...current.group, unread_count: 0 },
          general_unread_count: 0,
          topics: current.topics.map((topic) => ({ ...topic, unread_count: 0 })),
        };
      });
      void noise({ action: "sync_account", relays, interruptible: true }).catch(() => {
        // The local markers are immediate; cross-device sync retries normally.
      });
    } catch (cause) {
      setError(message(cause));
    } finally {
      groupReadInFlight.current.delete(readKey);
    }
  }, []);

  const syncDirectInbox = useCallback(async (
    markActiveRead: boolean,
    interruptible = false,
  ) => {
    const generation = refreshGeneration.current;
    const inbox = await noise<DirectInbox>({
      action: "direct_inbox",
      relays,
      interruptible,
    });
    if (!inbox) return;
    clearRecoveredRelayError();
    // The Rust client has already fetched and decrypted these messages. Keep them
    // even if another UI selection changed while that work was in flight, so an
    // unread badge can never point at a thread that still needs another fetch.
    for (const item of inbox.conversations) {
      directConversationCache.current.set(item.contact.public_key, item);
    }
    if (generation !== refreshGeneration.current) return;
    applyDirectInbox(inbox);
    const activePublicKey = desiredDirectPublicKeyRef.current
      ?? inbox.summary.directs.find((direct) => direct.is_active)?.public_key;
    const active = inbox.summary.directs.find((direct) => direct.public_key === activePublicKey);
    if (markActiveRead && active?.has_unread) await markDirectRead(active.public_key);
  }, [applyDirectInbox, clearRecoveredRelayError, markDirectRead]);

  const refresh = useCallback(async () => {
    if (groupSelectionInFlight.current) return;
    const generation = ++refreshGeneration.current;
    const local = await noise<LocalSummary>({ action: "status" });
    if (generation !== refreshGeneration.current) return;
    if (!local) {
      setSummary(null);
      setLoading(false);
      return;
    }

    if (sidebarMode === "groups") {
      const activeGroup = local.groups.find((group) => group.is_active);
      if (!activeGroup) {
        setSummary(local);
        setConversation(null);
        setGroupEncryption(null);
        setLoading(false);
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
        if (cached) {
          groupConversationCache.current.set(activeGroup.group_id, cached);
          setConversation(cached);
          setSummary(local);
          setLoading(false);
        }
      }

      // A fresh login must spend its first relay request on the selected
      // group's newest page. That page already carries the embedded image and
      // video posters, so it can unlock the complete recent feed without
      // waiting for encryption reconciliation or background group watches.
      let latestActivity: GroupActivityResult | null = null;
      let latestActivityError: unknown = null;
      try {
        await cancelBackgroundLoading();
        latestActivity = await syncGroupActivity(activeGroup.group_id, {
          topicId: activeTopicIdRef.current,
        });
      } catch (cause) {
        if (!isSupersededLoading(cause)) latestActivityError = cause;
      }
      if (generation !== refreshGeneration.current) return;
      if (latestActivity) {
        clearRecoveredRelayError();
        setSummary(latestActivity.summary);
        if (latestActivity.conversation) {
          groupConversationCache.current.set(
            activeGroup.group_id,
            latestActivity.conversation,
          );
          dirtyGroupIds.current.delete(activeGroup.group_id);
          setConversation(latestActivity.conversation);
        } else if (cached) {
          setConversation(cached);
        }
      } else {
        setSummary(local);
        if (cached) setConversation(cached);
      }
      setLoading(false);

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
      if (encryption && encryption.phase !== "active") {
        return;
      }

      // The first request intentionally unlocks the latest messages. A second,
      // non-blocking pass repairs the durable group state on installations
      // affected by v0.1.12 (artwork, membership, and topic definitions).
      if (latestActivity) {
        try {
          const recovered = await syncGroupActivity(activeGroup.group_id, {
            topicId: activeTopicIdRef.current,
          });
          if (generation !== refreshGeneration.current) return;
          if (recovered) {
            setSummary(recovered.summary);
            if (recovered.conversation) {
              groupConversationCache.current.set(activeGroup.group_id, recovered.conversation);
              setConversation(recovered.conversation);
            }
          }
        } catch (cause) {
          if (!isSupersededLoading(cause)) latestActivityError = cause;
        }
      }

      // If the priority fetch needed encryption repair, retry it now that the
      // group is active. Normal fresh logins never take this second path.
      if (!latestActivity) {
        try {
          latestActivity = await syncGroupActivity(activeGroup.group_id, {
            topicId: activeTopicIdRef.current,
          });
        } catch (cause) {
          if (!isSupersededLoading(cause)) latestActivityError = cause;
        }
      }
      const reconciled = await noise<LocalSummary>({ action: "status" });
      if (generation !== refreshGeneration.current) return;
      if (latestActivity?.conversation) {
        clearRecoveredRelayError();
        groupConversationCache.current.set(
          activeGroup.group_id,
          latestActivity.conversation,
        );
        dirtyGroupIds.current.delete(activeGroup.group_id);
        setConversation(latestActivity.conversation);
        setSummary(latestActivity.summary);
      } else {
        setSummary(reconciled);
        if (!cached && latestActivityError) throw latestActivityError;
      }
      if (needsReadBaseline) {
        void noise<LocalSummary>({
          action: "sync_account",
          relays,
          interruptible: true,
        })
          .then((synced) => {
            if (synced) setSummary(synced);
          })
          .catch(() => {
            // The local baseline is durable; encrypted cross-device sync retries normally.
          });
      }
      return;
    }

    setSummary(local);
    setLoading(false);
    await syncDirectInbox(true);
  }, [clearRecoveredRelayError, sidebarMode, syncDirectInbox]);

  const loadOlderGroupHistory = useCallback(async (groupId: string) => {
    try {
      const expanded = await noise<Conversation>({
        action: "load_older_group_history",
        group_id: groupId,
        relays,
      });
      if (!expanded) return;
      groupConversationCache.current.set(groupId, expanded);
      setConversation((current) =>
        current?.group.group_id === groupId ? expanded : current
      );
    } catch (cause) {
      setError(message(cause));
    }
  }, []);

  const loadOlderTopicHistory = useCallback(async (groupId: string, topicId: string) => {
    try {
      const expanded = await noise<Conversation>({
        action: "load_older_topic_history",
        group_id: groupId,
        topic_id: topicId,
        relays,
      });
      if (!expanded) return;
      groupConversationCache.current.set(groupId, expanded);
      setConversation((current) =>
        current?.group.group_id === groupId ? expanded : current
      );
    } catch (cause) {
      setError(message(cause));
    }
  }, []);

  useEffect(() => {
    if (!summary) {
      previousBlockedPublicKeyKey.current = "";
      return;
    }
    const blockedPublicKeys = new Set(summary?.hidden_public_keys ?? []);
    const previousBlockedPublicKeys = new Set(
      previousBlockedPublicKeyKey.current
        ? previousBlockedPublicKeyKey.current.split("|")
        : [],
    );
    const revealedSomeone = [...previousBlockedPublicKeys].some(
      (publicKey) => !blockedPublicKeys.has(publicKey),
    );
    previousBlockedPublicKeyKey.current = blockedPublicKeyKey;

    if (revealedSomeone) {
      groupConversationCache.current.clear();
      void refresh().catch((cause) => setError(message(cause)));
    }
    if (blockedPublicKeys.size === 0) return;
    for (const [groupId, cached] of groupConversationCache.current) {
      groupConversationCache.current.set(
        groupId,
        hideBlockedPeople(cached, blockedPublicKeys),
      );
    }
    for (const publicKey of blockedPublicKeys) {
      directConversationCache.current.delete(publicKey);
    }
    setConversation((current) =>
      current ? hideBlockedPeople(current, blockedPublicKeys) : current
    );
    setDirectConversation((current) =>
      current && blockedPublicKeys.has(current.contact.public_key) ? null : current
    );
    setDialog((current) =>
      current
      && (current.type === "person" || current.type === "block_person")
      && blockedPublicKeys.has(current.person.public_key)
        ? null
        : current
    );
  }, [blockedPublicKeyKey]);

  useEffect(() => {
    void refresh()
      .catch((cause) => {
        if (!isSupersededLoading(cause)) setError(message(cause));
      })
      .finally(() => setLoading(false));
  }, [refresh]);

  useEffect(() => {
    if (!isTauri || !identityPublicKey) return;
    void ensureNotificationPermission();
  }, [identityPublicKey]);

  const summaryActiveGroup = summary?.groups.find(
    (group) => group.group_id === desiredGroupIdRef.current,
  ) ?? summary?.groups.find((group) => group.is_active) ?? null;
  const activeGroup = summaryActiveGroup
    && conversation?.group.group_id === summaryActiveGroup.group_id
    ? withCurrentGroupProfile(summaryActiveGroup, conversation.group)
    : summaryActiveGroup;
  const activeGroupId = activeGroup?.group_id ?? null;
  const activeDirectPublicKey = summary?.directs.find((direct) => direct.is_active)?.public_key ?? null;
  const markCurrentGroupRead = useCallback(() => {
    if (!activeGroupId) return;
    if (activeTopicId) {
      void markActiveTopicRead(activeGroupId, activeTopicId);
    } else {
      void markActiveGroupRead(activeGroupId);
    }
  }, [activeGroupId, activeTopicId, markActiveGroupRead, markActiveTopicRead]);
  const activeGroupBackground = sidebarMode === "groups" ? activeGroup?.background ?? null : null;
  const activeAccentStyle = accentStyle(sidebarMode === "groups" ? activeGroup?.accent_color : null);
  const appBackgroundSource = useProfileImageSource(activeGroupBackground);
  const groupWatchKey = summary?.groups
    .map((group) => group.group_id)
    .sort()
    .join("|") ?? "";

  useEffect(() => {
    if (
      sidebarMode !== "groups"
      || !activeGroupId
      || !groupEncryption
      || groupEncryption.group_id !== activeGroupId
      || groupEncryption.phase === "active"
      || groupEncryption.phase === "removed"
    ) return;

    let stopped = false;
    const recover = async () => {
      let retryDelay = 650;
      while (!stopped && desiredGroupIdRef.current === activeGroupId) {
        try {
          if (summary?.identity.noise_id) {
            const reconciled = await noise<LocalSummary>({
              action: "sync_read_state",
              relays,
            });
            if (stopped || desiredGroupIdRef.current !== activeGroupId) return;
            if (reconciled) setSummary(reconciled);
          }

          const encryption = await syncGroupEncryption();
          if (stopped || desiredGroupIdRef.current !== activeGroupId) return;
          setGroupEncryption(encryption);
          if (encryption?.phase === "removed") {
            const reconciled = await noise<LocalSummary>({ action: "status" });
            if (!stopped) {
              setConversation(null);
              setSummary(reconciled);
            }
            return;
          }
          if (encryption?.phase === "active") {
            const activity = await syncGroupActivity(activeGroupId, {
              topicId: activeTopicIdRef.current,
            });
            if (stopped || desiredGroupIdRef.current !== activeGroupId) return;
            if (activity) {
              setSummary(activity.summary);
              if (activity.conversation) {
                groupConversationCache.current.set(activeGroupId, activity.conversation);
                setConversation(activity.conversation);
              }
            }
            void noise({
              action: "sync_account",
              relays,
              interruptible: true,
            }).catch(() => undefined);
            return;
          }
        } catch {
          // Recovery remains on screen and retries until the encrypted account
          // snapshot or group admission becomes available.
        }
        await new Promise((resolve) => window.setTimeout(resolve, retryDelay));
        retryDelay = Math.min(Math.round(retryDelay * 1.55), 4_000);
      }
    };
    void recover();
    return () => {
      stopped = true;
    };
  }, [
    activeGroupId,
    groupEncryption?.group_id,
    groupEncryption?.phase,
    sidebarMode,
    summary?.identity.noise_id,
  ]);

  const handleDeletedGroup = useCallback(async (groupId: string) => {
    groupWatchRevisions.current.delete(groupId);
    dirtyGroupIds.current.delete(groupId);
    groupConversationCache.current.delete(groupId);
    if (desiredGroupIdRef.current === groupId) {
      desiredGroupIdRef.current = null;
      activeTopicIdRef.current = null;
      setActiveTopicId(null);
      setConversation(null);
      setGroupEncryption(null);
    }
    await refresh();
    void noise({
      action: "sync_account",
      relays,
      interruptible: true,
    }).catch(() => {
      // The local deletion tombstone is already durable; account sync retries normally.
    });
  }, [refresh]);

  const admitPendingGroupMembers = useCallback(async (groupId: string) => {
    try {
      // Ask first: the question is answered from relay state without touching
      // local state, so sweeping every group cannot slow down sending a
      // message. Only a group that has somebody waiting pays for the real pass.
      const waiting = await noise<boolean>({
        action: "group_has_pending_admissions",
        group_id: groupId,
        relays,
      });
      if (!waiting) return;
      // An admission-only pass, even for the group on screen: it reads the
      // signed control log instead of the group's history, and the phase this
      // identity is in keeps coming from opening and recovering the group.
      await syncGroupEncryption(groupId);
    } catch {
      // Admission retries on the next control change or when the group opens.
    }
  }, []);

  const queueAdmissionPass = useCallback((groupId: string) => {
    // One group at a time: signing in to a long member list must not fire a
    // burst of relay round trips at the conversation the user is opening.
    admissionQueue.current = admissionQueue.current
      .then(() => new Promise<void>((resolve) => window.setTimeout(resolve, 250)))
      .then(() => admitPendingGroupMembers(groupId))
      .catch(() => undefined);
  }, [admitPendingGroupMembers]);

  const syncChangedGroupStreams = useCallback(async (
    groupId: string,
    change: GroupWatch,
    initial: boolean,
    preferredTopicId: string | null,
    isStopped: () => boolean,
    syncInitialGroup?: (groupId: string) => ReturnType<typeof syncGroupActivity>,
  ) => {
    const hintsComplete = change.change_hints_complete === true;
    const isVisibleGroup = desiredGroupIdRef.current === groupId;
    const readTarget = isVisibleGroup
      ? { topicId: activeTopicIdRef.current }
      : undefined;
    let topicSource = groupConversationCache.current.get(groupId);
    if (initial || !hintsComplete || change.control_changed) {
      const activity = initial && syncInitialGroup
        ? await syncInitialGroup(groupId)
        : await syncGroupActivity(groupId, readTarget);
      if (!activity || isStopped()) return false;
      setSummary(activity.summary);
      if (activity.conversation) {
        topicSource = activity.conversation;
        groupConversationCache.current.set(groupId, activity.conversation);
        dirtyGroupIds.current.delete(groupId);
        if (desiredGroupIdRef.current === groupId) {
          setConversation(activity.conversation);
        }
      }
    }

    const topics = (topicSource?.topics ?? []).filter((topic) => !topic.archived);
    const allTopicIds = topics.map((topic) => topic.topic_id);
    const hintedLocators = new Set(change.changed_stream_locators ?? []);
    const hintedTopicIds = hintsComplete
      ? topics
        .filter((topic) => hintedLocators.has(topic.stream_locator))
        .map((topic) => topic.topic_id)
      : allTopicIds;
    const openTopicId = preferredTopicId && allTopicIds.includes(preferredTopicId)
      ? preferredTopicId
      : null;
    const immediateTopicIds = hintsComplete
      ? hintedTopicIds
      : openTopicId
        ? [openTopicId]
        : [];
    immediateTopicIds.sort((left, right) => {
      if (left === openTopicId) return -1;
      if (right === openTopicId) return 1;
      return 0;
    });
    for (const topicId of immediateTopicIds) {
      try {
        const topicActivity = await syncTopicActivity(
          groupId,
          topicId,
          isVisibleGroup && activeTopicIdRef.current === topicId,
        );
        if (isStopped()) return false;
        if (!topicActivity) continue;
        setSummary(topicActivity.summary);
        if (topicActivity.conversation) {
          topicSource = topicActivity.conversation;
          groupConversationCache.current.set(groupId, topicActivity.conversation);
          if (desiredGroupIdRef.current === groupId) {
            setConversation(topicActivity.conversation);
          }
        }
      } catch {
        // This exact stream retries on its next revision or when opened.
      }
    }

    // A published join request only changes control state, so this is where a
    // member learns that somebody is waiting to be let in.
    if (initial || change.control_changed) queueAdmissionPass(groupId);
    return true;
  }, [queueAdmissionPass]);

  useEffect(() => {
    if (!identityPublicKey || !summary) return;
    const groups = summary.groups
      .filter((group) => sidebarMode !== "groups" || group.group_id !== activeGroupId);
    let stopped = false;
    let initialSyncQueue = Promise.resolve();
    const syncInitialGroup = (groupId: string) => {
      const task = initialSyncQueue.then(() => syncGroupActivity(groupId));
      initialSyncQueue = task.then(() => undefined, () => undefined);
      return task;
    };
    const watch = async (group: GroupSummary) => {
      let revision: number | null = groupWatchRevisions.current.get(group.group_id) ?? null;
      let pending: PendingGroupWatch | null = null;
      let draining = false;
      const requeue = (item: PendingGroupWatch) => {
        pending = pending ? mergePendingGroupWatch(item, pending) : item;
      };
      const drain = async () => {
        if (draining) return;
        draining = true;
        try {
          while (!stopped && pending) {
            const item = pending;
            pending = null;
            try {
              const preferredTopicId = desiredGroupIdRef.current === group.group_id
                ? activeTopicIdRef.current
                : null;
              const synced = await syncChangedGroupStreams(
                group.group_id,
                item.change,
                item.initial,
                preferredTopicId,
                () => stopped,
                syncInitialGroup,
              );
              if (!synced) {
                if (!stopped) {
                  requeue(item);
                  await new Promise((resolve) => window.setTimeout(resolve, 1500));
                }
                continue;
              }
              groupWatchRevisions.current.set(group.group_id, item.change.revision);
              if (item.initial) {
                scheduleAccountCacheSync();
                if (!group.read_state_initialized) {
                  void noise<LocalSummary>({
                    action: "sync_account",
                    relays,
                    interruptible: true,
                  })
                    .then((syncedSummary) => {
                      if (!stopped && syncedSummary) setSummary(syncedSummary);
                    })
                    .catch(() => {
                      // The local baseline is durable; encrypted cross-device sync retries normally.
                    });
                }
              }
            } catch {
              requeue(item);
              await new Promise((resolve) => window.setTimeout(resolve, 1500));
            }
          }
        } finally {
          draining = false;
          if (!stopped && pending) void drain();
        }
      };
      const enqueue = (change: GroupWatch, initial: boolean) => {
        pending = mergePendingGroupWatch(pending, { change, initial });
        void drain();
      };
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
          if (change.deleted) {
            await handleDeletedGroup(group.group_id);
            return;
          }
          revision = change.revision;
          updatePresenceScope(
            `group:${group.group_id}`,
            presenceStatusesFromWatch(change),
          );
          if (initial || change.changed) {
            if (!initial && change.changed) dirtyGroupIds.current.add(group.group_id);
            enqueue(change, initial);
          }
        } catch (cause) {
          if (message(cause) === DEVICE_SESSION_REVOKED_ERROR) {
            await noise({ action: "logout" }).catch(() => undefined);
            window.location.reload();
            return;
          }
          await new Promise((resolve) => window.setTimeout(resolve, 1500));
        }
      }
    };
    for (const group of groups) {
      // Publish this device's group-scoped KeyPackage before the user opens
      // the conversation. Founders can then start MLS immediately and admit
      // every available member without waiting for the whole roster.
      queueAdmissionPass(group.group_id);
      void watch(group);
    }
    return () => {
      stopped = true;
    };
  }, [
    activeGroupId,
    groupWatchKey,
    handleDeletedGroup,
    identityPublicKey,
    queueAdmissionPass,
    scheduleAccountCacheSync,
    sidebarMode,
    syncChangedGroupStreams,
    updatePresenceScope,
  ]);

  useEffect(() => {
    if (sidebarMode !== "groups" || !activeGroupId) return;
    let stopped = false;
    const watch = async () => {
      let revision: number | null = groupWatchRevisions.current.get(activeGroupId) ?? null;
      let pending: PendingGroupWatch | null = null;
      let draining = false;
      const requeue = (item: PendingGroupWatch) => {
        pending = pending ? mergePendingGroupWatch(item, pending) : item;
      };
      const drain = async () => {
        if (draining) return;
        draining = true;
        try {
          while (!stopped && pending) {
            const item = pending;
            pending = null;
            try {
              const synced = await syncChangedGroupStreams(
                activeGroupId,
                item.change,
                item.initial,
                activeTopicIdRef.current,
                () => stopped,
              );
              if (!synced) {
                if (!stopped) {
                  requeue(item);
                  await new Promise((resolve) => window.setTimeout(resolve, 1500));
                }
                continue;
              }
              groupWatchRevisions.current.set(activeGroupId, item.change.revision);
            } catch {
              requeue(item);
              await new Promise((resolve) => window.setTimeout(resolve, 1500));
            }
          }
        } finally {
          draining = false;
          if (!stopped && pending) void drain();
        }
      };
      const enqueue = (change: GroupWatch, initial: boolean) => {
        pending = mergePendingGroupWatch(pending, { change, initial });
        void drain();
      };
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
          if (change.deleted) {
            await handleDeletedGroup(activeGroupId);
            return;
          }
          revision = change.revision;
          updatePresenceScope(
            `group:${activeGroupId}`,
            presenceStatusesFromWatch(change),
          );
          if (initial || change.changed) {
            if (!initial) dirtyGroupIds.current.add(activeGroupId);
            enqueue(change, initial);
          }
        } catch (cause) {
          if (message(cause) === DEVICE_SESSION_REVOKED_ERROR) {
            await noise({ action: "logout" }).catch(() => undefined);
            window.location.reload();
            return;
          }
          await new Promise((resolve) => window.setTimeout(resolve, 1500));
        }
      }
    };
    void watch();
    return () => {
      stopped = true;
    };
  }, [
    activeGroupId,
    handleDeletedGroup,
    identityPublicKey,
    sidebarMode,
    syncChangedGroupStreams,
    updatePresenceScope,
  ]);

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
            await syncDirectInbox(
              sidebarModeRef.current === "directs",
              true,
            );
          } else if (change.changed) {
            await syncDirectInbox(
              sidebarModeRef.current === "directs",
              true,
            );
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
            const reconciled = await noise<LocalSummary>({
              action: "refresh_account_state",
              relays,
              interruptible: true,
            });
            if (!stopped && reconciled) setSummary(reconciled);
          }
        } catch (cause) {
          if (message(cause) === DEVICE_SESSION_REVOKED_ERROR) {
            await noise({ action: "logout" }).catch(() => undefined);
            window.location.reload();
            return;
          }
          await new Promise((resolve) => window.setTimeout(resolve, 1500));
        }
      }
    };
    void watch();
    return () => { stopped = true; };
  }, [identityPublicKey, summary?.identity.noise_id]);

  useEffect(() => {
    if (!identityPublicKey || !summary?.identity.noise_id) return;
    let stopped = false;
    const watch = async () => {
      let revision: number | null = null;
      while (!stopped) {
        try {
          const initial = revision === null;
          const change: GroupWatch | null = await noise<GroupWatch>({
            action: "watch_read_state",
            since: revision,
            relays,
          });
          if (stopped || !change) return;
          revision = change.revision;
          if (initial || change.changed) {
            const reconciled = await noise<LocalSummary>({
              action: "sync_read_state",
              relays,
            });
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
      if (!["media upload cancelled", "media download cancelled"].includes(message(cause))) {
        setError(message(cause));
      }
      return false;
    } finally {
      setBusy(false);
    }
  }

  async function performConcurrent(operation: () => Promise<void>) {
    setError(null);
    try {
      await operation();
      return true;
    } catch (cause) {
      if (message(cause) !== "media upload cancelled") setError(message(cause));
      return false;
    }
  }

  async function forwardMessage(
    source: MessageSummary,
    sourceScopeId: string,
    destination: ForwardDestination,
    showOriginalAuthor: boolean,
    onProgress: (progress: number) => void,
  ) {
    const controller = new AbortController();
    let pending: PendingMedia | null = null;
    const forwardedFrom = showOriginalAuthor
      ? source.forwarded_from ?? {
        public_key: source.author_public_key,
        username: source.username,
      }
      : null;
    try {
      setError(null);
      let attachment: MediaAttachment | null = null;
      if (source.attachment) {
        onProgress(1);
        pending = await pendingMediaFromAttachment(source.attachment, sourceScopeId);
        attachment = await uploadPendingMedia(
          pending,
          destination.type === "group"
            ? "upload_media_chunk_to_group"
            : "upload_direct_media_chunk_to",
          onProgress,
          controller.signal,
          destination.type === "group"
            ? { group_id: destination.groupId }
            : { public_key: destination.publicKey },
        );
      }
      if (destination.type === "group") {
        await noise({
          action: "say_to_group",
          group_id: destination.groupId,
          topic_id: destination.topicId,
          text: source.text,
          attachment,
          forwarded_from: forwardedFrom,
          relays,
        });
        if (destination.groupId === desiredGroupIdRef.current) {
          const activity = destination.topicId
            ? await syncTopicActivity(
                destination.groupId,
                destination.topicId,
                desiredGroupIdRef.current === destination.groupId
                  && activeTopicIdRef.current === destination.topicId,
              )
            : await syncGroupActivity(
                destination.groupId,
                desiredGroupIdRef.current === destination.groupId
                  ? { topicId: activeTopicIdRef.current }
                  : undefined,
              );
          if (activity) {
            setSummary(activity.summary);
            if (activity.conversation) {
              groupConversationCache.current.set(destination.groupId, activity.conversation);
              setConversation(activity.conversation);
            }
          }
        }
      } else {
        await noise({
          action: "say_direct_to",
          public_key: destination.publicKey,
          text: source.text,
          attachment,
          forwarded_from: forwardedFrom,
          relays,
        });
        if (sidebarMode === "directs") await syncDirectInbox(false);
      }
      onProgress(100);
      const local = await noise<LocalSummary>({ action: "status" });
      if (local) setSummary(local);
      void noise({
        action: "sync_account",
        relays,
        interruptible: true,
      }).catch(() => undefined);
      return true;
    } catch (cause) {
      setError(message(cause));
      return false;
    } finally {
      if (pending) URL.revokeObjectURL(pending.previewUrl);
    }
  }

  function applySavedGroup(local: LocalSummary, groupId: string) {
    setSummary(local);
    const updatedGroup = local.groups.find((group) => group.group_id === groupId);
    if (updatedGroup) {
      setConversation((current) => {
        if (current?.group.group_id !== groupId) return current;
        const updatedConversation = { ...current, group: updatedGroup };
        groupConversationCache.current.set(groupId, updatedConversation);
        return updatedConversation;
      });
    }
    setDialog(null);
    void noise({
      action: "sync_account",
      relays,
      interruptible: true,
    }).catch(() => {
      // The relay-confirmed settings are already durable locally. Account
      // backup retries normally and must not hold the settings dialog open.
    });
  }

  function applyTopicActivityResult(result: GroupActivityResult, groupId: string) {
    setSummary(result.summary);
    if (result.conversation) {
      groupConversationCache.current.set(groupId, result.conversation);
      setConversation((current) =>
        current?.group.group_id === groupId ? result.conversation : current
      );
    }
  }

  async function reorderTopics(groupId: string, topicIds: string[]) {
    let previousTopics: TopicSummary[] | null = null;
    setConversation((current) => {
      if (current?.group.group_id !== groupId) return current;
      previousTopics = current.topics;
      const byId = new Map(current.topics.map((topic) => [topic.topic_id, topic]));
      const reordered = topicIds
        .map((topicId) => byId.get(topicId))
        .filter((topic): topic is TopicSummary => Boolean(topic));
      const next = {
        ...current,
        topics: [...reordered, ...current.topics.filter((topic) => topic.archived)],
      };
      groupConversationCache.current.set(groupId, next);
      return next;
    });
    try {
      const result = await noise<GroupActivityResult>({
        action: "reorder_topics",
        topic_ids: topicIds,
        relays,
      });
      if (!result) throw new Error("the relay did not confirm the topic order");
      applyTopicActivityResult(result, groupId);
    } catch (cause) {
      if (previousTopics) {
        setConversation((current) => {
          if (current?.group.group_id !== groupId) return current;
          const restored = { ...current, topics: previousTopics! };
          groupConversationCache.current.set(groupId, restored);
          return restored;
        });
      }
      setError(message(cause));
    }
  }

  function applySavedIdentity(local: LocalSummary) {
    const identity = local.identity;
    const updateMessage = (item: MessageSummary): MessageSummary =>
      item.author_public_key === identity.public_key
        ? {
            ...item,
            username: identity.username,
            bio: identity.bio,
            avatar: identity.avatar,
            album: identity.album,
            accepts_direct_messages: identity.accepts_direct_messages,
            direct_message_policy: identity.direct_message_policy,
          }
        : item;
    const updateGroupConversation = (current: Conversation): Conversation => ({
      ...current,
      members: current.members.map((member) =>
        member.public_key === identity.public_key
          ? {
              ...member,
              username: identity.username,
              bio: identity.bio,
              avatar: identity.avatar,
              album: identity.album,
              accepts_direct_messages: identity.accepts_direct_messages,
              direct_message_policy: identity.direct_message_policy,
            }
          : member
      ),
      messages: current.messages.map(updateMessage),
    });
    const updateDirectConversation = (current: DirectConversation): DirectConversation => ({
      ...current,
      messages: current.messages.map(updateMessage),
    });

    setSummary(local);
    for (const [groupId, cached] of groupConversationCache.current) {
      groupConversationCache.current.set(groupId, updateGroupConversation(cached));
    }
    for (const [publicKey, cached] of directConversationCache.current) {
      directConversationCache.current.set(publicKey, updateDirectConversation(cached));
    }
    setConversation((current) => current ? updateGroupConversation(current) : current);
    setDirectConversation((current) => current ? updateDirectConversation(current) : current);
    setDialog(null);
    void noise({
      action: "sync_account",
      relays,
      interruptible: true,
    }).catch(() => {
      // The public profile update is already saved and published. Encrypted
      // account backup retries independently.
    });
  }

  async function updateBlock(person: PersonSummary, blocked: boolean) {
    return perform(async () => {
      const local = await noise<LocalSummary>({
        action: "set_block",
        public_key: person.public_key,
        username: person.username,
        bio: person.bio,
        avatar: person.avatar,
        album: person.album,
        accepts_direct_messages: person.accepts_direct_messages,
        direct_message_policy: person.direct_message_policy,
        blocked,
        relays,
      });
      if (!local) throw new Error(`could not ${blocked ? "block" : "unblock"} this person`);
      setSummary(local);
      if (blocked) {
        directConversationCache.current.delete(person.public_key);
        setDirectConversation((current) =>
          current?.contact.public_key === person.public_key ? null : current
        );
        desiredDirectPublicKeyRef.current =
          local.directs.find((direct) => direct.is_active)?.public_key ?? null;
      }
    });
  }

  async function selectGroup(group: GroupSummary) {
    topicSelectionGeneration.current += 1;
    setActiveTopicId(null);
    setPendingTopicId(null);
    if (group.group_id === activeGroupId && !pendingGroupId) {
      if (conversation?.group.group_id === group.group_id) return;
    }
    const previousGroupId = activeGroupId;
    const needsReadBaseline = !group.read_state_initialized;
    const generation = ++refreshGeneration.current;
    groupSelectionInFlight.current = true;
    setPendingGroupId(group.group_id);
    setError(null);
    setGroupEncryption(null);

    try {
      await cancelBackgroundLoading();
      if (previousGroupId !== group.group_id) {
        await cancelMediaDownloads();
        await cancelGroupLoading();
      }

      let cached = groupConversationCache.current.get(group.group_id);
      if (!cached) {
        cached = await noise<Conversation>({
          action: "cached_conversation",
          group_id: group.group_id,
        }) ?? undefined;
        if (generation !== refreshGeneration.current) return;
        if (cached) {
          groupConversationCache.current.set(group.group_id, cached);
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
      desiredGroupIdRef.current = group.group_id;
      setSummary(local);

      // Navigation is complete once the durable local selection and cache are
      // visible. Relay and encryption reconciliation must never extend a click.
      groupSelectionInFlight.current = false;
      setPendingGroupId(null);
      void hydrateSelectedGroup(
        group,
        generation,
        needsReadBaseline,
        Boolean(cached),
        previousGroupId,
      );
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

  async function selectTopic(topic: TopicSummary | null) {
    if (!activeGroupId || !conversation || conversation.group.group_id !== activeGroupId) return;
    const topicId = topic?.topic_id ?? null;
    const loadingKey = topicId ?? GENERAL_TOPIC_LOADING_KEY;
    const generation = ++topicSelectionGeneration.current;
    const hasCachedMessages = conversation.messages.some(
      (item) => (item.topic_id ?? null) === topicId,
    );
    setActiveTopicId(topicId);
    // Cached topic navigation is complete immediately. Relay reconciliation
    // continues below without dimming an already-usable topic.
    setPendingTopicId(null);
    setError(null);
    setLoadingTopicId(hasCachedMessages ? null : loadingKey);
    try {
      // Record the navigation locally before relay reconciliation. A slow or
      // failed topic refresh must not keep an already-open topic unread.
      if (topicId) {
        await markActiveTopicRead(activeGroupId, topicId);
      } else {
        await markActiveGroupRead(activeGroupId);
      }
      await cancelBackgroundLoading();
      const activity = topicId
        ? await syncTopicActivity(activeGroupId, topicId, true)
        : await syncGroupActivity(activeGroupId, { topicId: null });
      if (generation !== topicSelectionGeneration.current
        || desiredGroupIdRef.current !== activeGroupId) return;
      if (activity) {
        setSummary(activity.summary);
        if (activity.conversation) {
          groupConversationCache.current.set(activeGroupId, activity.conversation);
          setConversation(activity.conversation);
        }
      }
    } catch (cause) {
      if (generation === topicSelectionGeneration.current && !isSupersededLoading(cause)) {
        setError(message(cause));
      }
    } finally {
      if (generation === topicSelectionGeneration.current) {
        // A failed refresh or an older in-flight snapshot must not be allowed
        // to restore the unread state after the user has opened this topic.
        if (topicId) {
          await markActiveTopicRead(activeGroupId, topicId);
        } else {
          await markActiveGroupRead(activeGroupId);
        }
        setPendingTopicId(null);
        setLoadingTopicId(null);
      }
    }
  }

  async function hydrateSelectedGroup(
    group: GroupSummary,
    generation: number,
    needsReadBaseline: boolean,
    hasCachedConversation: boolean,
    previousGroupId: string | null,
  ) {
    try {
      const encryption = await syncGroupEncryption();
      if (generation !== refreshGeneration.current) return;
      setGroupEncryption(encryption);
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
      if (encryption && encryption.phase !== "active") {
        // Cached history remains readable while secure sending is restored.
        // A group without cache shows the explicit recovery state.
        return;
      }

      const activity = await syncGroupActivity(group.group_id, {
        topicId: activeTopicIdRef.current,
      });
      if (generation !== refreshGeneration.current) return;
      if (activity) {
        setSummary(activity.summary);
        if (activity.conversation) {
          groupConversationCache.current.set(group.group_id, activity.conversation);
          dirtyGroupIds.current.delete(group.group_id);
          setConversation(activity.conversation);
        }
      } else if (!hasCachedConversation) {
        throw new Error("the selected group has no locally available conversation");
      }
      if (activity) {
        const recovered = await syncGroupActivity(group.group_id, {
          topicId: activeTopicIdRef.current,
        });
        if (generation !== refreshGeneration.current) return;
        if (recovered) {
          setSummary(recovered.summary);
          if (recovered.conversation) {
            groupConversationCache.current.set(group.group_id, recovered.conversation);
            setConversation(recovered.conversation);
          }
        }
      }

      if (needsReadBaseline) {
        void noise<LocalSummary>({
          action: "sync_account",
          relays,
          interruptible: true,
        })
          .then((synced) => {
            if (generation === refreshGeneration.current && synced) setSummary(synced);
          })
          .catch((cause) => {
            if (!isSupersededLoading(cause)) {
              // The local baseline is durable; a later account sync retries it.
            }
          });
      }
    } catch (cause) {
      if (generation === refreshGeneration.current && !isSupersededLoading(cause)) {
        setError(message(cause));
      }
    }
  }

  async function selectDirect(direct: DirectSummary) {
    if (desiredDirectPublicKeyRef.current === direct.public_key && direct.is_active) return;
    const generation = ++refreshGeneration.current;
    desiredDirectPublicKeyRef.current = direct.public_key;
    setError(null);
    await cancelMediaDownloads();
    await cancelGroupLoading();
    await cancelBackgroundLoading();
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
      void noise({
        action: "publish_read_state",
        relays,
      }).catch(() => {
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
        album: person.album,
        accepts_direct_messages: person.accepts_direct_messages,
        direct_message_policy: person.direct_message_policy,
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
    void noise({
      action: "sync_account",
      relays,
      interruptible: true,
    }).catch(() => {
      // The DM is already available locally; encrypted account sync retries normally.
    });
  }

  async function openOrStartDirect(person: PersonSummary) {
    if (!summary) return;
    const existing = summary.directs.find(
      (direct) => direct.public_key === person.public_key,
    );
    if (!existing) {
      await startDirect(person);
      return;
    }
    setDialog(null);
    await switchSidebarMode("directs");
    await selectDirect(existing);
  }

  async function startDirectFromSignal(value: string) {
    if (!summary) return;
    const normalized = value.replace(/[\s-]/g, "").toUpperCase();
    const known = [...summary.directs, ...summary.known_people].find(
      (person) => noiseSignature(person.public_key).replace("-", "") === normalized,
    );
    if (known) {
      await openOrStartDirect({
        ...known,
        presence_status: presenceStatuses.get(known.public_key) ?? "offline",
      });
      return;
    }
    try {
      const resolved = await noise<DirectSummary>({
        action: "resolve_contact_signal",
        signature: value,
        relays,
      });
      if (!resolved) throw new Error("that noise signature could not be resolved");
      await openOrStartDirect({
        ...resolved,
        presence_status: "offline",
      });
    } catch (cause) {
      setError(message(cause));
    }
  }

  async function switchSidebarMode(nextMode: SidebarMode) {
    if (nextMode === sidebarMode) return;
    await cancelMediaDownloads();
    if (nextMode === "groups") {
      setSidebarMode("groups");
      return;
    }

    // Showing a cached DM must not wait for the durable selection/read markers.
    setSidebarMode("directs");
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
  }

  async function beginAddingAccount() {
    if (busy) return;
    setBusy(true);
    setLoading(true);
    setError(null);
    try {
      await Promise.all([
        cancelMediaDownloads(),
        cancelGroupLoading(),
        cancelBackgroundLoading(),
      ]);
      await startAddingLocalAccount();
      window.location.reload();
    } catch (cause) {
      setError(message(cause));
      setBusy(false);
      setLoading(false);
    }
  }

  async function cancelAddingAccount() {
    if (busy) return;
    setBusy(true);
    setLoading(true);
    setError(null);
    try {
      await cancelAddingLocalAccount();
      window.location.reload();
    } catch (cause) {
      setError(message(cause));
      setBusy(false);
      setLoading(false);
    }
  }

  async function selectLocalAccount(account: LocalAccount) {
    if (busy || account.id === localAccounts.active_account_id) return;
    setBusy(true);
    setLoading(true);
    setError(null);
    try {
      await Promise.all([
        cancelMediaDownloads(),
        cancelGroupLoading(),
        cancelBackgroundLoading(),
      ]);
      await switchLocalAccount(account.id);
      window.location.reload();
    } catch (cause) {
      setError(message(cause));
      setBusy(false);
      setLoading(false);
    }
  }

  if (loading) return <><Loading /><UpdateBanner {...updater} /></>;
  if (!summary) {
    return (
      <>
      <Onboarding
        busy={busy}
        addingAccount={localAccounts.adding_account}
        onCancelAdd={cancelAddingAccount}
        onCreate={(username, password, birthDate) =>
          perform(async () => {
            const avatar = await generateUserAvatar(`${username}:${crypto.randomUUID()}`);
            const local = await noise<LocalSummary>({
              action: "initialize",
              username,
              password,
              birth_date: birthDate,
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
            if (!local) throw new Error("sign in completed without restoring the local identity");
            desiredGroupIdRef.current =
              local.groups.find((group) => group.is_active)?.group_id ?? null;
            desiredDirectPublicKeyRef.current =
              local.directs.find((direct) => direct.is_active)?.public_key ?? null;
            setConversation(null);
            setDirectConversation(null);
            setGroupEncryption(null);
            // The identity is durable once sign_in returns. Leave onboarding
            // immediately; group hydration is an authenticated background
            // transition and must never leave a live sign-in form behind.
            setSummary(local);
            setLoading(false);
            void refresh().catch((cause) => {
              if (!isSupersededLoading(cause)) setError(message(cause));
            });
          }, false)
        }
      />
      {error && <ErrorToast error={error} onClose={() => setError(null)} />}
      <UpdateBanner {...updater} />
      </>
    );
  }

  const rawSelectedConversationState =
    conversation?.group.group_id === activeGroupId ? conversation : null;
  const selectedConversationState = rawSelectedConversationState
    ? withoutMessages(
      rawSelectedConversationState,
      deletedGroupMessageIds.get(rawSelectedConversationState.group.group_id) ?? new Set(),
    )
    : null;
  const selectedTopic = activeTopicId
    ? selectedConversationState?.topics.find(
        (topic) => topic.topic_id === activeTopicId && !topic.archived,
      ) ?? null
    : null;
  const effectiveTopicId = selectedTopic?.topic_id ?? null;
  const selectedGroupPending = activeGroupId ? optimisticGroupMessages.get(activeGroupId) ?? [] : [];
  const selectedConversation = selectedConversationState ? {
    ...selectedConversationState,
    messages: [
      ...selectedConversationState.messages.filter(
        (item) => (item.topic_id ?? null) === effectiveTopicId,
      ),
      ...selectedGroupPending.filter((pending) =>
        (pending.topic_id ?? null) === effectiveTopicId
        && !selectedConversationState.messages.some((item) => item.event_id === pending.event_id)
      ),
    ],
    has_older_messages: selectedTopic?.has_older_messages
      ?? selectedConversationState.has_older_messages,
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
  const selectedDirectContact = activeDirectPublicKey
    ? summary.directs.find((direct) => direct.public_key === activeDirectPublicKey) ?? null
    : null;
  const selectedPresenceStatuses = new Map(presenceStatuses);
  selectedPresenceStatuses.set(summary.identity.public_key, selfPresenceStatus);
  const openPerson = (person: PersonSummary) => {
    const known = summary.directs.find(
      (candidate) => candidate.public_key === person.public_key,
    )
      ?? summary.known_people.find(
        (candidate) => candidate.public_key === person.public_key,
      )
      ?? selectedConversationState?.members.find(
        (candidate) => candidate.public_key === person.public_key,
      )
      ?? (selectedDirectConversationState?.contact.public_key === person.public_key
        ? selectedDirectConversationState.contact
        : null)
      ?? (summary.identity.public_key === person.public_key
        ? summary.identity
        : null);
    setDialog({
      type: "person",
      person: known
        ? {
            public_key: known.public_key,
            username: known.username,
            bio: known.bio,
            avatar: known.avatar,
            album: known.album,
            accepts_direct_messages: known.accepts_direct_messages,
            direct_message_policy: known.direct_message_policy,
            presence_status: selectedPresenceStatuses.get(known.public_key) ?? "offline",
          }
        : person,
    });
  };
  const openSearchLocation = async (result: SearchLocationResult) => {
    const group = summary.groups.find((candidate) => candidate.group_id === result.group_id);
    if (!group) return;
    setDialog(null);
    setSidebarMode("groups");
    await selectGroup(group);
    activeTopicIdRef.current = result.topic_id;
    setActiveTopicId(result.topic_id);
    if (result.topic_id) {
      void markActiveTopicRead(result.group_id, result.topic_id);
    } else {
      void markActiveGroupRead(result.group_id);
    }
  };
  const openSearchMessage = async (result: SearchMessageResult) => {
    setDialog(null);
    if (result.direct_public_key) {
      const direct = summary.directs.find(
        (candidate) => candidate.public_key === result.direct_public_key,
      );
      if (!direct) return;
      await switchSidebarMode("directs");
      await selectDirect(direct);
      setMessageJump({
        eventId: result.event_id,
        directPublicKey: result.direct_public_key,
        nonce: Date.now(),
      });
      return;
    }
    if (!result.group_id) return;
    const group = summary.groups.find((candidate) => candidate.group_id === result.group_id);
    if (!group) return;
    setSidebarMode("groups");
    await selectGroup(group);
    activeTopicIdRef.current = result.topic_id;
    setActiveTopicId(result.topic_id);
    if (result.topic_id) {
      void markActiveTopicRead(result.group_id, result.topic_id);
    } else {
      void markActiveGroupRead(result.group_id);
    }
    setMessageJump({
      eventId: result.event_id,
      groupId: result.group_id,
      topicId: result.topic_id,
      nonce: Date.now(),
    });
  };
  const openSearchPerson = (result: SearchPersonResult) => {
    const person: PersonSummary = {
      public_key: result.public_key,
      username: result.username,
      bio: result.bio,
      avatar: result.avatar,
      album: result.album,
      accepts_direct_messages: result.accepts_direct_messages,
      direct_message_policy: result.direct_message_policy,
      presence_status: presenceStatuses.get(result.public_key) ?? "offline",
    };
    openPerson(person);
  };
  const visibleSummary = activeGroupId ? {
    ...summary,
    groups: summary.groups.map((group) => {
      const visible = activeGroup?.group_id === group.group_id
        ? withCurrentGroupProfile(group, activeGroup)
        : group;
      return {
        ...visible,
        is_active: group.group_id === activeGroupId,
      };
    }),
  } : summary;

  return (
    <div className={`app-shell ${appBackgroundSource ? "group-background-active" : ""}`} style={activeAccentStyle}>
      {appBackgroundSource && <div className="group-app-background" style={{ backgroundImage: `url(${JSON.stringify(appBackgroundSource)})` }} aria-hidden="true" />}
        <Sidebar
        summary={visibleSummary}
        conversation={selectedConversationState}
        mode={sidebarMode}
        pendingGroupId={pendingGroupId}
        pendingTopicId={pendingTopicId}
        activeTopicId={effectiveTopicId}
        directPresenceStatuses={presenceStatuses}
        selfPresenceStatus={selfPresenceStatus}
        accounts={localAccounts.accounts}
        activeAccountId={localAccounts.active_account_id}
        accountBusy={busy}
        onMode={(mode) => void switchSidebarMode(mode)}
        onMake={() => setDialog({ type: "make" })}
        onJoin={() => setDialog({ type: "join" })}
        onSearch={() => setDialog({ type: "search" })}
        onNewDirect={() => setDialog({ type: "new_direct" })}
        onSettings={() => setDialog({ type: "profile", profile: summary.identity })}
        onAddAccount={() => void beginAddingAccount()}
        onSwitchAccount={(account) => void selectLocalAccount(account)}
        onSignOut={() => setDialog({ type: "logout" })}
        onContextMenu={(group, x, y) => {
          setGroupMenu({ group, x, y });
        }}
        onDirectContextMenu={(direct, x, y) => setDirectMenu({ direct, x, y })}
        onSelect={(group) => void selectGroup(group)}
        onSelectTopic={(topic) => void selectTopic(topic)}
        onCreateTopic={(group) => setDialog({ type: "create_topic", group })}
        onManageTopic={(group, topic) => setDialog({ type: "topic", group, topic })}
        onReorderTopics={(group, topicIds) => reorderTopics(group.group_id, topicIds)}
        onSelectDirect={(direct) => void selectDirect(direct)}
      />

      <main className="conversation-pane">
        <section className={`mode-pane ${sidebarMode === "groups" ? "active" : "inactive"}`} aria-hidden={sidebarMode !== "groups"}>
          {selectedConversation ? (
            <ConversationPanel
              key={`${selectedConversation.group.group_id}:${effectiveTopicId ?? "general"}`}
              conversation={selectedConversation}
              topic={selectedTopic}
              loadingTopic={
                loadingTopicId === (selectedTopic?.topic_id ?? GENERAL_TOPIC_LOADING_KEY)
              }
              active={sidebarMode === "groups" && dialog === null}
              busy={busy || pendingGroupId === selectedConversation.group.group_id}
              hasBackground={Boolean(appBackgroundSource)}
              canEditGroup={selectedConversation.group.owner_public_key === summary.identity.public_key}
              unreadCount={selectedTopic?.unread_count ?? selectedConversation.general_unread_count}
              messageJump={messageJump?.groupId === selectedConversation.group.group_id
                && (messageJump.topicId ?? null) === (effectiveTopicId ?? null)
                ? messageJump
                : null}
              selfPublicKey={summary.identity.public_key}
              presenceStatuses={selectedPresenceStatuses}
              onGroupSettings={() => setDialog({ type: "group", group: selectedConversation.group })}
              onTopicSettings={selectedTopic
                ? () => setDialog({
                    type: "topic",
                    group: selectedConversation.group,
                    topic: selectedTopic,
                  })
                : undefined}
              onReports={() => setDialog({ type: "reports" })}
              onMedia={() => setDialog({ type: "media" })}
              onRules={() => setDialog({ type: "rules", group: selectedConversation.group })}
              onPerson={openPerson}
              onMessage={(person) => void startDirect(person)}
              onBlock={(person) => setDialog({ type: "block_person", person })}
              onDeleteMessage={(item) => setDialog({
                type: "delete_message",
                message: item,
                scopeId: selectedConversation.group.group_id,
              })}
              onDownload={(item) => perform(async () => {
                if (!item.attachment) throw new Error("this message has no media");
                if (!await downloadAttachment(
                  item.attachment,
                  selectedConversation.group.group_id,
                )) {
                  throw new Error("media download cancelled");
                }
              }, false)}
              onForward={(item) => setDialog({
                type: "forward_message",
                message: item,
                sourceScopeId: selectedConversation.group.group_id,
              })}
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
              onSetModerator={async (member, enabled) => {
                const groupId = selectedConversation.group.group_id;
                updateVisibleGroupModerator(groupId, member.public_key, enabled);
                try {
                  await noise({
                    action: "set_moderator",
                    member_public_key: member.public_key,
                    enabled,
                    relays,
                  });
                  return true;
                } catch (cause) {
                  updateVisibleGroupModerator(groupId, member.public_key, !enabled);
                  setError(message(cause));
                  return false;
                }
              }}
              onBan={(member) => setDialog({ type: "ban_member", member })}
              onReport={(message) => setDialog({ type: "report_message", message })}
              onReachedBottom={markCurrentGroupRead}
              onLoadOlder={() =>
                selectedTopic
                  ? loadOlderTopicHistory(
                      selectedConversation.group.group_id,
                      selectedTopic.topic_id,
                    )
                  : loadOlderGroupHistory(selectedConversation.group.group_id)
              }
              onSend={async (text, pending, replyToMessageId) => {
                const groupId = selectedConversation.group.group_id;
                const controller = new AbortController();
                const signal = controller.signal;
                let optimistic = await optimisticMessage(
                  summary.identity,
                  text,
                  pending,
                  replyToMessageId,
                  Boolean(pending),
                );
                optimistic = {
                  ...optimistic,
                  topic_id: effectiveTopicId,
                  upload_progress: pending ? 0 : undefined,
                };
                addOptimisticGroupMessage(groupId, optimistic);
                if (pending?.mediaPreview && optimistic.local_attachment) {
                  const optimisticId = optimistic.event_id;
                  const localAttachment = optimistic.local_attachment;
                  void pending.mediaPreview.then((preview) => {
                    if (!preview) return;
                    updateOptimisticGroupMessage(groupId, optimisticId, {
                      local_attachment: {
                        ...localAttachment,
                        poster_url: `data:${preview.mimeType};base64,${preview.dataBase64}`,
                        pixel_width: preview.pixelWidth,
                        pixel_height: preview.pixelHeight,
                      },
                    });
                  });
                }
                let attachment: MediaAttachment | null = null;
                let result: SentMessageResult | null = null;
                const sent = await performConcurrent(async () => {
                  attachment = await uploadPendingMedia(
                    pending,
                    "upload_media_chunk",
                    (progress) => updateOptimisticGroupMessage(
                      groupId,
                      optimistic.event_id,
                      { upload_progress: progress },
                    ),
                    signal,
                  );
                  if (signal.aborted) throw new Error("media upload cancelled");
                  result = await noise<SentMessageResult>({
                    action: "say",
                    text,
                    attachment,
                    reply_to_message_id: replyToMessageId,
                    topic_id: effectiveTopicId,
                    relays,
                  });
                  if (!result) throw new Error("the relay did not confirm the message");
                });
                if (!sent || !result) {
                  updateOptimisticGroupMessage(groupId, optimistic.event_id, {
                    upload_progress: undefined,
                    upload_error: pending ? "upload failed" : "could not send",
                  });
                  return false;
                }
                const confirmed = result as SentMessageResult;
                const confirmedAttachment = attachment as MediaAttachment | null;
                if (confirmedAttachment && optimistic.local_attachment) {
                  mediaCache.set(mediaCacheKey(confirmedAttachment), optimistic.local_attachment.preview_url);
                  sentMediaPreviewCache.set(confirmed.event_id, optimistic.local_attachment);
                }
                confirmOptimisticGroupMessage(groupId, optimistic.event_id, confirmed, confirmedAttachment);
                void refresh().catch((cause) => setError(message(cause)));
                void noise({
                  action: "sync_account",
                  relays,
                  interruptible: true,
                }).catch(() => undefined);
                return true;
              }}
            />
          ) : activeGroupId && groupEncryption?.group_id === activeGroupId
            && groupEncryption.phase !== "active"
            && groupEncryption.phase !== "removed" ? (
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
              contact={selectedDirectContact ?? selectedDirectConversation.contact}
              active={sidebarMode === "directs" && dialog === null}
              busy={busy}
              self={summary.identity}
              selfPresence={selfPresenceStatus}
              contactPresence={presenceStatuses.get(selectedDirectConversation.contact.public_key) ?? "offline"}
              messageJump={messageJump?.directPublicKey === selectedDirectConversation.contact.public_key
                ? messageJump
                : null}
              onPerson={openPerson}
              onAlbum={(person) => setDialog({ type: "album", person, editable: false })}
              onBlock={(person) => setDialog({ type: "block_person", person })}
              onDelete={() => setDialog({ type: "delete_direct", direct: selectedDirectConversation.contact })}
              onDownload={(item) => perform(async () => {
                if (!item.attachment) throw new Error("this message has no media");
                if (!await downloadAttachment(
                  item.attachment,
                  selectedDirectConversation.media_scope_id,
                )) {
                  throw new Error("media download cancelled");
                }
              }, false)}
              onForward={(item) => setDialog({
                type: "forward_message",
                message: item,
                sourceScopeId: selectedDirectConversation.media_scope_id,
              })}
              onSend={async (text, pending, onProgress, replyToMessageId, signal) => {
                const publicKey = selectedDirectConversation.contact.public_key;
                let optimistic = pending
                  ? null
                  : await optimisticMessage(summary.identity, text, null, replyToMessageId);
                if (optimistic) addOptimisticDirectMessage(publicKey, optimistic);
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
                  if (optimistic) removeOptimisticDirectMessage(publicKey, optimistic.event_id);
                  return false;
                }
                const confirmed = result as SentMessageResult;
                const confirmedAttachment = attachment as MediaAttachment | null;
                if (pending) {
                  optimistic = await optimisticMessage(summary.identity, text, pending, replyToMessageId);
                  addOptimisticDirectMessage(publicKey, optimistic);
                }
                if (!optimistic) return false;
                if (confirmedAttachment && optimistic.local_attachment) {
                  mediaCache.set(mediaCacheKey(confirmedAttachment), optimistic.local_attachment.preview_url);
                  sentMediaPreviewCache.set(confirmed.event_id, optimistic.local_attachment);
                }
                confirmOptimisticDirectMessage(publicKey, optimistic.event_id, confirmed, confirmedAttachment);
                void refresh().catch((cause) => setError(message(cause)));
                void noise({
                  action: "sync_account",
                  relays,
                  interruptible: true,
                }).catch(() => undefined);
                return true;
              }}
            />
          ) : activeDirectPublicKey ? <Loading /> : <EmptyDirects />}
        </section>
      </main>

      {dialog?.type === "make" && (
        <MakeDialog
          busy={busy}
          adultContentEnabled={summary.adult_access.adult_content_enabled}
          onClose={() => setDialog(null)}
          onSubmit={(name, contentRating) =>
            perform(async () => {
              const avatar = await generateGroupAvatar(`${name}:${crypto.randomUUID()}`);
              const result = await noise<MakeResult>({
                action: "make",
                name,
                content_rating: contentRating,
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
          adultAccess={summary.adult_access}
          devices={summary.devices}
          blockedPeople={summary.blocked_people}
          busy={busy}
          onClose={() => setDialog(null)}
          onDeleteAccount={() => setDialog({ type: "delete_account" })}
          onLogout={() => setDialog({ type: "logout" })}
          onSummary={(local) => {
            setSummary(local);
            setDialog({ type: "profile", profile: local.identity });
          }}
          onUnblock={(person) => updateBlock(person, false)}
          onAdultContentChange={(enabled) =>
            perform(async () => {
              const local = await noise<LocalSummary>({
                action: "set_adult_content_enabled",
                enabled,
                relays,
              });
              if (!local) throw new Error("the content preference was not updated");
              setSummary(local);
            }, false)
          }
          onRevokeDevice={(device) =>
            perform(async () => {
              const local = await noise<LocalSummary>({
                action: "revoke_device",
                device_id: device.device_id,
                relays,
              });
              if (!local) throw new Error("the device session was not updated");
              setSummary(local);
            }, false)
          }
          onSave={(username, bio, avatar, removeAvatar, directMessagePolicy) =>
            perform(async () => {
              const local = await noise<LocalSummary>({
                action: "update_profile",
                username,
                bio,
                avatar_data_base64: avatar,
                avatar_mime_type: avatar ? "image/jpeg" : null,
                remove_avatar: removeAvatar,
                accepts_direct_messages: directMessagePolicy !== "nobody",
                direct_message_policy: directMessagePolicy,
                relays,
              });
              if (!local) throw new Error("the relay did not return the updated profile");
              applySavedIdentity(local);
            }, false)
          }
        />
      )}
      {dialog?.type === "create_topic" && (
        <CreateTopicDialog
          busy={busy}
          onClose={() => setDialog(null)}
          onCreate={(name, icon) => perform(async () => {
            const existingIds = new Set(
              conversation?.group.group_id === dialog.group.group_id
                ? conversation.topics.map((topic) => topic.topic_id)
                : [],
            );
            const result = await noise<GroupActivityResult>({
              action: "create_topic",
              name,
              icon,
              relays,
            });
            if (!result) throw new Error("the relay did not return the new topic");
            applyTopicActivityResult(result, dialog.group.group_id);
            const created = result.conversation?.topics.find(
              (topic) => !existingIds.has(topic.topic_id),
            );
            if (created) setActiveTopicId(created.topic_id);
            setDialog(null);
          }, false)}
        />
      )}
      {dialog?.type === "topic" && (
        <TopicSettingsDialog
          topic={dialog.topic}
          busy={busy}
          onClose={() => setDialog(null)}
          onSave={(name, icon, locked) => perform(async () => {
            const result = await noise<GroupActivityResult>({
              action: "update_topic",
              topic_id: dialog.topic.topic_id,
              name,
              icon,
              locked,
              relays,
            });
            if (!result) throw new Error("the relay did not return the updated topic");
            applyTopicActivityResult(result, dialog.group.group_id);
            setDialog(null);
          }, false)}
          onArchive={() => perform(async () => {
            const result = await noise<GroupActivityResult>({
              action: "archive_topic",
              topic_id: dialog.topic.topic_id,
              relays,
            });
            if (!result) throw new Error("the relay did not confirm the archive");
            applyTopicActivityResult(result, dialog.group.group_id);
            if (activeTopicId === dialog.topic.topic_id) setActiveTopicId(null);
            setDialog(null);
          }, false)}
        />
      )}
      {dialog?.type === "group" && (
        <GroupSettingsDialog
          group={dialog.group}
          adultContentEnabled={summary.adult_access.adult_content_enabled}
          bannedMembers={conversation?.group.group_id === dialog.group.group_id ? conversation.banned_members : []}
          presenceStatuses={selectedPresenceStatuses}
          busy={busy}
          onClose={() => setDialog(null)}
          onUnban={(member) => perform(async () => {
            await noise({ action: "unban_member", member_public_key: member.public_key, relays });
            await refreshCachedGroup(dialog.group.group_id);
          })}
          onRotateFrequency={(revokeOnly) => perform(async () => {
            const local = await noise<LocalSummary>({ action: "rotate_frequency", revoke_only: revokeOnly, relays });
            if (!local) throw new Error("the relay did not return the updated frequency");
            setSummary(local);
            const updatedGroup = local.groups.find((group) => group.group_id === dialog.group.group_id);
            if (updatedGroup) setDialog({ type: "group", group: updatedGroup });
          })}
          onSave={(name, description, accentColor, contentRating, avatar, removeAvatar, background, removeBackground, mobileBackground, removeMobileBackground, membersCanSendMessages, membersCanSendMedia) =>
            perform(async () => {
              const local = await noise<LocalSummary>({
                action: "update_group_profile",
                name,
                description,
                rules: dialog.group.rules,
                accent_color: accentColor,
                content_rating: contentRating,
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
              if (!local) throw new Error("the relay did not return the updated group");
              applySavedGroup(local, dialog.group.group_id);
              if (dialog.group.content_rating === "general" && contentRating === "adult") {
                const updatedGroup = local.groups.find((group) => group.group_id === dialog.group.group_id);
                if (updatedGroup?.frequency) {
                  setDialog({
                    type: "frequency",
                    group: updatedGroup.name,
                    frequency: updatedGroup.frequency,
                  });
                }
              }
            }, false)
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
              if (!local) throw new Error("the relay did not return the updated group");
              applySavedGroup(local, dialog.group.group_id);
            }, false)
          }
        />
      )}
      {dialog?.type === "media" && selectedConversationState && (
        <MediaGalleryDialog
          group={selectedConversationState.group}
          messages={selectedConversationState.messages}
          onClose={() => setDialog(null)}
        />
      )}
      {dialog?.type === "forward_message" && (
        <ForwardMessageDialog
          message={dialog.message}
          groups={summary.groups}
          topicsByGroup={new Map(summary.groups.map((group) => [
            group.group_id,
            groupConversationCache.current.get(group.group_id)?.topics ?? [],
          ]))}
          people={[...summary.directs, ...summary.known_people]}
          selfPublicKey={summary.identity.public_key}
          onClose={() => setDialog(null)}
          onForward={(destination, showOriginalAuthor, onProgress) =>
            forwardMessage(
              dialog.message,
              dialog.sourceScopeId,
              destination,
              showOriginalAuthor,
              onProgress,
            )}
        />
      )}
      {dialog?.type === "report_message" && (
        <ReportMessageDialog
          message={dialog.message}
          busy={busy}
          onClose={() => setDialog(null)}
          onReport={(reason) => perform(async () => {
            await noise({ action: "report_message", message_event_id: dialog.message.event_id, reason, relays });
            if (activeGroupId) await refreshCachedGroup(activeGroupId);
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
          onDelete={async () => {
            const groupId = dialog.scopeId;
            const messageEventId = dialog.message.event_id;
            setDeletedGroupMessageIds((current) => {
              const next = new Map(current);
              const deleted = new Set(next.get(groupId) ?? []);
              deleted.add(messageEventId);
              next.set(groupId, deleted);
              return next;
            });
            const previous = groupConversationCache.current.get(groupId)
              ?? (conversation?.group.group_id === groupId ? conversation : null);
            if (previous) {
              const optimistic = withoutMessage(previous, messageEventId);
              groupConversationCache.current.set(groupId, optimistic);
              setConversation((current) =>
                current?.group.group_id === groupId
                  ? withoutMessage(current, messageEventId)
                  : current
              );
            }
            setDialog(null);

            try {
              await noise({
                action: "delete_message",
                message_event_id: messageEventId,
                relays,
              });
              return true;
            } catch (cause) {
              setError(message(cause));
              setDeletedGroupMessageIds((current) => {
                const next = new Map(current);
                const deleted = new Set(next.get(groupId) ?? []);
                deleted.delete(messageEventId);
                if (deleted.size > 0) next.set(groupId, deleted);
                else next.delete(groupId);
                return next;
              });
              if (!previous) return false;
              const cached = groupConversationCache.current.get(groupId) ?? previous;
              groupConversationCache.current.set(
                groupId,
                restoreMessage(cached, previous, messageEventId),
              );
              setConversation((current) =>
                current?.group.group_id === groupId
                  ? restoreMessage(current, previous, messageEventId)
                  : current
              );
              return false;
            }
          }}
        />
      )}
      {dialog?.type === "reports" && selectedConversationState && (
        <ReportsDialog
          reports={selectedConversationState.reports}
          presenceStatuses={selectedPresenceStatuses}
          busy={busy}
          onClose={() => setDialog(null)}
          onDismiss={(report) => perform(async () => {
            await noise({ action: "resolve_report", report_event_id: report.report_event_id, relays });
            await refreshCachedGroup(selectedConversationState.group.group_id);
          })}
          onDelete={async (report) => {
            setDialog({
              type: "delete_message",
              message: report.message,
              scopeId: selectedConversationState.group.group_id,
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
              if (activeGroupId) await refreshCachedGroup(activeGroupId);
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
              setDialog(null);
              void refresh().catch((cause) => setError(message(cause)));
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
              setDialog(null);
              void refresh().catch((cause) => setError(message(cause)));
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
            setDialog(null);
            void refresh().catch((cause) => setError(message(cause)));
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
            window.location.reload();
          }, false)}
        />
      )}
      {dialog?.type === "logout" && (
        <LogoutDialog
          busy={busy}
          onClose={() => setDialog({ type: "profile", profile: summary.identity })}
          onLogout={() => perform(async () => {
            await noise({ action: "logout" });
            window.location.reload();
          }, false)}
        />
      )}
      {dialog?.type === "person" && (
        <PersonDialog
          person={dialog.person}
          canMessage={dialog.person.public_key !== summary.identity.public_key && dialog.person.accepts_direct_messages}
          canBlock={dialog.person.public_key !== summary.identity.public_key}
          onMessage={() => void startDirect(dialog.person)}
          onAlbum={() => setDialog({ type: "album", person: dialog.person, editable: false })}
          onBlock={() => setDialog({ type: "block_person", person: dialog.person })}
          onClose={() => setDialog(null)}
        />
      )}
      {dialog?.type === "search" && (
        <GlobalSearchModal
          onClose={() => setDialog(null)}
          onLocation={(result) => void openSearchLocation(result)}
          onMessage={(result) => void openSearchMessage(result)}
          onPerson={(result) => void openSearchPerson(result)}
          onLoadOlder={async (scope) => {
            if (!scope.group_id) {
              await noise({
                action: "load_older_direct_history",
                relays,
              });
              return;
            }
            if (scope.topic_id) {
              await loadOlderTopicHistory(scope.group_id, scope.topic_id);
            } else {
              await loadOlderGroupHistory(scope.group_id);
            }
          }}
        />
      )}
      {dialog?.type === "new_direct" && (
        <NewDirectDialog
          people={[...summary.directs, ...summary.known_people]}
          selfPublicKey={summary.identity.public_key}
          busy={busy}
          onClose={() => setDialog(null)}
          onChoose={(person) => openOrStartDirect(person)}
          onSignal={(signal) => startDirectFromSignal(signal)}
        />
      )}
      {dialog?.type === "album" && (
        <ProfileAlbumDialog
          person={dialog.person}
          editable={dialog.editable}
          onClose={() => setDialog(null)}
          onSummary={(local) => setSummary(local)}
        />
      )}
      {dialog?.type === "block_person" && (
        <BlockPersonDialog
          person={dialog.person}
          busy={busy}
          onClose={() => setDialog({ type: "person", person: dialog.person })}
          onBlock={async () => {
            const blocked = await updateBlock(dialog.person, true);
            if (blocked) setDialog(null);
            return blocked;
          }}
        />
      )}
      {groupMenu && (
        <GroupContextMenu
          x={groupMenu.x}
          y={groupMenu.y}
          onClose={() => setGroupMenu(null)}
          onMarkRead={() => {
            const groupId = groupMenu.group.group_id;
            setGroupMenu(null);
            void markEntireGroupRead(groupId);
          }}
          onDelete={() => {
            setDialog({ type: "delete_group", group: groupMenu.group });
            setGroupMenu(null);
          }}
          onLeave={() => {
            setDialog({ type: "leave_group", group: groupMenu.group });
            setGroupMenu(null);
          }}
          hasUnread={groupMenu.group.unread_count > 0}
          isFounder={groupMenu.group.owner_public_key === summary.identity.public_key}
        />
      )}
      {directMenu && <DirectContextMenu
        x={directMenu.x}
        y={directMenu.y}
        onClose={() => setDirectMenu(null)}
        onBlock={() => {
          setDialog({ type: "block_person", person: directMenu.direct });
          setDirectMenu(null);
        }}
        onDelete={() => { setDialog({ type: "delete_direct", direct: directMenu.direct }); setDirectMenu(null); }}
      />}
      {error && <ErrorToast error={error} onClose={() => setError(null)} />}
      <UpdateBanner {...updater} />
    </div>
  );
}

function Sidebar({
  summary,
  conversation,
  mode,
  pendingGroupId,
  pendingTopicId,
  activeTopicId,
  directPresenceStatuses,
  selfPresenceStatus,
  accounts,
  activeAccountId,
  accountBusy,
  onMode,
  onMake,
  onJoin,
  onSearch,
  onNewDirect,
  onSettings,
  onAddAccount,
  onSwitchAccount,
  onSignOut,
  onContextMenu,
  onDirectContextMenu,
  onSelect,
  onSelectTopic,
  onCreateTopic,
  onManageTopic,
  onReorderTopics,
  onSelectDirect,
}: {
  summary: LocalSummary;
  conversation: Conversation | null;
  mode: SidebarMode;
  pendingGroupId: string | null;
  pendingTopicId: string | null;
  activeTopicId: string | null;
  directPresenceStatuses: Map<string, PresenceStatus>;
  selfPresenceStatus: PresenceStatus;
  accounts: LocalAccount[];
  activeAccountId: string | null;
  accountBusy: boolean;
  onMode: (mode: SidebarMode) => void;
  onMake: () => void;
  onJoin: () => void;
  onSearch: () => void;
  onNewDirect: () => void;
  onSettings: () => void;
  onAddAccount: () => void;
  onSwitchAccount: (account: LocalAccount) => void;
  onSignOut: () => void;
  onContextMenu: (group: GroupSummary, x: number, y: number) => void;
  onDirectContextMenu: (direct: DirectSummary, x: number, y: number) => void;
  onSelect: (group: GroupSummary) => void;
  onSelectTopic: (topic: TopicSummary | null) => void;
  onCreateTopic: (group: GroupSummary) => void;
  onManageTopic: (group: GroupSummary, topic: TopicSummary) => void;
  onReorderTopics: (group: GroupSummary, topicIds: string[]) => Promise<void>;
  onSelectDirect: (direct: DirectSummary) => void;
}) {
  const hasUnreadDirects = summary.directs.some((direct) => direct.has_unread);
  const canManageTopics = conversation?.group.owner_public_key === summary.identity.public_key
    || conversation?.members.some(
      (member) => member.public_key === summary.identity.public_key && member.is_moderator,
    ) === true;
  const canReorderTopics = conversation?.group.owner_public_key === summary.identity.public_key;
  const [draggedTopicId, setDraggedTopicId] = useState<string | null>(null);
  const [dropTarget, setDropTarget] = useState<{ topicId: string; after: boolean } | null>(null);
  const [accountMenuOpen, setAccountMenuOpen] = useState(false);
  const accountMenuRef = useRef<HTMLDivElement | null>(null);
  const topicDrag = useRef<{ pointerId: number; topicId: string; startY: number; moved: boolean } | null>(null);
  const dropTargetRef = useRef<{ topicId: string; after: boolean } | null>(null);
  const suppressTopicClick = useRef(false);
  const updateDropTarget = (target: { topicId: string; after: boolean } | null) => {
    dropTargetRef.current = target;
    setDropTarget(target);
  };
  useEffect(() => {
    if (!accountMenuOpen) return;
    const close = (event: PointerEvent) => {
      if (!accountMenuRef.current?.contains(event.target as Node)) {
        setAccountMenuOpen(false);
      }
    };
    document.addEventListener("pointerdown", close);
    return () => document.removeEventListener("pointerdown", close);
  }, [accountMenuOpen]);
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
        <button className="square-button" onClick={onSearch} title="search (⌘K)" aria-label="search"><Search size={16} /></button>
      </div>}
      {mode === "directs" && <div className="sidebar-actions direct-actions">
        <button className="wide-button" onClick={onNewDirect}><Plus size={15} /> direct message</button>
        <button className="square-button" onClick={onSearch} title="search (⌘K)" aria-label="search"><Search size={16} /></button>
      </div>}
      <div className="group-list">
        {mode === "groups" ? summary.groups.map((group) => {
          const groupConversation = group.is_active
            && conversation?.group.group_id === group.group_id
            ? conversation
            : null;
          const topics = groupConversation?.topics.filter((topic) => !topic.archived) ?? [];
          return (
            <div className="group-entry" key={group.group_id}>
              <button
                className={`group-row ${group.is_active ? "active" : ""} ${pendingGroupId === group.group_id ? "pending" : ""}`}
                onClick={() => onSelect(group)}
                onContextMenu={(event) => {
                  event.preventDefault();
                  onContextMenu(group, event.clientX, event.clientY);
                }}
              >
                <Avatar name={group.name} image={group.avatar} size={27} square />
                <span>{group.name}{group.content_rating === "adult" && <i className="adult-badge">18+</i>}</span>
                {group.unread_count > 0 && (
                  <span
                    className="group-unread-count"
                    aria-label={`${group.unread_count} unread ${group.unread_count === 1 ? "message" : "messages"}`}
                  >
                    {group.unread_count > 99 ? "99+" : group.unread_count}
                  </span>
                )}
              </button>
              {groupConversation && (
                <div className="group-topics">
                  <button
                    className={`topic-row ${activeTopicId === null ? "active" : ""}`}
                    onClick={() => onSelectTopic(null)}
                  >
                    <span className="topic-icon" aria-hidden="true">💬</span>
                    <span>General</span>
                    {groupConversation.general_unread_count > 0 && (
                      <i>{groupConversation.general_unread_count > 99 ? "99+" : groupConversation.general_unread_count}</i>
                    )}
                  </button>
                  {topics.map((topic) => (
                    <div
                      className={`topic-entry ${canReorderTopics ? "reorderable" : ""} ${draggedTopicId === topic.topic_id ? "dragging" : ""} ${dropTarget?.topicId === topic.topic_id ? `drop-target drop-${dropTarget.after ? "after" : "before"}` : ""}`}
                      key={topic.topic_id}
                      data-topic-id={topic.topic_id}
                      data-group-id={group.group_id}
                      onClick={(event) => {
                        if ((event.target as Element).closest(".topic-manage")) return;
                        if (suppressTopicClick.current) {
                          event.preventDefault();
                          return;
                        }
                        onSelectTopic(topic);
                      }}
                      onPointerDown={(event) => {
                        if (
                          !canReorderTopics
                          || event.button !== 0
                          || (event.target as Element).closest(".topic-manage")
                        ) return;
                        topicDrag.current = {
                          pointerId: event.pointerId,
                          topicId: topic.topic_id,
                          startY: event.clientY,
                          moved: false,
                        };
                        event.currentTarget.setPointerCapture(event.pointerId);
                      }}
                      onPointerMove={(event) => {
                        const drag = topicDrag.current;
                        if (!drag || drag.pointerId !== event.pointerId) return;
                        if (!drag.moved && Math.abs(event.clientY - drag.startY) < 4) return;
                        if (!drag.moved) {
                          drag.moved = true;
                          setDraggedTopicId(drag.topicId);
                        }
                        const target = document
                          .elementFromPoint(event.clientX, event.clientY)
                          ?.closest<HTMLElement>(".topic-entry[data-topic-id]");
                        if (
                          !target
                          || target.dataset.groupId !== group.group_id
                          || !target.dataset.topicId
                          || target.dataset.topicId === drag.topicId
                        ) {
                          updateDropTarget(null);
                          return;
                        }
                        const bounds = target.getBoundingClientRect();
                        updateDropTarget({
                          topicId: target.dataset.topicId,
                          after: event.clientY >= bounds.top + bounds.height / 2,
                        });
                      }}
                      onPointerUp={(event) => {
                        const drag = topicDrag.current;
                        if (!drag || drag.pointerId !== event.pointerId) return;
                        topicDrag.current = null;
                        setDraggedTopicId(null);
                        const target = dropTargetRef.current;
                        updateDropTarget(null);
                        if (!drag.moved || !target) return;
                        suppressTopicClick.current = true;
                        window.setTimeout(() => { suppressTopicClick.current = false; }, 0);
                        const topicIds = topics
                          .map((item) => item.topic_id)
                          .filter((topicId) => topicId !== drag.topicId);
                        const targetIndex = topicIds.indexOf(target.topicId);
                        topicIds.splice(targetIndex + (target.after ? 1 : 0), 0, drag.topicId);
                        void onReorderTopics(group, topicIds);
                      }}
                      onPointerCancel={() => {
                        topicDrag.current = null;
                        setDraggedTopicId(null);
                        updateDropTarget(null);
                      }}
                    >
                      <button
                        className={`topic-row ${activeTopicId === topic.topic_id ? "active" : ""} ${pendingTopicId === topic.topic_id ? "pending" : ""}`}
                      >
                        <span className="topic-icon" aria-hidden="true">{topic.icon || "💬"}</span>
                        <span>{topic.name}</span>
                        {topic.locked && <Shield size={11} />}
                        {canReorderTopics && <GripVertical className="topic-grip" size={12} aria-hidden="true" />}
                        {topic.unread_count > 0 && (
                          <i>{topic.unread_count > 99 ? "99+" : topic.unread_count}</i>
                        )}
                      </button>
                      {canManageTopics && (
                        <button
                          className="topic-manage"
                          aria-label={`manage ${topic.name}`}
                          onClick={() => onManageTopic(group, topic)}
                        >
                          <MoreHorizontal size={13} />
                        </button>
                      )}
                    </div>
                  ))}
                  {canManageTopics && (
                    <button className="topic-create" onClick={() => onCreateTopic(group)}>
                      <Plus size={12} /> new topic
                    </button>
                  )}
                </div>
              )}
            </div>
          );
        }) : summary.directs.map((direct) => (
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
      <div className="self-account" ref={accountMenuRef}>
        {accountMenuOpen && (
          <div className="account-switcher" role="menu" aria-label="accounts">
            <div className="account-switcher-list">
              {accounts.map((account) => {
                const active = account.id === activeAccountId;
                return (
                  <button
                    key={account.id}
                    className={active ? "active" : ""}
                    disabled={accountBusy}
                    onClick={() => {
                      if (active) {
                        setAccountMenuOpen(false);
                      } else {
                        onSwitchAccount(account);
                      }
                    }}
                    role="menuitemradio"
                    aria-checked={active}
                  >
                    <Avatar name={account.username} image={account.avatar} size={34} />
                    <span>
                      <strong>{account.username}</strong>
                      <small>{account.bio || "Noise account"}</small>
                    </span>
                    {active && <Check size={15} />}
                  </button>
                );
              })}
            </div>
            <div className="account-switcher-actions">
              <button
                disabled={accountBusy}
                onClick={() => {
                  setAccountMenuOpen(false);
                  onSettings();
                }}
                role="menuitem"
              >
                <Settings2 size={15} /> Settings
              </button>
              <button disabled={accountBusy} onClick={onAddAccount} role="menuitem">
                <UserPlus size={15} /> Add account
              </button>
              <button
                className="sign-out"
                disabled={accountBusy}
                onClick={() => {
                  setAccountMenuOpen(false);
                  onSignOut();
                }}
                role="menuitem"
              >
                <LogOut size={15} /> Sign out {summary.identity.username}
              </button>
            </div>
          </div>
        )}
        <button
          className="self-profile"
          onClick={() => setAccountMenuOpen((open) => !open)}
          aria-haspopup="menu"
          aria-expanded={accountMenuOpen}
        >
          <PresenceAvatar name={summary.identity.username} image={summary.identity.avatar} size={32} status={selfPresenceStatus} />
          <span><strong>{summary.identity.username}</strong><small>{summary.identity.bio || "build your identity"}</small></span>
          <MoreHorizontal size={15} />
        </button>
      </div>
    </aside>
  );
}

function GroupContextMenu({
  x,
  y,
  hasUnread,
  isFounder,
  onClose,
  onMarkRead,
  onDelete,
  onLeave,
}: {
  x: number;
  y: number;
  hasUnread: boolean;
  isFounder: boolean;
  onClose: () => void;
  onMarkRead: () => void;
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
      style={{ left: Math.min(x, window.innerWidth - 190), top: Math.min(y, window.innerHeight - 100) }}
      onMouseDown={(event) => event.stopPropagation()}
    >
      <button disabled={!hasUnread} onClick={onMarkRead}><Check size={14} /> mark group as read</button>
      {isFounder
        ? <button onClick={onDelete}><Trash2 size={14} /> delete group</button>
        : <button onClick={onLeave}><LogOut size={14} /> leave group</button>}
    </div>
  );
}

function DirectContextMenu({ x, y, onClose, onBlock, onDelete }: { x: number; y: number; onClose: () => void; onBlock: () => void; onDelete: () => void }) {
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
  return <div className="group-context-menu" style={{ left: Math.min(x, window.innerWidth - 190), top: Math.min(y, window.innerHeight - 100) }} onMouseDown={(event) => event.stopPropagation()}><button className="danger" onClick={onBlock}><ShieldOff size={14} /> block user</button><button onClick={onDelete}><Trash2 size={14} /> delete conversation</button></div>;
}

function MessageContextMenu({ x, y, busy, onClose, onReact, onReply, onForward, onDownload, onReport, onBlock, onDelete, onBan }: { x: number; y: number; busy: boolean; onClose: () => void; onReact?: () => void; onReply: () => void; onForward: () => void; onDownload?: () => Promise<boolean>; onReport?: () => void; onBlock?: () => void; onDelete?: () => void; onBan?: () => void }) {
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
  const menuHeight = 92 + (onReact ? 42 : 0) + (onDownload ? 42 : 0) + (onReport ? 42 : 0) + (onBlock ? 42 : 0) + (onDelete ? 42 : 0) + (onBan ? 42 : 0);
  return <div className="member-context-menu" style={{ left: Math.min(x, window.innerWidth - 200), top: Math.min(y, window.innerHeight - menuHeight) }} onMouseDown={(event) => event.stopPropagation()}>{onReact && <button disabled={busy || downloading} onClick={onReact}><SmilePlus size={14} /> react</button>}<button disabled={busy || downloading} onClick={onReply}><Reply size={14} /> reply</button><button disabled={busy || downloading} onClick={onForward}><Forward size={14} /> forward</button>{onDownload && <button disabled={busy || downloading || downloaded} onClick={() => { setDownloading(true); void onDownload().then((success) => { setDownloading(false); if (success) { setDownloaded(true); window.setTimeout(onClose, 650); } else { onClose(); } }); }}>{downloaded ? <Check size={14} /> : downloading ? <LoaderCircle className="spinner" size={14} /> : <Download size={14} />}{downloaded ? "downloaded" : downloading ? "downloading" : "download media"}</button>}{onReport && <button className="report-action" disabled={busy || downloading} onClick={onReport}><TriangleAlert size={14} /> report message</button>}{onBlock && <button className="danger" disabled={busy || downloading} onClick={onBlock}><ShieldOff size={14} /> block user</button>}{onDelete && <button className="danger" disabled={busy || downloading} onClick={onDelete}><Trash2 size={14} /> delete message</button>}{onBan && <button className="danger" disabled={busy || downloading} onClick={onBan}><UserRoundX size={14} /> ban member</button>}</div>;
}

function MemberContextMenu({ member, x, y, canDesignate, canBan, onClose, onMessage, onBlock, onSetModerator, onBan }: { member: MemberSummary; x: number; y: number; canDesignate: boolean; canBan: boolean; onClose: () => void; onMessage: () => void; onBlock: () => void; onSetModerator: (enabled: boolean) => void; onBan: () => void }) {
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
    <div className="member-context-menu" style={{ left: Math.min(x - 188, window.innerWidth - 196), top: Math.min(y, window.innerHeight - (90 + (canDesignate ? 42 : 0) + (canBan ? 42 : 0))) }} onMouseDown={(event) => event.stopPropagation()}>
      {member.accepts_direct_messages
        ? <button onClick={onMessage}><MessageCircle size={14} /> message</button>
        : <button disabled><MessageCircle size={14} /> DMs closed</button>}
      {canDesignate && <button onClick={() => onSetModerator(!member.is_moderator)}>{member.is_moderator ? <ShieldOff size={14} /> : <Shield size={14} />}{member.is_moderator ? "remove moderator" : "make moderator"}</button>}
      {canBan && <button className="danger" onClick={onBan}><UserRoundX size={14} /> ban member</button>}
      <button className="danger" onClick={onBlock}><ShieldOff size={14} /> block user</button>
    </div>
  );
}

type SavedMessageScroll =
  | { stuckAtBottom: true }
  | { stuckAtBottom: false; trackedMessageId: string; pixelOffset: number };

function useChunkedMessageList<T extends { event_id: string }>(
  conversationKey: string,
  messages: T[],
  hasRemoteHistory = false,
  onLoadRemoteHistory?: () => Promise<void>,
) {
  const ref = useRef<HTMLDivElement>(null);
  const positionedConversation = useRef<string | null>(null);
  const previousMessageCount = useRef(messages.length);
  const savedScroll = useRef<SavedMessageScroll>({ stuckAtBottom: true });
  const olderSentinel = useRef<HTMLDivElement>(null);
  const loadingOlder = useRef(false);
  const pendingLocalPage = useRef(false);
  // Set once the relays answer without adding anything, so a conversation whose
  // has_older_messages flag stays optimistically true cannot be asked forever.
  // The ref is read synchronously while paging; the state retires the affordance.
  const exhaustedRemoteHistory = useRef(false);
  const [remoteHistoryExhausted, setRemoteHistoryExhausted] = useState(false);
  const [loadingOlderHistory, setLoadingOlderHistory] = useState(false);
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
  const canLoadOlder = hasOlder || (hasRemoteHistory && !remoteHistoryExhausted);

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

  // The async history walk below outlives the render that started it, so read
  // paging inputs through a ref instead of a captured closure.
  const paging = useRef({ hasOlder, hasRemoteHistory, messageCount: messages.length, onLoadRemoteHistory });
  paging.current = { hasOlder, hasRemoteHistory, messageCount: messages.length, onLoadRemoteHistory };

  const loadOlder = useCallback(() => {
    if (loadingOlder.current) return;
    if (paging.current.hasOlder) {
      // Older messages are already in memory, so widening the window is enough.
      if (pendingLocalPage.current) return;
      pendingLocalPage.current = true;
      saveScrollPosition();
      setVisibleCount((current) => {
        const next = Math.min(paging.current.messageCount, current + MESSAGE_PAGE_SIZE);
        renderedMessageCounts.set(conversationKey, next);
        return next;
      });
      return;
    }
    if (!paging.current.hasRemoteHistory || !paging.current.onLoadRemoteHistory) return;
    if (exhaustedRemoteHistory.current) return;
    loadingOlder.current = true;
    setLoadingOlderHistory(true);
    void (async () => {
      const countBeforeLoad = previousMessageCount.current;
      try {
        for (let attempt = 0; attempt < REMOTE_HISTORY_ATTEMPTS; attempt += 1) {
          const fetchOlder = paging.current.onLoadRemoteHistory;
          if (!fetchOlder) break;
          saveScrollPosition();
          await fetchOlder();
          // Let React commit the expanded conversation before judging progress.
          await new Promise<void>((resolve) => { window.setTimeout(resolve, 0); });
          if (previousMessageCount.current !== countBeforeLoad) break;
          if (!paging.current.hasRemoteHistory) break;
        }
        if (previousMessageCount.current === countBeforeLoad) {
          exhaustedRemoteHistory.current = true;
          setRemoteHistoryExhausted(true);
        }
      } finally {
        // Always release the latch. A page that lands in another topic, a relay
        // error, or a cancelled load must not wedge every later scroll.
        loadingOlder.current = false;
        setLoadingOlderHistory(false);
      }
    })();
  }, [conversationKey, saveScrollPosition]);

  useLayoutEffect(() => {
    const element = ref.current;
    if (!element) return;
    if (positionedConversation.current !== conversationKey) {
      element.scrollTop = element.scrollHeight;
      positionedConversation.current = conversationKey;
      savedScroll.current = { stuckAtBottom: true };
      exhaustedRemoteHistory.current = false;
      setRemoteHistoryExhausted(false);
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
    if (previousMessageCount.current !== messages.length) {
      // History moved, so the relays are worth asking again.
      exhaustedRemoteHistory.current = false;
      setRemoteHistoryExhausted(false);
    }
    previousMessageCount.current = messages.length;
    pendingLocalPage.current = false;
    // Keep paging while the reader is parked near the top. Widening the local
    // window is instant and often only adds a handful of rows, so without this
    // the first step stalls there instead of continuing into the relay fetch
    // that actually needs a spinner.
    if (
      canLoadOlder
      && (element.scrollTop <= OLDER_MESSAGE_TRIGGER_DISTANCE
        || element.scrollHeight <= element.clientHeight + 1)
    ) {
      loadOlder();
    }
  });

  // Watching a sentinel above the oldest row keeps paging working even when the
  // container swallows scroll events or the new batch lands off screen.
  useEffect(() => {
    const element = ref.current;
    const sentinel = olderSentinel.current;
    if (!element || !sentinel || !canLoadOlder) return;
    const observer = new IntersectionObserver(
      (entries) => {
        if (entries.some((entry) => entry.isIntersecting)) loadOlder();
      },
      { root: element, rootMargin: `${OLDER_MESSAGE_TRIGGER_DISTANCE}px 0px 0px 0px` },
    );
    observer.observe(sentinel);
    return () => observer.disconnect();
  }, [canLoadOlder, conversationKey, loadOlder, messages.length]);

  const onScroll = useCallback(() => {
    const element = ref.current;
    if (!element) return;
    saveScrollPosition();
    if (element.scrollTop <= OLDER_MESSAGE_TRIGGER_DISTANCE) loadOlder();
  }, [loadOlder, saveScrollPosition]);

  const revealMessage = useCallback((messageId: string) => {
    const index = messages.findIndex((item) => item.event_id === messageId);
    if (index < 0) return false;
    const requiredCount = messages.length - index;
    setVisibleCount((current) => {
      const next = Math.max(current, requiredCount);
      renderedMessageCounts.set(conversationKey, next);
      return next;
    });
    return true;
  }, [conversationKey, messages]);

  return {
    ref,
    olderSentinel,
    onScroll,
    visibleMessages,
    renderedCount,
    atBottom,
    canLoadOlder,
    loadingOlder: loadingOlderHistory,
    revealMessage,
  };
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
  topic,
  loadingTopic,
  active,
  busy,
  hasBackground,
  canEditGroup,
  unreadCount,
  messageJump,
  selfPublicKey,
  presenceStatuses,
  onGroupSettings,
  onTopicSettings,
  onReports,
  onMedia,
  onRules,
  onPerson,
  onMessage,
  onBlock,
  onDeleteMessage,
  onDownload,
  onForward,
  onReaction,
  onSetModerator,
  onBan,
  onReport,
  onReachedBottom,
  onLoadOlder,
  onSend,
}: {
  conversation: Conversation;
  topic: TopicSummary | null;
  loadingTopic: boolean;
  active: boolean;
  busy: boolean;
  hasBackground: boolean;
  canEditGroup: boolean;
  unreadCount: number;
  messageJump: { eventId: string; nonce: number } | null;
  selfPublicKey: string;
  presenceStatuses: Map<string, PresenceStatus>;
  onGroupSettings: () => void;
  onTopicSettings?: () => void;
  onReports: () => void;
  onMedia: () => void;
  onRules: () => void;
  onPerson: (person: PersonSummary) => void;
  onMessage: (person: PersonSummary) => void;
  onBlock: (person: PersonSummary) => void;
  onDeleteMessage: (message: MessageSummary) => void;
  onDownload: (message: MessageSummary) => Promise<boolean>;
  onForward: (message: MessageSummary) => void;
  onReaction: (message: MessageSummary, emoji: string) => Promise<void>;
  onSetModerator: (member: MemberSummary, enabled: boolean) => Promise<boolean>;
  onBan: (member: MemberSummary) => void;
  onReport: (message: MessageSummary) => void;
  onReachedBottom: () => void;
  onLoadOlder: () => Promise<void>;
  onSend: (
    text: string,
    attachment: PendingMedia | null,
    replyToMessageId: string | null,
  ) => Promise<boolean>;
}) {
  const [draft, setDraft] = useState("");
  const [attachmentError, setAttachmentError] = useState<string | null>(null);
  const composerUploadKey = `group:${conversation.group.group_id}:${topic?.topic_id ?? "general"}`;
  const {
    attachment,
    setAttachment,
    takeAttachment,
  } = useComposerUpload(composerUploadKey);
  const [memberMenu, setMemberMenu] = useState<{ member: MemberSummary; x: number; y: number } | null>(null);
  const [messageMenu, setMessageMenu] = useState<{ message: MessageSummary; x: number; y: number } | null>(null);
  const [reactionPicker, setReactionPicker] = useState<{ message: MessageSummary; x: number; y: number } | null>(null);
  const [replyingTo, setReplyingTo] = useState<MessageSummary | null>(null);
  const fileInput = useRef<HTMLInputElement>(null);
  const composerInput = useRef<HTMLTextAreaElement>(null);
  useAutosizeComposer(composerInput, draft);
  const messageList = useChunkedMessageList(
    `${conversation.group.group_id}:${topic?.topic_id ?? "general"}`,
    conversation.messages,
    conversation.has_older_messages,
    onLoadOlder,
  );
  useEffect(() => {
    if (!messageJump || !messageList.revealMessage(messageJump.eventId)) return;
    const timer = window.setTimeout(() => {
      const row = messageList.ref.current?.querySelector<HTMLElement>(
        `[data-message-id="${CSS.escape(messageJump.eventId)}"]`,
      );
      if (!row) return;
      row.scrollIntoView({ block: "center", behavior: "smooth" });
      row.classList.remove("search-highlight");
      void row.offsetWidth;
      row.classList.add("search-highlight");
      window.setTimeout(() => row.classList.remove("search-highlight"), 1900);
    }, 0);
    return () => window.clearTimeout(timer);
  }, [messageJump?.eventId, messageJump?.nonce]);
  useEffect(() => {
    if (messageList.atBottom && unreadCount > 0) onReachedBottom();
  }, [conversation.messages.length, messageList.atBottom, onReachedBottom, unreadCount]);
  const selfMember = conversation.members.find((member) => member.public_key === selfPublicKey);
  const canModerate = canEditGroup || selfMember?.is_moderator === true;
  const topicAllowsPosting = !topic?.locked || canModerate;
  const canSendMessages = topicAllowsPosting
    && (canModerate || conversation.group.members_can_send_messages);
  const canSendMedia = topicAllowsPosting
    && (canModerate || conversation.group.members_can_send_media);
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
  const reactionPeople = new Map<string, PersonSummary>();
  for (const member of conversation.members) {
    reactionPeople.set(member.public_key, {
      ...member,
      presence_status: presenceStatuses.get(member.public_key) ?? "offline",
    });
  }
  for (const item of conversation.messages) {
    if (reactionPeople.has(item.author_public_key)) continue;
    reactionPeople.set(item.author_public_key, {
      public_key: item.author_public_key,
      username: item.username,
      bio: item.bio,
      avatar: item.avatar,
      album: item.album,
      accepts_direct_messages: item.accepts_direct_messages,
      direct_message_policy: item.direct_message_policy,
      presence_status: presenceStatuses.get(item.author_public_key) ?? "offline",
    });
  }
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
  const chooseMedia = useCallback((file?: File) => {
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
    setAttachment(preparePendingMedia(file));
    if (fileInput.current) fileInput.current.value = "";
  }, [setAttachment]);
  const mediaDragging = useComposerMediaIntake(
    active,
    canSendMedia && !busy,
    chooseMedia,
    (path) => {
      setAttachmentError(null);
      void pendingMediaFromNativePath(path)
        .then((pending) => {
          if (pending) setAttachment(pending);
        })
        .catch((cause) => setAttachmentError(message(cause)));
    },
  );
  async function chooseMediaFromDevice() {
    if (!isTauri) {
      fileInput.current?.click();
      return;
    }
    setAttachmentError(null);
    try {
      const pending = await chooseNativePendingMedia();
      if (pending) setAttachment(pending);
    } catch (cause) {
      setAttachmentError(message(cause));
    }
  }
  async function submit() {
    const text = draft.trim();
    if ((!text && !attachment)
      || busy
      || (text && !canSendMessages)
      || (attachment && !canSendMedia)) return;
    const submittedReply = replyingTo;
    const pendingAttachment = attachment ? takeAttachment() : null;
    setDraft("");
    setReplyingTo(null);
    void onSend(text, pendingAttachment, submittedReply?.message_id ?? null);
  }
  return (
    <div className={`conversation group-conversation ${hasBackground ? "has-background" : ""}`}>
      {mediaDragging && <div className="media-drop-overlay" aria-hidden="true"><Images size={34} /><strong>drop media to attach</strong><span>images, video, or audio</span></div>}
      <header className="chat-header" data-tauri-drag-region>
        <div className="group-identity static" data-tauri-drag-region>
          <Avatar name={conversation.group.name} image={conversation.group.avatar} size={36} square />
          <span>
            <strong>{conversation.group.name}{conversation.group.content_rating === "adult" && <i className="adult-badge">18+</i>}{topic ? ` / ${topic.name}` : ""}</strong>
            <small>{topic?.locked ? "locked topic" : conversation.group.description || "group"}</small>
          </span>
        </div>
        <div className="chat-header-actions">
          {canModerate && <button className={`icon-button media-button reports-button ${conversation.reports.length ? "has-reports" : ""}`} onClick={onReports} aria-label="moderation reports" title="moderation reports"><TriangleAlert size={17} />{conversation.reports.length > 0 && <i />}</button>}
          {canEditGroup && <button className="icon-button media-button" onClick={onGroupSettings} aria-label="group settings" title="group settings"><Settings2 size={17} /></button>}
          {canModerate && topic && onTopicSettings && <button className="icon-button media-button" onClick={onTopicSettings} aria-label="topic settings" title="topic settings"><MessageCircle size={17} /></button>}
          <button className="icon-button media-button" onClick={onMedia} aria-label="group media" title="group media"><Images size={17} /></button>
          <button className="rules-button" onClick={onRules}>Rules</button>
          {busy && <LoaderCircle className="spinner" size={14} />}
        </div>
      </header>
      <div className="messages" ref={messageList.ref} onScroll={messageList.onScroll}>
        {conversation.messages.length === 0 && (
          loadingTopic
            ? <MediaLoadStatus prominent />
            : <div className="quiet">{topic ? "this topic is quiet" : "the group is quiet"}</div>
        )}
        {messageList.canLoadOlder && (
          <OlderMessagesSentinel
            loading={messageList.loadingOlder}
            sentinel={messageList.olderSentinel}
          />
        )}
        {messageList.visibleMessages.map((item) => (
          <MessageRow key={item.event_id} message={item} own={item.author_public_key === selfPublicKey} presence={presenceStatuses.get(item.author_public_key) ?? "offline"} replyTo={conversation.messages.find((candidate) => candidate.message_id === item.reply_to_message_id)} onContextMenu={item.optimistic ? undefined : (event) => { event.preventDefault(); setMessageMenu({ message: item, x: event.clientX, y: event.clientY }); }} onToggleReaction={(emoji) => void onReaction(item, emoji)} reactionPeople={reactionPeople} onPerson={onPerson} mediaScopeId={conversation.group.group_id} />
        ))}
      </div>
      {selfMember && (canSendMessages || canSendMedia) ? <div className="composer">
        {replyingTo && <ReplyTarget message={replyingTo} mediaScopeId={conversation.group.group_id} onClose={() => setReplyingTo(null)} />}
        {attachment && <div className={`attachment-draft ${attachment.mimeType.startsWith("audio/") ? "audio" : ""}`}>{attachment.mimeType.startsWith("image/") ? <img src={attachment.previewUrl} alt="" /> : attachment.mimeType.startsWith("video/") ? <video src={attachment.previewUrl} muted playsInline preload="metadata" onLoadedMetadata={(event) => { const video = event.currentTarget; if (Number.isFinite(video.duration) && video.duration > 0) video.currentTime = Math.min(0.25, video.duration / 2); }} /> : <div className="audio-thumbnail"><AudioWaveform size={30} /></div>}<button onClick={() => setAttachment(null)} aria-label="remove attachment"><X size={14} /></button></div>}
        {attachmentError && <div className="attachment-error">{attachmentError}</div>}
        <button className="attach-button" disabled={busy || !canSendMedia} onClick={() => void chooseMediaFromDevice()} aria-label="attach media" title={canSendMedia ? "attach media" : "members cannot send media"}><Paperclip size={17} /></button>
        <input ref={fileInput} hidden type="file" accept="image/*,video/*,audio/*" onChange={(event) => void chooseMedia(event.target.files?.[0])} />
        <textarea
          ref={composerInput}
          rows={1}
          value={draft}
          disabled={!canSendMessages}
          placeholder={canSendMessages ? `send to ${topic?.name ?? "General"}` : topic?.locked ? "this topic is locked" : "members cannot send messages"}
          onChange={(event) => setDraft(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Enter" && !event.shiftKey) {
              event.preventDefault();
              void submit();
            }
          }}
        />
        <button className="send-button" disabled={(!draft.trim() && !attachment) || busy || (!!draft.trim() && !canSendMessages) || (!!attachment && !canSendMedia)} onClick={() => void submit()}><ArrowUp size={17} /></button>
      </div> : selfMember ? <div className="membership-revoked"><ShieldOff size={16} /> {topic?.locked ? "this topic is locked" : "only moderators can post right now"}</div> : <div className="membership-revoked"><UserRoundX size={16} /> you no longer have access to this group</div>}
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
        onBlock={() => { onBlock(memberMenu.member); setMemberMenu(null); }}
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
        onForward={() => { onForward(messageMenu.message); setMessageMenu(null); }}
        onDownload={messageMenu.message.attachment ? () => onDownload(messageMenu.message) : undefined}
        onReport={!canModerate && messageMenu.message.author_public_key !== selfPublicKey && !conversation.reported_message_event_ids.includes(messageMenu.message.event_id) ? () => { onReport(messageMenu.message); setMessageMenu(null); } : undefined}
        onBlock={messageMenu.message.author_public_key !== selfPublicKey ? () => {
          onBlock({
            public_key: messageMenu.message.author_public_key,
            username: messageMenu.message.username,
            bio: messageMenu.message.bio,
            avatar: messageMenu.message.avatar,
            album: messageMenu.message.album,
            accepts_direct_messages: messageMenu.message.accepts_direct_messages,
            direct_message_policy: messageMenu.message.direct_message_policy,
            presence_status: presenceStatuses.get(messageMenu.message.author_public_key) ?? "offline",
          });
          setMessageMenu(null);
        } : undefined}
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

function DirectConversationPanel({ conversation, contact, active, busy, self, selfPresence, contactPresence, messageJump, onPerson, onAlbum, onBlock, onDelete, onDownload, onForward, onSend }: { conversation: DirectConversation; contact: DirectSummary; active: boolean; busy: boolean; self: IdentitySummary; selfPresence: PresenceStatus; contactPresence: PresenceStatus; messageJump: { eventId: string; nonce: number } | null; onPerson: (person: PersonSummary) => void; onAlbum: (person: PersonSummary) => void; onBlock: (person: PersonSummary) => void; onDelete: () => void; onDownload: (message: MessageSummary) => Promise<boolean>; onForward: (message: MessageSummary) => void; onSend: (text: string, attachment: PendingMedia | null, onProgress: (progress: number) => void, replyToMessageId: string | null, signal: AbortSignal) => Promise<boolean> }) {
  const [draft, setDraft] = useState("");
  const [attachmentError, setAttachmentError] = useState<string | null>(null);
  const composerUploadKey = `direct:${contact.public_key}`;
  const {
    attachment,
    progress: uploadProgress,
    controller: uploadController,
    setAttachment,
    setProgress: setUploadProgress,
    setController: setUploadController,
  } = useComposerUpload(composerUploadKey);
  const [messageMenu, setMessageMenu] = useState<{ message: MessageSummary; x: number; y: number } | null>(null);
  const [replyingTo, setReplyingTo] = useState<MessageSummary | null>(null);
  const fileInput = useRef<HTMLInputElement>(null);
  const composerInput = useRef<HTMLTextAreaElement>(null);
  useAutosizeComposer(composerInput, draft);
  const messageList = useChunkedMessageList(
    contact.public_key,
    conversation.messages,
  );
  useEffect(() => {
    if (!messageJump || !messageList.revealMessage(messageJump.eventId)) return;
    const timer = window.setTimeout(() => {
      const row = messageList.ref.current?.querySelector<HTMLElement>(
        `[data-message-id="${CSS.escape(messageJump.eventId)}"]`,
      );
      if (!row) return;
      row.scrollIntoView({ block: "center", behavior: "smooth" });
      row.classList.remove("search-highlight");
      void row.offsetWidth;
      row.classList.add("search-highlight");
      window.setTimeout(() => row.classList.remove("search-highlight"), 1900);
    }, 0);
    return () => window.clearTimeout(timer);
  }, [messageJump?.eventId, messageJump?.nonce]);
  const chooseMedia = useCallback((file?: File) => {
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
    setAttachment(preparePendingMedia(file));
    if (fileInput.current) fileInput.current.value = "";
  }, [setAttachment]);
  const mediaDragging = useComposerMediaIntake(
    active,
    contact.accepts_direct_messages && !busy && uploadProgress === null,
    chooseMedia,
    (path) => {
      setAttachmentError(null);
      void pendingMediaFromNativePath(path)
        .then((pending) => {
          if (pending) setAttachment(pending);
        })
        .catch((cause) => setAttachmentError(message(cause)));
    },
  );
  async function chooseMediaFromDevice() {
    if (!isTauri) {
      fileInput.current?.click();
      return;
    }
    setAttachmentError(null);
    try {
      const pending = await chooseNativePendingMedia();
      if (pending) setAttachment(pending);
    } catch (cause) {
      setAttachmentError(message(cause));
    }
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
    setUploadController(controller);
    const sent = await onSend(text, pendingAttachment, setUploadProgress, submittedReply?.message_id ?? null, controller.signal);
    const ownsUploadState = composerUpload(composerUploadKey).controller === controller;
    if (ownsUploadState) {
      setUploadController(null);
      setUploadProgress(null);
    }
    if (sent && ownsUploadState) {
      setAttachment(null);
    } else if (!sent && ownsUploadState) {
      setDraft((current) => current || submittedDraft);
      setReplyingTo((current) => current ?? submittedReply);
    }
  }
  const person = { public_key: contact.public_key, username: contact.username, bio: contact.bio, avatar: contact.avatar, album: contact.album, accepts_direct_messages: contact.accepts_direct_messages, direct_message_policy: contact.direct_message_policy, presence_status: contactPresence };
  return (
    <div className="conversation direct-conversation">
      {mediaDragging && <div className="media-drop-overlay" aria-hidden="true"><Images size={34} /><strong>drop media to attach</strong><span>images, video, or audio</span></div>}
      <header className="chat-header" data-tauri-drag-region>
        <div className="group-identity static" data-tauri-drag-region>
          <PresenceAvatar name={contact.username} image={contact.avatar} size={36} status={contactPresence} />
          <span><strong>{contact.username}</strong><small>{contact.bio || "encrypted direct message"}</small></span>
        </div>
        <div className="chat-header-actions"><button className="icon-button media-button delete-direct-button" onClick={onDelete} aria-label="delete conversation" title="delete conversation"><Trash2 size={16} /></button>{busy && <LoaderCircle className="spinner" size={14} />}</div>
      </header>
      <div className="messages" ref={messageList.ref} onScroll={messageList.onScroll}>
        {conversation.messages.length === 0 && <div className="quiet">start the conversation</div>}
        {messageList.canLoadOlder && (
          <OlderMessagesSentinel
            loading={messageList.loadingOlder}
            sentinel={messageList.olderSentinel}
          />
        )}
        {messageList.visibleMessages.map((rawItem) => {
          const item = withCurrentDirectProfile(rawItem, self, contact);
          const rawReply = conversation.messages.find(
            (candidate) => candidate.message_id === item.reply_to_message_id,
          );
          const replyTo = rawReply ? withCurrentDirectProfile(rawReply, self, contact) : undefined;
          return <MessageRow key={item.event_id} message={item} own={item.author_public_key === self.public_key} presence={item.author_public_key === self.public_key ? selfPresence : contactPresence} replyTo={replyTo} onContextMenu={item.optimistic ? undefined : (event) => { event.preventDefault(); setMessageMenu({ message: item, x: event.clientX, y: event.clientY }); }} onPerson={onPerson} mediaScopeId={conversation.media_scope_id} />;
        })}
      </div>
      {contact.accepts_direct_messages ? <div className="composer">
        {replyingTo && <ReplyTarget message={replyingTo} mediaScopeId={conversation.media_scope_id} onClose={() => setReplyingTo(null)} />}
        {attachment && <div className={`attachment-draft ${attachment.mimeType.startsWith("audio/") ? "audio" : ""}`}>{attachment.mimeType.startsWith("image/") ? <img src={attachment.previewUrl} alt="" /> : attachment.mimeType.startsWith("video/") ? <video src={attachment.previewUrl} muted playsInline preload="metadata" onLoadedMetadata={(event) => primeVideoFrame(event.currentTarget)} /> : <div className="audio-thumbnail"><AudioWaveform size={30} /></div>}{uploadProgress !== null && <div className="attachment-progress"><i style={{ width: `${uploadProgress}%` }} /><span>{uploadProgress === 0 && attachment.mimeType.startsWith("video/") ? "preparing video" : `${uploadProgress}%`}</span></div>}<button onClick={() => { uploadController?.abort(); setUploadController(null); setAttachment(null); setUploadProgress(null); }} aria-label={uploadProgress !== null ? "cancel upload" : "remove attachment"}><X size={14} /></button></div>}
        {attachmentError && <div className="attachment-error">{attachmentError}</div>}
        <button className="attach-button" disabled={busy} onClick={() => void chooseMediaFromDevice()} aria-label="attach media"><Paperclip size={17} /></button>
        <input ref={fileInput} hidden type="file" accept="image/*,video/*,audio/*" onChange={(event) => void chooseMedia(event.target.files?.[0])} />
        <textarea ref={composerInput} rows={1} value={draft} placeholder={`message ${contact.username}`} onChange={(event) => setDraft(event.target.value)} onKeyDown={(event) => { if (event.key === "Enter" && !event.shiftKey) { event.preventDefault(); void submit(); } }} />
        <button className="send-button" disabled={(!draft.trim() && !attachment) || busy} onClick={() => void submit()}><ArrowUp size={17} /></button>
      </div> : <div className="membership-revoked"><MessageCircle size={16} /> {contact.username} isn’t accepting DMs</div>}
      <aside className="member-sidebar direct-profile-sidebar">
        <button className="direct-profile-identity" onClick={() => onPerson(person)}>
          <PresenceAvatar name={contact.username} image={contact.avatar} size={72} status={contactPresence} />
          <strong>{contact.username}</strong>
        </button>
        <div className="noise-signature"><small>noise signature</small><strong>{noiseSignature(contact.public_key)}</strong></div>
        <p>{contact.bio || "no bio yet"}</p>
        <div className="direct-profile-actions">
          <button className="profile-album" onClick={() => onAlbum(person)}><Images size={14} /> {albumButtonLabel(contact.album)}</button>
          <button className="profile-block" onClick={() => onBlock(person)}><ShieldOff size={14} /> block</button>
        </div>
        <span className={`direct-profile-status ${contact.direct_message_policy !== "nobody" ? "open" : "closed"}`}><i />{contact.direct_message_policy === "everyone" ? "accepting DMs" : contact.direct_message_policy === "shared_groups" ? "shared groups only" : "DMs closed"}</span>
      </aside>
      <AppVersionFooter />
      {messageMenu && <MessageContextMenu x={messageMenu.x} y={messageMenu.y} busy={busy} onClose={() => setMessageMenu(null)} onReply={() => { setReplyingTo(messageMenu.message); setMessageMenu(null); window.setTimeout(() => composerInput.current?.focus(), 0); }} onForward={() => { onForward(messageMenu.message); setMessageMenu(null); }} onDownload={messageMenu.message.attachment ? () => onDownload(messageMenu.message) : undefined} />}
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
      <div className="member-sidebar-footer-details">
        <CopyButton
          value="8402 6053 0554"
          label="Official: 8402 6053 0554"
          className="official-frequency"
        />
        {isTauri
          ? <span>{version ? `Beta V.${version}` : "Beta"}</span>
          : <a href="https://makenoise.chat/#download" target="_blank" rel="noreferrer"><Download size={12} />{desktopDownloadLabel}</a>}
      </div>
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
        <section className="about-source">
          <strong>open source</strong>
          <p>noise is licensed under AGPL-3.0-only. <a href="https://github.com/GnosysLabs/noise" target="_blank" rel="noreferrer">Read or download the source code.</a></p>
        </section>
      </div>
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
  reactionPeople,
  onPerson,
  mediaScopeId,
}: {
  message: MessageSummary;
  own: boolean;
  presence?: PresenceStatus;
  replyTo?: MessageSummary;
  onContextMenu?: (event: React.MouseEvent<HTMLElement>) => void;
  onToggleReaction?: (emoji: string) => void;
  reactionPeople?: Map<string, PersonSummary>;
  onPerson: (person: PersonSummary) => void;
  mediaScopeId?: string;
}) {
  const person = { public_key: message.author_public_key, username: message.username, bio: message.bio, avatar: message.avatar, album: message.album, accepts_direct_messages: message.accepts_direct_messages, direct_message_policy: message.direct_message_policy, presence_status: presence };
  const forwardedPerson = message.forwarded_from
    ? {
        public_key: message.forwarded_from.public_key,
        username: message.forwarded_from.username,
        bio: "",
        avatar: null,
        album: null,
        accepts_direct_messages: false,
        direct_message_policy: "nobody" as const,
      }
    : null;
  const localAttachment = message.local_attachment ?? sentMediaPreviewCache.get(message.event_id);
  const jumboEmojiCount = !localAttachment && !message.attachment
    ? emojiOnlyCount(message.text)
    : null;
  const previewUrl = firstLink(message.text);
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
      <div className="message-body">
        <div className="message-meta"><button onClick={() => onPerson(person)}>{message.username}</button></div>
        {forwardedPerson && <div className="message-forwarded"><Forward size={13} /><span>Forwarded from</span><button onClick={() => onPerson(forwardedPerson)}>{forwardedPerson.username}</button></div>}
        {message.reply_to_message_id && <div className="message-reply-reference">{replyTo ? <>{replyTo.attachment && <ReplyMediaThumbnail message={replyTo as MessageSummary & { attachment: MediaAttachment }} scopeId={mediaScopeId} />}<span className="message-reply-copy"><strong>{replyTo.username}</strong><span>{replyPreview(replyTo)}</span></span></> : <span>original message unavailable</span>}</div>}
        {message.text && <p className={jumboEmojiCount ? `emoji-only emoji-only-${jumboEmojiCount}` : undefined}>{linkify(message.text)}</p>}
        {previewUrl && <LinkPreviewCard url={previewUrl} />}
        {localAttachment
          ? <LocalMessageMedia attachment={localAttachment} manifest={message.attachment} scopeId={mediaScopeId} uploadProgress={message.upload_progress} uploadError={message.upload_error} />
          : message.attachment && <MessageMedia attachment={message.attachment} scopeId={mediaScopeId} />}
        <time className="message-time">{formatTime(message.created_at_millis)}</time>
        {message.reactions && message.reactions.length > 0 && <MessageReactions reactions={message.reactions} people={reactionPeople} onToggle={onToggleReaction} onPerson={onPerson} />}
      </div>
    </article>
  );
}

function LinkPreviewCard({ url }: { url: string }) {
  const preview = useLinkPreview(url);
  if (!preview) return null;
  return (
    <a
      className={`link-preview ${preview.image_data_url ? "with-image" : ""}`}
      href={preview.url}
      onClick={(event) => openExternalLink(event, preview.url)}
      rel="noopener noreferrer"
      target="_blank"
    >
      {preview.image_data_url && (
        <img alt="" loading="lazy" src={preview.image_data_url} />
      )}
      <span className="link-preview-copy">
        <small>{preview.site_name || previewHost(preview)}</small>
        <strong>{preview.title}</strong>
        {preview.description && <span>{preview.description}</span>}
      </span>
    </a>
  );
}

function previewHost(preview: LinkPreview) {
  try {
    return new URL(preview.url).hostname.replace(/^www\./, "");
  } catch {
    return "";
  }
}

function MessageReactions({
  reactions,
  people,
  onToggle,
  onPerson,
}: {
  reactions: ReactionSummary[];
  people?: Map<string, PersonSummary>;
  onToggle?: (emoji: string) => void;
  onPerson: (person: PersonSummary) => void;
}) {
  return (
    <div className="message-reactions">
      {reactions.map((reaction) => (
        <ReactionChip
          key={reaction.emoji}
          reaction={reaction}
          people={people}
          onToggle={onToggle}
          onPerson={onPerson}
        />
      ))}
    </div>
  );
}

function ReactionChip({
  reaction,
  people,
  onToggle,
  onPerson,
}: {
  reaction: ReactionSummary;
  people?: Map<string, PersonSummary>;
  onToggle?: (emoji: string) => void;
  onPerson: (person: PersonSummary) => void;
}) {
  const trigger = useRef<HTMLButtonElement>(null);
  const openTimer = useRef<number | null>(null);
  const closeTimer = useRef<number | null>(null);
  const [position, setPosition] = useState<{ left: number; top?: number; bottom?: number } | null>(null);
  const reactors: PersonSummary[] = reaction.reactor_public_keys.map((publicKey) =>
    people?.get(publicKey) ?? {
      public_key: publicKey,
      username: "noise user",
      bio: "",
      avatar: null,
      album: null,
      accepts_direct_messages: false,
      direct_message_policy: "nobody" as const,
    }
  );
  const clearTimer = (timer: React.MutableRefObject<number | null>) => {
    if (timer.current !== null) window.clearTimeout(timer.current);
    timer.current = null;
  };
  const open = () => {
    clearTimer(closeTimer);
    if (position || openTimer.current !== null) return;
    openTimer.current = window.setTimeout(() => {
      openTimer.current = null;
      const rect = trigger.current?.getBoundingClientRect();
      if (!rect) return;
      const estimatedHeight = Math.min(260, 10 + reactors.length * 40);
      const left = Math.max(
        12,
        Math.min(rect.left + rect.width / 2 - 100, window.innerWidth - 212),
      );
      setPosition(rect.bottom + 8 + estimatedHeight <= window.innerHeight
        ? { left, top: rect.bottom + 8 }
        : { left, bottom: window.innerHeight - rect.top + 8 });
    }, 240);
  };
  const close = () => {
    clearTimer(openTimer);
    clearTimer(closeTimer);
    closeTimer.current = window.setTimeout(() => {
      closeTimer.current = null;
      setPosition(null);
    }, 120);
  };
  useEffect(() => () => {
    clearTimer(openTimer);
    clearTimer(closeTimer);
  }, []);
  return (
    <span className="reaction-chip-wrap" onMouseEnter={open} onMouseLeave={close}>
      <button
        ref={trigger}
        type="button"
        className={reaction.reacted_by_self ? "mine" : undefined}
        disabled={!onToggle}
        aria-haspopup="dialog"
        aria-expanded={Boolean(position)}
        aria-label={reaction.reacted_by_self ? `remove ${reaction.emoji} reaction` : `react ${reaction.emoji}`}
        onFocus={open}
        onBlur={close}
        onClick={() => onToggle?.(reaction.emoji)}
      >
        <span>{reaction.emoji}</span>
        <small>{reaction.count}</small>
      </button>
      {position && createPortal(
        <div
          className="reaction-users-popover"
          role="dialog"
          aria-label={`People who reacted ${reaction.emoji}`}
          style={position}
          onMouseEnter={() => clearTimer(closeTimer)}
          onMouseLeave={close}
        >
          <div className="reaction-users-list">
            {reactors.map((person) => (
              <button
                key={person.public_key}
                type="button"
                onClick={(event) => {
                  event.stopPropagation();
                  setPosition(null);
                  onPerson(person);
                }}
              >
                <PresenceAvatar name={person.username} image={person.avatar} size={30} status={person.presence_status ?? "offline"} />
                <span className="reaction-user-copy"><strong>{person.username}</strong>{person.username === "noise user" && <small>{noiseSignature(person.public_key)}</small>}</span>
              </button>
            ))}
          </div>
        </div>,
        document.body,
      )}
    </span>
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
  const image = attachment.mime_type.startsWith("image/");
  const video = attachment.mime_type.startsWith("video/");
  const { source, failed } = useMediaSource(
    attachment,
    scopeId,
    !video && !localAttachment && !embeddedPreview ? "background" : null,
  );
  const posterCacheKey = mediaCacheKey(attachment);
  const poster = embeddedPreview ?? videoPosterCache.get(posterCacheKey);
  if (image) {
    const imageSource = localAttachment?.preview_url ?? poster ?? source;
    const loading = !localAttachment && !embeddedPreview && !source;
    return <span className="reply-media-thumbnail">{imageSource && <img src={imageSource} alt="" />}{loading && <MediaLoadStatus failed={failed} compact />}</span>;
  }
  if (video) {
    return <span className="reply-media-thumbnail video">{poster ? <img src={poster} alt="" /> : <span className="reply-video-placeholder"><NoiseMark size={16} monochrome /></span>}<i><Play size={9} fill="currentColor" /></i></span>;
  }
  return <span className="reply-media-thumbnail audio"><AudioWaveform size={18} /></span>;
}

function MessageMedia({ attachment, scopeId, autoplayVideo = false }: { attachment: MediaAttachment; scopeId?: string; autoplayVideo?: boolean }) {
  const visibility = useMediaPriority<HTMLDivElement>();
  const image = attachment.mime_type.startsWith("image/");
  const video = attachment.mime_type.startsWith("video/");
  const [videoRequested, setVideoRequested] = useState(autoplayVideo);
  useEffect(() => {
    if (autoplayVideo) setVideoRequested(true);
  }, [autoplayVideo]);
  useEffect(() => {
    if (
      !video
      || videoRequested
      || !visibility.priority
      || visibility.priority === "background"
    ) return;
    void prewarmMediaBootstrap(attachment, scopeId, visibility.priority);
  }, [attachment, scopeId, video, videoRequested, visibility.priority]);
  const { source, failed, retry: retrySource } = useMediaSource(
    attachment,
    scopeId,
    video
      ? videoRequested
        ? "visible"
        : isTauri && visibility.priority === "visible"
          ? "visible"
          : null
      : visibility.priority,
  );
  const poster = mediaPoster(attachment);
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
          autoPlay={videoRequested}
          playbackRequested={videoRequested}
          onRequestPlayback={() => setVideoRequested(true)}
          onSourceError={retrySource}
          failed={failed}
        />
      ) : source ? (
        <audio src={source} controls preload="metadata" />
      ) : (
        <div className="media-loading"><MediaLoadStatus failed={failed} /></div>
      )}
    </div>
  );
}

function prewarmMediaBootstrap(
  attachment: MediaAttachment,
  scopeId: string | undefined,
  priority: MediaLoadPriority,
) {
  if (attachment.chunks[0]?.storage?.v !== 2) return Promise.resolve();
  const key = `bootstrap:${mediaCacheKey(attachment)}`;
  const pending = mediaBootstrapPromises.get(key);
  if (pending) {
    mediaBootstrapScheduler.promote(key, priority);
    return pending;
  }
  const request = mediaBootstrapScheduler
    .enqueue(key, priority, async () => {
      const startupByteLength = Math.min(1024 * 1024, attachment.byte_length);
      await noise<AttachmentRangeData>({
        action: "fetch_attachment_range",
        attachment,
        scope_id: scopeId,
        offset: 0,
        byte_length: startupByteLength,
        relays,
      });
      return "";
    })
    .then(() => undefined)
    .finally(() => {
      if (mediaBootstrapPromises.get(key) === request) {
        mediaBootstrapPromises.delete(key);
      }
    });
  mediaBootstrapPromises.set(key, request);
  return request;
}

function LocalMessageMedia({
  attachment,
  manifest,
  scopeId,
  uploadProgress,
  uploadError,
}: {
  attachment: NonNullable<MessageSummary["local_attachment"]>;
  manifest: MediaAttachment | null;
  scopeId?: string;
  uploadProgress?: number;
  uploadError?: string;
}) {
  const poster = attachment.poster_url ?? (manifest ? mediaPoster(manifest) : undefined);
  const posterCacheKey = manifest ? mediaCacheKey(manifest) : undefined;
  const video = attachment.mime_type.startsWith("video/");
  const pendingVideo = video && (uploadProgress !== undefined || Boolean(uploadError));
  const pixelWidth = attachment.pixel_width ?? manifest?.pixel_width;
  const pixelHeight = attachment.pixel_height ?? manifest?.pixel_height;
  return (
    <div className="message-media local-message-media">
      <div className="local-message-media-frame">
        {attachment.mime_type.startsWith("image/") ? (
          <ChatImage source={attachment.preview_url} preview={poster ?? attachment.preview_url} cacheKey={posterCacheKey ?? attachment.preview_url} pixelWidth={pixelWidth} pixelHeight={pixelHeight} />
        ) : pendingVideo ? (
          <div className="chat-video media-pending" style={mediaFrameStyle(pixelWidth, pixelHeight, 288, 176)}>
            {poster ? (
              <img className="chat-video-poster-cover" src={poster} alt="" aria-hidden="true" />
            ) : (
              <span className="chat-video-placeholder" aria-hidden="true">
                <NoiseMark size={34} monochrome />
              </span>
            )}
            {!uploadError && <MediaLoadStatus prominent />}
          </div>
        ) : video ? (
          <ChatVideo source={attachment.preview_url} poster={poster} posterCacheKey={posterCacheKey} pixelWidth={pixelWidth} pixelHeight={pixelHeight} />
        ) : (
          <audio src={attachment.preview_url} controls preload="metadata" />
        )}
        {(uploadProgress !== undefined || uploadError) && (
          <div className={`attachment-progress ${uploadError ? "failed" : ""}`}>
            {!uploadError && <i style={{ width: `${uploadProgress ?? 0}%` }} />}
            <span>{uploadError ?? (uploadProgress === 0 && attachment.mime_type.startsWith("video/") ? "preparing video" : `${uploadProgress ?? 0}%`)}</span>
          </div>
        )}
      </div>
    </div>
  );
}

function mediaPoster(attachment: MediaAttachment) {
  return attachment.preview_data_base64 && attachment.preview_mime_type
    ? `data:${attachment.preview_mime_type};base64,${attachment.preview_data_base64}`
    : undefined;
}

function requestMediaSource(
  attachment: MediaAttachment,
  scopeId: string | undefined,
  priority: MediaLoadPriority,
) {
  const cacheKey = mediaCacheKey(attachment);
  const cached = mediaCache.get(cacheKey);
  if (cached) return Promise.resolve(cached);
  const pending = mediaLoadPromises.get(cacheKey);
  if (pending) {
    mediaLoadScheduler.promote(cacheKey, priority);
    return pending;
  }
  const generation = mediaCacheGeneration;

  // Registering a video stream is a local operation, not a media download:
  // the desktop app hands the video element a noise-media:// URL, the browser
  // build hands it a same-origin URL served by the media service worker.
  // Putting registration behind the three-slot image queue made a Play click
  // wait for unrelated feed images before the video element even had a URL.
  if (attachment.mime_type.startsWith("video/")) {
    const streamPromise = isTauri
      ? registerMediaStream({
          action: "fetch_attachment_range",
          attachment,
          scope_id: scopeId,
        })
      : webMediaStreamReady()
        ? Promise.resolve(registerWebMediaStream(attachment, scopeId))
        : null;
    if (streamPromise) {
      let streamRequest: Promise<string>;
      streamRequest = streamPromise
        .then((stream) => {
          if (!stream) throw new Error("video stream is unavailable");
          if (generation === mediaCacheGeneration) mediaCache.set(cacheKey, stream);
          return stream;
        })
        .finally(() => {
          if (mediaLoadPromises.get(cacheKey) === streamRequest) {
            mediaLoadPromises.delete(cacheKey);
          }
        });
      mediaLoadPromises.set(cacheKey, streamRequest);
      return streamRequest;
    }
  }

  let request: Promise<string>;
  request = mediaLoadScheduler.enqueue(cacheKey, priority, async () => {
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
      } catch (cause) {
        if (
          isSupersededLoading(cause)
          || mediaFailureIsPermanent(cause)
          || attempt === 11
        ) throw cause;
        const delay = Math.min(400 * 1.6 ** attempt, 3000);
        await new Promise<void>((resolve) => window.setTimeout(resolve, delay));
      }
    }
    throw new Error("media is unavailable");
  }).finally(() => {
    if (mediaLoadPromises.get(cacheKey) === request) mediaLoadPromises.delete(cacheKey);
  });
  mediaLoadPromises.set(cacheKey, request);
  return request;
}

async function downloadAttachment(attachment: MediaAttachment, scopeId?: string) {
  if (isTauri) {
    const { save } = await import("@tauri-apps/plugin-dialog");
    const destinationPath = await save({
      defaultPath: attachment.file_name || "noise-media",
      title: "Save decrypted media",
    });
    if (!destinationPath) return false;
    const data = await noise<AttachmentData>({
      action: "fetch_attachment",
      attachment,
      scope_id: scopeId,
      relays,
    });
    if (!data) throw new Error("media is unavailable");
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke<string>("download_media", {
      sourcePath: data.file_path,
      fileName: attachment.file_name,
      destinationPath,
    });
    return true;
  }
  const data = await noise<AttachmentData>({
    action: "fetch_attachment",
    attachment,
    scope_id: scopeId,
    relays,
  });
  if (!data) throw new Error("media is unavailable");
  const link = document.createElement("a");
  link.href = data.file_path;
  link.download = attachment.file_name || "noise-media";
  link.style.display = "none";
  document.body.append(link);
  link.click();
  link.remove();
  return true;
}

async function pendingMediaFromAttachment(
  attachment: MediaAttachment,
  scopeId: string,
): Promise<PendingMedia> {
  const data = await noise<AttachmentData>({
    action: "fetch_attachment",
    attachment,
    scope_id: scopeId,
    relays,
  });
  if (!data) throw new Error("media is unavailable");
  const source = isTauri
    ? (await import("@tauri-apps/api/core")).convertFileSrc(data.file_path)
    : data.file_path;
  const response = await fetch(source);
  if (!response.ok) throw new Error("media could not be prepared for forwarding");
  const blob = await response.blob();
  if (blob.size !== attachment.byte_length) {
    throw new Error("forwarded media does not match the original attachment");
  }
  const file = new File([blob], attachment.file_name || "noise-media", {
    type: attachment.mime_type || data.mime_type,
    lastModified: Date.now(),
  });
  const embeddedPreview = attachment.preview_data_base64
    && attachment.preview_mime_type === "image/jpeg"
    && attachment.pixel_width
    && attachment.pixel_height
    ? Promise.resolve({
        dataBase64: attachment.preview_data_base64,
        mimeType: "image/jpeg" as const,
        pixelWidth: attachment.pixel_width,
        pixelHeight: attachment.pixel_height,
      })
    : null;
  return {
    name: file.name,
    mimeType: file.type,
    byteLength: file.size,
    file: Promise.resolve(file),
    previewUrl: URL.createObjectURL(file),
    mediaPreview: embeddedPreview,
  };
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
    && !mediaPoster(attachment)
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
    video.preload = "metadata";
    let previewIndex = 0;
    let settled = false;
    let previewTimes: number[] = [];
    const timeout = window.setTimeout(finish, 8_000);
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

function useMediaSource(
  attachment: MediaAttachment,
  scopeId?: string,
  priority: MediaLoadPriority | null = "visible",
) {
  const cacheKey = mediaCacheKey(attachment);
  const [loaded, setLoaded] = useState<{ cacheKey: string; source: string } | null>(() => {
    const source = mediaCache.get(cacheKey);
    return source ? { cacheKey, source } : null;
  });
  const [failedKey, setFailedKey] = useState<string | null>(null);
  const retryRequest = useRef<Promise<boolean> | null>(null);
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
    if (!priority) return;
    let active = true;
    let retryTimer: number | undefined;
    let retryRound = 0;
    setFailedKey(null);
    const load = () => {
      setFailedKey(null);
      void requestMediaSource(attachment, scopeId, priority)
        .then((next) => {
          if (active) setLoaded({ cacheKey, source: next });
        })
        .catch((cause) => {
          if (!active) return;
          if (isSupersededLoading(cause)) return;
          setFailedKey(cacheKey);
          if (mediaFailureIsPermanent(cause)) return;
          const delay = Math.min(5_000 * 1.7 ** retryRound, 30_000);
          retryRound += 1;
          retryTimer = window.setTimeout(load, delay);
        });
    };
    load();
    return () => {
      active = false;
      if (retryTimer !== undefined) window.clearTimeout(retryTimer);
    };
  }, [attachment, cacheKey, priority, scopeId]);
  const retry = useCallback(() => {
    if (retryRequest.current) return retryRequest.current;
    mediaCache.delete(cacheKey);
    mediaLoadPromises.delete(cacheKey);
    setFailedKey(null);
    const request = requestMediaSource(attachment, scopeId, "visible")
      .then((next) => {
        setLoaded({ cacheKey, source: next });
        return true;
      })
      .catch(() => {
        setFailedKey(cacheKey);
        return false;
      })
      .finally(() => {
        if (retryRequest.current === request) retryRequest.current = null;
      });
    retryRequest.current = request;
    return request;
  }, [attachment, cacheKey, scopeId]);
  return { source, failed: failedKey === cacheKey, retry };
}

function useMediaPriority<T extends HTMLElement>() {
  const ref = useRef<T>(null);
  const [priority, setPriority] = useState<MediaLoadPriority | null>(null);
  useEffect(() => {
    const element = ref.current;
    if (!element) return;
    let visible = false;
    let nearby = false;
    let activated = false;
    const update = () => {
      if (visible) {
        activated = true;
        setPriority("visible");
      } else if (nearby) {
        activated = true;
        setPriority("nearby");
      } else if (activated) {
        setPriority("background");
      }
    };
    const visibleObserver = new IntersectionObserver(([entry]) => {
      visible = entry.isIntersecting;
      update();
    });
    const nearbyObserver = new IntersectionObserver(([entry]) => {
      nearby = entry.isIntersecting;
      update();
    }, { rootMargin: "900px 0px" });
    const fallback = window.setTimeout(() => {
      activated = true;
      update();
      if (!visible && !nearby) setPriority("background");
    }, 1_200);
    visibleObserver.observe(element);
    nearbyObserver.observe(element);
    return () => {
      window.clearTimeout(fallback);
      visibleObserver.disconnect();
      nearbyObserver.disconnect();
    };
  }, []);
  return { ref, priority };
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
  const [expanded, setExpanded] = useState(false);
  useEffect(() => {
    const knownDimensions = suppliedDimensions ?? mediaDimensionCache.get(cacheKey) ?? null;
    setDimensions(knownDimensions);
    setReady(Boolean(source && decodedImageCache.has(cacheKey) && knownDimensions));
  }, [cacheKey, pixelHeight, pixelWidth, source]);
  useEffect(() => {
    if (!expanded) return;
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") setExpanded(false);
    };
    window.addEventListener("keydown", closeOnEscape);
    return () => window.removeEventListener("keydown", closeOnEscape);
  }, [expanded]);
  const style = mediaFrameStyle(dimensions?.width, dimensions?.height);
  const visibleSource = source ?? preview;
  const lightboxRoot = document.querySelector<HTMLElement>(".app-shell")
    ?? document.body;
  const expandedImageStyle: CSSProperties | undefined =
    dimensions?.width && dimensions.height
      ? {
        width: `min(90vw, ${(90 * dimensions.width) / dimensions.height}vh)`,
        height: "auto",
      }
      : undefined;
  return (
    <span
      className="chat-image"
      style={style}
      role={visibleSource ? "button" : undefined}
      tabIndex={visibleSource ? 0 : undefined}
      aria-label={visibleSource ? "view image full size" : undefined}
      onClick={visibleSource ? () => setExpanded(true) : undefined}
      onKeyDown={visibleSource ? (event) => {
        if (event.target !== event.currentTarget) return;
        if (event.key === "Enter" || event.key === " ") {
          event.preventDefault();
          setExpanded(true);
        }
      } : undefined}
    >
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
      {!ready && <MediaLoadStatus failed={failed} compact={Boolean(preview)} />}
      {expanded && visibleSource && createPortal(
        <div
          className="image-lightbox"
          role="dialog"
          aria-modal="true"
          aria-label="expanded image"
          onClick={(event) => {
            event.stopPropagation();
            setExpanded(false);
          }}
          onKeyDown={(event) => event.stopPropagation()}
        >
          <button
            type="button"
            className="image-lightbox-close"
            onClick={() => setExpanded(false)}
            aria-label="close expanded image"
            autoFocus
          >
            <X size={18} />
          </button>
          <img
            src={visibleSource}
            alt="shared media, expanded"
            style={expandedImageStyle}
            onClick={(event) => event.stopPropagation()}
          />
        </div>,
        lightboxRoot,
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
  playbackRequested = false,
  onRequestPlayback,
  onSourceError,
  failed = false,
}: {
  source?: string;
  poster?: string;
  posterCacheKey?: string;
  pixelWidth?: number | null;
  pixelHeight?: number | null;
  autoPlay?: boolean;
  playbackRequested?: boolean;
  onRequestPlayback?: () => void;
  onSourceError?: () => Promise<boolean>;
  failed?: boolean;
}) {
  const [playing, setPlaying] = useState(false);
  const [muted, setMuted] = useState(false);
  const [hasStarted, setHasStarted] = useState(false);
  const [currentTime, setCurrentTime] = useState(0);
  const [duration, setDuration] = useState(0);
  const [videoReady, setVideoReady] = useState(false);
  const [playbackFrameReady, setPlaybackFrameReady] = useState(false);
  const [buffering, setBuffering] = useState(false);
  const [playbackFailed, setPlaybackFailed] = useState(false);
  const [decodedPoster, setDecodedPoster] = useState(
    () => (posterCacheKey ? videoPosterCache.get(posterCacheKey) : undefined) ?? poster,
  );
  const [measuredDimensions, setMeasuredDimensions] = useState(() =>
    posterCacheKey ? mediaDimensionCache.get(posterCacheKey) ?? null : null
  );
  const [fullscreen, setFullscreen] = useState(false);
  const video = useRef<HTMLVideoElement>(null);
  const frame = useRef<HTMLDivElement>(null);
  const automaticRecoveryAttempts = useRef(0);
  const sourceRecoveryInFlight = useRef(false);
  // Give visible desktop videos their local stream before Play so WebKit can
  // finish metadata/range probes without adding that setup to the click.
  const activeSource = source;
  useEffect(() => {
    setPlaying(false);
    setMuted(false);
    setVideoReady(false);
    setPlaybackFrameReady(false);
    setHasStarted(false);
    setCurrentTime(0);
    setDuration(0);
    setBuffering(false);
    setPlaybackFailed(false);
    sourceRecoveryInFlight.current = false;
  }, [activeSource]);
  useEffect(() => {
    automaticRecoveryAttempts.current = 0;
  }, [posterCacheKey]);
  useEffect(() => {
    const syncFullscreen = () => {
      const active = activeFullscreenElement();
      setFullscreen(active !== null && (active === frame.current || active === video.current));
    };
    syncFullscreen();
    document.addEventListener("fullscreenchange", syncFullscreen);
    document.addEventListener("webkitfullscreenchange", syncFullscreen);
    return () => {
      document.removeEventListener("fullscreenchange", syncFullscreen);
      document.removeEventListener("webkitfullscreenchange", syncFullscreen);
    };
  }, []);
  useEffect(() => {
    const element = video.current;
    if (!autoPlay || !activeSource || !element) return;
    element.currentTime = 0;
    void element.play().catch(() => undefined);
    return () => {
      element.pause();
      element.currentTime = 0;
    };
  }, [activeSource, autoPlay]);
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
    const cached = posterCacheKey ? videoPosterCache.get(posterCacheKey) : undefined;
    if (!poster) {
      setDecodedPoster(cached);
      return;
    }
    let active = true;
    void imageIsNearBlack(poster).then((nearBlack) => {
      if (!active) return;
      if (nearBlack) {
        setDecodedPoster(cached);
        return;
      }
      setDecodedPoster(cached ?? poster);
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
      if (videoFrameIsNearBlack(element)) return;
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
  const frameStyle = mediaFrameStyle(frameWidth, frameHeight, 288, 176);
  const togglePlayback = () => {
    if (onRequestPlayback && !playbackRequested) {
      onRequestPlayback();
      return;
    }
    const element = video.current;
    if (!element || !activeSource) return;
    if (element.paused) {
      setPlaybackFailed(false);
      void element.play().catch(() => setPlaybackFailed(true));
    } else {
      element.pause();
    }
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
  const toggleFullscreen = () => {
    if (activeFullscreenElement()) {
      leaveFullscreen();
      return;
    }
    enterFullscreen(frame.current, video.current);
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
  const retryPlayback = () => {
    const element = video.current;
    if (!element || !activeSource) return;
    setPlaybackFailed(false);
    setBuffering(true);
    if (onSourceError) {
      automaticRecoveryAttempts.current = 0;
      sourceRecoveryInFlight.current = true;
      void onSourceError().then((recovered) => {
        if (!recovered) {
          sourceRecoveryInFlight.current = false;
          setBuffering(false);
          setPlaybackFailed(true);
        }
      });
      return;
    }
    element.load();
    void element.play().catch(() => setPlaybackFailed(true));
  };
  const loading = playbackRequested
    && (!activeSource || (!decodedPoster && !videoReady));
  const waitingForFirstPlaybackFrame = playbackRequested
    && !playbackFrameReady
    && !playbackFailed;
  const showStartButton = !hasStarted
    && !playbackFailed
    && (onRequestPlayback
      ? !playbackRequested || Boolean(activeSource)
      : Boolean(activeSource));
  return <div
    ref={frame}
    className={`chat-video ${hasStarted ? "started" : ""} ${loading ? "media-pending" : ""} ${fullscreen ? "fullscreen" : ""}`}
    style={fullscreen ? undefined : frameStyle}
  >
    <video
      ref={video}
      src={activeSource}
      poster={decodedPoster}
      width={frameWidth ?? undefined}
      height={frameHeight ?? undefined}
      autoPlay={autoPlay}
      muted={muted}
      playsInline
      preload={autoPlay ? "auto" : "metadata"}
      onLoadStart={() => {
        setVideoReady(false);
        if (hasStarted || autoPlay) setBuffering(true);
      }}
      onCanPlay={(event) => {
        setVideoReady(true);
        setBuffering(false);
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
        setVideoReady(true);
      }}
      onLoadedData={(event) => {
        setVideoReady(true);
        capturePoster(event.currentTarget);
      }}
      onPlay={() => {
        setPlaying(true);
        setHasStarted(true);
        setPlaybackFailed(false);
      }}
      onPlaying={(event) => {
        automaticRecoveryAttempts.current = 0;
        setVideoReady(true);
        setBuffering(false);
        revealOnRenderedFrame(event.currentTarget);
      }}
      onPause={() => setPlaying(false)}
      onEnded={() => setPlaying(false)}
      onTimeUpdate={(event) => setCurrentTime(event.currentTarget.currentTime)}
      onDurationChange={(event) => {
        setDuration(
          Number.isFinite(event.currentTarget.duration)
            ? event.currentTarget.duration
            : 0,
        );
      }}
      onVolumeChange={(event) => setMuted(event.currentTarget.muted)}
      onWaiting={() => setBuffering(true)}
      onStalled={() => setBuffering(true)}
      onSuspend={() => {
        if (!hasStarted) setBuffering(false);
      }}
      onError={() => {
        setPlaying(false);
        if (sourceRecoveryInFlight.current) return;
        if (
          onSourceError
          && playbackRequested
          && automaticRecoveryAttempts.current < 3
        ) {
          automaticRecoveryAttempts.current += 1;
          sourceRecoveryInFlight.current = true;
          setPlaybackFailed(false);
          setBuffering(true);
          void onSourceError().then((recovered) => {
            if (!recovered) {
              sourceRecoveryInFlight.current = false;
              setBuffering(false);
              setPlaybackFailed(true);
            }
          });
          return;
        }
        setBuffering(false);
        setPlaybackFailed(true);
      }}
      onClick={togglePlayback}
      onDoubleClick={toggleFullscreen}
      onKeyDown={(event) => {
        if (event.key === "Enter" || event.key === " ") {
          event.preventDefault();
          togglePlayback();
          return;
        }
        if (event.key === "f" || event.key === "F") {
          event.preventDefault();
          toggleFullscreen();
        }
      }}
      tabIndex={0}
      aria-label="video"
      title="play or pause video"
    />
    {decodedPoster && !playbackFrameReady && (
      <img
        className="chat-video-poster-cover"
        src={decodedPoster}
        alt=""
        aria-hidden="true"
      />
    )}
    {!decodedPoster && !playbackFrameReady && (
      <span className="chat-video-placeholder" aria-hidden="true">
        <NoiseMark size={34} monochrome />
      </span>
    )}
    {(loading || buffering || waitingForFirstPlaybackFrame) && (
      <MediaLoadStatus
        failed={failed}
        compact={!waitingForFirstPlaybackFrame && Boolean(decodedPoster)}
        prominent={waitingForFirstPlaybackFrame}
      />
    )}
    {showStartButton && (
      <button
        type="button"
        className="chat-video-start"
        onClick={togglePlayback}
        aria-label="play video"
        title="play video"
      >
        <Play size={25} fill="currentColor" />
      </button>
    )}
    <div className="noise-video-controls" aria-label="video controls">
      <button
        type="button"
        className="noise-video-control-button"
        disabled={!activeSource}
        onClick={togglePlayback}
        aria-label={playing ? "pause video" : "play video"}
        title={playing ? "pause" : "play"}
      >
        {playing
          ? <Pause size={16} fill="currentColor" />
          : <Play size={16} fill="currentColor" />}
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
        disabled={!activeSource || !duration}
      />
      <span className="noise-video-time">{formatVideoTime(duration)}</span>
      <button
        type="button"
        className="noise-video-control-button"
        disabled={!activeSource}
        onClick={toggleMuted}
        aria-label={muted ? "unmute video" : "mute video"}
        title={muted ? "unmute" : "mute"}
      >
        {muted ? <VolumeX size={17} /> : <Volume2 size={17} />}
      </button>
      <button
        type="button"
        className="noise-video-control-button"
        disabled={!activeSource}
        onClick={toggleFullscreen}
        aria-label={fullscreen ? "exit full screen" : "play video full screen"}
        title={fullscreen ? "exit full screen" : "full screen"}
      >
        {fullscreen ? <Minimize size={16} /> : <Maximize size={16} />}
      </button>
    </div>
    {playbackFailed && activeSource && (
      <button type="button" className="chat-video-retry" onClick={retryPlayback}>
        <LoaderCircle size={18} />
        <span>retry video</span>
      </button>
    )}
  </div>;
}

type FullscreenElement = HTMLElement & { webkitRequestFullscreen?: () => void };
type FullscreenVideo = HTMLVideoElement & { webkitEnterFullscreen?: () => void };
type FullscreenDocument = Document & {
  webkitFullscreenElement?: Element | null;
  webkitExitFullscreen?: () => void;
};

function activeFullscreenElement() {
  const scope = document as FullscreenDocument;
  return scope.fullscreenElement ?? scope.webkitFullscreenElement ?? null;
}

function leaveFullscreen() {
  const scope = document as FullscreenDocument;
  if (scope.exitFullscreen) {
    void Promise.resolve(scope.exitFullscreen()).catch(() => undefined);
    return;
  }
  scope.webkitExitFullscreen?.();
}

function enterFullscreen(frame: HTMLElement | null, video: HTMLVideoElement | null) {
  const target = frame as FullscreenElement | null;
  // iOS only ever fullscreens the video itself, so fall back to its native player.
  const nativeVideoFullscreen = () =>
    (video as FullscreenVideo | null)?.webkitEnterFullscreen?.();
  if (target?.requestFullscreen) {
    void Promise.resolve(target.requestFullscreen()).catch(nativeVideoFullscreen);
    return;
  }
  if (target?.webkitRequestFullscreen) {
    target.webkitRequestFullscreen();
    return;
  }
  nativeVideoFullscreen();
}

function formatVideoTime(seconds: number) {
  if (!Number.isFinite(seconds) || seconds < 0) return "0:00";
  const wholeSeconds = Math.floor(seconds);
  const minutes = Math.floor(wholeSeconds / 60);
  return `${minutes}:${String(wholeSeconds % 60).padStart(2, "0")}`;
}

type MediaMessage = MessageSummary & { attachment: MediaAttachment };

function ForwardMessageDialog({
  message,
  groups,
  topicsByGroup,
  people,
  selfPublicKey,
  onClose,
  onForward,
}: {
  message: MessageSummary;
  groups: GroupSummary[];
  topicsByGroup: Map<string, TopicSummary[]>;
  people: DirectSummary[];
  selfPublicKey: string;
  onClose: () => void;
  onForward: (
    destination: ForwardDestination,
    showOriginalAuthor: boolean,
    onProgress: (progress: number) => void,
  ) => Promise<boolean>;
}) {
  const [query, setQuery] = useState("");
  const [showOriginalAuthor, setShowOriginalAuthor] = useState(true);
  const [forwardingTo, setForwardingTo] = useState<string | null>(null);
  const [progress, setProgress] = useState(0);
  const normalized = query.trim().toLowerCase();
  const uniquePeople = [...new Map(
    people
      .filter((person) =>
        person.public_key !== selfPublicKey
        && person.accepts_direct_messages
      )
      .map((person) => [person.public_key, person]),
  ).values()];
  const visibleGroups = groups
    .map((group) => {
      const topics = (topicsByGroup.get(group.group_id) ?? [])
        .filter((topic) => !topic.archived)
        .filter((topic) =>
          !normalized
          || topic.name.toLowerCase().includes(normalized)
          || group.name.toLowerCase().includes(normalized)
        );
      const groupMatches = !normalized || group.name.toLowerCase().includes(normalized);
      return { group, topics, groupMatches };
    })
    .filter(({ groupMatches, topics }) => groupMatches || topics.length);
  const visiblePeople = uniquePeople.filter((person) =>
    !normalized || person.username.toLowerCase().includes(normalized)
  );
  const forward = async (destination: ForwardDestination) => {
    if (forwardingTo) return;
    setForwardingTo(destination.label);
    setProgress(0);
    const sent = await onForward(destination, showOriginalAuthor, setProgress);
    if (sent) {
      window.setTimeout(onClose, 450);
    } else {
      setForwardingTo(null);
      setProgress(0);
    }
  };
  const attachmentLabel = message.attachment?.mime_type.startsWith("video/")
    ? "video"
    : message.attachment?.mime_type.startsWith("image/")
      ? "photo"
      : message.attachment?.mime_type.startsWith("audio/")
        ? "audio"
        : message.attachment
          ? "media"
          : null;
  return (
    <Modal onClose={forwardingTo ? () => undefined : onClose}>
      <DialogHeading
        icon={<Forward />}
        title="forward message"
        detail="choose a group, topic, or person"
      />
      <div className="forward-preview">
        <strong>{attachmentLabel ? `${attachmentLabel}${message.text ? " + message" : ""}` : "message"}</strong>
        <span>{message.text || message.attachment?.file_name || "encrypted media"}</span>
      </div>
      <label className="settings-toggle-row forward-author-toggle">
        <span>
          <strong>show original author</strong>
          <small>include forwarded from {message.forwarded_from?.username ?? message.username}</small>
        </span>
        <input
          type="checkbox"
          role="switch"
          checked={showOriginalAuthor}
          disabled={Boolean(forwardingTo)}
          onChange={(event) => setShowOriginalAuthor(event.target.checked)}
        />
      </label>
      <input
        className="forward-search"
        value={query}
        disabled={Boolean(forwardingTo)}
        onChange={(event) => setQuery(event.target.value)}
        placeholder="search destinations"
        autoFocus
      />
      <div className="forward-destinations">
        {visibleGroups.length > 0 && <section>
          <h4>groups & topics</h4>
          {visibleGroups.map(({ group, topics, groupMatches }) => (
            <div className="forward-group" key={group.group_id}>
              {groupMatches && (
                <button
                  disabled={Boolean(forwardingTo)}
                  onClick={() => void forward({
                    type: "group",
                    groupId: group.group_id,
                    topicId: null,
                    label: `${group.name} / General`,
                  })}
                >
                  <Avatar name={group.name} image={group.avatar} size={32} square />
                  <span><strong>{group.name}</strong><small>💬 General</small></span>
                </button>
              )}
              {topics.map((topic) => (
                <button
                  className="forward-topic"
                  disabled={Boolean(forwardingTo)}
                  key={topic.topic_id}
                  onClick={() => void forward({
                    type: "group",
                    groupId: group.group_id,
                    topicId: topic.topic_id,
                    label: `${group.name} / ${topic.name}`,
                  })}
                >
                  <span className="forward-topic-icon">{topic.icon}</span>
                  <span><strong>{topic.name}</strong><small>{group.name}</small></span>
                </button>
              ))}
            </div>
          ))}
        </section>}
        {visiblePeople.length > 0 && <section>
          <h4>people</h4>
          {visiblePeople.map((person) => (
            <button
              disabled={Boolean(forwardingTo)}
              key={person.public_key}
              onClick={() => void forward({
                type: "direct",
                publicKey: person.public_key,
                label: person.username,
              })}
            >
              <PresenceAvatar name={person.username} image={person.avatar} size={32} status="offline" />
              <span><strong>{person.username}</strong><small>direct message</small></span>
            </button>
          ))}
        </section>}
        {!visibleGroups.length && !visiblePeople.length && (
          <div className="quiet">no destinations match that search</div>
        )}
      </div>
      {forwardingTo && (
        <div className="forward-progress">
          <span><LoaderCircle className="spinner" size={14} /> forwarding to {forwardingTo}</span>
          {message.attachment && <i><b style={{ width: `${progress}%` }} /></i>}
        </div>
      )}
      <DialogButtons
        onClose={onClose}
        closeDisabled={Boolean(forwardingTo)}
        closeLabel="cancel"
      >{null}</DialogButtons>
    </Modal>
  );
}

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

const profileAlbumCache = new Map<string, ProfileAlbumData>();

type PendingAlbumUpload = {
  id: string;
  file: File;
  previewUrl: string;
  attachment?: MediaAttachment;
};

function profileAlbumMessage(person: PersonSummary, item: ProfileAlbumItem): MediaMessage {
  return {
    event_id: item.id,
    message_id: item.id,
    author_public_key: person.public_key,
    username: person.username,
    bio: person.bio,
    avatar: person.avatar,
    album: person.album,
    accepts_direct_messages: person.accepts_direct_messages,
    direct_message_policy: person.direct_message_policy,
    text: "",
    attachment: item.attachment,
    reply_to_message_id: null,
    created_at_millis: item.created_at_millis,
    reactions: [],
  };
}

function ProfileAlbumDialog({
  person,
  editable,
  onClose,
  onSummary,
  embedded = false,
}: {
  person: PersonSummary;
  editable: boolean;
  onClose?: () => void;
  onSummary: (summary: LocalSummary) => void;
  embedded?: boolean;
}) {
  const cached = person.album ? profileAlbumCache.get(person.album.blob_id) ?? null : null;
  const [album, setAlbum] = useState(person.album);
  const [data, setData] = useState<ProfileAlbumData | null>(
    cached ?? (person.album ? null : { scope_id: "", items: [] }),
  );
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [pendingUploads, setPendingUploads] = useState<PendingAlbumUpload[]>([]);
  const [uploadProgress, setUploadProgress] = useState<number | null>(null);
  const [uploadIndex, setUploadIndex] = useState(0);
  const [clearing, setClearing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const fileInput = useRef<HTMLInputElement>(null);
  const uploadController = useRef<AbortController | null>(null);
  const pendingUploadsRef = useRef<PendingAlbumUpload[]>([]);
  const items = data?.items ?? [];
  const media = items.map((item) => profileAlbumMessage(person, item));
  const selectedIndex = selectedId
    ? media.findIndex((item) => item.event_id === selectedId)
    : -1;
  const selected = selectedIndex >= 0 ? media[selectedIndex] : null;
  const showPrevious = selectedIndex > 0;
  const showNext = selectedIndex >= 0 && selectedIndex < media.length - 1;

  useEffect(() => {
    if (!album) {
      setData({ scope_id: "", items: [] });
      return;
    }
    const known = profileAlbumCache.get(album.blob_id);
    if (known) {
      setData(known);
      return;
    }
    let active = true;
    setData(null);
    setError(null);
    void noise<ProfileAlbumData>({
      action: "fetch_profile_album",
      public_key: person.public_key,
      album,
      relays,
    }).then((next) => {
      if (!active || !next) return;
      profileAlbumCache.set(album.blob_id, next);
      setData(next);
    }).catch((cause) => {
      if (active) setError(message(cause));
    });
    return () => { active = false; };
  }, [album, person.public_key]);

  useEffect(() => {
    if (!selected) return;
    const navigate = (event: KeyboardEvent) => {
      if (event.key === "ArrowLeft" && showPrevious) {
        event.preventDefault();
        setSelectedId(media[selectedIndex - 1].event_id);
      } else if (event.key === "ArrowRight" && showNext) {
        event.preventDefault();
        setSelectedId(media[selectedIndex + 1].event_id);
      }
    };
    window.addEventListener("keydown", navigate);
    return () => window.removeEventListener("keydown", navigate);
  }, [media, selected, selectedIndex, showNext, showPrevious]);

  useEffect(() => {
    pendingUploadsRef.current = pendingUploads;
  }, [pendingUploads]);

  useEffect(() => () => {
    uploadController.current?.abort();
    for (const pending of pendingUploadsRef.current) {
      URL.revokeObjectURL(pending.previewUrl);
    }
  }, []);

  async function save(nextItems: ProfileAlbumItem[]) {
    const local = await noise<LocalSummary>({
      action: "update_profile_album",
      items: nextItems,
      relays,
    });
    if (!local) throw new Error("the album could not be updated");
    onSummary(local);
    void noise({ action: "sync_account", relays }).catch(() => {
      // The signed profile update and local album are already durable.
      // Encrypted cross-device account sync retries in the background.
    });
    const nextAlbum = local.identity.album;
    if (!nextAlbum) {
      setAlbum(null);
      setData({ scope_id: "", items: [] });
      setSelectedId(null);
      return;
    }
    const next = await noise<ProfileAlbumData>({
      action: "fetch_profile_album",
      public_key: local.identity.public_key,
      album: nextAlbum,
      relays,
    });
    if (!next) throw new Error("the updated album could not be loaded");
    profileAlbumCache.set(nextAlbum.blob_id, next);
    setAlbum(nextAlbum);
    setData(next);
  }

  function queueMedia(files?: FileList | null) {
    if (!files?.length || uploadProgress !== null) return;
    const incoming = Array.from(files);
    const batchSlots = Math.max(0, 10 - pendingUploads.length);
    const albumSlots = Math.max(0, 48 - items.length - pendingUploads.length);
    const available = Math.min(batchSlots, albumSlots);
    const existing = new Set(
      pendingUploads.map(({ file }) => `${file.name}:${file.size}:${file.lastModified}`),
    );
    const accepted: PendingAlbumUpload[] = [];
    let invalid = 0;
    let duplicate = 0;
    for (const file of incoming) {
      if (accepted.length >= available) break;
      if (!/^(image|video)\//.test(file.type) || !file.size || file.size > 500 * 1024 * 1024) {
        invalid += 1;
        continue;
      }
      const fingerprint = `${file.name}:${file.size}:${file.lastModified}`;
      if (existing.has(fingerprint)) {
        duplicate += 1;
        continue;
      }
      existing.add(fingerprint);
      accepted.push({
        id: crypto.randomUUID(),
        file,
        previewUrl: URL.createObjectURL(file),
      });
    }
    if (accepted.length) setPendingUploads((current) => [...current, ...accepted]);
    if (items.length >= 48 || albumSlots === 0) {
      setError("your album can hold up to 48 items");
    } else if (incoming.length > available) {
      setError(`you can add up to ${available} more ${available === 1 ? "item" : "items"} in this upload`);
    } else if (invalid) {
      setError("photos and videos can be up to 500 MB each");
    } else if (duplicate && !accepted.length) {
      setError("those items are already selected");
    } else {
      setError(null);
    }
    if (fileInput.current) fileInput.current.value = "";
  }

  function removeQueued(id: string) {
    if (uploadProgress !== null) return;
    setPendingUploads((current) => {
      const removed = current.find((item) => item.id === id);
      if (removed) URL.revokeObjectURL(removed.previewUrl);
      return current.filter((item) => item.id !== id);
    });
    setError(null);
  }

  function clearQueue() {
    if (uploadProgress !== null) return;
    for (const pending of pendingUploads) URL.revokeObjectURL(pending.previewUrl);
    setPendingUploads([]);
    setError(null);
  }

  async function uploadQueuedMedia() {
    if (!pendingUploads.length || uploadProgress !== null) return;
    setError(null);
    setUploadProgress(1);
    setUploadIndex(0);
    const controller = new AbortController();
    uploadController.current = controller;
    try {
      const uploaded: ProfileAlbumItem[] = [];
      for (let index = 0; index < pendingUploads.length; index += 1) {
        const queued = pendingUploads[index];
        setUploadIndex(index);
        let attachment = queued.attachment;
        if (!attachment) {
          const pending: PendingMedia = {
            name: queued.file.name,
            mimeType: queued.file.type,
            byteLength: queued.file.size,
            file: Promise.resolve(queued.file),
            previewUrl: queued.previewUrl,
            mediaPreview: queued.file.type.startsWith("video/")
              ? prepareVideoPreviewSource(queued.previewUrl)
              : prepareImagePreviewSource(queued.previewUrl),
          };
          attachment = await uploadPendingMedia(
            pending,
            "upload_profile_media_chunk",
            (progress) => {
              const overall = ((index + progress / 100) / pendingUploads.length) * 100;
              setUploadProgress(Math.min(99, Math.round(overall)));
            },
            controller.signal,
          ) ?? undefined;
          if (attachment) {
            setPendingUploads((current) => current.map((item) =>
              item.id === queued.id ? { ...item, attachment } : item
            ));
          }
        }
        const id = attachment?.chunks[0]?.blob_id;
        if (!attachment || !id) throw new Error("the uploaded media is incomplete");
        uploaded.push({
          id,
          attachment,
          created_at_millis: Date.now() + index,
        });
      }
      setUploadProgress(100);
      await save([...uploaded, ...items]);
      for (const pending of pendingUploads) URL.revokeObjectURL(pending.previewUrl);
      setPendingUploads([]);
    } catch (cause) {
      if (message(cause) !== "media upload cancelled") setError(message(cause));
    } finally {
      if (uploadController.current === controller) uploadController.current = null;
      setUploadProgress(null);
      setUploadIndex(0);
      if (fileInput.current) fileInput.current.value = "";
    }
  }

  async function removeSelected() {
    if (!selected || uploadProgress !== null) return;
    setError(null);
    try {
      await save(items.filter((item) => item.id !== selected.event_id));
      setSelectedId(null);
    } catch (cause) {
      setError(message(cause));
    }
  }

  async function clearAlbum() {
    if (!album || uploadProgress !== null || clearing) return;
    if (!window.confirm("Clear every item from your album? This cannot be undone.")) return;
    setClearing(true);
    setError(null);
    try {
      await save([]);
    } catch (cause) {
      setError(message(cause));
    } finally {
      setClearing(false);
    }
  }

  const displayedItemCount = data ? items.length : album?.item_count ?? 0;
  const content = (
    <>
      {!embedded && <DialogHeading
        icon={<Images />}
        title={`${person.username}'s album`}
        detail={`${displayedItemCount} of 48 ${displayedItemCount === 1 ? "item" : "items"}`}
      />}
      {editable && (
        <div className="profile-album-toolbar">
          <button className="primary" disabled={clearing || uploadProgress !== null || items.length + pendingUploads.length >= 48 || pendingUploads.length >= 10} onClick={() => fileInput.current?.click()}>
            <Plus size={15} /> select photos or videos
          </button>
          {album && <button className="danger" disabled={clearing || uploadProgress !== null} onClick={() => void clearAlbum()}>
            {clearing ? <LoaderCircle className="spinner" size={14} /> : <Trash2 size={14} />}
            {clearing ? "clearing album" : "clear album"}
          </button>}
          <small>{pendingUploads.length
            ? `${pendingUploads.length} of 10 selected`
            : embedded
              ? `${items.length} of 48 items · up to 10 at once`
              : "up to 10 at once"}</small>
          <input ref={fileInput} hidden multiple type="file" accept="image/*,video/*" onChange={(event) => queueMedia(event.target.files)} />
        </div>
      )}
      {editable && pendingUploads.length > 0 && (
        <section className="profile-album-queue">
          <div className="profile-album-queue-heading">
            <span>
              <strong>{uploadProgress === null ? `${pendingUploads.length} ready to add` : `uploading ${uploadIndex + 1} of ${pendingUploads.length}`}</strong>
              <small>{uploadProgress === null ? "review your selection before adding it to your album" : `${uploadProgress}% complete`}</small>
            </span>
            <div>
              <button disabled={uploadProgress !== null} onClick={clearQueue}>clear</button>
              <button className="primary" disabled={uploadProgress !== null} onClick={() => void uploadQueuedMedia()}>
                <ArrowUp size={14} /> add {pendingUploads.length}
              </button>
              {uploadProgress !== null && <button className="cancel" onClick={() => uploadController.current?.abort()} aria-label="cancel upload"><X size={14} /></button>}
            </div>
          </div>
          <div className="profile-album-queue-items">
            {pendingUploads.map((pending, index) => (
              <div className={`profile-album-queue-item ${uploadProgress !== null && index === uploadIndex ? "uploading" : ""} ${pending.attachment ? "uploaded" : ""}`} key={pending.id}>
                {pending.file.type.startsWith("video/")
                  ? <video src={pending.previewUrl} muted playsInline preload="metadata" />
                  : <img src={pending.previewUrl} alt="" />}
                {pending.file.type.startsWith("video/") && <i><Play size={12} fill="currentColor" /></i>}
                {pending.attachment && <em><Check size={12} /></em>}
                <button disabled={uploadProgress !== null} onClick={() => removeQueued(pending.id)} aria-label={`remove ${pending.file.name}`}><X size={12} /></button>
              </div>
            ))}
          </div>
          {uploadProgress !== null && <span className="profile-album-progress"><i style={{ width: `${uploadProgress}%` }} /></span>}
        </section>
      )}
      {error && <p className="profile-album-error">{error}</p>}
      {selected && data ? (
        <div className="gallery-view">
          <button className="gallery-back" onClick={() => setSelectedId(null)}><ArrowLeft size={14} /> {albumButtonLabel(album)}</button>
          <div className="gallery-viewer">
            <button className="gallery-nav previous" disabled={!showPrevious} onClick={() => showPrevious && setSelectedId(media[selectedIndex - 1].event_id)} aria-label="previous media"><ChevronLeft size={25} /></button>
            <MessageMedia key={selected.event_id} attachment={selected.attachment} scopeId={data.scope_id} autoplayVideo />
            <button className="gallery-nav next" disabled={!showNext} onClick={() => showNext && setSelectedId(media[selectedIndex + 1].event_id)} aria-label="next media"><ChevronRight size={25} /></button>
          </div>
          <div className="profile-album-meta">
            <small>{selectedIndex + 1} of {media.length} · {formatGalleryDate(selected.created_at_millis)}</small>
            {editable && <button className="danger" onClick={() => void removeSelected()}><Trash2 size={13} /> remove</button>}
          </div>
        </div>
      ) : data ? (
        media.length ? (
          <div className="media-gallery">
            {media.map((item) => <GalleryTile key={item.event_id} message={item} scopeId={data.scope_id} onOpen={() => setSelectedId(item.event_id)} />)}
          </div>
        ) : (
          <div className="empty-gallery"><Images size={27} /><span>{editable ? "add photos and videos to your album" : "this album is empty"}</span></div>
        )
      ) : (
        <div className="empty-gallery"><LoaderCircle className="spinner" size={25} /><span>loading album</span></div>
      )}
    </>
  );
  if (embedded) return <div className="profile-album-content embedded">{content}</div>;
  return <Modal onClose={onClose ?? (() => undefined)} wide className="profile-album-modal"><div className="profile-album-content">{content}</div></Modal>;
}

function GalleryTile({ message, scopeId, onOpen }: { message: MediaMessage; scopeId: string; onOpen: () => void }) {
  const { attachment } = message;
  const visibility = useMediaPriority<HTMLButtonElement>();
  const image = attachment.mime_type.startsWith("image/");
  const video = attachment.mime_type.startsWith("video/");
  const { source, failed } = useMediaSource(
    attachment,
    scopeId,
    video ? null : visibility.priority,
  );
  const thumbnail = useGalleryThumbnail(attachment, source);
  const loading = video
    ? false
    : !source || (image && !thumbnail);
  return (
    <button ref={visibility.ref} className={`gallery-tile ${image ? "image" : video ? "video" : "audio"}`} onClick={onOpen} aria-label={`open media shared by ${message.username}`}>
      {(image || video) && thumbnail && <img src={thumbnail} alt="" />}
      {video && !thumbnail && <span className="gallery-video-placeholder"><NoiseMark size={28} monochrome /></span>}
      {!image && !video && source && <span className="gallery-audio"><AudioWaveform size={30} /><small>audio</small></span>}
      {loading && <span className="gallery-loading"><MediaLoadStatus failed={failed} /></span>}
      {video && !loading && <i className="gallery-play"><Play size={15} fill="currentColor" /></i>}
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

function useProfileImageSource(
  image: ProfileImage | null,
  preservePreviousUntilReady = false,
) {
  const storageKey = image?.storage ? JSON.stringify(image.storage) : "";
  const [loaded, setLoaded] = useState<{ blobId: string; source: string } | null>(() => {
    if (!image) return null;
    const source = avatarCache.get(image.blob_id);
    return source ? { blobId: image.blob_id, source } : null;
  });
  const exactSource = image
    ? loaded?.blobId === image.blob_id
      ? loaded.source
      : avatarCache.get(image.blob_id)
    : undefined;
  const source = exactSource
    ?? (image && preservePreviousUntilReady ? loaded?.source : undefined);
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
    if (!preservePreviousUntilReady) setLoaded(null);
    let active = true;
    let retryTimer: number | undefined;
    const load = async (attempt: number) => {
      try {
        const source = await loadProfileImageSource(target);
        if (active) setLoaded({ blobId: target.blob_id, source });
      } catch (cause) {
        if (!active || mediaFailureIsPermanent(cause)) return;
        const delay = [500, 1_200, 2_500, 5_000, 10_000, 20_000][attempt] ?? 30_000;
        retryTimer = window.setTimeout(() => void load(attempt + 1), delay);
      }
    };
    void load(0);
    return () => {
      active = false;
      if (retryTimer !== undefined) window.clearTimeout(retryTimer);
    };
  }, [
    image?.blob_id,
    image?.key_base64,
    image?.mime_type,
    image?.byte_length,
    storageKey,
    preservePreviousUntilReady,
  ]);
  return source;
}

function Avatar({ name, image, size, square = false }: { name: string; image: ProfileImage | null; size: number; square?: boolean }) {
  const source = useProfileImageSource(image, true);
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

function Onboarding({
  busy,
  addingAccount,
  onCancelAdd,
  onCreate,
  onSignIn,
}: {
  busy: boolean;
  addingAccount: boolean;
  onCancelAdd: () => void;
  onCreate: (username: string, password: string, birthDate: string) => Promise<boolean>;
  onSignIn: (noiseId: string, password: string) => Promise<boolean>;
}) {
  const [mode, setMode] = useState<"create" | "signin">("create");
  const [username, setUsername] = useState("");
  const [noiseId, setNoiseId] = useState("");
  const [password, setPassword] = useState("");
  const [confirmation, setConfirmation] = useState("");
  const [birthDate, setBirthDate] = useState("");
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
  const usernameReady = username.trim().length > 0
    && Array.from(username.trim()).length <= MAX_DISPLAY_NAME_LENGTH;
  const passwordReady = passwordRequirements.every((requirement) => requirement.met);
  const birthDateReady = /^\d{4}-\d{2}-\d{2}$/.test(birthDate);
  const createReady = usernameReady && passwordReady && birthDateReady;
  const submitCreate = () => {
    setCreateAttempted(true);
    if (busy || !createReady) return;
    void onCreate(username.trim(), password, birthDate);
  };
  return (
    <div className="onboarding" data-tauri-drag-region>
      {addingAccount && (
        <button className="onboarding-cancel-add" disabled={busy} onClick={onCancelAdd}>
          <ArrowLeft size={14} /> back to my account
        </button>
      )}
      <NoiseMark size={54} />
      <h1>noise</h1>
      <p>no phone number. no email. just your noise ID and password.</p>
      <div className="onboarding-tabs">
        <button className={mode === "create" ? "active" : ""} onClick={() => setMode("create")}>create identity</button>
        <button className={mode === "signin" ? "active" : ""} onClick={() => setMode("signin")}>sign in</button>
      </div>
      {mode === "create" ? <>
        <input autoFocus value={username} maxLength={MAX_DISPLAY_NAME_LENGTH} aria-invalid={createAttempted && !usernameReady} onChange={(event) => setUsername(event.target.value)} placeholder="display name" />
        <label className="onboarding-birth-date">
          <span>birth date · Noise is 18+</span>
          <input type="date" autoComplete="bday" value={birthDate} aria-invalid={createAttempted && !birthDateReady} onChange={(event) => setBirthDate(event.target.value)} />
          <small>checked for 18+ eligibility, then discarded</small>
        </label>
        <input type="password" autoComplete="new-password" value={password} aria-describedby="password-requirements" aria-invalid={createAttempted && !passwordReady} onChange={(event) => setPassword(event.target.value)} placeholder="strong password" />
        <input type="password" autoComplete="new-password" value={confirmation} aria-describedby="password-requirements" aria-invalid={createAttempted && password !== confirmation} onChange={(event) => setConfirmation(event.target.value)} placeholder="confirm password" onKeyDown={(event) => { if (event.key === "Enter") submitCreate(); }} />
        <div id="password-requirements" className={`password-requirements${createAttempted && !createReady ? " invalid" : ""}`} aria-live="polite">
          <strong>password requirements</strong>
          <ul>
            {passwordRequirements.map((requirement) => <li key={requirement.label} className={requirement.met ? "met" : ""}><Check size={11} /> {requirement.label}</li>)}
          </ul>
          {createAttempted && !createReady && <span><TriangleAlert size={12} /> {!usernameReady ? "enter a display name to continue" : !birthDateReady ? "enter your birth date to continue" : "complete the requirements above to continue"}</span>}
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
  return <div className="empty-group"><MessagesSquare size={38} /><h2>no direct messages</h2><p>start a conversation with a person or noise signature</p></div>;
}

type GlobalSearchChoice =
  | { kind: "message"; result: SearchMessageResult }
  | { kind: "location"; result: SearchLocationResult }
  | { kind: "person"; result: SearchPersonResult };

function NewDirectDialog({
  people,
  selfPublicKey,
  busy,
  onClose,
  onChoose,
  onSignal,
}: {
  people: DirectSummary[];
  selfPublicKey: string;
  busy: boolean;
  onClose: () => void;
  onChoose: (person: PersonSummary) => Promise<void>;
  onSignal: (signal: string) => Promise<void>;
}) {
  const [query, setQuery] = useState("");
  const [signal, setSignal] = useState("");
  const [resolving, setResolving] = useState(false);
  const uniquePeople = new Map<string, DirectSummary>();
  for (const person of people) {
    if (person.public_key !== selfPublicKey) uniquePeople.set(person.public_key, person);
  }
  const normalizedQuery = query.trim().toLowerCase();
  const matches = [...uniquePeople.values()]
    .filter((person) => !normalizedQuery
      || person.username.toLowerCase().includes(normalizedQuery)
      || person.bio.toLowerCase().includes(normalizedQuery)
      || noiseSignature(person.public_key).toLowerCase().includes(normalizedQuery))
    .sort((left, right) => left.username.localeCompare(right.username))
    .slice(0, 12);
  const normalizedSignal = signal.replace(/[\s-]/g, "").toUpperCase();
  const signalReady = /^[0-9ABCDEFGHJKMNPQRSTVWXYZ]{12}$/.test(normalizedSignal);
  const submitSignal = async () => {
    if (!signalReady || busy || resolving) return;
    setResolving(true);
    try {
      await onSignal(normalizedSignal);
    } finally {
      setResolving(false);
    }
  };

  return (
    <Modal onClose={onClose} className="new-direct-modal">
      <DialogHeading
        icon={<MessageCircle />}
        title="new direct message"
        detail="choose someone you know or enter their noise signature"
      />
      <label className="new-direct-search">
        <Search size={15} />
        <input
          autoFocus
          value={query}
          placeholder="Search people"
          onChange={(event) => setQuery(event.target.value)}
        />
      </label>
      <div className="new-direct-people">
        {matches.length ? matches.map((person) => (
          <button
            key={person.public_key}
            disabled={busy}
            onClick={() => void onChoose({
              ...person,
              presence_status: "offline",
            })}
          >
            <PresenceAvatar
              name={person.username}
              image={person.avatar}
              size={34}
              status="offline"
            />
            <span className="new-direct-person-copy">
              <strong>{person.username}</strong>
              {person.bio && <small>{person.bio}</small>}
              <small className="new-direct-person-signature">{noiseSignature(person.public_key)}</small>
            </span>
          </button>
        )) : (
          <p>{query.trim() ? "No known people match that search." : "People from shared groups will appear here."}</p>
        )}
      </div>
      <div className="new-direct-divider"><span>or use a signal</span></div>
      <div className="new-direct-signal">
        <label>
          <small>noise signature</small>
          <input
            value={signal}
            spellCheck={false}
            autoCapitalize="none"
            autoCorrect="off"
            placeholder="TW5TKT-VZNX4D"
            onChange={(event) => setSignal(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter" && signalReady && !busy && !resolving) {
                event.preventDefault();
                void submitSignal();
              }
            }}
          />
        </label>
        <button
          className="primary"
          disabled={!signalReady || busy || resolving}
          onClick={() => void submitSignal()}
        >
          {busy || resolving ? <LoaderCircle className="spinner" size={14} /> : <ArrowUp size={14} />}
          start dm
        </button>
      </div>
      <p className="new-direct-privacy">
        Enter the signature exactly as shown in the other person’s settings. It does not reveal the private Noise ID used to sign in.
      </p>
    </Modal>
  );
}

const emptySearchResults: SearchResults = {
  messages: [],
  locations: [],
  people: [],
  has_more_history: false,
  older_scopes: [],
};

function GlobalSearchModal({
  onClose,
  onMessage,
  onLocation,
  onPerson,
  onLoadOlder,
}: {
  onClose: () => void;
  onMessage: (result: SearchMessageResult) => void;
  onLocation: (result: SearchLocationResult) => void;
  onPerson: (result: SearchPersonResult) => void;
  onLoadOlder: (scope: SearchHistoryScope) => Promise<void>;
}) {
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<SearchResults>(emptySearchResults);
  const [loading, setLoading] = useState(false);
  const [loadingOlder, setLoadingOlder] = useState(false);
  const [showOlderSearchStatus, setShowOlderSearchStatus] = useState(false);
  const [olderSearchError, setOlderSearchError] = useState<string | null>(null);
  const [selectedIndex, setSelectedIndex] = useState(0);
  const searchSequence = useRef(0);
  const olderSearchSequence = useRef(0);
  const loadingOlderRef = useRef(false);
  const queryRef = useRef("");
  const selectedRef = useRef<HTMLButtonElement | null>(null);
  const historySentinelRef = useRef<HTMLDivElement | null>(null);
  const groupMessages = results.messages.filter((result) => !result.direct_public_key);
  const directMessages = results.messages.filter((result) => result.direct_public_key);
  const canSearchOlderHistory = Boolean(
    query.trim()
    && results.has_more_history
    && results.older_scopes[0],
  );
  const choices: GlobalSearchChoice[] = [
    ...groupMessages.map((result): GlobalSearchChoice => ({ kind: "message", result })),
    ...directMessages.map((result): GlobalSearchChoice => ({ kind: "message", result })),
    ...results.locations.map((result): GlobalSearchChoice => ({ kind: "location", result })),
    ...results.people.map((result): GlobalSearchChoice => ({ kind: "person", result })),
  ];

  const runSearch = useCallback(async (value: string, resetSelection = true) => {
    const trimmed = value.trim();
    const sequence = ++searchSequence.current;
    if (!trimmed) {
      setResults(emptySearchResults);
      setLoading(false);
      return;
    }
    setLoading(true);
    try {
      const found = await noise<SearchResults>({
        action: "search_local",
        query: trimmed,
        limit: 60,
      });
      if (sequence === searchSequence.current) {
        setResults(found ?? emptySearchResults);
        if (resetSelection) setSelectedIndex(0);
      }
    } finally {
      if (sequence === searchSequence.current) setLoading(false);
    }
  }, []);

  useEffect(() => {
    queryRef.current = query;
    setOlderSearchError(null);
    setShowOlderSearchStatus(false);
    const timer = window.setTimeout(() => {
      void runSearch(query);
    }, 120);
    return () => window.clearTimeout(timer);
  }, [query, runSearch]);

  useEffect(() => () => {
    olderSearchSequence.current += 1;
  }, []);

  const loadOlderPage = useCallback(async () => {
    const requestedQuery = queryRef.current.trim();
    const scope = results.older_scopes[0];
    if (
      !requestedQuery
      || !scope
      || loadingOlderRef.current
      || olderSearchError
    ) return;

    const sequence = ++olderSearchSequence.current;
    loadingOlderRef.current = true;
    setLoadingOlder(true);
    setOlderSearchError(null);
    const statusTimer = window.setTimeout(() => {
      if (
        sequence === olderSearchSequence.current
        && requestedQuery === queryRef.current.trim()
      ) {
        setShowOlderSearchStatus(true);
      }
    }, 400);

    try {
      await onLoadOlder(scope);
      if (
        sequence !== olderSearchSequence.current
        || requestedQuery !== queryRef.current.trim()
      ) {
        return;
      }
      await runSearch(requestedQuery, false);
    } catch {
      if (
        sequence === olderSearchSequence.current
        && requestedQuery === queryRef.current.trim()
      ) {
        setOlderSearchError("Couldn’t search all history.");
      }
    } finally {
      window.clearTimeout(statusTimer);
      if (sequence === olderSearchSequence.current) {
        loadingOlderRef.current = false;
        setLoadingOlder(false);
        setShowOlderSearchStatus(false);
      }
    }
  }, [
    loadingOlder,
    olderSearchError,
    onLoadOlder,
    results.older_scopes,
    runSearch,
  ]);

  useEffect(() => {
    const sentinel = historySentinelRef.current;
    const scope = results.older_scopes[0];
    if (
      !sentinel
      || !query.trim()
      || !results.has_more_history
      || !scope
      || loadingOlder
      || olderSearchError
    ) {
      return;
    }

    const root = sentinel.closest(".global-search-results");
    const observer = new IntersectionObserver((entries) => {
      if (entries.some((entry) => entry.isIntersecting)) {
        void loadOlderPage();
      }
    }, {
      root,
      rootMargin: "0px 0px 120px",
      threshold: 0.01,
    });
    observer.observe(sentinel);
    return () => observer.disconnect();
  }, [
    loadOlderPage,
    loadingOlder,
    olderSearchError,
    query,
    results.has_more_history,
    results.older_scopes,
  ]);

  useEffect(() => {
    selectedRef.current?.scrollIntoView({ block: "nearest" });
  }, [selectedIndex]);

  const openChoice = (choice?: GlobalSearchChoice) => {
    if (!choice) return;
    if (choice.kind === "message") onMessage(choice.result);
    else if (choice.kind === "location") onLocation(choice.result);
    else onPerson(choice.result);
  };
  const messageDescription = (result: SearchMessageResult) =>
    result.text.trim()
      || (result.attachment?.mime_type.startsWith("video/") ? "Video"
        : result.attachment?.mime_type.startsWith("audio/") ? "Audio"
          : result.attachment ? "Photo" : "Message");
  let itemIndex = -1;
  const nextIndex = () => {
    itemIndex += 1;
    return itemIndex;
  };

  return (
    <Modal onClose={onClose} className="global-search-modal">
      <div className="global-search-input">
        <Search size={18} />
        <input
          autoFocus
          value={query}
          placeholder="Search messages, groups, topics, and people"
          aria-label="search noise"
          onChange={(event) => setQuery(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "ArrowDown") {
              event.preventDefault();
              if (choices.length) {
                setSelectedIndex((current) => Math.min(choices.length - 1, current + 1));
              }
            } else if (event.key === "ArrowUp") {
              event.preventDefault();
              setSelectedIndex((current) => Math.max(0, current - 1));
            } else if (event.key === "Enter") {
              event.preventDefault();
              openChoice(choices[selectedIndex]);
            }
          }}
        />
        {loading && <LoaderCircle className="spinner" size={16} />}
        <kbd>{navigator.platform.toLowerCase().includes("mac") ? "⌘K" : "Ctrl K"}</kbd>
      </div>
      <div className="global-search-results">
        {!query.trim() && (
          <div className="global-search-empty">
            <Search size={28} />
            <strong>search your noise</strong>
            <span>Decrypted messages are searched privately on this device.</span>
          </div>
        )}
        {query.trim()
          && !loading
          && !canSearchOlderHistory
          && choices.length === 0 && (
          <div className="global-search-empty">
            <Search size={28} />
            <strong>no matches</strong>
            <span>Try another word.</span>
          </div>
        )}
        {[
          { title: "Group messages", messages: groupMessages },
          { title: "Direct messages", messages: directMessages },
        ].map((section) => section.messages.length > 0 && (
          <section className="global-search-section" key={section.title}>
            <h3>{section.title}</h3>
            {section.messages.map((result) => {
              const index = nextIndex();
              const poster = result.attachment ? mediaPoster(result.attachment) : undefined;
              const location = result.direct_public_key
                ? "Direct message"
                : [result.group_name, result.topic_name].filter(Boolean).join(" / ");
              return (
                <button
                  key={`message:${result.event_id}`}
                  ref={index === selectedIndex ? selectedRef : undefined}
                  className={`global-search-row message-result ${index === selectedIndex ? "selected" : ""}`}
                  onMouseMove={() => setSelectedIndex(index)}
                  onClick={() => onMessage(result)}
                >
                  <span className="search-result-avatar"><Avatar name={result.username} image={result.avatar} size={34} /></span>
                  <span className="search-result-copy">
                    <span><strong>{result.username}</strong><time>{new Date(result.created_at_millis).toLocaleDateString([], { month: "short", day: "numeric", year: new Date(result.created_at_millis).getFullYear() === new Date().getFullYear() ? undefined : "numeric" })}</time></span>
                    <small>{messageDescription(result)}</small>
                    <em>{location}</em>
                  </span>
                  {result.attachment && (
                    <span className="search-result-media">
                      {poster ? <img src={poster} alt="" /> : result.attachment.mime_type.startsWith("video/") ? <Play size={15} /> : <Images size={15} />}
                    </span>
                  )}
                </button>
              );
            })}
          </section>
        ))}
        {results.locations.length > 0 && (
          <section className="global-search-section">
            <h3>Groups &amp; Topics</h3>
            {results.locations.map((result) => {
              const index = nextIndex();
              return (
                <button
                  key={`location:${result.group_id}:${result.topic_id ?? result.topic_name ?? "group"}`}
                  ref={index === selectedIndex ? selectedRef : undefined}
                  className={`global-search-row ${index === selectedIndex ? "selected" : ""}`}
                  onMouseMove={() => setSelectedIndex(index)}
                  onClick={() => onLocation(result)}
                >
                  {result.topic_name
                    ? <span className="search-topic-icon">{result.topic_icon || "💬"}</span>
                    : <Avatar name={result.group_name} image={result.group_avatar} size={34} square />}
                  <span className="search-result-copy">
                    <strong>{result.topic_name ?? result.group_name}</strong>
                    <small>{result.topic_name ? result.group_name : "Group"}</small>
                  </span>
                </button>
              );
            })}
          </section>
        )}
        {results.people.length > 0 && (
          <section className="global-search-section">
            <h3>People &amp; DMs</h3>
            {results.people.map((result) => {
              const index = nextIndex();
              return (
                <button
                  key={`person:${result.public_key}`}
                  ref={index === selectedIndex ? selectedRef : undefined}
                  className={`global-search-row ${index === selectedIndex ? "selected" : ""}`}
                  onMouseMove={() => setSelectedIndex(index)}
                  onClick={() => onPerson(result)}
                >
                  <Avatar name={result.username} image={result.avatar} size={34} />
                  <span className="search-result-copy">
                    <strong>{result.username}</strong>
                    <small>{result.bio || (result.has_direct ? "Direct message" : "noise profile")}</small>
                  </span>
                </button>
              );
            })}
          </section>
        )}
        {canSearchOlderHistory && (
          <div
            ref={historySentinelRef}
            className="search-history-sentinel"
            aria-live="polite"
          >
            {olderSearchError ? (
              <>
                <span>{olderSearchError}</span>
                <button
                  className="search-history-retry"
                  onClick={() => setOlderSearchError(null)}
                >
                  retry
                </button>
              </>
            ) : showOlderSearchStatus ? (
              <>
                <LoaderCircle className="spinner" size={14} />
                <span>looking further back…</span>
              </>
            ) : null}
          </div>
        )}
      </div>
      <div className="global-search-footer">
        <span>↑↓ navigate</span><span>↵ open</span><span>esc close</span>
        <strong><Shield size={11} /> local, decrypted search</strong>
      </div>
    </Modal>
  );
}

function MakeDialog({ busy, adultContentEnabled, onClose, onSubmit }: { busy: boolean; adultContentEnabled: boolean; onClose: () => void; onSubmit: (name: string, contentRating: GroupContentRating) => Promise<boolean> }) {
  const [name, setName] = useState("");
  const [adult, setAdult] = useState(false);
  return <Modal onClose={onClose}><DialogHeading icon={<UsersRound />} title="create group" detail="give the group a name" /><input autoFocus value={name} onChange={(event) => setName(event.target.value)} placeholder="group name" /><label className="settings-toggle-row make-adult-group"><span><strong>18+ group</strong><small>{adultContentEnabled ? "allows consensual adult content and adds a permanent 18+ label" : "enable adult groups in Settings → Content first"}</small></span><input type="checkbox" role="switch" checked={adult} disabled={!adultContentEnabled} onChange={(event) => setAdult(event.target.checked)} /></label><DialogButtons onClose={onClose}><button className="primary" disabled={!name.trim() || busy} onClick={() => void onSubmit(name.trim(), adult ? "adult" : "general")}>create group</button></DialogButtons></Modal>;
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
  return <Modal onClose={onClose}><DialogHeading icon={<NoiseMark size={28} />} title="this is your noise ID" detail="you’ll use it with your password to sign in on any device" /><div className="frequency-card">{noiseId}</div><p className="noise-id-warning">Save this somewhere private. noise cannot recover it for you.</p><DialogButtons><CopyButton value={noiseId} label="copy noise ID" /><button className="primary" onClick={onClose}>I saved it</button></DialogButtons></Modal>;
}

function SettingsDialog({ profile, adultAccess, devices, blockedPeople, busy, onClose, onSave, onUnblock, onAdultContentChange, onRevokeDevice, onSummary, onLogout, onDeleteAccount }: { profile: IdentitySummary; adultAccess: AdultAccessSummary; devices: DeviceSummary[]; blockedPeople: DirectSummary[]; busy: boolean; onClose: () => void; onSave: (username: string, bio: string, avatar: string | null, remove: boolean, directMessagePolicy: DirectMessagePolicy) => Promise<boolean>; onUnblock: (person: DirectSummary) => Promise<boolean>; onAdultContentChange: (enabled: boolean) => Promise<boolean>; onRevokeDevice: (device: DeviceSummary) => Promise<boolean>; onSummary: (summary: LocalSummary) => void; onLogout: () => void; onDeleteAccount: () => void }) {
  const [tab, setTab] = useState<"identity" | "album" | "privacy" | "content" | "blocks" | "account">("identity");
  const [username, setUsername] = useState(profile.username);
  const [bio, setBio] = useState(profile.bio);
  const [directMessagePolicy, setDirectMessagePolicy] = useState<DirectMessagePolicy>(profile.direct_message_policy);
  const [saving, setSaving] = useState(false);
  const [contentSaving, setContentSaving] = useState(false);
  const [confirmingDeviceId, setConfirmingDeviceId] = useState<string | null>(null);
  const image = useImageSelection();
  const settingsChanged = username.trim() !== profile.username
    || bio !== profile.bio
    || directMessagePolicy !== profile.direct_message_policy
    || image.base64 !== null
    || image.removed;
  const displayNameLength = Array.from(username.trim()).length;
  const locked = busy || saving || contentSaving;
  const saveSettings = async () => {
    if (locked || !settingsChanged) return;
    setSaving(true);
    try {
      await onSave(
        username.trim(),
        bio,
        image.base64,
        image.removed,
        directMessagePolicy,
      );
    } finally {
      setSaving(false);
    }
  };
  return (
    <Modal onClose={onClose} closeDisabled={saving || contentSaving} className="user-settings-modal">
      <DialogHeading icon={<Settings2 />} title="settings" detail="your noise identity" />
      <div className="group-settings-tabs user-tabs" role="tablist" aria-label="user settings sections">
        <button disabled={locked} className={tab === "identity" ? "active" : ""} role="tab" aria-selected={tab === "identity"} onClick={() => setTab("identity")}>Identity</button>
        <button disabled={locked} className={tab === "album" ? "active" : ""} role="tab" aria-selected={tab === "album"} onClick={() => setTab("album")}>Album</button>
        <button disabled={locked} className={tab === "privacy" ? "active" : ""} role="tab" aria-selected={tab === "privacy"} onClick={() => setTab("privacy")}>Privacy</button>
        <button disabled={locked} className={tab === "content" ? "active" : ""} role="tab" aria-selected={tab === "content"} onClick={() => setTab("content")}>Content</button>
        <button disabled={locked} className={tab === "blocks" ? "active" : ""} role="tab" aria-selected={tab === "blocks"} onClick={() => setTab("blocks")}>Blocks{blockedPeople.length > 0 && <i>{blockedPeople.length}</i>}</button>
        <button disabled={locked} className={tab === "account" ? "active" : ""} role="tab" aria-selected={tab === "account"} onClick={() => setTab("account")}>Account</button>
      </div>
      <div className="group-settings-panel user-settings-panel" role="tabpanel">
        {tab === "identity" && <div className="group-settings-identity">
          <div className="identity-profile-row">
            <div className="identity-editor"><ImagePicker name={username} existing={profile.avatar} selection={image} disabled={locked} /><small>public identity</small></div>
            <LabeledArea label="display name" count={`${displayNameLength}/${MAX_DISPLAY_NAME_LENGTH}`}><input disabled={locked} value={username} maxLength={MAX_DISPLAY_NAME_LENGTH} onChange={(event) => setUsername(event.target.value)} /></LabeledArea>
          </div>
          <LabeledArea label="bio" count={`${bio.length}/160`}><textarea disabled={locked} value={bio} onChange={(event) => setBio(event.target.value)} /></LabeledArea>
          <section className="settings-section identity-signal-setting">
            <h3>noise signature</h3>
            <div className="noise-id-setting">
              <strong>{noiseSignature(profile.public_key)}</strong>
              <ContactSignalCopyButton publicKey={profile.public_key} />
            </div>
            <p>Share this signature so someone can start an encrypted DM with you. Noise keeps it discoverable automatically, and it does not reveal your private Noise ID.</p>
          </section>
        </div>}
        {tab === "album" && <ProfileAlbumDialog
          embedded
          editable
          person={{
            public_key: profile.public_key,
            username: profile.username,
            bio: profile.bio,
            avatar: profile.avatar,
            album: profile.album,
            accepts_direct_messages: profile.accepts_direct_messages,
            direct_message_policy: profile.direct_message_policy,
            presence_status: "online",
          }}
          onSummary={onSummary}
        />}
        {tab === "privacy" && <section className="settings-section user-privacy-settings">
          <h3>direct messages</h3>
          <div className="direct-message-policy" role="radiogroup" aria-label="who can direct message you">
            {([
              ["everyone", "everyone", "allow anyone with your noise signature or a shared group to message you"],
              ["shared_groups", "shared groups only", "only allow messages from current members of your groups"],
              ["nobody", "nobody", "do not accept incoming direct messages"],
            ] as const).map(([value, label, detail]) => (
              <button
                type="button"
                role="radio"
                aria-checked={directMessagePolicy === value}
                className={directMessagePolicy === value ? "selected" : ""}
                disabled={locked}
                key={value}
                onClick={() => setDirectMessagePolicy(value)}
              >
                <span><strong>{label}</strong><small>{detail}</small></span>
                <i>{directMessagePolicy === value && <Check size={13} />}</i>
              </button>
            ))}
          </div>
        </section>}
        {tab === "content" && <section className="settings-section user-content-settings">
          <h3>adult groups</h3>
          <label className="settings-toggle-row">
            <span>
              <strong>show 18+ groups</strong>
              <small>consensual adult content is allowed in groups marked 18+. These groups stay hidden unless you turn this on.</small>
            </span>
            <input
              type="checkbox"
              role="switch"
              checked={adultAccess.adult_content_enabled}
              disabled={locked || !adultAccess.age_attested}
              onChange={(event) => {
                const enabled = event.target.checked;
                setContentSaving(true);
                void onAdultContentChange(enabled).finally(() => setContentSaving(false));
              }}
            />
          </label>
          <p>Your birth date is not stored. Existing launch accounts were migrated as known adults; this separate preference still started off.</p>
        </section>}
        {tab === "blocks" && <section className="settings-section user-block-settings">
          <h3>blocked users</h3>
          {blockedPeople.length ? <div className="banned-user-list">{blockedPeople.map((person) => <div className="banned-user-row" key={person.public_key}><Avatar name={person.username} image={person.avatar} size={30} /><span><strong>{person.username}</strong><small>{person.bio || "hidden from your noise"}</small></span><button disabled={locked} onClick={() => void onUnblock(person)}>unblock</button></div>)}</div> : <p className="empty-banned-users">you have not blocked anyone</p>}
        </section>}
        {tab === "account" && <div className="user-account-settings">
          {profile.noise_id && <section className="settings-section"><h3>noise ID</h3><div className="noise-id-setting"><strong>{profile.noise_id}</strong><CopyButton value={profile.noise_id} label="copy" /></div><p>Use this with your password to sign in on another device.</p></section>}
          {profile.noise_id && <section className="settings-section">
            <h3>signed-in devices</h3>
            <div className="device-session-list">
              {devices.map((device) => <div className="device-session-row" key={device.device_id}>
                <Laptop size={18} />
                <span>
                  <strong>{device.name}{device.is_current && <i>this device</i>}</strong>
                  <small>{device.platform} · {formatDeviceActivity(device.last_seen_at_millis)}</small>
                </span>
                {!device.is_current && <button
                  className={confirmingDeviceId === device.device_id ? "danger confirm" : "danger"}
                  disabled={locked}
                  onClick={() => {
                    if (confirmingDeviceId !== device.device_id) {
                      setConfirmingDeviceId(device.device_id);
                      return;
                    }
                    setConfirmingDeviceId(null);
                    void onRevokeDevice(device);
                  }}
                >{confirmingDeviceId === device.device_id ? "confirm" : "log out"}</button>}
              </div>)}
            </div>
            <p>Logging out a device removes its official Noise session the next time it connects.</p>
          </section>}
          {profile.noise_id && <section className="settings-section account-session"><span><strong>log out on this device</strong><small>Your encrypted identity remains available on the relay network.</small></span><button disabled={locked} onClick={onLogout}>log out</button></section>}
          <section className="settings-danger"><span><strong>delete account</strong><small>erase this identity and its encrypted account vault</small></span><button className="danger" disabled={locked} onClick={onDeleteAccount}>delete account</button></section>
        </div>}
      </div>
      <DialogButtons onClose={onClose} closeDisabled={saving || contentSaving} closeLabel={settingsChanged ? "cancel" : "close"}>
        {tab === "identity" && (profile.avatar || image.preview) && <button className="danger" disabled={locked} onClick={image.remove}>remove photo</button>}
        {settingsChanged && <button className="primary" disabled={!username.trim() || displayNameLength > MAX_DISPLAY_NAME_LENGTH || bio.length > 160 || locked} onClick={() => void saveSettings()}>
          {saving && <LoaderCircle className="spinner" size={13} />} {saving ? "saving" : "save settings"}
        </button>}
      </DialogButtons>
    </Modal>
  );
}

function GroupSettingsDialog({ group, adultContentEnabled, bannedMembers, presenceStatuses, busy, onClose, onSave, onUnban, onRotateFrequency }: { group: GroupSummary; adultContentEnabled: boolean; bannedMembers: BannedMemberSummary[]; presenceStatuses: Map<string, PresenceStatus>; busy: boolean; onClose: () => void; onSave: (name: string, description: string, accentColor: string, contentRating: GroupContentRating, avatar: string | null, removeAvatar: boolean, background: string | null, removeBackground: boolean, mobileBackground: string | null, removeMobileBackground: boolean, membersCanSendMessages: boolean, membersCanSendMedia: boolean) => Promise<boolean>; onUnban: (member: BannedMemberSummary) => Promise<boolean>; onRotateFrequency: (revokeOnly: boolean) => Promise<boolean> }) {
  const [tab, setTab] = useState<"identity" | "appearance" | "general" | "banned">("identity");
  const [revokeArmed, setRevokeArmed] = useState(false);
  const [name, setName] = useState(group.name);
  const [description, setDescription] = useState(group.description);
  const [accentColor, setAccentColor] = useState(group.accent_color || DEFAULT_ACCENT_COLOR);
  const [contentRating, setContentRating] = useState<GroupContentRating>(group.content_rating);
  const [membersCanSendMessages, setMembersCanSendMessages] = useState(group.members_can_send_messages);
  const [membersCanSendMedia, setMembersCanSendMedia] = useState(group.members_can_send_media);
  const image = useImageSelection();
  const background = useBackgroundSelection("desktop");
  const mobileBackground = useBackgroundSelection("mobile");
  const hasGroupIcon = Boolean(image.preview || (!image.removed && group.avatar));
  const settingsChanged = name.trim() !== group.name
    || description !== group.description
    || accentColor !== group.accent_color
    || contentRating !== group.content_rating
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
          <h3>content label</h3>
          <label className="settings-toggle-row"><span><strong>18+ group</strong><small>{group.content_rating === "adult" ? "this permanent label cannot be removed" : adultContentEnabled ? "permanently marks the group and replaces its frequency so old unlabeled invites stop working" : "enable adult groups in your Content settings first"}</small></span><input type="checkbox" role="switch" checked={contentRating === "adult"} disabled={group.content_rating === "adult" || !adultContentEnabled} onChange={(event) => setContentRating(event.target.checked ? "adult" : "general")} /></label>
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
        {settingsChanged && <button className="primary" disabled={!name.trim() || name.length > 80 || description.length > 200 || background.busy || mobileBackground.busy || busy} onClick={() => void onSave(name.trim(), description, accentColor, contentRating, image.base64, image.removed, background.base64, background.removed, mobileBackground.base64, mobileBackground.removed, membersCanSendMessages, membersCanSendMedia)}>save settings</button>}
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
  return <Modal onClose={onClose}><DialogHeading icon={<Trash2 />} title="delete conversation?" detail={direct.username} /><p className="deletion-warning">Choose whether noise should erase this thread only from this device or send a signed erasure to both users’ noise clients.</p><div className="direct-delete-options"><button disabled={busy} onClick={() => void onDelete(false)}><strong>just for me</strong><small>erase this device’s history and cached media</small></button><button className="danger" disabled={busy} onClick={() => void onDelete(true)}><strong>for both of us</strong><small>ask all synced noise clients to erase the thread</small></button></div><DialogButtons onClose={onClose} closeLabel="cancel">{busy && <LoaderCircle className="spinner" size={14} />}</DialogButtons></Modal>;
}

function DeleteAccountDialog({ busy, ownedGroupCount, onClose, onDelete }: { busy: boolean; ownedGroupCount: number; onClose: () => void; onDelete: (deleteGroupMessages: boolean, deleteDirectThreads: boolean) => Promise<boolean> }) {
  const [deleteGroupMessages, setDeleteGroupMessages] = useState(false);
  const [deleteDirectThreads, setDeleteDirectThreads] = useState(false);
  return <Modal onClose={onClose}><DialogHeading icon={<UserRoundX />} title="delete your account?" detail="this permanently erases the identity on this device" />{ownedGroupCount > 0 && <p className="deletion-warning">{ownedGroupCount === 1 ? "The group you founded" : `The ${ownedGroupCount} groups you founded`} will also be permanently deleted so no group is left with a missing founder.</p>}<div className="account-delete-options"><label className="ban-history-option"><input type="checkbox" checked={deleteGroupMessages} onChange={(event) => setDeleteGroupMessages(event.target.checked)} /><span><strong>delete all messages I sent in groups</strong><small>send a signed removal to every group before leaving</small></span></label><label className="ban-history-option"><input type="checkbox" checked={deleteDirectThreads} onChange={(event) => setDeleteDirectThreads(event.target.checked)} /><span><strong>delete all DM threads</strong><small>ask both users’ noise clients to erase every thread and cached media</small></span></label></div><p className="deletion-fine-print">noise can erase relay data and tell official clients to forget it, but it cannot recall screenshots, exports, backups, or modified clients.</p><DialogButtons onClose={onClose}><button className="delete-confirm" disabled={busy} onClick={() => void onDelete(deleteGroupMessages, deleteDirectThreads)}>{busy && <LoaderCircle className="spinner" size={13} />} delete account</button></DialogButtons></Modal>;
}

function LogoutDialog({ busy, onClose, onLogout }: { busy: boolean; onClose: () => void; onLogout: () => Promise<boolean> }) {
  return <Modal onClose={onClose} compact><DialogHeading icon={<LogOut />} title="log out on this device?" detail="your account stays encrypted on the relay network" /><p className="deletion-warning">Local identity data and cached media will be removed. Sign back in with your noise ID and password.</p><DialogButtons onClose={onClose}><button className="primary" disabled={busy} onClick={() => void onLogout()}>{busy && <LoaderCircle className="spinner" size={13} />} log out</button></DialogButtons></Modal>;
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
      <p className="deletion-warning">This removes the message from the group history for everyone. It cannot be undone in noise.</p>
      <DialogButtons onClose={onClose}>
        <button className="delete-confirm" disabled={busy} onClick={() => void onDelete()}>
          {busy && <LoaderCircle className="spinner" size={13} />} delete message
        </button>
      </DialogButtons>
    </Modal>
  );
}

function BlockPersonDialog({ person, busy, onClose, onBlock }: { person: PersonSummary; busy: boolean; onClose: () => void; onBlock: () => Promise<boolean> }) {
  return (
    <Modal onClose={onClose}>
      <DialogHeading
        icon={<ShieldOff />}
        title={`block ${person.username}?`}
        detail="you will disappear from each other across noise"
      />
      <p className="deletion-warning">noise will hide both profiles, posts, reactions, and presence from each other. Direct messaging will stop, and this conversation and its cached media will be removed from your devices.</p>
      <DialogButtons onClose={onClose}>
        <button className="delete-confirm" disabled={busy} onClick={() => void onBlock()}>
          {busy && <LoaderCircle className="spinner" size={13} />} block user
        </button>
      </DialogButtons>
    </Modal>
  );
}

function PersonDialog({ person, canMessage, canBlock, onMessage, onAlbum, onBlock, onClose }: { person: PersonSummary; canMessage: boolean; canBlock: boolean; onMessage: () => void; onAlbum: () => void; onBlock: () => void; onClose: () => void }) {
  return <Modal onClose={onClose} compact><div className="person-card"><PresenceAvatar name={person.username} image={person.avatar} size={72} status={person.presence_status ?? "offline"} /><h2>{person.username}</h2><div className="noise-signature"><small>noise signature</small><strong>{noiseSignature(person.public_key)}</strong></div><p>{person.bio || "no bio yet"}</p><div className="person-actions">{canMessage && <button className="profile-message" onClick={onMessage}><MessageCircle size={15} /> dm</button>}<button className="profile-album" onClick={onAlbum}><Images size={15} /> {albumButtonLabel(person.album)}</button>{canBlock && <button className="profile-block" onClick={onBlock}><ShieldOff size={15} /> block</button>}</div></div></Modal>;
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

function CreateTopicDialog({
  busy,
  onClose,
  onCreate,
}: {
  busy: boolean;
  onClose: () => void;
  onCreate: (name: string, icon: string) => Promise<boolean>;
}) {
  const [name, setName] = useState("");
  const [icon, setIcon] = useState("💬");
  return (
    <Modal onClose={onClose} compact>
      <div className="topic-dialog">
        <div className="dialog-heading">
          <span className="topic-dialog-icon">{icon}</span>
          <span><strong>new topic</strong><small>shared with everyone in this group</small></span>
        </div>
        <TopicIconPicker value={icon} onChange={setIcon} />
        <label>
          <span>name</span>
          <input
            autoFocus
            maxLength={80}
            value={name}
            onChange={(event) => setName(event.target.value)}
            placeholder="Announcements"
          />
        </label>
        <div className="dialog-actions">
          <button className="secondary" disabled={busy} onClick={onClose}>cancel</button>
          <button disabled={busy || !name.trim()} onClick={() => void onCreate(name.trim(), icon)}>
            {busy ? <LoaderCircle className="spinner" size={14} /> : <Plus size={14} />} create topic
          </button>
        </div>
      </div>
    </Modal>
  );
}

function TopicSettingsDialog({
  topic,
  busy,
  onClose,
  onSave,
  onArchive,
}: {
  topic: TopicSummary;
  busy: boolean;
  onClose: () => void;
  onSave: (name: string, icon: string, locked: boolean) => Promise<boolean>;
  onArchive: () => Promise<boolean>;
}) {
  const [name, setName] = useState(topic.name);
  const [icon, setIcon] = useState(topic.icon || "💬");
  const [locked, setLocked] = useState(topic.locked);
  const [archiveArmed, setArchiveArmed] = useState(false);
  return (
    <Modal onClose={onClose} compact>
      <div className="topic-dialog">
        <div className="dialog-heading">
          <span className="topic-dialog-icon">{icon}</span>
          <span><strong>topic settings</strong><small>names are encrypted; membership stays shared</small></span>
        </div>
        <TopicIconPicker value={icon} onChange={setIcon} />
        <label>
          <span>name</span>
          <input
            autoFocus
            maxLength={80}
            value={name}
            onChange={(event) => setName(event.target.value)}
          />
        </label>
        <label className="topic-lock-toggle">
          <input
            type="checkbox"
            checked={locked}
            onChange={(event) => setLocked(event.target.checked)}
          />
          <span><strong>lock topic</strong><small>only moderators can post</small></span>
        </label>
        <button
          className="topic-archive danger"
          disabled={busy}
          onClick={() => {
            if (!archiveArmed) {
              setArchiveArmed(true);
              return;
            }
            void onArchive();
          }}
        >
          <Trash2 size={14} /> {archiveArmed ? "click again to archive" : "archive topic"}
        </button>
        <div className="dialog-actions">
          <button className="secondary" disabled={busy} onClick={onClose}>cancel</button>
          <button
            disabled={busy || !name.trim()}
            onClick={() => void onSave(name.trim(), icon, locked)}
          >
            {busy ? <LoaderCircle className="spinner" size={14} /> : <Check size={14} />} save
          </button>
        </div>
      </div>
    </Modal>
  );
}

function TopicIconPicker({ value, onChange }: { value: string; onChange: (icon: string) => void }) {
  const button = useRef<HTMLButtonElement | null>(null);
  const [pickerPosition, setPickerPosition] = useState<{ x: number; y: number } | null>(null);
  return (
    <fieldset className="topic-icon-picker">
      <legend>icon</legend>
      <button
        ref={button}
        type="button"
        className="topic-icon-select"
        onClick={() => {
          const bounds = button.current?.getBoundingClientRect();
          if (bounds) setPickerPosition({ x: bounds.left, y: bounds.bottom + 8 });
        }}
      >
        <span>{value}</span>
        choose emoji
      </button>
      {pickerPosition && (
        <ReactionPicker
          x={pickerPosition.x}
          y={pickerPosition.y}
          onClose={() => setPickerPosition(null)}
          onPick={(icon) => {
            onChange(icon);
            setPickerPosition(null);
          }}
        />
      )}
    </fieldset>
  );
}

function Modal({ children, onClose, compact = false, wide = false, closeDisabled = false, className = "" }: { children: React.ReactNode; onClose: () => void; compact?: boolean; wide?: boolean; closeDisabled?: boolean; className?: string }) {
  return <div className="modal-backdrop" onMouseDown={closeDisabled ? undefined : onClose}><section className={`modal ${compact ? "compact" : ""} ${wide ? "wide" : ""} ${className}`.trim()} onMouseDown={(event) => event.stopPropagation()}><button className="modal-close" disabled={closeDisabled} onClick={onClose} aria-label={closeDisabled ? "saving settings" : "close"}>{closeDisabled ? <LoaderCircle className="spinner" size={14} /> : <X size={15} />}</button>{children}</section></div>;
}

function DialogHeading({ icon, title, detail }: { icon: React.ReactNode; title: string; detail: string }) {
  return <div className="dialog-heading"><span>{icon}</span><h2>{title}</h2><p>{detail}</p></div>;
}

function DialogButtons({ children, onClose, closeDisabled = false, closeLabel = "cancel" }: { children: React.ReactNode; onClose?: () => void; closeDisabled?: boolean; closeLabel?: string }) {
  return <div className="dialog-buttons">{onClose && <button disabled={closeDisabled} onClick={onClose}>{closeLabel}</button>}<span />{children}</div>;
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
    return <div className="update-banner ready"><span><strong>noise {status.version} is ready</strong><small>{status.restartFailed ? "restart failed · close and reopen noise" : "update installed · restart when you're ready"}</small></span><button onClick={restart}>{status.restartFailed ? "try restart" : "restart noise"}</button></div>;
  }
  return <div className="update-banner failed"><span><strong>update failed</strong><small>your current version is still intact</small></span><button onClick={retry}>try again</button><button className="update-dismiss" onClick={dismiss} aria-label="dismiss update"><X size={14} /></button></div>;
}

function Loading() {
  return (
    <div className="loading" role="status" aria-label="loading noise">
      <NoiseMark size={44} className="noise-loading-indicator" />
    </div>
  );
}

async function syncGroupEncryption(groupId?: string): Promise<GroupEncryptionStatus | null> {
  try {
    return await noise<GroupEncryptionStatus>({
      action: "sync_group_encryption",
      group_id: groupId ?? null,
      relays,
    });
  } catch (cause) {
    if (message(cause).includes("unknown variant `sync_group_encryption`")) return null;
    throw cause;
  }
}

async function cancelGroupLoading() {
  if (!isTauri) return;
  try {
    await noise({ action: "cancel_group_loading" });
  } catch (cause) {
    if (!message(cause).includes("unknown variant `cancel_group_loading`")) throw cause;
  }
}

async function cancelBackgroundLoading() {
  if (!isTauri) return;
  try {
    await noise({ action: "cancel_background_loading" });
  } catch (cause) {
    if (!message(cause).includes("unknown variant `cancel_background_loading`")) throw cause;
  }
}

async function cancelMediaDownloads() {
  mediaLoadScheduler.cancelQueued();
  if (!isTauri) return;
  try {
    await noise({ action: "cancel_media_loading" });
  } catch (cause) {
    if (!message(cause).includes("unknown variant `cancel_media_loading`")) throw cause;
  }
}

function isSupersededLoading(cause: unknown) {
  return message(cause).includes("loading superseded");
}

function isRelayConnectivityError(error: string) {
  return error.toLowerCase().includes("no relay was reachable");
}

type GroupActivityReadTarget = {
  topicId: string | null;
};

async function syncGroupActivity(
  groupId: string,
  readTarget?: GroupActivityReadTarget,
): Promise<GroupActivityResult | null> {
  try {
    const result = await noise<GroupActivityResult | LocalSummary>({
      action: "sync_group_activity",
      group_id: groupId,
      mark_read: Boolean(readTarget),
      read_topic_id: readTarget?.topicId ?? undefined,
      relays,
    });
    if (!result) return null;
    return "summary" in result
      ? result
      : { summary: result, conversation: null };
  } catch (cause) {
    const error = message(cause);
    if (
      error.includes("unknown variant `sync_group_activity`")
      || error.includes("unsupported noise action: sync_group_activity")
    ) return null;
    throw cause;
  }
}

async function syncTopicActivity(
  groupId: string,
  topicId: string,
  markRead = false,
): Promise<GroupActivityResult | null> {
  return noise<GroupActivityResult>({
    action: "sync_topic_activity",
    group_id: groupId,
    topic_id: topicId,
    mark_read: markRead,
    relays,
  });
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

async function markTopicRead(groupId: string, topicId: string): Promise<LocalSummary | null> {
  return noise<LocalSummary>({
    action: "mark_topic_read",
    group_id: groupId,
    topic_id: topicId,
  });
}

function EncryptionPending({ phase }: { phase: GroupEncryptionStatus["phase"] }) {
  return (
    <div className="encryption-pending">
      <Shield />
      <strong>
        {phase === "waiting_for_admission" ? "joining this group" : "restoring this group"}
      </strong>
      <span>
        {phase === "waiting_for_admission" || phase === "waiting_for_device"
          ? "any member who is online admits this identity automatically"
          : "restoring encrypted group access from your noise account"}
      </span>
      <small>nothing to approve — this screen updates on its own</small>
    </div>
  );
}

function formatTime(millis: number) {
  return new Intl.DateTimeFormat(undefined, { hour: "numeric", minute: "2-digit" }).format(new Date(millis));
}

function formatDeviceActivity(millis: number) {
  const elapsed = Math.max(0, Date.now() - millis);
  if (elapsed < 2 * 60_000) return "active now";
  if (elapsed < 60 * 60_000) return `active ${Math.floor(elapsed / 60_000)}m ago`;
  if (elapsed < 24 * 60 * 60_000) return `active ${Math.floor(elapsed / (60 * 60_000))}h ago`;
  return `active ${new Intl.DateTimeFormat(undefined, {
    month: "short",
    day: "numeric",
    year: new Date(millis).getFullYear() === new Date().getFullYear()
      ? undefined
      : "numeric",
  }).format(new Date(millis))}`;
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

async function uploadPendingMedia(
  pending: PendingMedia | null,
  action:
    | "upload_media_chunk"
    | "upload_media_chunk_to_group"
    | "upload_direct_media_chunk"
    | "upload_direct_media_chunk_to"
    | "upload_profile_media_chunk",
  onProgress: (progress: number) => void,
  signal: AbortSignal,
  target: Record<string, string> = {},
): Promise<MediaAttachment | null> {
  if (!pending) return null;
  const file = await pending.file;
  const mediaPreview = pending.mediaPreview;
  const chunks: MediaChunk[] = [];
  const mimeType = file.type || pending.mimeType;
  const streaming = mimeType.startsWith("video/") || mimeType.startsWith("audio/");
  const bootstrapBytes = 2 * 1024 * 1024;
  let offset = 0;
  while (offset < file.size) {
    if (signal.aborted) throw new Error("media upload cancelled");
    const chunkSize = streaming && offset < bootstrapBytes
      ? 256 * 1024
      : 1024 * 1024;
    const chunk = await noise<MediaChunk>({
      action,
      ...target,
      data_base64: await fileBase64(file.slice(offset, offset + chunkSize)),
      relays,
    });
    if (signal.aborted) throw new Error("media upload cancelled");
    if (!chunk) throw new Error("relay did not return a media chunk reference");
    chunks.push(chunk);
    onProgress(Math.min(95, Math.round(((offset + chunk.byte_length) / file.size) * 95)));
    offset += chunk.byte_length;
  }
  if (signal.aborted) throw new Error("media upload cancelled");
  const preview = mediaPreview ? await mediaPreview : null;
  if (signal.aborted) throw new Error("media upload cancelled");
  return {
    file_name: file.name || pending.name,
    mime_type: mimeType,
    byte_length: file.size,
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
