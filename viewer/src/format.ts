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
