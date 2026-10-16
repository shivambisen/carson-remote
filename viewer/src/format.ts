const OS_LABELS: Record<string, string> = { macos: "macOS", windows: "Windows", linux: "Linux" };

export function osLabel(os: string): string {
  return OS_LABELS[os] ?? os;
}

/** "just now", "5 min ago", "3 h ago", "2 d ago", or "never" for the zero time. */
export function timeAgo(iso: string, now: Date = new Date()): string {
  const t = Date.parse(iso);
  if (Number.isNaN(t) || t <= 0 || iso.startsWith("0001-")) return "never";
  const s = Math.max(0, Math.round((now.getTime() - t) / 1000));
  if (s < 60) return "just now";
  if (s < 3600) return `${Math.floor(s / 60)} min ago`;
  if (s < 86400) return `${Math.floor(s / 3600)} h ago`;
  return `${Math.floor(s / 86400)} d ago`;
}

/** "Mozilla/5.0 (Macintosh…) … Chrome/150.0 Safari/537.36" → "Chrome on macOS". */
export function browserLabel(ua: string): string {
  const browser = /Edg\//.test(ua) ? "Edge" : /Firefox\//.test(ua) ? "Firefox" : /Chrome\//.test(ua) ? "Chrome" : /Safari\//.test(ua) ? "Safari" : "";
  const os = /Mac OS X|Macintosh/.test(ua) ? "macOS" : /Windows/.test(ua) ? "Windows" : /Android/.test(ua) ? "Android" : /iPhone|iPad/.test(ua) ? "iOS" : /Linux/.test(ua) ? "Linux" : "";
  return [browser, os].filter(Boolean).join(" on ") || ua.slice(0, 40);
}
