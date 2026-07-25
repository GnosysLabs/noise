import { useEffect, useState } from "react";
import { noise, relays } from "./api";
import type { LinkPreview } from "./types";

const previewCache = new Map<string, Promise<LinkPreview | null>>();
const pendingRequests: Array<() => void> = [];
let activeRequests = 0;

function schedule<T>(operation: () => Promise<T>): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const start = () => {
      activeRequests += 1;
      void operation().then(resolve, reject).finally(() => {
        activeRequests -= 1;
        pendingRequests.shift()?.();
      });
    };
    if (activeRequests < 4) start();
    else pendingRequests.push(start);
  });
}

function fetchPreview(url: string): Promise<LinkPreview | null> {
  let request = previewCache.get(url);
  if (!request) {
    request = schedule(() =>
      noise<LinkPreview>({ action: "fetch_link_preview", url, relays }),
    ).catch(() => null);
    previewCache.set(url, request);
    if (previewCache.size > 128) {
      previewCache.delete(previewCache.keys().next().value!);
    }
  }
  return request;
}

export function useLinkPreview(url: string | null): LinkPreview | null {
  const [preview, setPreview] = useState<LinkPreview | null>(null);

  useEffect(() => {
    setPreview(null);
    if (!url) return;
    let cancelled = false;
    void fetchPreview(url).then((next) => {
      if (!cancelled) setPreview(next);
    });
    return () => {
      cancelled = true;
    };
  }, [url]);

  return preview;
}
