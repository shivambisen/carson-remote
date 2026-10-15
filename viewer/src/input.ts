/** Binary input protocol (see proto/input-vectors.json, carson-proto/src/input.rs). */

export type InputMessage =
  | { type: "pointer_move"; x: number; y: number }
  | { type: "button"; button: number; down: boolean }
  | { type: "wheel"; dx: number; dy: number }
  | { type: "key"; down: boolean; mods: number; code: string }
  | { type: "clipboard_chunk"; last: boolean; data: string }
  | { type: "release_all" };

export const Mods = { SHIFT: 1, CTRL: 2, ALT: 4, META: 8, CAPS_LOCK: 16 } as const;
export const MAX_CLIPBOARD_BYTES = 1 << 20;
export const CLIPBOARD_CHUNK_BYTES = 16 * 1024;
const MAX_KEY_CODE_LEN = 32;

const enc = new TextEncoder();
const clampI16 = (v: number) => Math.max(-32768, Math.min(32767, Math.round(v)));
const clampU16 = (v: number) => Math.max(0, Math.min(65535, Math.round(v)));

export function encode(m: InputMessage): Uint8Array<ArrayBuffer> {
  switch (m.type) {
    case "pointer_move": {
      const b = new DataView(new ArrayBuffer(5));
      b.setUint8(0, 1);
      b.setUint16(1, clampU16(m.x), true);
      b.setUint16(3, clampU16(m.y), true);
      return new Uint8Array(b.buffer);
    }
    case "button":
      return Uint8Array.of(2, m.button & 0xff, m.down ? 1 : 0);
    case "wheel": {
      const b = new DataView(new ArrayBuffer(5));
      b.setUint8(0, 3);
      b.setInt16(1, clampI16(m.dx), true);
      b.setInt16(3, clampI16(m.dy), true);
      return new Uint8Array(b.buffer);
    }
    case "key": {
      const code = enc.encode(m.code).slice(0, MAX_KEY_CODE_LEN);
      const out = new Uint8Array(5 + code.length);
      const b = new DataView(out.buffer);
      b.setUint8(0, 4);
      b.setUint8(1, m.down ? 1 : 0);
      b.setUint16(2, m.mods & 0xffff, true);
      b.setUint8(4, code.length);
      out.set(code, 5);
      return out;
    }
    case "clipboard_chunk": {
      const data = enc.encode(m.data);
      const out = new Uint8Array(2 + data.length);
      out[0] = 5;
      out[1] = m.last ? 1 : 0;
      out.set(data, 2);
      return out;
    }
    case "release_all":
      return Uint8Array.of(6);
  }
}

/** Splits text into clipboard chunks on code-point boundaries (truncating at 1 MiB). */
export function clipboardChunks(text: string): InputMessage[] {
  const chunks: InputMessage[] = [];
  let current = "";
  let currentBytes = 0;
  let total = 0;
  for (const ch of text) {
    const n = enc.encode(ch).length;
    if (total + n > MAX_CLIPBOARD_BYTES) break;
    if (currentBytes + n > CLIPBOARD_CHUNK_BYTES) {
      chunks.push({ type: "clipboard_chunk", last: false, data: current });
      current = "";
      currentBytes = 0;
    }
    current += ch;
    currentBytes += n;
    total += n;
  }
  chunks.push({ type: "clipboard_chunk", last: true, data: current });
  return chunks;
}

/** Reassembles clipboard chunks from the agent (bytes arrive as decoded strings). */
export class ClipboardAssembler {
  private parts: string[] = [];
  private bytes = 0;
  private overflow = false;

  push(last: boolean, data: string): string | null {
    this.bytes += enc.encode(data).length;
    if (this.bytes > MAX_CLIPBOARD_BYTES) this.overflow = true;
    else if (!this.overflow) this.parts.push(data);
    if (!last) return null;
    const result = this.overflow ? null : this.parts.join("");
    this.parts = [];
    this.bytes = 0;
    this.overflow = false;
    return result;
  }
}

/** Decodes the one message type the agent sends the viewer: clipboard chunks. */
export function decodeClipboardChunk(buf: Uint8Array): { last: boolean; data: string } | null {
  if (buf.length < 2 || buf[0] !== 5 || buf.length - 2 > CLIPBOARD_CHUNK_BYTES) return null;
  try {
    return { last: (buf[1]! & 1) === 1, data: new TextDecoder("utf-8", { fatal: true }).decode(buf.subarray(2)) };
  } catch {
    return null;
  }
}

export function modsOf(e: { shiftKey: boolean; ctrlKey: boolean; altKey: boolean; metaKey: boolean; getModifierState?(k: string): boolean }): number {
  return (
    (e.shiftKey ? Mods.SHIFT : 0) |
    (e.ctrlKey ? Mods.CTRL : 0) |
    (e.altKey ? Mods.ALT : 0) |
    (e.metaKey ? Mods.META : 0) |
    (e.getModifierState?.("CapsLock") ? Mods.CAPS_LOCK : 0)
  );
}

/** Wheel deltas in pixels (DOM_DELTA_LINE ≈ 40 px, DOM_DELTA_PAGE ≈ one page). */
export function wheelPixels(e: { deltaX: number; deltaY: number; deltaMode: number }, pageHeight: number): { dx: number; dy: number } {
  const k = e.deltaMode === 1 ? 40 : e.deltaMode === 2 ? pageHeight : 1;
  return { dx: e.deltaX * k, dy: e.deltaY * k };
}

/**
 * Maps a client point to normalized 0..65535 coordinates over the video's *content* box,
 * accounting for object-fit: contain letterboxing. Returns null outside the picture.
 */
export function normalizePoint(
  clientX: number,
  clientY: number,
  box: { left: number; top: number; width: number; height: number },
  videoWidth: number,
  videoHeight: number,
): { x: number; y: number } | null {
  if (!videoWidth || !videoHeight || !box.width || !box.height) return null;
  const scale = Math.min(box.width / videoWidth, box.height / videoHeight);
  const w = videoWidth * scale;
  const h = videoHeight * scale;
  const left = box.left + (box.width - w) / 2;
  const top = box.top + (box.height - h) / 2;
  const fx = (clientX - left) / w;
  const fy = (clientY - top) / h;
  if (fx < 0 || fy < 0 || fx > 1 || fy > 1) return null;
  return { x: Math.round(fx * 65535), y: Math.round(fy * 65535) };
}
