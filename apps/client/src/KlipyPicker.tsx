import { LoaderCircle, Search, X } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { noise, relays } from "./api";

type KlipyKind = "gif" | "sticker" | "clip";

type KlipyResult = {
  id: string;
  kind: KlipyKind;
  title: string;
  preview_url: string;
  preview_blur: string | null;
  full_url: string;
  full_mp4_url: string | null;
  full_webp_url: string | null;
  width: number;
  height: number;
  mime_type: string;
};

const tabs: Array<{ kind: KlipyKind; label: string }> = [
  { kind: "gif", label: "GIFs" },
  { kind: "sticker", label: "stickers" },
  { kind: "clip", label: "clips" },
];

export function KlipyPicker({
  disabled,
  onPick,
}: {
  disabled: boolean;
  onPick: (file: File, onProgress: (progress: number) => void) => Promise<boolean>;
}) {
  const [open, setOpen] = useState(false);
  const [kind, setKind] = useState<KlipyKind>("gif");
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<KlipyResult[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [pickingId, setPickingId] = useState<string | null>(null);
  const [sendProgress, setSendProgress] = useState<number | null>(null);
  const root = useRef<HTMLDivElement>(null);
  const searchInput = useRef<HTMLInputElement>(null);
  const requestId = useRef(0);

  useEffect(() => {
    if (!open) return;
    const close = (event: MouseEvent) => {
      if (!root.current?.contains(event.target as Node)) setOpen(false);
    };
    const escape = (event: KeyboardEvent) => {
      if (event.key === "Escape") setOpen(false);
    };
    window.addEventListener("mousedown", close);
    window.addEventListener("keydown", escape);
    window.setTimeout(() => searchInput.current?.focus(), 0);
    return () => {
      window.removeEventListener("mousedown", close);
      window.removeEventListener("keydown", escape);
    };
  }, [open]);

  useEffect(() => {
    if (!open) return;
    const currentRequest = ++requestId.current;
    const timer = window.setTimeout(() => {
      setLoading(true);
      setError(null);
      void noise<KlipyResult[]>({
        action: "fetch_klipy_media",
        kind,
        query: query.trim() || null,
        limit: 24,
        relays,
      })
        .then((next) => {
          if (currentRequest !== requestId.current) return;
          setResults(next ?? []);
        })
        .catch((cause) => {
          if (currentRequest !== requestId.current) return;
          setResults([]);
          const detail = cause instanceof Error ? cause.message : String(cause);
          setError(
            detail.includes("klipy_not_configured")
              ? "GIF search is not configured yet."
              : "GIF search is unavailable right now.",
          );
        })
        .finally(() => {
          if (currentRequest === requestId.current) setLoading(false);
        });
    }, query.trim() ? 250 : 0);
    return () => window.clearTimeout(timer);
  }, [kind, open, query]);

  async function pick(result: KlipyResult) {
    setPickingId(result.id);
    setError(null);
    let downloaded = false;
    try {
      const response = await fetch(result.full_url, {
        credentials: "omit",
        redirect: "follow",
      });
      if (!response.ok) throw new Error("GIF download failed");
      const blob = await response.blob();
      if (!blob.size || blob.size > 500 * 1024 * 1024) {
        throw new Error("GIF is too large");
      }
      downloaded = true;
      const extension = result.mime_type === "image/gif"
        ? "gif"
        : result.mime_type === "image/webp"
          ? "webp"
          : "mp4";
      const safeId = result.id.replace(/[^a-zA-Z0-9_-]/g, "-").slice(0, 80) || "media";
      const sent = await onPick(new File(
        [blob],
        `klipy-${result.kind}-${safeId}.${extension}`,
        { type: result.mime_type },
      ), setSendProgress);
      if (!sent) throw new Error("media send failed");
      setOpen(false);
    } catch {
      setError(downloaded
        ? "That media could not be sent."
        : "That media could not be downloaded.");
    } finally {
      setPickingId(null);
      setSendProgress(null);
    }
  }

  return (
    <div className="klipy-picker-shell" ref={root}>
      <button
        className={`gif-button ${open ? "active" : ""}`}
        type="button"
        disabled={disabled}
        onClick={() => {
          setOpen((current) => !current);
          setError(null);
        }}
        aria-label="GIF keyboard"
        aria-expanded={open}
        title="GIF keyboard"
      >
        GIF
      </button>
      {open && (
        <div className="klipy-picker" role="dialog" aria-label="GIF keyboard">
          <div className="klipy-tabs">
            {tabs.map((tab) => (
              <button
                type="button"
                className={kind === tab.kind ? "active" : ""}
                key={tab.kind}
                onClick={() => {
                  setKind(tab.kind);
                  setResults([]);
                }}
              >
                {tab.label}
              </button>
            ))}
          </div>
          <label className="klipy-search">
            <Search size={14} aria-hidden="true" />
            <input
              ref={searchInput}
              value={query}
              maxLength={100}
              onChange={(event) => setQuery(event.target.value)}
              placeholder={`search ${kind === "gif" ? "GIFs" : `${kind}s`}`}
            />
            {query && (
              <button type="button" onClick={() => setQuery("")} aria-label="clear search">
                <X size={13} />
              </button>
            )}
          </label>
          <div className="klipy-results">
            {error ? (
              <div className="klipy-status error">{error}</div>
            ) : loading && results.length === 0 ? (
              <div className="klipy-status"><LoaderCircle className="spinner" size={22} /></div>
            ) : results.length === 0 ? (
              <div className="klipy-status">no results</div>
            ) : (
              results.map((result) => (
                <button
                  type="button"
                  className={pickingId === result.id ? "picking" : ""}
                  key={`${result.kind}:${result.id}`}
                  disabled={pickingId !== null}
                  onClick={() => void pick(result)}
                  title={result.title || `send ${result.kind}`}
                >
                  <img src={result.preview_url} alt={result.title} loading="lazy" />
                  {pickingId === result.id && (
                    <span>
                      {sendProgress === null
                        ? <LoaderCircle className="spinner" size={20} />
                        : <small>{sendProgress}%</small>}
                    </span>
                  )}
                </button>
              ))
            )}
          </div>
          <div className="klipy-attribution">
            <a href="https://klipy.com" target="_blank" rel="noreferrer">Powered by KLIPY</a>
            {loading && results.length > 0 && <span>updating…</span>}
          </div>
        </div>
      )}
    </div>
  );
}
