/**
 * End-to-end device identity (docs §7). The agent signs its DTLS fingerprint with its device
 * key; the browser enforces that the DTLS certificate matches the SDP fingerprint, so a valid
 * signature proves the media really terminates at that device, even if the server is hostile.
 *
 * The public key comes from the server, so it is pinned on first use per device: a later
 * change is refused (re-pairing creates a new device ID, so a legitimate change never happens).
 * See proto/dtls-binding.json for the exact format.
 */

/** First a=fingerprint value, normalized: lowercase algorithm, space, uppercase hex. */
export function sdpFingerprint(sdp: string): string | null {
  for (const raw of sdp.split(/\r?\n/)) {
    const line = raw.trim();
    if (!line.startsWith("a=fingerprint:")) continue;
    const [algo, hex] = line.slice("a=fingerprint:".length).trim().split(/\s+/);
    if (!algo || !hex) return null;
    return `${algo.toLowerCase()} ${hex.toUpperCase()}`;
  }
  return null;
}

export function dtlsBindingMessage(sid: string, fingerprint: string): Uint8Array<ArrayBuffer> {
  return new TextEncoder().encode(`carson-dtls:v1\n${sid}\n${fingerprint}`);
}

export function fromBase64Url(s: string): Uint8Array<ArrayBuffer> {
  const b64 = s.replace(/-/g, "+").replace(/_/g, "/") + "=".repeat((4 - (s.length % 4)) % 4);
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

/** Verifies an offer's `carson_sig` against the device's public key. Never throws. */
export async function verifyOffer(publicKeyB64: string, sid: string, sdp: string, signatureB64: unknown): Promise<boolean> {
  try {
    if (typeof signatureB64 !== "string") return false;
    const fp = sdpFingerprint(sdp);
    if (!fp) return false;
    const key = await crypto.subtle.importKey("raw", fromBase64Url(publicKeyB64), { name: "Ed25519" }, false, ["verify"]);
    return await crypto.subtle.verify({ name: "Ed25519" }, key, fromBase64Url(signatureB64), dtlsBindingMessage(sid, fp));
  } catch {
    return false;
  }
}

/** Short identity for comparing by eye with `carson-agent status` (first 96 bits, grouped). */
export function keyId(publicKeyB64: string): string {
  return publicKeyB64.slice(0, 16).match(/.{1,4}/g)?.join("-") ?? "";
}

export interface PinStore {
  get(k: string): string | null;
  set(k: string, v: string): void;
}

/** localStorage, tolerating private mode / blocked storage (then pinning is per page load). */
export const browserPins: PinStore = (() => {
  const mem = new Map<string, string>();
  return {
    get: (k) => {
      try {
        return localStorage.getItem(k);
      } catch {
        return mem.get(k) ?? null;
      }
    },
    set: (k, v) => {
      try {
        localStorage.setItem(k, v);
      } catch {
        mem.set(k, v);
      }
    },
  };
})();

/** Trust-on-first-use check for a device's public key. */
export function checkPin(store: PinStore, deviceId: string, publicKeyB64: string): "new" | "match" | "mismatch" {
  const k = `carson.devicekey.${deviceId}`;
  const pinned = store.get(k);
  if (pinned === null) {
    store.set(k, publicKeyB64);
    return "new";
  }
  return pinned === publicKeyB64 ? "match" : "mismatch";
}
