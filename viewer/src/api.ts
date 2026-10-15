/** Thin client for carson-server's JSON API. Cookies carry the session (same origin). */

export interface User {
  id: string;
  email: string;
  display_name: string;
}

export interface Device {
  id: string;
  name: string;
  os: string;
  agent_version: string;
  created_at: string;
  last_seen_at: string;
  online: boolean;
}

export interface Providers {
  providers: string[];
  dev: boolean;
}

export interface PairingPreview {
  name: string;
  os: string;
  expires_at: string;
}

export class ApiError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
  ) {
    super(message);
  }
}

async function request<T>(method: string, path: string, body?: unknown): Promise<T> {
  const res = await fetch(path, {
    method,
    credentials: "same-origin",
    headers: body === undefined ? {} : { "Content-Type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (res.status === 204) return undefined as T;
  const data: unknown = await res.json().catch(() => ({}));
  if (!res.ok) {
    const err = (data as { error?: { code?: string; message?: string } }).error;
    throw new ApiError(res.status, err?.code ?? "http_error", err?.message ?? `HTTP ${res.status}`);
  }
  return data as T;
}

export const api = {
  me: () => request<User>("GET", "/v1/me"),
  providers: () => request<Providers>("GET", "/auth/providers"),
  devLogin: (email: string) => request<void>("POST", "/auth/dev/login", { email }),
  logout: () => request<void>("POST", "/auth/logout"),
  devices: () => request<{ devices: Device[] }>("GET", "/v1/devices").then((r) => r.devices),
  pairLookup: (code: string) => request<PairingPreview>("POST", "/v1/pair/lookup", { code }),
  pairApprove: (code: string) => request<Device>("POST", "/v1/pair/approve", { code }),
};

/** URL that starts an OAuth/OIDC login and returns to `returnTo` afterwards. */
export function loginUrl(provider: string, returnTo: string): string {
  return `/auth/${encodeURIComponent(provider)}/login?return_to=${encodeURIComponent(returnTo)}`;
}
