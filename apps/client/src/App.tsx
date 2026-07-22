import {
  ArrowUp,
  AudioWaveform,
  Camera,
  Copy,
  Globe2,
  LoaderCircle,
  Plus,
  Radio,
  RefreshCw,
  Settings2,
  Trash2,
  Users,
  X,
} from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { isTauri, noise, prepareImage, relays } from "./api";
import { generateGroupAvatar } from "./groupAvatar";
import type {
  AvatarData,
  Conversation,
  GroupSummary,
  IdentitySummary,
  LocalSummary,
  MakeResult,
  MemberSummary,
  MessageSummary,
  ProfileImage,
} from "./types";

type Dialog =
  | { type: "make" }
  | { type: "join" }
  | { type: "frequency"; group: string; frequency: string }
  | { type: "profile"; profile: IdentitySummary }
  | { type: "group"; group: GroupSummary }
  | { type: "delete_group"; group: GroupSummary }
  | { type: "members"; members: MemberSummary[] }
  | { type: "person"; person: Pick<MemberSummary, "username" | "bio" | "avatar"> };

const avatarCache = new Map<string, string>();

export default function App() {
  const [summary, setSummary] = useState<LocalSummary | null>(null);
  const [conversation, setConversation] = useState<Conversation | null>(null);
  const [dialog, setDialog] = useState<Dialog | null>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [groupMenu, setGroupMenu] = useState<{
    group: GroupSummary;
    x: number;
    y: number;
  } | null>(null);

  const refresh = useCallback(async () => {
    const local = await noise<LocalSummary>({ action: "status" });
    setSummary(local);
    if (local?.groups.some((group) => group.is_active)) {
      const next = await noise<Conversation>({ action: "conversation", relays });
      setConversation(next);
      const reconciled = await noise<LocalSummary>({ action: "status" });
      setSummary(reconciled);
    } else {
      setConversation(null);
    }
  }, []);

  useEffect(() => {
    if (!isTauri) {
      setLoading(false);
      return;
    }
    void refresh()
      .catch((cause) => setError(message(cause)))
      .finally(() => setLoading(false));
  }, [refresh]);

  async function perform(operation: () => Promise<void>) {
    if (busy) return false;
    setBusy(true);
    setError(null);
    try {
      await operation();
      return true;
    } catch (cause) {
      setError(message(cause));
      return false;
    } finally {
      setBusy(false);
    }
  }

  if (!isTauri) return <BrowserFoundation />;
  if (loading) return <Loading />;
  if (!summary) {
    return (
      <Onboarding
        busy={busy}
        onSubmit={(username) =>
          perform(async () => {
            const local = await noise<LocalSummary>({ action: "initialize", username });
            setSummary(local);
          })
        }
      />
    );
  }

  return (
    <div className="app-shell">
      <Sidebar
        summary={summary}
        onMake={() => setDialog({ type: "make" })}
        onJoin={() => setDialog({ type: "join" })}
        onProfile={() => setDialog({ type: "profile", profile: summary.identity })}
        onContextMenu={(group, x, y) => {
          if (group.owner_public_key !== summary.identity.public_key) return;
          setGroupMenu({ group, x, y });
        }}
        onSelect={(group) => {
          if (group.is_active) return;
          void perform(async () => {
            const local = await noise<LocalSummary>({
              action: "select_group",
              group_id: group.group_id,
            });
            setSummary(local);
            await refresh();
          });
        }}
      />

      <main className="conversation-pane">
        {conversation ? (
          <ConversationPanel
            conversation={conversation}
            busy={busy}
            onGroup={() => setDialog({ type: "group", group: conversation.group })}
            onMembers={() => setDialog({ type: "members", members: conversation.members })}
            onPerson={(person) => setDialog({ type: "person", person })}
            onRefresh={() => void perform(refresh)}
            onSend={(text) =>
              perform(async () => {
                await noise<null>({ action: "say", text, relays });
                await refresh();
              })
            }
          />
        ) : (
          <EmptyGroup
            onMake={() => setDialog({ type: "make" })}
            onJoin={() => setDialog({ type: "join" })}
          />
        )}
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
      {dialog?.type === "profile" && (
        <ProfileDialog
          profile={dialog.profile}
          busy={busy}
          onClose={() => setDialog(null)}
          onSave={(bio, avatar, removeAvatar) =>
            perform(async () => {
              const local = await noise<LocalSummary>({
                action: "update_profile",
                bio,
                avatar_data_base64: avatar,
                avatar_mime_type: avatar ? "image/jpeg" : null,
                remove_avatar: removeAvatar,
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
        <GroupDialog
          group={dialog.group}
          canEdit={dialog.group.owner_public_key === summary.identity.public_key}
          busy={busy}
          onClose={() => setDialog(null)}
          onSave={(name, description, avatar, removeAvatar) =>
            perform(async () => {
              const local = await noise<LocalSummary>({
                action: "update_group_profile",
                name,
                description,
                avatar_data_base64: avatar,
                avatar_mime_type: avatar ? "image/jpeg" : null,
                remove_avatar: removeAvatar,
                relays,
              });
              setSummary(local);
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
              avatarCache.clear();
              await refresh();
              setDialog(null);
            })
          }
        />
      )}
      {dialog?.type === "members" && (
        <MembersDialog members={dialog.members} onClose={() => setDialog(null)} />
      )}
      {dialog?.type === "person" && (
        <PersonDialog person={dialog.person} onClose={() => setDialog(null)} />
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
        />
      )}
      {error && <ErrorToast error={error} onClose={() => setError(null)} />}
    </div>
  );
}

function Sidebar({
  summary,
  onMake,
  onJoin,
  onProfile,
  onContextMenu,
  onSelect,
}: {
  summary: LocalSummary;
  onMake: () => void;
  onJoin: () => void;
  onProfile: () => void;
  onContextMenu: (group: GroupSummary, x: number, y: number) => void;
  onSelect: (group: GroupSummary) => void;
}) {
  return (
    <aside className="sidebar">
      <div className="sidebar-drag" data-tauri-drag-region />
      <div className="brand"><AudioWaveform size={19} /><strong>noise</strong></div>
      <div className="sidebar-actions">
        <button className="wide-button" onClick={onMake}><Plus size={15} /> make noise</button>
        <button className="square-button" onClick={onJoin} title="tune in"><Radio size={16} /></button>
      </div>
      <div className="group-list">
        {summary.groups.map((group) => (
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
            {group.is_active && <i />}
          </button>
        ))}
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
  onClose,
  onDelete,
}: {
  x: number;
  y: number;
  onClose: () => void;
  onDelete: () => void;
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
      <button onClick={onDelete}><Trash2 size={14} /> delete group</button>
    </div>
  );
}

function ConversationPanel({
  conversation,
  busy,
  onGroup,
  onMembers,
  onPerson,
  onRefresh,
  onSend,
}: {
  conversation: Conversation;
  busy: boolean;
  onGroup: () => void;
  onMembers: () => void;
  onPerson: (person: Pick<MemberSummary, "username" | "bio" | "avatar">) => void;
  onRefresh: () => void;
  onSend: (text: string) => Promise<boolean>;
}) {
  const [draft, setDraft] = useState("");
  const bottom = useRef<HTMLDivElement>(null);
  useEffect(() => bottom.current?.scrollIntoView(), [conversation.messages.length]);
  async function submit() {
    const text = draft.trim();
    if (!text || busy) return;
    setDraft("");
    if (!(await onSend(text))) setDraft(text);
  }
  return (
    <div className="conversation">
      <header className="chat-header" data-tauri-drag-region>
        <button className="group-identity" onClick={onGroup}>
          <Avatar name={conversation.group.name} image={conversation.group.avatar} size={36} square />
          <span><strong>{conversation.group.name}</strong><small>{conversation.group.description || "view group profile"}</small></span>
        </button>
        <div className="chat-header-actions">
          <button onClick={onMembers}>{conversation.members.length} {conversation.members.length === 1 ? "signal" : "signals"}</button>
          {busy && <LoaderCircle className="spinner" size={14} />}
          <button className="icon-button" onClick={onRefresh} title="refresh"><RefreshCw size={14} /></button>
        </div>
      </header>
      <div className="messages">
        {conversation.messages.length === 0 && <div className="quiet">the group is quiet</div>}
        {conversation.messages.map((item) => (
          <MessageRow key={item.event_id} message={item} onPerson={onPerson} />
        ))}
        <div ref={bottom} />
      </div>
      <div className="composer">
        <textarea
          rows={1}
          value={draft}
          placeholder="send noise"
          onChange={(event) => setDraft(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Enter" && !event.shiftKey) {
              event.preventDefault();
              void submit();
            }
          }}
        />
        <button disabled={!draft.trim() || busy} onClick={() => void submit()}><ArrowUp size={17} /></button>
      </div>
      <aside className="member-sidebar">
        <div className="member-sidebar-heading">
          <strong>signals</strong>
          <span>{conversation.members.length}</span>
        </div>
        <div className="member-sidebar-list">
          {conversation.members.map((member) => (
            <button key={member.public_key} className="member-sidebar-row" onClick={() => onPerson(member)}>
              <Avatar name={member.username} image={member.avatar} size={30} />
              <span className="member-sidebar-copy">
                <span>
                  <strong>@{member.username}</strong>
                  {member.public_key === conversation.group.owner_public_key && <i>founder</i>}
                </span>
                <small>{member.bio || "tuned in"}</small>
              </span>
            </button>
          ))}
        </div>
      </aside>
    </div>
  );
}

function MessageRow({
  message,
  onPerson,
}: {
  message: MessageSummary;
  onPerson: (person: Pick<MemberSummary, "username" | "bio" | "avatar">) => void;
}) {
  const person = { username: message.username, bio: message.bio, avatar: message.avatar };
  return (
    <article className="message-row">
      <button onClick={() => onPerson(person)}><Avatar name={message.username} image={message.avatar} size={34} /></button>
      <div><div className="message-meta"><button onClick={() => onPerson(person)}>@{message.username}</button><time>{formatTime(message.created_at_millis)}</time></div><p>{message.text}</p></div>
    </article>
  );
}

function Avatar({ name, image, size, square = false }: { name: string; image: ProfileImage | null; size: number; square?: boolean }) {
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
    void noise<AvatarData>({ action: "fetch_avatar", image: target, relays }).then((data) => {
      if (!data || !active) return;
      const value = `data:${data.mime_type};base64,${data.data_base64}`;
      avatarCache.set(target.blob_id, value);
      setLoaded({ blobId: target.blob_id, source: value });
    }).catch(() => undefined);
    return () => { active = false; };
  }, [image]);
  return (
    <span className={`avatar ${square ? "square" : ""}`} style={{ width: size, height: size }}>
      {source ? <img src={source} alt="" /> : <b>{name.slice(0, 1).toUpperCase()}</b>}
    </span>
  );
}

function Onboarding({ busy, onSubmit }: { busy: boolean; onSubmit: (username: string) => Promise<boolean> }) {
  const [username, setUsername] = useState("");
  return (
    <div className="onboarding" data-tauri-drag-region>
      <AudioWaveform size={40} />
      <h1>noise</h1>
      <p>no phone number. no email. just a name and a key.</p>
      <input autoFocus value={username} onChange={(event) => setUsername(event.target.value)} placeholder="choose a username" />
      <button disabled={!username.trim() || busy} onClick={() => void onSubmit(username.trim())}>{busy && <LoaderCircle className="spinner" size={14} />} enter noise</button>
      <small>your identity is generated on this device</small>
    </div>
  );
}

function EmptyGroup({ onMake, onJoin }: { onMake: () => void; onJoin: () => void }) {
  return <div className="empty-group"><Radio size={38} /><h2>nothing but noise</h2><p>make a group or enter a frequency someone gave you</p><div><button onClick={onMake}>make noise</button><button onClick={onJoin}>tune in</button></div></div>;
}

function MakeDialog({ busy, onClose, onSubmit }: { busy: boolean; onClose: () => void; onSubmit: (name: string) => Promise<boolean> }) {
  const [name, setName] = useState("");
  return <Modal onClose={onClose}><DialogHeading icon={<AudioWaveform />} title="make noise" detail="name the group" /><input autoFocus value={name} onChange={(event) => setName(event.target.value)} placeholder="group name" /><DialogButtons onClose={onClose}><button className="primary" disabled={!name.trim() || busy} onClick={() => void onSubmit(name.trim())}>make noise</button></DialogButtons></Modal>;
}

function JoinDialog({ busy, onClose, onSubmit }: { busy: boolean; onClose: () => void; onSubmit: (frequency: string) => Promise<boolean> }) {
  const [frequency, setFrequency] = useState("");
  const digits = frequency.replace(/\D/g, "").slice(0, 12);
  return <Modal onClose={onClose}><DialogHeading icon={<Radio />} title="tune in" detail="enter a 12-digit frequency" /><input autoFocus className="frequency-input" value={frequency} onChange={(event) => setFrequency(event.target.value)} placeholder="0000 0000 0000" /><DialogButtons onClose={onClose}><button className="primary" disabled={digits.length !== 12 || busy} onClick={() => void onSubmit(digits)}>tune in</button></DialogButtons></Modal>;
}

function FrequencyDialog({ group, frequency, onClose }: { group: string; frequency: string; onClose: () => void }) {
  return <Modal onClose={onClose}><DialogHeading icon={<Radio />} title="you're live" detail={`share this frequency to invite people to ${group}`} /><div className="frequency-card">{frequency}</div><DialogButtons><button onClick={() => void navigator.clipboard.writeText(frequency)}><Copy size={14} /> copy frequency</button><button className="primary" onClick={onClose}>done</button></DialogButtons></Modal>;
}

function ProfileDialog({ profile, busy, onClose, onSave }: { profile: IdentitySummary; busy: boolean; onClose: () => void; onSave: (bio: string, avatar: string | null, remove: boolean) => Promise<boolean> }) {
  const [bio, setBio] = useState(profile.bio);
  const image = useImageSelection();
  return <Modal onClose={onClose}><div className="identity-editor"><ImagePicker name={profile.username} existing={profile.avatar} selection={image} /><h2>@{profile.username}</h2><small>your public identity</small></div><LabeledArea label="bio" count={`${bio.length}/160`}><textarea value={bio} onChange={(event) => setBio(event.target.value)} /></LabeledArea><DialogButtons onClose={onClose}>{(profile.avatar || image.preview) && <button className="danger" onClick={image.remove}>remove photo</button>}<button className="primary" disabled={bio.length > 160 || busy} onClick={() => void onSave(bio, image.base64, image.removed)}>save profile</button></DialogButtons></Modal>;
}

function GroupDialog({ group, canEdit, busy, onClose, onSave }: { group: GroupSummary; canEdit: boolean; busy: boolean; onClose: () => void; onSave: (name: string, description: string, avatar: string | null, remove: boolean) => Promise<boolean> }) {
  const [name, setName] = useState(group.name);
  const [description, setDescription] = useState(group.description);
  const image = useImageSelection();
  return <Modal onClose={onClose}><div className="identity-editor"><ImagePicker name={group.name} existing={group.avatar} selection={image} square disabled={!canEdit} /><small>{canEdit ? "group identity" : "group"}</small></div><LabeledArea label="name"><input value={name} disabled={!canEdit} onChange={(event) => setName(event.target.value)} /></LabeledArea><LabeledArea label="description" count={canEdit ? `${description.length}/200` : undefined}><textarea value={description} disabled={!canEdit} onChange={(event) => setDescription(event.target.value)} /></LabeledArea>{!canEdit && <p className="founder-note">managed by the group founder</p>}<DialogButtons onClose={onClose}>{canEdit && (group.avatar || image.preview) && <button className="danger" onClick={image.remove}>remove icon</button>}{canEdit && <button className="primary" disabled={!name.trim() || name.length > 80 || description.length > 200 || busy} onClick={() => void onSave(name.trim(), description, image.base64, image.removed)}>save group</button>}</DialogButtons></Modal>;
}

function DeleteGroupDialog({ group, busy, onClose, onDelete }: { group: GroupSummary; busy: boolean; onClose: () => void; onDelete: () => Promise<boolean> }) {
  const warning = group.remote_deletion_supported
    ? "This permanently erases its messages, invitation, and group media from the relays. It cannot be undone."
    : "This older group predates authenticated relay deletion. It will be removed from this device; groups made from this version onward are erased from the relays too.";
  return <Modal onClose={onClose} compact><DialogHeading icon={<Trash2 />} title="delete group?" detail={group.name} /><p className="deletion-warning">{warning}</p><DialogButtons onClose={onClose}><button className="delete-confirm" disabled={busy} onClick={() => void onDelete()}>{busy && <LoaderCircle className="spinner" size={13} />} {group.remote_deletion_supported ? "delete group" : "remove group"}</button></DialogButtons></Modal>;
}

function MembersDialog({ members, onClose }: { members: MemberSummary[]; onClose: () => void }) {
  return <Modal onClose={onClose} compact><DialogHeading icon={<Users />} title="signals" detail={`${members.length} tuned in`} /><div className="member-list">{members.map((member) => <div key={member.public_key}><Avatar name={member.username} image={member.avatar} size={38} /><span><strong>@{member.username}</strong><small>{member.bio}</small></span></div>)}</div></Modal>;
}

function PersonDialog({ person, onClose }: { person: Pick<MemberSummary, "username" | "bio" | "avatar">; onClose: () => void }) {
  return <Modal onClose={onClose} compact><div className="person-card"><Avatar name={person.username} image={person.avatar} size={72} /><h2>@{person.username}</h2><p>{person.bio || "no bio yet"}</p></div></Modal>;
}

function Modal({ children, onClose, compact = false }: { children: React.ReactNode; onClose: () => void; compact?: boolean }) {
  return <div className="modal-backdrop" onMouseDown={onClose}><section className={`modal ${compact ? "compact" : ""}`} onMouseDown={(event) => event.stopPropagation()}><button className="modal-close" onClick={onClose}><X size={15} /></button>{children}</section></div>;
}

function DialogHeading({ icon, title, detail }: { icon: React.ReactNode; title: string; detail: string }) {
  return <div className="dialog-heading"><span>{icon}</span><h2>{title}</h2><p>{detail}</p></div>;
}

function DialogButtons({ children, onClose }: { children: React.ReactNode; onClose?: () => void }) {
  return <div className="dialog-buttons">{onClose && <button onClick={onClose}>cancel</button>}<span />{children}</div>;
}

function LabeledArea({ label, count, children }: { label: string; count?: string; children: React.ReactNode }) {
  return <label className="labeled-area"><span><strong>{label}</strong><small>{count}</small></span>{children}</label>;
}

function ImagePicker({ name, existing, selection, square = false, disabled = false }: { name: string; existing: ProfileImage | null; selection: ReturnType<typeof useImageSelection>; square?: boolean; disabled?: boolean }) {
  const input = useRef<HTMLInputElement>(null);
  return <button className="image-picker" disabled={disabled} onClick={() => input.current?.click()}><span className={`avatar ${square ? "square" : ""}`} style={{ width: 96, height: 96 }}>{selection.preview ? <img src={selection.preview} alt="" /> : <Avatar name={name} image={selection.removed ? null : existing} size={96} square={square} />}</span>{!disabled && <i><Camera size={13} /></i>}<input ref={input} hidden type="file" accept="image/*" onChange={(event) => void selection.choose(event.target.files?.[0])} /></button>;
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

function Loading() { return <div className="loading"><LoaderCircle className="spinner" /></div>; }

function BrowserFoundation() {
  return <div className="browser-foundation"><Globe2 size={42} /><h1>noise for the browser</h1><p>The shared interface is running. The browser still needs the Rust cryptography compiled to WASM and IndexedDB identity storage before it can safely enter a live group.</p><small>desktop uses this exact React build through Tauri</small></div>;
}

function formatTime(millis: number) {
  return new Intl.DateTimeFormat(undefined, { hour: "numeric", minute: "2-digit" }).format(new Date(millis));
}

function message(cause: unknown) { return cause instanceof Error ? cause.message : String(cause); }
