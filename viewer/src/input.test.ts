import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { ClipboardAssembler, clipboardChunks, CLIPBOARD_CHUNK_BYTES, decodeClipboardChunk, encode, modsOf, normalizePoint, wheelPixels, type InputMessage } from "./input";

const hex = (b: Uint8Array) => [...b].map((x) => x.toString(16).padStart(2, "0")).join("");

describe("input encoding", () => {
  const fixture = JSON.parse(readFileSync(new URL("../../proto/input-vectors.json", import.meta.url), "utf8")) as {
    vectors: { name: string; hex: string; message: InputMessage }[];
  };
  for (const v of fixture.vectors) {
    it(`matches vector ${v.name}`, () => expect(hex(encode(v.message))).toBe(v.hex));
  }

  it("clamps out-of-range values", () => {
    expect(hex(encode({ type: "pointer_move", x: -5, y: 70000 }))).toBe("010000ffff");
    expect(hex(encode({ type: "wheel", dx: 1e6, dy: -1e6 }))).toBe("03ff7f0080");
  });
});

describe("clipboard", () => {
  it("chunks on code points and reassembles", () => {
    const text = "é".repeat(CLIPBOARD_CHUNK_BYTES);
    const chunks = clipboardChunks(text);
    expect(chunks.length).toBeGreaterThan(1);
    const asm = new ClipboardAssembler();
    let out: string | null = null;
    for (const c of chunks) {
      const d = decodeClipboardChunk(encode(c))!;
      out = asm.push(d.last, d.data);
    }
    expect(out).toBe(text);
  });

  it("rejects non-clipboard or invalid frames", () => {
    expect(decodeClipboardChunk(Uint8Array.of(6))).toBeNull();
    expect(decodeClipboardChunk(Uint8Array.of(5, 1, 0xff))).toBeNull();
  });
});

describe("normalizePoint", () => {
  const box = { left: 0, top: 0, width: 1000, height: 1000 };
  it("maps through letterboxing (16:9 video in a square box)", () => {
    // Picture is 1000x562.5, centered: top bar is 218.75 px.
    expect(normalizePoint(500, 500, box, 1920, 1080)).toEqual({ x: 32768, y: 32768 });
    expect(normalizePoint(0, 218.75, box, 1920, 1080)).toEqual({ x: 0, y: 0 });
    expect(normalizePoint(1000, 781.25, box, 1920, 1080)).toEqual({ x: 65535, y: 65535 });
    expect(normalizePoint(500, 100, box, 1920, 1080)).toBeNull(); // in the black bar
  });
  it("needs a decoded video", () => expect(normalizePoint(1, 1, box, 0, 0)).toBeNull());
});

describe("modifiers and wheel", () => {
  it("packs modifier bits", () => {
    expect(modsOf({ shiftKey: true, ctrlKey: false, altKey: false, metaKey: true })).toBe(9);
    expect(modsOf({ shiftKey: false, ctrlKey: true, altKey: true, metaKey: false, getModifierState: (k) => k === "CapsLock" })).toBe(22);
  });
  it("converts wheel units to pixels", () => {
    expect(wheelPixels({ deltaX: 0, deltaY: 3, deltaMode: 1 }, 800)).toEqual({ dx: 0, dy: 120 });
    expect(wheelPixels({ deltaX: 2, deltaY: -1, deltaMode: 0 }, 800)).toEqual({ dx: 2, dy: -1 });
    expect(wheelPixels({ deltaX: 0, deltaY: 1, deltaMode: 2 }, 800)).toEqual({ dx: 0, dy: 800 });
  });
});
