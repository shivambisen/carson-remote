import { describe, expect, it } from "vitest";
import { isValidIceUrl, sanitizeIceServers } from "./session";

describe("isValidIceUrl", () => {
  it("accepts well-formed STUN and TURN URLs", () => {
    for (const u of [
      "stun:stun.cloudflare.com:3478",
      "turn:global.relay.metered.ca:80",
      "turn:global.relay.metered.ca:80?transport=tcp",
      "turns:global.relay.metered.ca:443?transport=tcp",
    ]) {
      expect(isValidIceUrl(u)).toBe(true);
    }
  });

  it("rejects a bare trailing '?' (the misconfigured-env-var case) and other malformed URLs", () => {
    for (const u of ["turn:global.relay.metered.ca:80?", "", "not-a-url", "https://example.com"]) {
      expect(isValidIceUrl(u)).toBe(false);
    }
  });
});

describe("sanitizeIceServers", () => {
  it("drops only the malformed URL within a server entry, keeping the valid ones", () => {
    const out = sanitizeIceServers([
      { urls: ["turn:global.relay.metered.ca:80", "turn:global.relay.metered.ca:80?"], username: "u", credential: "c" },
    ]);
    expect(out).toEqual([{ urls: ["turn:global.relay.metered.ca:80"], username: "u", credential: "c" }]);
  });

  it("drops a whole entry when every one of its URLs is malformed", () => {
    expect(sanitizeIceServers([{ urls: ["turn:x?"] }, { urls: ["stun:stun.l.google.com:19302"] }])).toEqual([
      { urls: ["stun:stun.l.google.com:19302"] },
    ]);
  });
});
