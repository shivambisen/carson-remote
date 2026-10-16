import { describe, expect, it } from "vitest";
import { detectPlatform, DOWNLOADS, refinePlatform } from "./downloads";

const MAC = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Safari/605.1.15";
const WIN = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/150.0 Safari/537.36";
const IPHONE = "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15";

describe("platform detection", () => {
  it("recognises desktops and phones", () => {
    expect(detectPlatform(WIN)).toBe("windows");
    expect(detectPlatform(MAC)).toBe("mac-arm");
    expect(detectPlatform(IPHONE)).toBe("other");
    expect(detectPlatform("Mozilla/5.0 (X11; Linux x86_64)")).toBe("other");
  });
  it("uses client hints for Intel Macs", async () => {
    const intel = { platform: "macOS", getHighEntropyValues: async () => ({ architecture: "x86" }) };
    expect(await refinePlatform("mac-arm", intel)).toBe("mac-intel");
    const arm = { platform: "macOS", getHighEntropyValues: async () => ({ architecture: "arm" }) };
    expect(await refinePlatform("mac-arm", arm)).toBe("mac-arm");
  });
  it("links straight to the newest release's files", () => {
    for (const d of Object.values(DOWNLOADS)) {
      expect(d.href).toBe(`https://github.com/shivambisen/carson-remote/releases/latest/download/${d.file}`);
    }
  });
});
