import { useEffect, useState } from "react";

export type Route =
  | { page: "login" | "devices" | "pair" | "activity" | "connect" | "connected"; params: URLSearchParams }
  | { page: "view"; params: URLSearchParams; deviceId: string };

export function parseRoute(hash: string): Route {
  const [path = "", query = ""] = hash.replace(/^#/, "").split("?");
  const params = new URLSearchParams(query);
  const view = /^\/view\/([A-Za-z0-9_]+)$/.exec(path);
  if (view?.[1]) return { page: "view", params, deviceId: view[1] };
  const pages = ["login", "pair", "activity", "connect", "connected"] as const;
  const page = pages.find((p) => path === `/${p}`) ?? "devices";
  return { page, params };
}

export function useRoute(): Route {
  const [hash, setHash] = useState(location.hash);
  useEffect(() => {
    const onChange = () => setHash(location.hash);
    window.addEventListener("hashchange", onChange);
    return () => window.removeEventListener("hashchange", onChange);
  }, []);
  return parseRoute(hash);
}

export function navigate(hash: string): void {
  location.hash = hash;
}

/** Sends the user to sign-in, coming back to the current page afterwards. */
export function toLogin(): void {
  navigate("#/login?next=" + encodeURIComponent("/" + (location.hash || "#/devices")));
}

/** "/#/pair?x=1" → "#/pair?x=1"; anything without a hash goes to the device list. */
export function hashOf(returnTo: string): string {
  const i = returnTo.indexOf("#");
  return i >= 0 ? returnTo.slice(i) : "#/devices";
}
