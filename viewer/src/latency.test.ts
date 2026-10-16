import { describe, expect, it } from "vitest";
import { BARCODE_BITS, BARCODE_BLOCK, ClockSync, decodeBarcode, LatencyMeter } from "./latency";

/** Renders a barcode exactly like the agent's test pattern. */
function render(stamp: number): { rgba: Uint8ClampedArray; width: number } {
  const width = 1280;
  const rgba = new Uint8ClampedArray(width * BARCODE_BLOCK * 4);
  const bits: number[] = [];
  for (let b = 7; b >= 0; b--) bits.push((0xb2 >> b) & 1);
  for (let b = 47; b >= 0; b--) bits.push(Math.floor(stamp / 2 ** b) % 2);
  for (let bit = 0; bit < BARCODE_BITS; bit++) {
    const v = bits[bit] ? 250 : 5; // slightly off pure values, as after compression
    for (let y = 0; y < BARCODE_BLOCK; y++)
      for (let x = bit * BARCODE_BLOCK; x < (bit + 1) * BARCODE_BLOCK; x++) rgba.set([v, v, v, 255], (y * width + x) * 4);
  }
  return { rgba, width };
}

describe("barcode", () => {
  it("decodes the agent's 48-bit timestamp", () => {
    const stamp = 1_792_000_123_456 % 2 ** 48;
    const { rgba, width } = render(stamp);
    expect(decodeBarcode(rgba, width)).toBe(stamp);
  });

  it("rejects frames without the sync byte (real screens)", () => {
    expect(decodeBarcode(new Uint8ClampedArray(1280 * 16 * 4), 1280)).toBeNull();
    expect(decodeBarcode(new Uint8ClampedArray(100), 10)).toBeNull();
  });
});

describe("latency", () => {
  it("uses the lowest-RTT pong for the clock offset", () => {
    const c = new ClockSync();
    c.onPong(1000, 1500 + 40, 1080); // rtt 80 → offset 500
    c.onPong(2000, 2500 + 5, 2010); // rtt 10 → offset 500 (better sample)
    c.onPong(3000, 9999, 3200); // worse rtt: ignored
    expect(c.offsetMs).toBe(500);
  });

  it("computes latency and percentiles across clock offset", () => {
    const m = new LatencyMeter();
    // Agent clock is 500 ms ahead; frames captured 40..60 ms before presentation.
    for (let i = 0; i < 100; i++) m.add(10_000 + 500 - (40 + (i % 21)), 10_000, 500);
    expect(m.percentile(50)).toBe(50);
    expect(m.percentile(95)).toBeGreaterThanOrEqual(58);
    expect(m.count).toBe(100);
  });
});
