import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { MESSAGE_TYPES, signalingUrl } from "./signaling";

describe("signalingUrl", () => {
  it("uses wss for https origins", () => {
    expect(signalingUrl("https://carson.example.com")).toBe("wss://carson.example.com/v1/ws");
  });

  it("uses ws for http origins and keeps the port", () => {
    expect(signalingUrl("http://localhost:5173")).toBe("ws://localhost:5173/v1/ws");
  });

  it("rejects other protocols", () => {
    expect(() => signalingUrl("file:///tmp")).toThrow(/unsupported/);
  });
});

describe("MESSAGE_TYPES", () => {
  it("matches the shared proto fixture", () => {
    const fixture = readFileSync(new URL("../../proto/message-types.txt", import.meta.url), "utf8")
      .split("\n")
      .filter(Boolean);
    expect([...MESSAGE_TYPES]).toEqual(fixture);
  });
});
