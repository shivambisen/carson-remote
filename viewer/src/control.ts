/** Viewer-side remote control: turns browser events into input messages. */
import { clipboardChunks, modsOf, type InputMessage } from "./input";

export interface KeyLike {
  code: string;
  type: string; // "keydown" | "keyup"
  shiftKey: boolean;
  ctrlKey: boolean;
  altKey: boolean;
  metaKey: boolean;
  getModifierState?(k: string): boolean;
}

/** Shortcuts the viewer keeps for itself. */
export function isViewerShortcut(e: KeyLike): boolean {
  return e.ctrlKey && e.altKey && e.shiftKey && e.code === "KeyD"; // stats overlay
}

const PASTE_WAIT_MS = 150;

/**
 * Routes keys and clipboard. Ctrl/Cmd+V is special: the browser only reveals clipboard text
 * through the `paste` event that follows the keydown, so the keydown is held until the text
 * has been sent (or a short timeout passes), making the host paste the viewer's clipboard.
 */
export class InputRouter {
  private readonly pressed = new Set<string>();
  private pendingPaste: { msg: InputMessage; timer: ReturnType<typeof setTimeout> } | null = null;

  constructor(
    private readonly send: (m: InputMessage) => void,
    private readonly timers: { set: typeof setTimeout; clear: typeof clearTimeout } = { set: setTimeout, clear: clearTimeout },
  ) {}

  /** Handles a key event. Returns true if the browser's default action should be prevented. */
  key(e: KeyLike): boolean {
    if (isViewerShortcut(e)) return false;
    const down = e.type === "keydown";
    const msg: InputMessage = { type: "key", down, mods: modsOf(e), code: e.code };
    if (!e.code) return false;
    if (down) {
      this.pressed.add(e.code);
      if (e.code === "KeyV" && (e.ctrlKey || e.metaKey) && !this.pendingPaste) {
        this.pendingPaste = { msg, timer: this.timers.set(() => this.flushPaste(), PASTE_WAIT_MS) };
        return false; // let the browser fire `paste`
      }
    } else {
      if (!this.pressed.delete(e.code)) return true; // never pressed while focused
      if (e.code === "KeyV") this.flushPaste();
    }
    this.send(msg);
    return true;
  }

  /** Clipboard text from a `paste` event: sent before the held Ctrl/Cmd+V. */
  paste(text: string): void {
    for (const chunk of clipboardChunks(text)) this.send(chunk);
    this.flushPaste();
  }

  /** Focus lost, tab hidden, control disabled: release everything on the host. */
  releaseAll(): void {
    if (this.pendingPaste) {
      this.timers.clear(this.pendingPaste.timer);
      this.pendingPaste = null;
    }
    this.pressed.clear();
    this.send({ type: "release_all" });
  }

  private flushPaste(): void {
    if (!this.pendingPaste) return;
    this.timers.clear(this.pendingPaste.timer);
    const { msg } = this.pendingPaste;
    this.pendingPaste = null;
    this.send(msg);
  }
}

/** Collapses bursts of pointer moves / wheel deltas into one message per animation frame. */
export class FrameCoalescer {
  private move: InputMessage | null = null;
  private wheel = { dx: 0, dy: 0 };
  private scheduled = false;

  constructor(
    private readonly send: (m: InputMessage) => void,
    private readonly schedule: (cb: () => void) => void = (cb) => void requestAnimationFrame(cb),
  ) {}

  pointer(x: number, y: number): void {
    this.move = { type: "pointer_move", x, y };
    this.arm();
  }

  addWheel(dx: number, dy: number): void {
    this.wheel.dx += dx;
    this.wheel.dy += dy;
    this.arm();
  }

  /** Sends any pending move immediately (before a click, so it lands in the right place). */
  flush(): void {
    if (this.move) this.send(this.move);
    this.move = null;
    const dx = Math.trunc(this.wheel.dx);
    const dy = Math.trunc(this.wheel.dy);
    if (dx || dy) this.send({ type: "wheel", dx, dy });
    this.wheel.dx -= dx;
    this.wheel.dy -= dy;
  }

  private arm(): void {
    if (this.scheduled) return;
    this.scheduled = true;
    this.schedule(() => {
      this.scheduled = false;
      this.flush();
    });
  }
}
