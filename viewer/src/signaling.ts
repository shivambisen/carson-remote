/** Signaling helpers shared by the viewer. See docs/carson-design.md §6. */

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
