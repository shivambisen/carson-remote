import { describe, expect, it } from "vitest";
import { summarize } from "./stats";

const report = (bytes: number, extra: Record<string, unknown> = {}) =>
  new Map(
    [
      { id: "IT1", type: "inbound-rtp", kind: "video", bytesReceived: bytes, framesPerSecond: 29.7, frameWidth: 1920, frameHeight: 1080, framesDecoded: 100, totalDecodeTime: 0.25, jitterBufferDelay: 1.5, jitterBufferEmittedCount: 100, packetsLost: 2, ...extra },
      { id: "T1", type: "transport", selectedCandidatePairId: "CP1" },
      { id: "CP1", type: "candidate-pair", state: "succeeded", nominated: true, currentRoundTripTime: 0.004, localCandidateId: "L1", remoteCandidateId: "R1" },
      { id: "L1", type: "local-candidate", candidateType: "host" },
      { id: "R1", type: "remote-candidate", candidateType: "prflx" },
      { id: "IA", type: "inbound-rtp", kind: "audio", bytesReceived: 999999 },
    ].map((e) => [e.id, e]),
  );

describe("summarize", () => {
  it("reports video, latency components and a direct path", () => {
    const first = summarize(report(1_000_000), null, 1000);
    expect(first.stats.kbps).toBe(0); // no previous sample yet
    const { stats } = summarize(report(1_500_000), first.sample, 2000);
    expect(stats).toMatchObject({
      fps: 30,
      kbps: 4000, // 500 kB in 1 s
      width: 1920,
      height: 1080,
      rttMs: 4,
      jitterBufferMs: 15,
      decodeMs: 2.5,
      packetsLost: 2,
      path: "direct",
      candidateType: "host→prflx",
    });
  });

  it("detects relayed connections and falls back to the nominated pair", () => {
    const r = report(0);
    r.delete("T1");
    r.set("L1", { id: "L1", type: "local-candidate", candidateType: "relay" });
    expect(summarize(r, null, 0).stats.path).toBe("relay");
  });

  it("reads map-like reports (RTCStatsReport is not a Map)", () => {
    const m = report(10);
    const mapLike = { values: () => m.values(), [Symbol.iterator]: () => m.entries() };
    expect(summarize(mapLike, null, 0).stats.width).toBe(1920);
  });

  it("handles an empty report", () => {
    const { stats } = summarize(new Map(), null, 0);
    expect(stats).toMatchObject({ fps: 0, kbps: 0, rttMs: null, jitterBufferMs: null, decodeMs: null, path: "unknown" });
  });
});
