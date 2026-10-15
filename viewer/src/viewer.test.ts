import { describe, expect, it } from "vitest";
import type { Device } from "./api";
import { loginUrl } from "./api";
import { Backoff } from "./backoff";
import { osLabel, timeAgo } from "./format";
import { formatCode, isCompleteCode, normalizeCode } from "./pairing";
import { applyMessage, type PresenceState } from "./signaling";

const dev = (id: string, online = false): Device => ({
  id,
  name: id,
  os: "macos",
  agent_version: "",
  created_at: "2026-10-01T00:00:00Z",
  last_seen_at: "0001-01-01T00:00:00Z",
  online,
});

describe("applyMessage", () => {
  const empty: PresenceState = { user: null, devices: [] };
  const user = { id: "usr_1", email: "a@x.com", display_name: "a" };

  it("replaces state on hello.ok", () => {
    const s = applyMessage(empty, { v: 1, type: "hello.ok", body: { user, devices: [dev("d1")] } });
    expect(s.user?.id).toBe("usr_1");
    expect(s.devices.map((d) => d.id)).toEqual(["d1"]);
  });

  it("updates only the matching device on presence", () => {
    const s0 = applyMessage(empty, { v: 1, type: "hello.ok", body: { user, devices: [dev("d1"), dev("d2")] } });
    const s1 = applyMessage(s0, {
      v: 1,
      type: "presence",
      body: { device_id: "d2", online: true, last_seen_at: "2026-10-15T10:00:00Z" },
    });
    expect(s1.devices.map((d) => d.online)).toEqual([false, true]);
    expect(s1.devices[1]?.last_seen_at).toBe("2026-10-15T10:00:00Z");
    expect(s0.devices[1]?.online).toBe(false); // immutable
  });

  it("ignores presence for unknown devices, unknown types and other versions", () => {
    const s0 = applyMessage(empty, { v: 1, type: "hello.ok", body: { user, devices: [dev("d1")] } });
    const p = { device_id: "nope", online: true, last_seen_at: "" };
    expect(applyMessage(s0, { v: 1, type: "presence", body: p }).devices).toEqual(s0.devices);
    expect(applyMessage(s0, { v: 1, type: "future.thing" })).toBe(s0);
    expect(applyMessage(s0, { v: 2, type: "hello.ok", body: { user, devices: [] } })).toBe(s0);
  });
});

describe("pairing codes", () => {
  it("normalizes and formats as the user types", () => {
    expect(formatCode("bcdf")).toBe("BCDF");
    expect(formatCode("bcdfg")).toBe("BCDF-G");
    expect(formatCode(" bcdf-ghjk ")).toBe("BCDF-GHJK");
    expect(formatCode("bcdfghjkxyz")).toBe("BCDF-GHJK"); // capped at 8
    expect(normalizeCode("aeiou1-0")).toBe(""); // vowels and digits are never in codes
  });

  it("detects complete codes", () => {
    expect(isCompleteCode("BCDF-GHJK")).toBe(true);
    expect(isCompleteCode("BCDF-GHJ")).toBe(false);
  });
});

describe("format", () => {
  const now = new Date("2026-10-15T12:00:00Z");
  it("renders relative times", () => {
    expect(timeAgo("0001-01-01T00:00:00Z", now)).toBe("never");
    expect(timeAgo("2026-10-15T11:59:30Z", now)).toBe("just now");
    expect(timeAgo("2026-10-15T11:55:00Z", now)).toBe("5 min ago");
    expect(timeAgo("2026-10-15T09:00:00Z", now)).toBe("3 h ago");
    expect(timeAgo("2026-10-13T12:00:00Z", now)).toBe("2 d ago");
    expect(timeAgo("garbage", now)).toBe("never");
  });

  it("labels operating systems", () => {
    expect(osLabel("macos")).toBe("macOS");
    expect(osLabel("plan9")).toBe("plan9");
  });
});

describe("Backoff", () => {
  it("stays within [d/2, d] and caps at 10s", () => {
    for (const r of [0, 0.5, 0.999]) {
      const b = new Backoff(500, 10_000, () => r);
      const delays = Array.from({ length: 8 }, () => b.next());
      delays.forEach((d, i) => {
        const cap = Math.min(10_000, 500 * 2 ** i);
        expect(d).toBeGreaterThanOrEqual(cap / 2);
        expect(d).toBeLessThanOrEqual(cap);
      });
    }
  });

  it("resets to the base delay", () => {
    const b = new Backoff(500, 10_000, () => 1);
    for (let i = 0; i < 6; i++) b.next();
    b.reset();
    expect(b.next()).toBe(500);
  });
});

describe("loginUrl", () => {
  it("encodes the return path", () => {
    expect(loginUrl("github", "/#/pair?code=AB")).toBe("/auth/github/login?return_to=%2F%23%2Fpair%3Fcode%3DAB");
  });
});
