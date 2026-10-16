/**
 * Glass-to-glass latency from the agent's test-pattern barcode (see carson-capture
 * test_pattern.rs): 56 blocks of 16 px along the top-left edge, sync byte 0xB2 then the
 * capture time in ms (low 48 bits), MSB first, white = 1.
 *
 * latency = viewer clock at presentation + (agent clock − viewer clock) − capture time.
 * The clock offset comes from ping/pong over the data channel (best = lowest round trip).
 */

export const BARCODE_BLOCK = 16;
export const BARCODE_BITS = 56;
const SYNC = 0xb2;
const LOW48 = 2 ** 48;

/** Decodes the barcode from RGBA pixels covering at least the top 16 rows × 896 columns. */
export function decodeBarcode(rgba: Uint8ClampedArray, width: number): number | null {
  if (width < BARCODE_BITS * BARCODE_BLOCK) return null;
  const y = BARCODE_BLOCK >> 1;
  let sync = 0;
  let stamp = 0;
  for (let bit = 0; bit < BARCODE_BITS; bit++) {
    const x = bit * BARCODE_BLOCK + (BARCODE_BLOCK >> 1);
    const i = (y * width + x) * 4;
    const g = rgba[i + 1];
    if (g === undefined) return null;
    const v = g > 127 ? 1 : 0;
    if (bit < 8) sync = (sync << 1) | v;
    else stamp = stamp * 2 + v; // beyond 32 bits: avoid bitwise ops
  }
  return sync === SYNC ? stamp : null;
}

/** Tracks the agent−viewer clock offset; the lowest-RTT sample is the most accurate. */
export class ClockSync {
  private bestRtt = Infinity;
  offsetMs: number | null = null;

  /** `t` = viewer time when the ping was sent, `agentMs` = agent time when it answered. */
  onPong(t: number, agentMs: number, now: number): void {
    const rtt = now - t;
    if (rtt < 0 || rtt > this.bestRtt) return;
    this.bestRtt = rtt;
    this.offsetMs = agentMs - (t + rtt / 2);
  }
}

export class LatencyMeter {
  private samples: number[] = [];
  constructor(private readonly max = 300) {}

  /** Adds one frame: `stamp` from the barcode, `presentedAt` viewer clock (ms). */
  add(stamp: number, presentedAt: number, offsetMs: number): number {
    const agentNow = (presentedAt + offsetMs) % LOW48;
    let latency = agentNow - stamp;
    if (latency < -LOW48 / 2) latency += LOW48; // 48-bit wraparound
    this.samples.push(latency);
    if (this.samples.length > this.max) this.samples.shift();
    return latency;
  }

  get count(): number {
    return this.samples.length;
  }

  percentile(p: number): number | null {
    if (this.samples.length === 0) return null;
    const sorted = [...this.samples].sort((a, b) => a - b);
    return sorted[Math.min(sorted.length - 1, Math.floor((p / 100) * sorted.length))]!;
  }
}
