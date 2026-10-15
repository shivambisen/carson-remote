/** Connection stats for the debug overlay, derived from RTCPeerConnection.getStats(). */

export interface ViewStats {
  fps: number;
  kbps: number;
  width: number;
  height: number;
  /** Round-trip time of the selected candidate pair. */
  rttMs: number | null;
  /** Average time frames spend in the jitter buffer. */
  jitterBufferMs: number | null;
  decodeMs: number | null;
  packetsLost: number;
  /** "direct" (host/srflx/prflx) or "relay" (TURN). */
  path: "direct" | "relay" | "unknown";
  candidateType: string;
}

export interface StatsSample {
  at: number; // ms
  bytesReceived: number;
}

type Entry = Record<string, unknown> & { type: string; id: string };

const num = (v: unknown): number | undefined => (typeof v === "number" && Number.isFinite(v) ? v : undefined);

/**
 * Summarizes a stats report. `prev` is the previous sample (for bitrate); returns the new sample
 * to pass next time.
 */
export function summarize(report: { values(): Iterable<Entry> }, prev: StatsSample | null, now: number): { stats: ViewStats; sample: StatsSample } {
  // RTCStatsReport is map-like but not a Map: always read through values().
  const entries = [...report.values()];
  const byId = new Map(entries.map((e) => [e.id, e]));
  const video = entries.find((e) => e.type === "inbound-rtp" && e.kind === "video");

  const bytes = num(video?.bytesReceived) ?? 0;
  const kbps = prev && now > prev.at ? Math.max(0, ((bytes - prev.bytesReceived) * 8) / (now - prev.at)) : 0;
  const decoded = num(video?.framesDecoded) ?? 0;
  const emitted = num(video?.jitterBufferEmittedCount) ?? 0;

  const transport = entries.find((e) => e.type === "transport");
  const pair =
    (typeof transport?.selectedCandidatePairId === "string" ? byId.get(transport.selectedCandidatePairId) : undefined) ??
    entries.find((e) => e.type === "candidate-pair" && e.state === "succeeded" && e.nominated === true);
  const local = typeof pair?.localCandidateId === "string" ? byId.get(pair.localCandidateId) : undefined;
  const remote = typeof pair?.remoteCandidateId === "string" ? byId.get(pair.remoteCandidateId) : undefined;
  const types = [local?.candidateType, remote?.candidateType].filter((t): t is string => typeof t === "string");
  const rtt = num(pair?.currentRoundTripTime);
  const jbDelay = num(video?.jitterBufferDelay);
  const decodeTime = num(video?.totalDecodeTime);

  return {
    stats: {
      fps: Math.round(num(video?.framesPerSecond) ?? 0),
      kbps: Math.round(kbps),
      width: num(video?.frameWidth) ?? 0,
      height: num(video?.frameHeight) ?? 0,
      rttMs: rtt === undefined ? null : Math.round(rtt * 1000),
      jitterBufferMs: jbDelay !== undefined && emitted > 0 ? Math.round((jbDelay / emitted) * 1000) : null,
      decodeMs: decodeTime !== undefined && decoded > 0 ? Math.round((decodeTime / decoded) * 1000 * 10) / 10 : null,
      packetsLost: num(video?.packetsLost) ?? 0,
      path: types.length === 0 ? "unknown" : types.includes("relay") ? "relay" : "direct",
      candidateType: types.join("→") || "?",
    },
    sample: { at: now, bytesReceived: bytes },
  };
}
