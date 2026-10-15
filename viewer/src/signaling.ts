/** Signaling helpers shared by the viewer. See docs/carson-design.md §6. */

import type { Device, User } from "./api";
import { Backoff } from "./backoff";

export const PROTOCOL_VERSION = 1;

export const MESSAGE_TYPES = [
  "hello",
  "hello.ok",
  "presence",
  "session.request",
  "session.accept",
  "session.reject",
  "sdp.offer",
  "sdp.answer",
  "ice.candidate",
  "session.ice_restart",
  "session.resume",
  "session.end",
  "turn.creds",
] as const;

export type MessageType = (typeof MESSAGE_TYPES)[number];

/** Signaling WebSocket URL for a page served from `origin` (http→ws, https→wss). */
export function signalingUrl(origin: string): string {
  const url = new URL("/v1/ws", origin);
  if (url.protocol === "https:") url.protocol = "wss:";
  else if (url.protocol === "http:") url.protocol = "ws:";
  else throw new Error(`unsupported origin protocol: ${url.protocol}`);
  return url.toString();
}


export interface Envelope {
  v: number;
  type: string;
  id?: string;
  sid?: string;
  body?: unknown;
}

export interface PresenceState {
  user: User | null;
  devices: Device[];
}

interface HelloOk {
  user: User;
  devices: Device[];
}

interface PresenceBody {
  device_id: string;
  online: boolean;
  last_seen_at: string;
}

/** Applies one server message to the device list. Unknown messages leave state unchanged. */
export function applyMessage(state: PresenceState, env: Envelope): PresenceState {
  if (env.v !== PROTOCOL_VERSION) return state;
  switch (env.type) {
    case "hello.ok": {
      const b = env.body as HelloOk;
      return { user: b.user, devices: b.devices };
    }
    case "presence": {
      const p = env.body as PresenceBody;
      return {
        ...state,
        devices: state.devices.map((d) =>
          d.id === p.device_id ? { ...d, online: p.online, last_seen_at: p.last_seen_at } : d,
        ),
      };
    }
    default:
      return state;
  }
}

export type ConnectionStatus = "connecting" | "live" | "reconnecting";

/**
 * Keeps a signaling WebSocket open and reports device presence. Each (re)connect
 * receives a fresh hello.ok snapshot, so no events are lost across reconnects.
 */
export class PresenceClient {
  private ws: WebSocket | null = null;
  private state: PresenceState = { user: null, devices: [] };
  private readonly backoff = new Backoff();
  private timer: ReturnType<typeof setTimeout> | undefined;
  private stopped = false;

  constructor(
    private readonly onState: (s: PresenceState) => void,
    private readonly onStatus: (s: ConnectionStatus) => void,
    private readonly onUnauthorized: () => void,
    private readonly url = signalingUrl(location.origin),
  ) {}

  start(): void {
    this.stopped = false;
    this.connect("connecting");
  }

  stop(): void {
    this.stopped = true;
    clearTimeout(this.timer);
    this.ws?.close(1000, "leaving");
    this.ws = null;
  }

  private connect(status: ConnectionStatus): void {
    this.onStatus(status);
    const ws = new WebSocket(this.url);
    this.ws = ws;
    let opened = false;
    ws.onopen = () => {
      opened = true;
      this.backoff.reset();
      this.onStatus("live");
    };
    ws.onmessage = (ev: MessageEvent<string>) => {
      try {
        this.state = applyMessage(this.state, JSON.parse(ev.data) as Envelope);
        this.onState(this.state);
      } catch {
        // Ignore malformed frames; the server never sends them.
      }
    };
    ws.onclose = () => {
      if (this.stopped || this.ws !== ws) return;
      if (!opened) {
        // Browsers hide the HTTP status of a failed upgrade; check whether we are still signed in.
        void fetch("/v1/me", { credentials: "same-origin" }).then((r) => {
          if (r.status === 401) this.onUnauthorized();
        }, () => {});
      }
      this.timer = setTimeout(() => this.connect("reconnecting"), this.backoff.next());
      this.onStatus("reconnecting");
    };
  }
}
