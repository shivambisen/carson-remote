import { useSyncExternalStore } from "react";
import { APP_URL, APP_URL_FALLBACK } from "./downloads";

/**
 * Some campus and office networks block the glenley.in name outright. The same app also answers on
 * its onrender.com address, so when app.glenley.in can't be reached the links point there instead.
 */
let current = APP_URL;
const listeners = new Set<() => void>();

async function probe(): Promise<void> {
  const ctrl = new AbortController();
  const timer = setTimeout(() => ctrl.abort(), 5000);
  try {
    // no-cors: we only need to know whether a connection is possible, not to read the answer.
    await fetch(`${APP_URL}/healthz`, { mode: "no-cors", cache: "no-store", signal: ctrl.signal });
  } catch {
    current = APP_URL_FALLBACK;
    listeners.forEach((l) => l());
  } finally {
    clearTimeout(timer);
  }
}

if (typeof window !== "undefined") void probe();

/** The app address that works from this network. */
export function useAppUrl(): string {
  return useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => current,
    () => APP_URL,
  );
}
