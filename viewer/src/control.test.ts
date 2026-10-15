import { describe, expect, it, vi } from "vitest";
import { FrameCoalescer, InputRouter, type KeyLike } from "./control";
import type { InputMessage } from "./input";

const key = (type: "keydown" | "keyup", code: string, m: Partial<KeyLike> = {}): KeyLike => ({
  type, code, shiftKey: false, ctrlKey: false, altKey: false, metaKey: false, ...m,
});

function router() {
  const sent: InputMessage[] = [];
  vi.useFakeTimers();
  const r = new InputRouter((m) => sent.push(m), { set: setTimeout, clear: clearTimeout });
  return { r, sent };
}

describe("InputRouter", () => {
  it("forwards keys with modifiers and prevents browser defaults", () => {
    const { r, sent } = router();
    expect(r.key(key("keydown", "KeyA", { shiftKey: true }))).toBe(true);
    expect(r.key(key("keyup", "KeyA"))).toBe(true);
    expect(sent).toEqual([
      { type: "key", down: true, mods: 1, code: "KeyA" },
      { type: "key", down: false, mods: 0, code: "KeyA" },
    ]);
  });

  it("does not release keys that were never pressed while focused", () => {
    const { r, sent } = router();
    r.key(key("keyup", "KeyQ"));
    expect(sent).toEqual([]);
  });

  it("sends pasted text before the held Cmd+V", () => {
    const { r, sent } = router();
    r.key(key("keydown", "MetaLeft", { metaKey: true }));
    expect(r.key(key("keydown", "KeyV", { metaKey: true }))).toBe(false); // browser must fire paste
    expect(sent).toHaveLength(1);
    r.paste("hello");
    expect(sent.slice(1)).toEqual([
      { type: "clipboard_chunk", last: true, data: "hello" },
      { type: "key", down: true, mods: 8, code: "KeyV" },
    ]);
  });

  it("sends Cmd+V anyway if no paste event arrives", () => {
    const { r, sent } = router();
    r.key(key("keydown", "KeyV", { ctrlKey: true }));
    expect(sent).toEqual([]);
    vi.advanceTimersByTime(200);
    expect(sent).toEqual([{ type: "key", down: true, mods: 2, code: "KeyV" }]);
  });

  it("keyup of V flushes a held paste first", () => {
    const { r, sent } = router();
    r.key(key("keydown", "KeyV", { metaKey: true }));
    r.key(key("keyup", "KeyV", { metaKey: true }));
    expect(sent.map((m) => (m.type === "key" ? `${m.code}:${m.down}` : m.type))).toEqual(["KeyV:true", "KeyV:false"]);
  });

  it("release_all clears state and cancels a held paste", () => {
    const { r, sent } = router();
    r.key(key("keydown", "KeyV", { metaKey: true }));
    r.releaseAll();
    vi.advanceTimersByTime(200);
    expect(sent).toEqual([{ type: "release_all" }]);
    r.key(key("keyup", "KeyV"));
    expect(sent).toHaveLength(1);
  });

  it("leaves the stats shortcut to the viewer", () => {
    const { r, sent } = router();
    expect(r.key(key("keydown", "KeyD", { ctrlKey: true, altKey: true, shiftKey: true }))).toBe(false);
    expect(sent).toEqual([]);
  });
});

describe("FrameCoalescer", () => {
  it("sends only the latest pointer position per frame and sums wheel deltas", () => {
    const sent: InputMessage[] = [];
    let frame: (() => void) | null = null;
    const c = new FrameCoalescer((m) => sent.push(m), (cb) => (frame = cb));
    c.pointer(1, 1);
    c.pointer(2, 2);
    c.addWheel(0, 10.5);
    c.addWheel(0, 10);
    expect(sent).toEqual([]);
    frame!();
    expect(sent).toEqual([
      { type: "pointer_move", x: 2, y: 2 },
      { type: "wheel", dx: 0, dy: 20 },
    ]);
    c.addWheel(0, 0.6); // 0.5 carried + 0.6 = 1.1 → 1
    frame!();
    expect(sent[2]).toEqual({ type: "wheel", dx: 0, dy: 1 });
  });
});
