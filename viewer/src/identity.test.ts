import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { checkPin, keyId, sdpFingerprint, verifyOffer, type PinStore } from "./identity";

const v = JSON.parse(readFileSync(new URL("../../proto/dtls-binding.json", import.meta.url), "utf8")) as Record<string, string>;

describe("DTLS binding", () => {
  it("normalizes the SDP fingerprint like the agent", () => {
    expect(sdpFingerprint(v.sdp!)).toBe(v.fingerprint);
    expect(sdpFingerprint("v=0\r\ns=-\r\n")).toBeNull();
  });

  it("verifies the shared vector (signed with Go's ed25519)", async () => {
    expect(await verifyOffer(v.public_key!, v.sid!, v.sdp!, v.signature)).toBe(true);
  });

  it("rejects tampering, other sessions, missing signatures and other keys", async () => {
    const swapped = v.sdp!.replace("0a:1b", "ff:1b"); // attacker's certificate
    expect(await verifyOffer(v.public_key!, v.sid!, swapped, v.signature)).toBe(false);
    expect(await verifyOffer(v.public_key!, "ses_other", v.sdp!, v.signature)).toBe(false);
    expect(await verifyOffer(v.public_key!, v.sid!, v.sdp!, undefined)).toBe(false);
    expect(await verifyOffer(v.public_key!, v.sid!, v.sdp!, "garbage")).toBe(false);
    const otherKey = "A" + v.public_key!.slice(1);
    expect(await verifyOffer(otherKey, v.sid!, v.sdp!, v.signature)).toBe(false);
  });

  it("formats the key id like carson-agent status", () => {
    const pk = v.public_key!;
    expect(keyId(pk)).toBe(`${pk.slice(0, 4)}-${pk.slice(4, 8)}-${pk.slice(8, 12)}-${pk.slice(12, 16)}`);
  });
});

describe("key pinning", () => {
  it("trusts on first use, then refuses a different key", () => {
    const m = new Map<string, string>();
    const store: PinStore = { get: (k) => m.get(k) ?? null, set: (k, val) => void m.set(k, val) };
    expect(checkPin(store, "dev_1", "keyA")).toBe("new");
    expect(checkPin(store, "dev_1", "keyA")).toBe("match");
    expect(checkPin(store, "dev_1", "keyB")).toBe("mismatch");
    expect(checkPin(store, "dev_2", "keyB")).toBe("new");
  });
});
