import {
  ArrowLeft,
  ArrowUp,
  AudioWaveform,
  Camera,
  Check,
  Copy,
  Globe2,
  Images,
  LoaderCircle,
  LogOut,
  MessageCircle,
  MessagesSquare,
  MoreHorizontal,
  Paperclip,
  Play,
  Plus,
  Radio,
  Reply,
  ScrollText,
  Settings2,
  Shield,
  ShieldOff,
  Trash2,
  TriangleAlert,
  UserRoundX,
  UsersRound,
  X,
} from "lucide-react";
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import type { CSSProperties } from "react";
import { relaunch } from "@tauri-apps/plugin-process";
import { check } from "@tauri-apps/plugin-updater";
import { isTauri, noise, prepareGroupBackground, prepareImage, relays } from "./api";
import { generateGroupAvatar, generateUserAvatar } from "./groupAvatar";
import type {
  AttachmentData,
  AvatarData,
  BannedMemberSummary,
  Conversation,
  DirectConversation,
  DirectSummary,
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
  ReportSummary,
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
  | { type: "ban_member"; member: MemberSummary }
  | { type: "leave_group"; group: GroupSummary }
  | { type: "delete_group"; group: GroupSummary }
  | { type: "delete_direct"; direct: DirectSummary }
  | { type: "delete_account" }
  | { type: "logout" }
  | { type: "person"; person: PersonSummary };

type PersonSummary = Pick<MemberSummary, "public_key" | "username" | "bio" | "avatar" | "accepts_direct_messages">;
type SidebarMode = "groups" | "directs";
const DEFAULT_ACCENT_COLOR = "#7758ED";
const ACCENT_PRESETS = ["#7758ED", "#E84D8A", "#F06A3C", "#E0A82E", "#43B581", "#24A6A6", "#4D82F0", "#A45EE5"];

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
};

type UpdateStatus =
  | { phase: "downloading"; version: string; progress: number | null }
  | { phase: "ready"; version: string }
  | { phase: "failed" };

function useAutoUpdater() {
  const [status, setStatus] = useState<UpdateStatus | null>(null);

  const checkForUpdate = useCallback(async () => {
    let updateFound = false;
    try {
      const update = await check();
      if (!update) return;
      updateFound = true;
      let downloaded = 0;
      let total = 0;
      setStatus({ phase: "downloading", version: update.version, progress: null });
      await update.downloadAndInstall((event) => {
        if (event.event === "Started") {
          total = event.data.contentLength ?? 0;
        } else if (event.event === "Progress") {
          downloaded += event.data.chunkLength;
        }
        const progress = total > 0 ? Math.min(100, Math.round((downloaded / total) * 100)) : null;
        setStatus({ phase: "downloading", version: update.version, progress });
      });
      setStatus({ phase: "ready", version: update.version });
    } catch (cause) {
      console.error("Noise update failed", cause);
      if (updateFound) setStatus({ phase: "failed" });
    }
  }, []);

  useEffect(() => {
    if (!isTauri || import.meta.env.DEV) return;
    const timer = window.setTimeout(() => void checkForUpdate(), 4000);
    return () => window.clearTimeout(timer);
  }, [checkForUpdate]);

  const restart = async () => {
    try {
      await relaunch();
    } catch (cause) {
      console.error("Noise could not restart after updating", cause);
      setStatus({ phase: "failed" });
    }
  };

  return {
    status,
    retry: () => void checkForUpdate(),
    restart: () => void restart(),
    dismiss: () => setStatus(null),
  };
}

export default function App() {
  const [summary, setSummary] = useState<LocalSummary | null>(null);
  const [conversation, setConversation] = useState<Conversation | null>(null);
  const [directConversation, setDirectConversation] = useState<DirectConversation | null>(null);
  const [sidebarMode, setSidebarMode] = useState<SidebarMode>("groups");
  const [dialog, setDialog] = useState<Dialog | null>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const updater = useAutoUpdater();
  const refreshGeneration = useRef(0);
  const groupConversationCache = useRef(new Map<string, Conversation>());
  const directConversationCache = useRef(new Map<string, DirectConversation>());
  const [groupMenu, setGroupMenu] = useState<{
    group: GroupSummary;
    x: number;
    y: number;
  } | null>(null);
  const [directMenu, setDirectMenu] = useState<{ direct: DirectSummary; x: number; y: number } | null>(null);

  const refresh = useCallback(async () => {
    const generation = ++refreshGeneration.current;
    const local = await noise<LocalSummary>({ action: "status" });
    if (generation !== refreshGeneration.current) return;
    setSummary(local);
    if (!local) return;

    if (sidebarMode === "groups") {
      const activeGroup = local.groups.find((group) => group.is_active);
      if (!activeGroup) {
        setConversation(null);
        return;
      }
      const cached = groupConversationCache.current.get(activeGroup.group_id);
      if (cached) setConversation(cached);
      const nextConversation = await noise<Conversation>({ action: "conversation", relays });
      const reconciled = await noise<LocalSummary>({ action: "status" });
      if (generation !== refreshGeneration.current) return;
      if (nextConversation) {
        groupConversationCache.current.set(nextConversation.group.group_id, nextConversation);
        setConversation(nextConversation);
      }
      setSummary(reconciled);
      return;
    }

    const activeDirect = local.directs.find((direct) => direct.is_active);
    if (!activeDirect) {
      const reconciled = await noise<LocalSummary>({ action: "sync_directs", relays });
      if (generation === refreshGeneration.current) {
        setSummary(reconciled);
        setDirectConversation(null);
      }
      return;
    }
    const cached = directConversationCache.current.get(activeDirect.public_key);
    if (cached) setDirectConversation(cached);
    const nextDirectConversation = await noise<DirectConversation>({ action: "direct_conversation", relays });
    const reconciled = await noise<LocalSummary>({ action: "status" });
    if (generation !== refreshGeneration.current) return;
    if (nextDirectConversation) {
      directConversationCache.current.set(nextDirectConversation.contact.public_key, nextDirectConversation);
      setDirectConversation(nextDirectConversation);
    }
    setSummary(reconciled);
  }, [sidebarMode]);

  const syncDirectSummary = useCallback(async () => {
    const reconciled = await noise<LocalSummary>({ action: "sync_directs", relays });
    if (reconciled) {
      setSummary((current) => current ? {
        ...current,
        directs: reconciled.directs,
        known_people: reconciled.known_people,
      } : reconciled);
    }
  }, []);
  const sidebarModeRef = useRef(sidebarMode);
  const refreshRef = useRef(refresh);
  useEffect(() => {
    sidebarModeRef.current = sidebarMode;
    refreshRef.current = refresh;
  }, [refresh, sidebarMode]);

  useEffect(() => {
    if (!isTauri) {
      setLoading(false);
      return;
    }
    void refresh()
      .catch((cause) => setError(message(cause)))
      .finally(() => setLoading(false));
  }, [refresh]);

  const activeGroup = summary?.groups.find((group) => group.is_active) ?? null;
  const activeGroupId = activeGroup?.group_id ?? null;
  const activeDirectPublicKey = summary?.directs.find((direct) => direct.is_active)?.public_key ?? null;
  const activeGroupBackground = sidebarMode === "groups" ? activeGroup?.background ?? null : null;
  const activeAccentStyle = accentStyle(sidebarMode === "groups" ? activeGroup?.accent_color : null);
  const appBackgroundSource = useProfileImageSource(activeGroupBackground);
  useEffect(() => {
    if (!isTauri || sidebarMode !== "groups" || !activeGroupId) return;
    let stopped = false;
    const watch = async () => {
      let revision: number | null = null;
      while (!stopped) {
        try {
          const initial = revision === null;
          const change: GroupWatch | null = await noise<GroupWatch>({
            action: "watch_group",
            since: revision,
            relays,
          });
          if (stopped || !change) return;
          revision = change.revision;
          if (!initial && change.changed) await refresh();
        } catch {
          await new Promise((resolve) => window.setTimeout(resolve, 1500));
        }
      }
    };
    void watch();
    return () => {
      stopped = true;
    };
  }, [activeGroupId, refresh, sidebarMode]);

  const identityPublicKey = summary?.identity.public_key ?? null;
  useEffect(() => {
    if (!isTauri || !identityPublicKey) return;
    let stopped = false;
    const watch = async () => {
      let revision: number | null = null;
      while (!stopped) {
        try {
          const initial = revision === null;
          const change: GroupWatch | null = await noise<GroupWatch>({ action: "watch_direct", since: revision, relays });
          if (stopped || !change) return;
          revision = change.revision;
          if (initial) {
            if (sidebarModeRef.current === "groups") await syncDirectSummary();
          } else if (change.changed) {
            if (sidebarModeRef.current === "directs") await refreshRef.current();
            else await syncDirectSummary();
          }
        } catch {
          await new Promise((resolve) => window.setTimeout(resolve, 1500));
        }
      }
    };
    void watch();
    return () => { stopped = true; };
  }, [identityPublicKey, syncDirectSummary]);

  useEffect(() => {
    if (!isTauri || !identityPublicKey || !summary?.identity.noise_id) return;
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
      setError(message(cause));
      return false;
    } finally {
      setBusy(false);
    }
  }

  async function selectGroup(group: GroupSummary) {
    if (group.is_active) return;
    const generation = ++refreshGeneration.current;
    setError(null);
    const cached = groupConversationCache.current.get(group.group_id);
    if (cached) setConversation(cached);
    setSummary((current) => current ? {
      ...current,
      groups: current.groups.map((candidate) => ({
        ...candidate,
        is_active: candidate.group_id === group.group_id,
      })),
    } : current);

    try {
      const local = await noise<LocalSummary>({ action: "select_group", group_id: group.group_id });
      if (generation !== refreshGeneration.current) return;
      setSummary(local);
      const fresh = await noise<Conversation>({ action: "conversation", relays });
      const reconciled = await noise<LocalSummary>({ action: "status" });
      if (generation !== refreshGeneration.current) return;
      if (fresh) {
        groupConversationCache.current.set(fresh.group.group_id, fresh);
        setConversation(fresh);
      }
      setSummary(reconciled);
    } catch (cause) {
      if (generation === refreshGeneration.current) setError(message(cause));
    }
  }

  async function selectDirect(direct: DirectSummary) {
    if (direct.is_active) return;
    const generation = ++refreshGeneration.current;
    setError(null);
    const cached = directConversationCache.current.get(direct.public_key);
    if (cached) setDirectConversation(cached);
    setSummary((current) => current ? {
      ...current,
      directs: current.directs.map((candidate) => ({
        ...candidate,
        is_active: candidate.public_key === direct.public_key,
      })),
    } : current);

    try {
      const local = await noise<LocalSummary>({ action: "select_direct", public_key: direct.public_key });
      if (generation !== refreshGeneration.current) return;
      setSummary(local);
      const fresh = await noise<DirectConversation>({ action: "direct_conversation", relays });
      const reconciled = await noise<LocalSummary>({ action: "status" });
      if (generation !== refreshGeneration.current) return;
      if (fresh) {
        directConversationCache.current.set(fresh.contact.public_key, fresh);
        setDirectConversation(fresh);
      }
      setSummary(reconciled);
    } catch (cause) {
      if (generation === refreshGeneration.current) setError(message(cause));
    }
  }

  async function startDirect(person: PersonSummary) {
    await perform(async () => {
      const local = await noise<LocalSummary>({
        action: "start_direct",
        public_key: person.public_key,
        username: person.username,
        bio: person.bio,
        avatar: person.avatar,
        accepts_direct_messages: person.accepts_direct_messages,
      });
      setSummary(local);
      setDialog(null);
      setSidebarMode("directs");
    });
  }

  if (!isTauri) return <BrowserFoundation />;
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

  const selectedConversation = conversation?.group.group_id === activeGroupId ? conversation : null;
  const selectedDirectConversation = directConversation?.contact.public_key === activeDirectPublicKey
    ? directConversation
    : null;

  return (
    <div className={`app-shell ${appBackgroundSource ? "group-background-active" : ""}`} style={activeAccentStyle}>
      {appBackgroundSource && <div className="group-app-background" style={{ backgroundImage: `url(${JSON.stringify(appBackgroundSource)})` }} aria-hidden="true" />}
      <Sidebar
        summary={summary}
        mode={sidebarMode}
        onMode={setSidebarMode}
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
              busy={busy}
              hasBackground={Boolean(appBackgroundSource)}
              canEditGroup={selectedConversation.group.owner_public_key === summary.identity.public_key}
              selfPublicKey={summary.identity.public_key}
              onGroupSettings={() => setDialog({ type: "group", group: selectedConversation.group })}
              onReports={() => setDialog({ type: "reports" })}
              onMedia={() => setDialog({ type: "media" })}
              onRules={() => setDialog({ type: "rules", group: selectedConversation.group })}
              onPerson={(person) => setDialog({ type: "person", person })}
              onMessage={(person) => void startDirect(person)}
              onDeleteMessage={(messageEventId) =>
                perform(async () => {
                  await noise({ action: "delete_message", message_event_id: messageEventId, relays });
                  await refresh();
                })
              }
              onSetModerator={(member, enabled) =>
                perform(async () => {
                  await noise({ action: "set_moderator", member_public_key: member.public_key, enabled, relays });
                  await refresh();
                })
              }
              onBan={(member) => setDialog({ type: "ban_member", member })}
              onReport={(message) => setDialog({ type: "report_message", message })}
              onSend={(text, pending, onProgress, replyToMessageId) =>
                perform(async () => {
                  let attachment: MediaAttachment | null = null;
                  if (pending) {
                    const chunks: MediaChunk[] = [];
                    const chunkSize = 1024 * 1024;
                    for (let offset = 0; offset < pending.file.size; offset += chunkSize) {
                      const chunk = await noise<MediaChunk>({
                        action: "upload_media_chunk",
                        data_base64: await fileBase64(pending.file.slice(offset, offset + chunkSize)),
                        relays,
                      });
                      if (!chunk) throw new Error("relay did not return a media chunk reference");
                      chunks.push(chunk);
                      onProgress(Math.min(100, Math.round(((offset + chunk.byte_length) / pending.file.size) * 100)));
                    }
                    attachment = {
                      file_name: pending.name,
                      mime_type: pending.mimeType,
                      byte_length: pending.byteLength,
                      chunks,
                    };
                  }
                  await noise<null>({
                    action: "say",
                    text,
                    attachment,
                    reply_to_message_id: replyToMessageId,
                    relays,
                  });
                  await refresh();
                })
              }
            />
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
              onPerson={(person) => setDialog({ type: "person", person })}
              onDelete={() => setDialog({ type: "delete_direct", direct: selectedDirectConversation.contact })}
              onSend={(text, pending, onProgress, replyToMessageId) => perform(async () => {
                const attachment = await uploadPendingMedia(pending, "upload_direct_media_chunk", onProgress);
                await noise({ action: "say_direct", text, attachment, reply_to_message_id: replyToMessageId, relays });
                await refresh();
              })}
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
          onSave={(name, description, accentColor, avatar, removeAvatar, background, removeBackground, membersCanSendMessages, membersCanSendMedia) =>
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
      {dialog?.type === "reports" && conversation && (
        <ReportsDialog
          reports={conversation.reports}
          busy={busy}
          onClose={() => setDialog(null)}
          onDismiss={(report) => perform(async () => {
            await noise({ action: "resolve_report", report_event_id: report.report_event_id, relays });
            await refresh();
          })}
          onDelete={(report) => perform(async () => {
            await noise({ action: "delete_message", message_event_id: report.message.event_id, relays });
            await refresh();
          })}
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
              mediaCache.clear();
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
              mediaCache.clear();
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
            mediaCache.clear();
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
            mediaCache.clear();
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
            mediaCache.clear();
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
      <div className="sidebar-drag" data-tauri-drag-region />
      <div className="brand"><NoiseMark size={22} /><strong>noise</strong></div>
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
            className={`group-row ${group.is_active ? "active" : ""}`}
            key={group.group_id}
            onClick={() => onSelect(group)}
            onContextMenu={(event) => {
              event.preventDefault();
              onContextMenu(group, event.clientX, event.clientY);
            }}
          >
            <Avatar name={group.name} image={group.avatar} size={27} square />
            <span>{group.name}</span>
          </button>
        )) : summary.directs.map((direct) => (
          <button
            className={`group-row direct-row ${direct.is_active ? "active" : ""}`}
            key={direct.public_key}
            onClick={() => onSelectDirect(direct)}
            onContextMenu={(event) => { event.preventDefault(); onDirectContextMenu(direct, event.clientX, event.clientY); }}
          >
            <Avatar name={direct.username} image={direct.avatar} size={27} />
            <span>@{direct.username}</span>
            {direct.has_unread && <span className="direct-unread-dot" aria-label={`unread messages from ${direct.username}`} />}
          </button>
        ))}
        {mode === "directs" && summary.directs.length === 0 && <div className="empty-direct-list">message someone from a shared group</div>}
      </div>
      <button className="self-profile" onClick={onProfile}>
        <Avatar name={summary.identity.username} image={summary.identity.avatar} size={32} />
        <span><strong>@{summary.identity.username}</strong><small>{summary.identity.bio || "build your identity"}</small></span>
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

function MessageContextMenu({ x, y, busy, onClose, onReply, onReport, onDelete, onBan }: { x: number; y: number; busy: boolean; onClose: () => void; onReply: () => void; onReport?: () => void; onDelete?: () => void; onBan?: () => void }) {
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
  const menuHeight = 50 + (onReport ? 42 : 0) + (onDelete ? 42 : 0) + (onBan ? 42 : 0);
  return <div className="member-context-menu" style={{ left: Math.min(x, window.innerWidth - 200), top: Math.min(y, window.innerHeight - menuHeight) }} onMouseDown={(event) => event.stopPropagation()}><button disabled={busy} onClick={onReply}><Reply size={14} /> reply</button>{onReport && <button className="report-action" disabled={busy} onClick={onReport}><TriangleAlert size={14} /> report message</button>}{onDelete && <button className="danger" disabled={busy} onClick={onDelete}><Trash2 size={14} /> delete message</button>}{onBan && <button className="danger" disabled={busy} onClick={onBan}><UserRoundX size={14} /> ban member</button>}</div>;
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

function useMessageListPosition(conversationKey: string, messageCount: number) {
  const ref = useRef<HTMLDivElement>(null);
  const positionedConversation = useRef<string | null>(null);
  const previousMessageCount = useRef(messageCount);
  const shouldFollowNewMessages = useRef(true);

  useLayoutEffect(() => {
    const element = ref.current;
    if (!element) return;
    if (positionedConversation.current !== conversationKey) {
      element.scrollTop = element.scrollHeight;
      positionedConversation.current = conversationKey;
      previousMessageCount.current = messageCount;
      shouldFollowNewMessages.current = true;
      return;
    }
    if (messageCount > previousMessageCount.current && shouldFollowNewMessages.current) {
      element.scrollTo({ top: element.scrollHeight, behavior: "smooth" });
    }
    previousMessageCount.current = messageCount;
  }, [conversationKey, messageCount]);

  const onScroll = useCallback(() => {
    const element = ref.current;
    if (!element) return;
    shouldFollowNewMessages.current = element.scrollHeight - element.scrollTop - element.clientHeight < 96;
  }, []);

  return { ref, onScroll };
}

function ConversationPanel({
  conversation,
  busy,
  hasBackground,
  canEditGroup,
  selfPublicKey,
  onGroupSettings,
  onReports,
  onMedia,
  onRules,
  onPerson,
  onMessage,
  onDeleteMessage,
  onSetModerator,
  onBan,
  onReport,
  onSend,
}: {
  conversation: Conversation;
  busy: boolean;
  hasBackground: boolean;
  canEditGroup: boolean;
  selfPublicKey: string;
  onGroupSettings: () => void;
  onReports: () => void;
  onMedia: () => void;
  onRules: () => void;
  onPerson: (person: PersonSummary) => void;
  onMessage: (person: PersonSummary) => void;
  onDeleteMessage: (messageEventId: string) => Promise<boolean>;
  onSetModerator: (member: MemberSummary, enabled: boolean) => Promise<boolean>;
  onBan: (member: MemberSummary) => void;
  onReport: (message: MessageSummary) => void;
  onSend: (text: string, attachment: PendingMedia | null, onProgress: (progress: number) => void, replyToMessageId: string | null) => Promise<boolean>;
}) {
  const [draft, setDraft] = useState("");
  const [attachment, setAttachment] = useState<PendingMedia | null>(null);
  const [attachmentError, setAttachmentError] = useState<string | null>(null);
  const [uploadProgress, setUploadProgress] = useState<number | null>(null);
  const [memberMenu, setMemberMenu] = useState<{ member: MemberSummary; x: number; y: number } | null>(null);
  const [messageMenu, setMessageMenu] = useState<{ message: MessageSummary; x: number; y: number } | null>(null);
  const [replyingTo, setReplyingTo] = useState<MessageSummary | null>(null);
  const fileInput = useRef<HTMLInputElement>(null);
  const composerInput = useRef<HTMLTextAreaElement>(null);
  const messageList = useMessageListPosition(conversation.group.group_id, conversation.messages.length);
  const selfMember = conversation.members.find((member) => member.public_key === selfPublicKey);
  const canModerate = canEditGroup || selfMember?.is_moderator === true;
  const canSendMessages = canModerate || conversation.group.members_can_send_messages;
  const canSendMedia = canModerate || conversation.group.members_can_send_media;
  const sortedMembers = [...conversation.members].sort((left, right) => {
    const rank = (member: MemberSummary) => member.public_key === conversation.group.owner_public_key ? 0 : member.is_moderator ? 1 : 2;
    return rank(left) - rank(right);
  });
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
    });
    if (fileInput.current) fileInput.current.value = "";
  }
  async function submit() {
    const text = draft.trim();
    if ((!text && !attachment) || busy || (text && !canSendMessages) || (attachment && !canSendMedia)) return;
    const pendingAttachment = attachment;
    if (pendingAttachment) setUploadProgress(0);
    const sent = await onSend(text, pendingAttachment, setUploadProgress, replyingTo?.message_id ?? null);
    setUploadProgress(null);
    if (sent) {
      setDraft("");
      setAttachment(null);
      setReplyingTo(null);
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
        {conversation.messages.map((item) => (
          <MessageRow key={item.event_id} message={item} own={item.author_public_key === selfPublicKey} replyTo={conversation.messages.find((candidate) => candidate.message_id === item.reply_to_message_id)} onContextMenu={(event) => { event.preventDefault(); setMessageMenu({ message: item, x: event.clientX, y: event.clientY }); }} onPerson={onPerson} />
        ))}
      </div>
      {selfMember && (canSendMessages || canSendMedia) ? <div className="composer">
        {replyingTo && <ReplyTarget message={replyingTo} onClose={() => setReplyingTo(null)} />}
        {attachment && <div className={`attachment-draft ${attachment.mimeType.startsWith("audio/") ? "audio" : ""}`}>{attachment.mimeType.startsWith("image/") ? <img src={attachment.previewUrl} alt="" /> : attachment.mimeType.startsWith("video/") ? <video src={attachment.previewUrl} muted playsInline preload="metadata" onLoadedMetadata={(event) => { const video = event.currentTarget; if (Number.isFinite(video.duration) && video.duration > 0) video.currentTime = Math.min(0.25, video.duration / 2); }} /> : <div className="audio-thumbnail"><AudioWaveform size={30} /></div>}{uploadProgress !== null && <div className="attachment-progress"><i style={{ width: `${uploadProgress}%` }} /><span>{uploadProgress}%</span></div>}<button disabled={busy} onClick={() => setAttachment(null)} aria-label="remove attachment"><X size={14} /></button></div>}
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
        <div className="member-sidebar-heading">
          <strong>members</strong>
          <span>{conversation.members.length}</span>
        </div>
        <div className="member-sidebar-list">
          {sortedMembers.map((member) => (
            <div key={member.public_key} className="member-sidebar-row">
              <button className="member-sidebar-main" onClick={() => onPerson(member)}>
                <Avatar name={member.username} image={member.avatar} size={30} />
                <span className="member-sidebar-copy">
                  <span>
                    <strong>@{member.username}</strong>
                    {member.public_key === conversation.group.owner_public_key ? <i>founder</i> : member.is_moderator && <i>mod</i>}
                  </span>
                  <small>{member.bio || "tuned in"}</small>
                </span>
              </button>
              {member.public_key !== selfPublicKey && <button className="member-actions" aria-label={`actions for ${member.username}`} onClick={(event) => { const rect = event.currentTarget.getBoundingClientRect(); setMemberMenu({ member, x: rect.right, y: rect.bottom + 4 }); }}><MoreHorizontal size={15} /></button>}
            </div>
          ))}
        </div>
      </aside>
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
        onReply={() => { setReplyingTo(messageMenu.message); setMessageMenu(null); window.setTimeout(() => composerInput.current?.focus(), 0); }}
        onReport={!canModerate && messageMenu.message.author_public_key !== selfPublicKey && !conversation.reported_message_event_ids.includes(messageMenu.message.event_id) ? () => { onReport(messageMenu.message); setMessageMenu(null); } : undefined}
        onDelete={(canModerate || messageMenu.message.author_public_key === selfPublicKey) ? () => { void onDeleteMessage(messageMenu.message.event_id); setMessageMenu(null); } : undefined}
        onBan={(() => {
          const member = conversation.members.find((candidate) => candidate.public_key === messageMenu.message.author_public_key);
          const canBanAuthor = member
            && member.public_key !== selfPublicKey
            && member.public_key !== conversation.group.owner_public_key
            && (canEditGroup || !member.is_moderator);
          return canBanAuthor ? () => { onBan(member); setMessageMenu(null); } : undefined;
        })()}
      />}
    </div>
  );
}

function DirectConversationPanel({ conversation, busy, selfPublicKey, onPerson, onDelete, onSend }: { conversation: DirectConversation; busy: boolean; selfPublicKey: string; onPerson: (person: PersonSummary) => void; onDelete: () => void; onSend: (text: string, attachment: PendingMedia | null, onProgress: (progress: number) => void, replyToMessageId: string | null) => Promise<boolean> }) {
  const [draft, setDraft] = useState("");
  const [attachment, setAttachment] = useState<PendingMedia | null>(null);
  const [attachmentError, setAttachmentError] = useState<string | null>(null);
  const [uploadProgress, setUploadProgress] = useState<number | null>(null);
  const [messageMenu, setMessageMenu] = useState<{ message: MessageSummary; x: number; y: number } | null>(null);
  const [replyingTo, setReplyingTo] = useState<MessageSummary | null>(null);
  const fileInput = useRef<HTMLInputElement>(null);
  const composerInput = useRef<HTMLTextAreaElement>(null);
  const messageList = useMessageListPosition(conversation.contact.public_key, conversation.messages.length);
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
    setAttachment({ name: file.name, mimeType: file.type, byteLength: file.size, file, previewUrl: URL.createObjectURL(file) });
    if (fileInput.current) fileInput.current.value = "";
  }
  async function submit() {
    const text = draft.trim();
    if ((!text && !attachment) || busy) return;
    const pendingAttachment = attachment;
    if (pendingAttachment) setUploadProgress(0);
    const sent = await onSend(text, pendingAttachment, setUploadProgress, replyingTo?.message_id ?? null);
    setUploadProgress(null);
    if (sent) {
      setDraft("");
      setAttachment(null);
      setReplyingTo(null);
    }
  }
  const person = { public_key: conversation.contact.public_key, username: conversation.contact.username, bio: conversation.contact.bio, avatar: conversation.contact.avatar, accepts_direct_messages: conversation.contact.accepts_direct_messages };
  return (
    <div className="conversation direct-conversation">
      <header className="chat-header" data-tauri-drag-region>
        <div className="group-identity static" data-tauri-drag-region>
          <Avatar name={conversation.contact.username} image={conversation.contact.avatar} size={36} />
          <span><strong>@{conversation.contact.username}</strong><small>{conversation.contact.bio || "encrypted direct message"}</small></span>
        </div>
        <div className="chat-header-actions"><button className="icon-button media-button delete-direct-button" onClick={onDelete} aria-label="delete conversation" title="delete conversation"><Trash2 size={16} /></button>{busy && <LoaderCircle className="spinner" size={14} />}</div>
      </header>
      <div className="messages" ref={messageList.ref} onScroll={messageList.onScroll}>
        {conversation.messages.length === 0 && <div className="quiet">start the conversation</div>}
        {conversation.messages.map((item) => <MessageRow key={item.event_id} message={item} own={item.author_public_key === selfPublicKey} replyTo={conversation.messages.find((candidate) => candidate.message_id === item.reply_to_message_id)} onContextMenu={(event) => { event.preventDefault(); setMessageMenu({ message: item, x: event.clientX, y: event.clientY }); }} onPerson={onPerson} mediaScopeId={conversation.media_scope_id} />)}
      </div>
      {conversation.contact.accepts_direct_messages ? <div className="composer">
        {replyingTo && <ReplyTarget message={replyingTo} onClose={() => setReplyingTo(null)} />}
        {attachment && <div className={`attachment-draft ${attachment.mimeType.startsWith("audio/") ? "audio" : ""}`}>{attachment.mimeType.startsWith("image/") ? <img src={attachment.previewUrl} alt="" /> : attachment.mimeType.startsWith("video/") ? <video src={attachment.previewUrl} muted playsInline preload="metadata" onLoadedMetadata={(event) => primeVideoFrame(event.currentTarget)} /> : <div className="audio-thumbnail"><AudioWaveform size={30} /></div>}{uploadProgress !== null && <div className="attachment-progress"><i style={{ width: `${uploadProgress}%` }} /><span>{uploadProgress}%</span></div>}<button disabled={busy} onClick={() => setAttachment(null)} aria-label="remove attachment"><X size={14} /></button></div>}
        {attachmentError && <div className="attachment-error">{attachmentError}</div>}
        <button className="attach-button" disabled={busy} onClick={() => fileInput.current?.click()} aria-label="attach media"><Paperclip size={17} /></button>
        <input ref={fileInput} hidden type="file" accept="image/*,video/*,audio/*" onChange={(event) => void chooseMedia(event.target.files?.[0])} />
        <textarea ref={composerInput} rows={1} value={draft} placeholder={`message @${conversation.contact.username}`} onChange={(event) => setDraft(event.target.value)} onKeyDown={(event) => { if (event.key === "Enter" && !event.shiftKey) { event.preventDefault(); void submit(); } }} />
        <button className="send-button" disabled={(!draft.trim() && !attachment) || busy} onClick={() => void submit()}><ArrowUp size={17} /></button>
      </div> : <div className="membership-revoked"><MessageCircle size={16} /> @{conversation.contact.username} isn’t accepting DMs</div>}
      <aside className="member-sidebar direct-profile-sidebar">
        <button className="direct-profile-identity" onClick={() => onPerson(person)}>
          <Avatar name={conversation.contact.username} image={conversation.contact.avatar} size={72} />
          <strong>@{conversation.contact.username}</strong>
        </button>
        <div className="noise-signature"><small>Noise Signature</small><strong>{noiseSignature(conversation.contact.public_key)}</strong></div>
        <p>{conversation.contact.bio || "no bio yet"}</p>
        <span className={`direct-profile-status ${conversation.contact.accepts_direct_messages ? "open" : "closed"}`}><i />{conversation.contact.accepts_direct_messages ? "accepting DMs" : "DMs closed"}</span>
      </aside>
      {messageMenu && <MessageContextMenu x={messageMenu.x} y={messageMenu.y} busy={busy} onClose={() => setMessageMenu(null)} onReply={() => { setReplyingTo(messageMenu.message); setMessageMenu(null); window.setTimeout(() => composerInput.current?.focus(), 0); }} />}
    </div>
  );
}

function ReplyTarget({ message, onClose }: { message: MessageSummary; onClose: () => void }) {
  return <div className="reply-target"><Reply size={15} /><span><small>replying to @{message.username}</small><strong>{replyPreview(message)}</strong></span><button onClick={onClose} aria-label="cancel reply"><X size={14} /></button></div>;
}

function MessageRow({
  message,
  own,
  replyTo,
  onContextMenu,
  onPerson,
  mediaScopeId,
}: {
  message: MessageSummary;
  own: boolean;
  replyTo?: MessageSummary;
  onContextMenu?: (event: React.MouseEvent<HTMLElement>) => void;
  onPerson: (person: PersonSummary) => void;
  mediaScopeId?: string;
}) {
  const person = { public_key: message.author_public_key, username: message.username, bio: message.bio, avatar: message.avatar, accepts_direct_messages: message.accepts_direct_messages };
  return (
    <article
      className={`message-row ${own ? "own" : ""}`}
      onMouseDown={onContextMenu ? (event) => { if (event.button === 2) event.preventDefault(); } : undefined}
      onContextMenu={onContextMenu ? (event) => {
        event.preventDefault();
        window.getSelection()?.removeAllRanges();
        onContextMenu?.(event);
      } : undefined}
    >
      <button onClick={() => onPerson(person)}><Avatar name={message.username} image={message.avatar} size={34} /></button>
      <div className="message-body"><div className="message-meta"><button onClick={() => onPerson(person)}>@{message.username}</button></div>{message.reply_to_message_id && <div className="message-reply-reference">{replyTo ? <><strong>@{replyTo.username}</strong><span>{replyPreview(replyTo)}</span></> : <span>original message unavailable</span>}</div>}{message.text && <p>{message.text}</p>}{message.attachment && <MessageMedia attachment={message.attachment} scopeId={mediaScopeId} />}<time className="message-time">{formatTime(message.created_at_millis)}</time></div>
    </article>
  );
}

function replyPreview(message: MessageSummary) {
  const text = message.text.trim();
  if (text) return text.length > 96 ? `${text.slice(0, 96)}…` : text;
  if (message.attachment?.mime_type.startsWith("image/")) return "image";
  if (message.attachment?.mime_type.startsWith("video/")) return "video";
  if (message.attachment?.mime_type.startsWith("audio/")) return "audio";
  return "message";
}

function MessageMedia({ attachment, scopeId }: { attachment: MediaAttachment; scopeId?: string }) {
  const { source, failed } = useMediaSource(attachment, scopeId);
  return <div className="message-media">{source ? attachment.mime_type.startsWith("image/") ? <img src={source} alt="shared media" /> : attachment.mime_type.startsWith("video/") ? <ChatVideo source={source} /> : <audio src={source} controls preload="metadata" /> : <div className="media-loading">{failed ? "media unavailable" : <><LoaderCircle className="spinner" size={15} /> decrypting media</>}</div>}</div>;
}

function useMediaSource(attachment: MediaAttachment, scopeId?: string) {
  const cacheKey = attachment.chunks.map((chunk) => chunk.blob_id).join(":");
  const [source, setSource] = useState(() => mediaCache.get(cacheKey) ?? null);
  const [failed, setFailed] = useState(false);
  useEffect(() => {
    if (source) return;
    let active = true;
    void noise<AttachmentData>({ action: "fetch_attachment", attachment, scope_id: scopeId, relays })
      .then(async (data) => {
        if (!active || !data) return;
        const { convertFileSrc } = await import("@tauri-apps/api/core");
        const next = convertFileSrc(data.file_path);
        mediaCache.set(cacheKey, next);
        setSource(next);
      })
      .catch(() => active && setFailed(true));
    return () => { active = false; };
  }, [attachment, cacheKey, scopeId, source]);
  return { source, failed };
}

function ChatVideo({ source }: { source: string }) {
  const [showControls, setShowControls] = useState(false);
  return <video
    src={source}
    controls={showControls}
    playsInline
    preload="auto"
    onLoadedMetadata={(event) => primeVideoFrame(event.currentTarget)}
    onMouseEnter={() => setShowControls(true)}
    onMouseLeave={() => setShowControls(false)}
    onFocus={() => setShowControls(true)}
    onBlur={() => setShowControls(false)}
    onPointerDown={() => setShowControls(true)}
  />;
}

type MediaMessage = MessageSummary & { attachment: MediaAttachment };

function MediaGalleryDialog({ group, messages, onClose }: { group: GroupSummary; messages: MessageSummary[]; onClose: () => void }) {
  const media = messages.filter((item): item is MediaMessage => item.attachment !== null);
  const [selected, setSelected] = useState<MediaMessage | null>(null);
  return (
    <Modal onClose={onClose} wide>
      <DialogHeading icon={<Images />} title="group media" detail={`${media.length} ${media.length === 1 ? "upload" : "uploads"} in ${group.name}`} />
      {selected ? (
        <div className="gallery-view">
          <button className="gallery-back" onClick={() => setSelected(null)}><ArrowLeft size={14} /> all media</button>
          <div className="gallery-viewer"><MessageMedia key={selected.event_id} attachment={selected.attachment} /></div>
          <small>shared by @{selected.username} · {formatGalleryDate(selected.created_at_millis)}</small>
        </div>
      ) : media.length ? (
        <div className="media-gallery">
          {media.map((item) => <GalleryTile key={item.event_id} message={item} onOpen={() => setSelected(item)} />)}
        </div>
      ) : (
        <div className="empty-gallery"><Images size={27} /><span>no media has been shared yet</span></div>
      )}
    </Modal>
  );
}

function GalleryTile({ message, onOpen }: { message: MediaMessage; onOpen: () => void }) {
  const { attachment } = message;
  const { source, failed } = useMediaSource(attachment);
  const image = attachment.mime_type.startsWith("image/");
  const video = attachment.mime_type.startsWith("video/");
  return (
    <button className={`gallery-tile ${image ? "image" : video ? "video" : "audio"}`} onClick={onOpen} aria-label={`open media shared by ${message.username}`}>
      {source ? image ? <img src={source} alt="" /> : video ? <video src={source} muted playsInline preload="auto" onLoadedMetadata={(event) => primeVideoFrame(event.currentTarget)} /> : <span className="gallery-audio"><AudioWaveform size={30} /><small>audio</small></span> : <span className="gallery-loading">{failed ? <X size={16} /> : <LoaderCircle className="spinner" size={16} />}</span>}
      {video && source && <i className="gallery-play"><Play size={15} fill="currentColor" /></i>}
    </button>
  );
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

function Onboarding({ busy, onCreate, onSignIn }: { busy: boolean; onCreate: (username: string, password: string) => Promise<boolean>; onSignIn: (noiseId: string, password: string) => Promise<boolean> }) {
  const [mode, setMode] = useState<"create" | "signin">("create");
  const [username, setUsername] = useState("");
  const [noiseId, setNoiseId] = useState("");
  const [password, setPassword] = useState("");
  const [confirmation, setConfirmation] = useState("");
  const displayedNoiseId = noiseId.match(/.{1,4}/g)?.join(" ") ?? "";
  const createReady = username.trim().length > 0 && password.length >= 16 && password === confirmation;
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
        <input autoFocus value={username} maxLength={32} onChange={(event) => setUsername(event.target.value)} placeholder="display name" />
        <input type="password" autoComplete="new-password" value={password} onChange={(event) => setPassword(event.target.value)} placeholder="strong password" />
        <input type="password" autoComplete="new-password" value={confirmation} onChange={(event) => setConfirmation(event.target.value)} placeholder="confirm password" onKeyDown={(event) => { if (event.key === "Enter" && createReady) void onCreate(username.trim(), password); }} />
        <button disabled={!createReady || busy} onClick={() => void onCreate(username.trim(), password)}>{busy && <LoaderCircle className="spinner" size={14} />} create identity</button>
        <small>use 16+ characters and a password manager or long passphrase</small>
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

function GroupSettingsDialog({ group, bannedMembers, busy, onClose, onSave, onUnban, onRotateFrequency }: { group: GroupSummary; bannedMembers: BannedMemberSummary[]; busy: boolean; onClose: () => void; onSave: (name: string, description: string, accentColor: string, avatar: string | null, removeAvatar: boolean, background: string | null, removeBackground: boolean, membersCanSendMessages: boolean, membersCanSendMedia: boolean) => Promise<boolean>; onUnban: (member: BannedMemberSummary) => Promise<boolean>; onRotateFrequency: (revokeOnly: boolean) => Promise<boolean> }) {
  const [tab, setTab] = useState<"identity" | "appearance" | "general" | "banned">("identity");
  const [revokeArmed, setRevokeArmed] = useState(false);
  const [name, setName] = useState(group.name);
  const [description, setDescription] = useState(group.description);
  const [accentColor, setAccentColor] = useState(group.accent_color || DEFAULT_ACCENT_COLOR);
  const [membersCanSendMessages, setMembersCanSendMessages] = useState(group.members_can_send_messages);
  const [membersCanSendMedia, setMembersCanSendMedia] = useState(group.members_can_send_media);
  const image = useImageSelection();
  const background = useBackgroundSelection();
  const hasGroupIcon = Boolean(image.preview || (!image.removed && group.avatar));
  const settingsChanged = name.trim() !== group.name
    || description !== group.description
    || accentColor !== group.accent_color
    || membersCanSendMessages !== group.members_can_send_messages
    || membersCanSendMedia !== group.members_can_send_media
    || image.base64 !== null
    || image.removed
    || background.base64 !== null
    || background.removed;
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
          <BackgroundPicker existing={group.background} selection={background} disabled={busy} />
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
          {bannedMembers.length ? <div className="banned-user-list">{bannedMembers.map((member) => <div className="banned-user-row" key={member.public_key}><Avatar name={member.username} image={member.avatar} size={30} /><span><strong>@{member.username}</strong><small>{member.bio || "banned from this group"}</small></span><button disabled={busy} onClick={() => void onUnban(member)}>unban</button></div>)}</div> : <p className="empty-banned-users">no one is banned</p>}
        </section>}
      </div>
      <DialogButtons onClose={onClose} closeLabel={settingsChanged ? "cancel" : "close"}>
        {settingsChanged && <button className="primary" disabled={!name.trim() || name.length > 80 || description.length > 200 || background.busy || busy} onClick={() => void onSave(name.trim(), description, accentColor, image.base64, image.removed, background.base64, background.removed, membersCanSendMessages, membersCanSendMedia)}>save settings</button>}
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

function ReportMessageDialog({ message, busy, onClose, onReport }: { message: MessageSummary; busy: boolean; onClose: () => void; onReport: (reason: string) => Promise<boolean> }) {
  const [reason, setReason] = useState("");
  return (
    <Modal onClose={onClose} compact>
      <DialogHeading icon={<TriangleAlert />} title="report message?" detail="send this to the group’s moderation queue" />
      <div className="report-target-preview"><strong>@{message.username}</strong><p>{reportMessagePreview(message)}</p></div>
      <LabeledArea label="details (optional)" count={`${reason.length}/280`}><textarea autoFocus maxLength={280} value={reason} placeholder="what should moderators know?" onChange={(event) => setReason(event.target.value)} /></LabeledArea>
      <DialogButtons onClose={onClose}><button className="report-confirm" disabled={busy} onClick={() => void onReport(reason.trim())}>{busy && <LoaderCircle className="spinner" size={13} />} report message</button></DialogButtons>
    </Modal>
  );
}

function ReportsDialog({ reports, busy, onClose, onDismiss, onDelete }: { reports: ReportSummary[]; busy: boolean; onClose: () => void; onDismiss: (report: ReportSummary) => Promise<boolean>; onDelete: (report: ReportSummary) => Promise<boolean> }) {
  return (
    <Modal onClose={onClose} wide>
      <DialogHeading icon={<TriangleAlert />} title="reports" detail={reports.length === 1 ? "1 report needs review" : `${reports.length} reports need review`} />
      {reports.length ? <div className="reports-queue">{reports.map((report) => (
        <article className="report-card" key={report.report_event_id}>
          <div className="reported-message-author"><Avatar name={report.message.username} image={report.message.avatar} size={34} /><span><strong>@{report.message.username}</strong><small>posted {formatGalleryDate(report.message.created_at_millis)}</small></span></div>
          <p className="reported-message-copy">{reportMessagePreview(report.message)}</p>
          <div className="reporter-context"><Avatar name={report.reporter_username} image={report.reporter_avatar} size={24} /><span><small>reported by @{report.reporter_username} · {formatGalleryDate(report.created_at_millis)}</small><strong>{report.reason || "no additional details"}</strong></span></div>
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
  return <Modal onClose={onClose} compact><DialogHeading icon={<UserRoundX />} title={`ban @${member.username}?`} detail="they will be removed from the group" /><label className="ban-history-option"><input type="checkbox" checked={deleteMessages} onChange={(event) => setDeleteMessages(event.target.checked)} /><span><strong>delete all their messages</strong><small>also removes their media from the group history and gallery</small></span></label><DialogButtons onClose={onClose}><button className="delete-confirm" disabled={busy} onClick={() => void onBan(deleteMessages)}>{busy && <LoaderCircle className="spinner" size={13} />} ban member</button></DialogButtons></Modal>;
}

function LeaveGroupDialog({ group, busy, onClose, onLeave }: { group: GroupSummary; busy: boolean; onClose: () => void; onLeave: () => Promise<boolean> }) {
  return <Modal onClose={onClose} compact><DialogHeading icon={<LogOut />} title="leave group?" detail={group.name} /><p className="deletion-warning">This removes the group, its decrypted media cache, and its local data from this device.</p><DialogButtons onClose={onClose}><button className="delete-confirm" disabled={busy} onClick={() => void onLeave()}>{busy && <LoaderCircle className="spinner" size={13} />} leave group</button></DialogButtons></Modal>;
}

function DeleteDirectDialog({ direct, busy, onClose, onDelete }: { direct: DirectSummary; busy: boolean; onClose: () => void; onDelete: (forBoth: boolean) => Promise<boolean> }) {
  return <Modal onClose={onClose}><DialogHeading icon={<Trash2 />} title="delete conversation?" detail={`@${direct.username}`} /><p className="deletion-warning">Choose whether Noise should erase this thread only from this device or send a signed erasure to both users’ Noise clients.</p><div className="direct-delete-options"><button disabled={busy} onClick={() => void onDelete(false)}><strong>just for me</strong><small>erase this device’s history and cached media</small></button><button className="danger" disabled={busy} onClick={() => void onDelete(true)}><strong>for both of us</strong><small>ask all synced Noise clients to erase the thread</small></button></div><DialogButtons onClose={onClose} closeLabel="cancel">{busy && <LoaderCircle className="spinner" size={14} />}</DialogButtons></Modal>;
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

function PersonDialog({ person, canMessage, onMessage, onClose }: { person: PersonSummary; canMessage: boolean; onMessage: () => void; onClose: () => void }) {
  return <Modal onClose={onClose} compact><div className="person-card"><Avatar name={person.username} image={person.avatar} size={72} /><h2>@{person.username}</h2><div className="noise-signature"><small>Noise Signature</small><strong>{noiseSignature(person.public_key)}</strong></div><p>{person.bio || "no bio yet"}</p>{canMessage && <button className="profile-message" onClick={onMessage}><MessageCircle size={15} /> message</button>}</div></Modal>;
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

function BackgroundPicker({ existing, selection, disabled = false }: { existing: ProfileImage | null; selection: ReturnType<typeof useBackgroundSelection>; disabled?: boolean }) {
  const input = useRef<HTMLInputElement>(null);
  const existingSource = useProfileImageSource(selection.removed ? null : existing);
  const source = selection.preview ?? existingSource;
  const hasBackground = Boolean(selection.preview || (!selection.removed && existing));
  return (
    <div className="background-picker">
      <div className="background-picker-control">
        <button className="background-picker-preview" disabled={disabled || selection.busy} onClick={() => input.current?.click()}>
          {source
            ? <img src={source} alt="selected group chat background" />
            : hasBackground
              ? <span><LoaderCircle className="spinner" size={16} /></span>
              : <span><Camera size={17} /> add background</span>}
          {source && <i><Camera size={12} /></i>}
        </button>
        {hasBackground && <button className="background-picker-remove" disabled={disabled || selection.busy} onClick={selection.remove} aria-label="remove chat background" title="remove chat background"><X size={11} /></button>}
      </div>
      <input ref={input} hidden type="file" accept="image/*" onChange={(event) => { const target = event.currentTarget; void selection.choose(target.files?.[0]).finally(() => { target.value = ""; }); }} />
      <small>chat background</small>
      <em>1920 × 1080 recommended</em>
      {selection.error && <p>{selection.error}</p>}
    </div>
  );
}

function useBackgroundSelection() {
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
        const data = await prepareGroupBackground(file);
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
  if (status.phase === "downloading") {
    return <div className="update-banner"><span><strong>Noise {status.version}</strong><small>{status.progress === null ? "downloading update" : `downloading update · ${status.progress}%`}</small></span><div className="update-progress"><i style={{ width: `${status.progress ?? 8}%` }} /></div></div>;
  }
  if (status.phase === "ready") {
    return <div className="update-banner ready"><span><strong>Noise {status.version} is ready</strong><small>restart to finish updating</small></span><button onClick={restart}>restart Noise</button></div>;
  }
  return <div className="update-banner failed"><span><strong>update failed</strong><small>your current version is still intact</small></span><button onClick={retry}>try again</button><button className="update-dismiss" onClick={dismiss} aria-label="dismiss update"><X size={14} /></button></div>;
}

function Loading() { return <div className="loading"><LoaderCircle className="spinner" /></div>; }

function BrowserFoundation() {
  return <div className="browser-foundation"><Globe2 size={42} /><h1>noise for the browser</h1><p>The shared interface is running. The browser still needs the Rust cryptography compiled to WASM and IndexedDB identity storage before it can safely enter a live group.</p><small>desktop uses this exact React build through Tauri</small></div>;
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
  video.currentTime = Number.isFinite(video.duration) && video.duration > 0
    ? Math.min(0.05, video.duration / 2)
    : 0.001;
}

async function uploadPendingMedia(pending: PendingMedia | null, action: "upload_media_chunk" | "upload_direct_media_chunk", onProgress: (progress: number) => void): Promise<MediaAttachment | null> {
  if (!pending) return null;
  const chunks: MediaChunk[] = [];
  const chunkSize = 1024 * 1024;
  for (let offset = 0; offset < pending.file.size; offset += chunkSize) {
    const chunk = await noise<MediaChunk>({
      action,
      data_base64: await fileBase64(pending.file.slice(offset, offset + chunkSize)),
      relays,
    });
    if (!chunk) throw new Error("relay did not return a media chunk reference");
    chunks.push(chunk);
    onProgress(Math.min(100, Math.round(((offset + chunk.byte_length) / pending.file.size) * 100)));
  }
  return {
    file_name: pending.name,
    mime_type: pending.mimeType,
    byte_length: pending.byteLength,
    chunks,
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
