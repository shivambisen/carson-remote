/** Direct download links: always the newest release, one click, no GitHub page in between. */
const LATEST = "https://github.com/shivambisen/carson-remote/releases/latest/download";

export const APP_URL = "https://app.glenley.in";
export const SOURCE_URL = "https://github.com/shivambisen/carson-remote";

export type Platform = "mac-arm" | "mac-intel" | "windows" | "other";

export interface Download {
  platform: Exclude<Platform, "other">;
  label: string;
  detail: string;
  file: string;
  href: string;
}

export const DOWNLOADS: Record<Download["platform"], Download> = {
  "mac-arm": {
    platform: "mac-arm",
    label: "macOS",
    detail: "Apple Silicon (M1 and later) · macOS 14+",
    file: "carson-macos-arm64.zip",
    href: `${LATEST}/carson-macos-arm64.zip`,
  },
  "mac-intel": {
    platform: "mac-intel",
    label: "macOS (Intel)",
    detail: "Intel Macs · macOS 14+",
    file: "carson-macos-x86_64.zip",
    href: `${LATEST}/carson-macos-x86_64.zip`,
  },
  windows: {
    platform: "windows",
    label: "Windows",
    detail: "Windows 10 (1903) or later · 64-bit",
    file: "carson-setup-windows-x86_64.exe",
    href: `${LATEST}/carson-setup-windows-x86_64.exe`,
  },
};

interface NavigatorUAData {
  platform?: string;
  mobile?: boolean;
  getHighEntropyValues?: (hints: string[]) => Promise<{ architecture?: string }>;
}

/** Best guess from the user agent (sync); refine with `refinePlatform` where the browser allows. */
export function detectPlatform(ua = navigator.userAgent, uaData?: NavigatorUAData): Platform {
  const platform = uaData?.platform ?? "";
  if (uaData?.mobile || /iPhone|iPad|iPod|Android/i.test(ua)) return "other";
  if (/Windows/i.test(platform) || /Windows NT/i.test(ua)) return "windows";
  if (/mac/i.test(platform) || /Macintosh|Mac OS X/i.test(ua)) {
    // Touch-capable "Macintosh" is an iPad in desktop mode.
    if (typeof navigator !== "undefined" && navigator.maxTouchPoints > 1) return "other";
    return "mac-arm"; // every Mac sold since 2021; Safari doesn't say which
  }
  return "other";
}

/** Chromium can tell Apple Silicon from Intel; others keep the guess. */
export async function refinePlatform(guess: Platform, uaData?: NavigatorUAData): Promise<Platform> {
  if (guess !== "mac-arm" || !uaData?.getHighEntropyValues) return guess;
  try {
    const { architecture } = await uaData.getHighEntropyValues(["architecture"]);
    return architecture === "x86" ? "mac-intel" : guess;
  } catch {
    return guess;
  }
}

export function currentUAData(): NavigatorUAData | undefined {
  return (navigator as Navigator & { userAgentData?: NavigatorUAData }).userAgentData;
}
