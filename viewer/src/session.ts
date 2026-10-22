/**
 * One view-only session with a device: signaling over the carson WebSocket, media over
 * WebRTC. The agent offers (it owns the video track); the browser answers.
 */
import { Backoff } from "./backoff";
import { ClipboardAssembler, decodeClipboardChunk, encode, type InputMessage } from "./input";
import { verifyOffer } from "./identity";
import { ClockSync } from "./latency";
import { PROTOCOL_VERSION, signalingUrl, type Envelope } from "./signaling";

export interface DisplayDescriptor {
  index: number;
  name: string;
  width: number;
  height: number;
  primary: boolean;
}

export type ControlMessage =
  | { type: "displays"; displays: DisplayDescriptor[]; current: number }
  | { type: "input_status"; available: boolean; message?: string }
  | { type: "pin_required"; attempts_left: number }
  | { type: "pin_result"; ok: boolean; attempts_left: number; retry_after_s?: number }
  | { type: "encoder_status"; encoder: string; bitrate_kbps: number; fps: number; scale_pct: number };

export interface SessionCallbacks {
  onState: (s: SessionState) => void;
  onControl?: (m: ControlMessage) => void;
  /** Text copied on the remote computer. */
  onClipboard?: (text: string) => void;
}

/** Pointer moves are dropped (not queued) when the unreliable channel backs up. */
const MAX_POINTER_BUFFER = 16 * 1024;
/** A disconnected media path gets this long to recover before we ask for an ICE restart. */
const ICE_RESTART_DELAY_MS = 2000;
/** Without media for this long, the session is over (matches the agent and server). */
const MEDIA_LOSS_TIMEOUT_MS = 30_000;

export interface SessionOptions {
  /** Only use TURN relay candidates (diagnostics: proves the relay path works). */
  forceRelay?: boolean;
  /** The device's (pinned) public key. Every offer's DTLS fingerprint must be signed by it. */
  publicKey?: string;
}

export type SessionState =
  | { kind: "connecting"; detail: string }
  | { kind: "live" }
  /** The media path dropped; recovering (ICE restart / signaling resume). Video may freeze. */
  | { kind: "reconnecting"; detail: string }
  | { kind: "ended"; reason: string; message?: string };

interface IceServer {
  urls: string[];
  username?: string;
  credential?: string;
}

// A single malformed URL (e.g. a trailing "?" from a misconfigured relay env var) makes the
// browser reject the whole RTCPeerConnection config, so drop just the bad ones here first.
export function isValidIceUrl(u: string): boolean {
  const m = /^(stun|stuns|turn|turns):([^?]+)(\?(.*))?$/.exec(u);
  if (!m) return false;
  if (!m[2]) return false; // no host[:port]
  if (m[3] && !m[4]) return false; // "?" with nothing after it
  return true;
}

export function sanitizeIceServers(servers: IceServer[]): IceServer[] {
  return servers
    .map((s) => ({ ...s, urls: (Array.isArray(s.urls) ? s.urls : [s.urls]).filter(isValidIceUrl) }))
    .filter((s) => s.urls.length > 0);
}

const REASONS: Record<string, string> = {
  permission_denied: "glenley on that computer is missing Screen Recording permission. Grant it in System Settings → Privacy & Security → Screen Recording, then restart the agent.",
  capture_failed: "The other computer could not capture its screen.",
  device_offline: "That device is offline.",
  unknown_device: "That device is not on your account.",
  busy: "Someone is already viewing that device.",
  rate_limited: "Too many connection attempts. Wait a minute and try again.",
  payment_required: "Get glenley for this account to connect (₹99, once).",
  peer_disconnected: "The other computer disconnected.",
  connection_failed: "Could not connect to the other computer, even through the relay.",
  connection_lost: "The connection was lost and could not be recovered within 30 seconds.",
  resume_failed: "The session could not be resumed after the connection dropped.",
  identity_mismatch: "Security check failed: the connection was not signed by that device's key. Nothing was shown. If this keeps happening, someone may be intercepting the connection.",
  device_revoked: "This device was removed from your account.",
  connect_timeout: "The connection timed out.",
  agent_error: "The agent hit an error and ended the session.",
  user: "Session ended.",
  declined: "The person at that computer declined the connection.",
  consent_timeout: "No one answered on the other computer within 30 seconds.",
  consent_unavailable: "The other computer could not show the approval prompt. Set up unattended access with a PIN on it (glenley-agent unattended on --pin).",
  pin_locked: "Too many wrong PINs. Unattended access on that computer is locked for 15 minutes.",
  pin_timeout: "The PIN was not entered in time.",
  host_disconnected: "The person at the other computer ended the session.",
  banner_unavailable: "The other computer could not show its \"being controlled\" notice, so the session was not started.",
};

export function describeReason(reason: string, message?: string): string {
  return REASONS[reason] ?? message ?? `Session ended (${reason}).`;
}

export class ViewerSession {
  private ws: WebSocket | null = null;
  private pc: RTCPeerConnection | null = null;
  private sid: string | null = null;
  private pendingCandidates: RTCIceCandidateInit[] = [];
  private ended = false;
  private readonly requestId = crypto.randomUUID();
  private ptr: RTCDataChannel | null = null;
  private ctl: RTCDataChannel | null = null;
  private readonly clipboard = new ClipboardAssembler();
  private readonly onState: (s: SessionState) => void;
  private resumeToken: string | null = null;
  private readonly wsBackoff = new Backoff();
  private reconnectTimer: ReturnType<typeof setTimeout> | undefined;
  private restartTimer: ReturnType<typeof setTimeout> | undefined;
  private lostTimer: ReturnType<typeof setTimeout> | undefined;
  private telemetryTimer: ReturnType<typeof setInterval> | undefined;
  private lastLoss: { lost: number; received: number } | null = null;
  /** Agent−viewer clock offset (ms), for glass-to-glass latency. */
  readonly clock = new ClockSync();

  constructor(
    private readonly deviceId: string,
    private readonly video: HTMLVideoElement,
    private readonly callbacks: SessionCallbacks,
    private readonly options: SessionOptions = {},
  ) {
    this.onState = callbacks.onState;
  }

  /**
   * Sends one input message: pointer moves on the unreliable channel, the rest reliably.
   * `reliable` forces a pointer move onto the ordered channel (right before a click, so the
   * click cannot overtake it).
   */
  sendInput(m: InputMessage, reliable = false): void {
    if (m.type === "pointer_move" && !reliable) {
      if (this.ptr?.readyState === "open" && this.ptr.bufferedAmount < MAX_POINTER_BUFFER) this.ptr.send(encode(m));
      return;
    }
    if (this.ctl?.readyState === "open") this.ctl.send(encode(m));
  }

  /** Sends the unattended-access PIN. It travels only inside the encrypted data channel. */
  sendPin(pin: string): void {
    if (this.ctl?.readyState === "open") this.ctl.send(JSON.stringify({ type: "pin", pin }));
  }

  selectDisplay(index: number): void {
    if (this.ctl?.readyState === "open") this.ctl.send(JSON.stringify({ type: "select_display", index }));
  }

  /** Channel properties as negotiated (diagnostics). */
  channelInfo(): { label: string; ordered: boolean; maxRetransmits: number | null }[] {
    return [this.ptr, this.ctl].filter((c): c is RTCDataChannel => !!c).map((c) => ({ label: c.label, ordered: c.ordered, maxRetransmits: c.maxRetransmits }));
  }

  start(): void {
    this.onState({ kind: "connecting", detail: "Contacting server…" });
    this.connectSignaling();
  }

  /**
   * Opens the signaling socket. If it drops after the session exists, we reconnect and
   * resume (the server holds the session for 30 s); media keeps flowing peer-to-peer meanwhile.
   */
  private connectSignaling(): void {
    const ws = new WebSocket(signalingUrl(location.origin));
    this.ws = ws;
    ws.onopen = () => this.wsBackoff.reset();
    ws.onmessage = (ev: MessageEvent<string>) => {
      let env: Envelope;
      try {
        env = JSON.parse(ev.data) as Envelope;
      } catch {
        return;
      }
      void this.handle(env).catch((err: unknown) => this.end("agent_error", String(err)));
    };
    ws.onclose = () => {
      if (this.ended || this.ws !== ws) return;
      if (!this.sid || !this.resumeToken) {
        this.end("peer_disconnected"); // no session to resume yet
        return;
      }
      this.reconnectTimer = setTimeout(() => this.connectSignaling(), this.wsBackoff.next());
    };
  }

  /** The underlying peer connection, for stats. */
  /** Data-path health, for diagnostics: SCTP association and each data channel's state. */
  channelStates(): { sctp: string; ctl: string; ptr: string } {
    return {
      sctp: this.pc?.sctp?.state ?? "none",
      ctl: this.ctl?.readyState ?? "not-announced",
      ptr: this.ptr?.readyState ?? "not-announced",
    };
  }

  get peer(): RTCPeerConnection | null {
    return this.pc;
  }

  stop(): void {
    if (!this.ended && this.sid) this.send("session.end", { reason: "user" });
    this.end("user");
  }

  private send(type: string, body: unknown, id?: string): void {
    if (this.ws?.readyState !== WebSocket.OPEN) return;
    const env: Envelope = { v: PROTOCOL_VERSION, type, body };
    if (this.sid) env.sid = this.sid;
    if (id) env.id = id;
    this.ws.send(JSON.stringify(env));
  }

  private async handle(env: Envelope): Promise<void> {
    if (env.v !== PROTOCOL_VERSION || this.ended) return;
    // Before a session exists, only our own request's replies count.
    const ours = env.sid ? env.sid === this.sid : env.id === this.requestId;
    switch (env.type) {
      case "hello.ok":
        if (this.sid && this.resumeToken) {
          this.send("session.resume", { token: this.resumeToken });
          return;
        }
        this.send("session.request", { device_id: this.deviceId }, this.requestId);
        this.onState({ kind: "connecting", detail: "Asking the device…" });
        return;
      case "session.resume": {
        if (!ours) return;
        const r = env.body as { resumed?: boolean };
        if (!r.resumed) return this.end("resume_failed");
        if (this.pc && this.pc.connectionState !== "connected") this.send("session.ice_restart", {});
        return;
      }
      case "turn.creds": {
        if (env.id !== this.requestId) return;
        this.sid = env.sid ?? null;
        const { ice_servers, resume_token } = env.body as { ice_servers: IceServer[]; resume_token?: string };
        this.resumeToken = resume_token ?? null;
        this.createPeer(ice_servers);
        this.onState({ kind: "connecting", detail: "Waiting for the other computer to allow the connection…" });
        return;
      }
      case "session.accept":
        if (ours) this.onState({ kind: "connecting", detail: "Connecting…" });
        return;
      case "sdp.offer": {
        if (!ours || !this.pc || !this.sid) return;
        const offer = env.body as RTCSessionDescriptionInit & { carson_sig?: unknown };
        // End-to-end identity: refuse media from anything but the paired device.
        if (!this.options.publicKey || !(await verifyOffer(this.options.publicKey, this.sid, offer.sdp ?? "", offer.carson_sig))) {
          this.send("session.end", { reason: "identity_mismatch" });
          this.end("identity_mismatch");
          return;
        }
        await this.pc.setRemoteDescription({ type: offer.type, sdp: offer.sdp });
        for (const c of this.pendingCandidates.splice(0)) await this.pc.addIceCandidate(c);
        const answer = await this.pc.createAnswer();
        await this.pc.setLocalDescription(answer);
        this.send("sdp.answer", { type: answer.type, sdp: answer.sdp });
        return;
      }
      case "ice.candidate": {
        if (!ours || !this.pc) return;
        const c = env.body as RTCIceCandidateInit;
        if (this.pc.remoteDescription) await this.pc.addIceCandidate(c);
        else this.pendingCandidates.push(c);
        return;
      }
      case "session.reject":
      case "session.end": {
        if (!ours) return;
        const b = (env.body ?? {}) as { reason?: string; message?: string };
        this.end(b.reason ?? "ended", b.message);
        return;
      }
      default:
        return; // presence etc.
    }
  }

  private relayAvailable = false;

  /** Whether the server offered a TURN relay for this session. */
  hasRelay(): boolean {
    return this.relayAvailable;
  }

  private createPeer(iceServers: IceServer[]): void {
    const clean = sanitizeIceServers(iceServers);
    this.relayAvailable = clean.some((s) => (Array.isArray(s.urls) ? s.urls : [s.urls]).some((u) => /^turns?:/.test(u)));
    let pc: RTCPeerConnection;
    try {
      pc = new RTCPeerConnection({ iceServers: clean, iceTransportPolicy: this.options.forceRelay ? "relay" : "all" });
    } catch (e) {
      // One malformed relay entry must not silently hang every connection for 30s: fall back to
      // no relay (direct/STUN only still has a chance) rather than leaving this.pc unset.
      console.error("invalid ICE server config, retrying without a relay", e);
      this.relayAvailable = false;
      pc = new RTCPeerConnection({ iceServers: clean.filter((s) => !(Array.isArray(s.urls) ? s.urls : [s.urls]).some((u) => /^turns?:/.test(u))) });
    }
    this.pc = pc;
    pc.onicecandidate = (ev) => {
      if (ev.candidate) this.send("ice.candidate", ev.candidate.toJSON());
    };
    pc.ondatachannel = (ev) => {
      const dc = ev.channel;
      dc.binaryType = "arraybuffer";
      if (dc.label === "ptr") this.ptr = dc;
      else if (dc.label === "ctl") {
        this.ctl = dc;
        dc.onmessage = (m: MessageEvent<string | ArrayBuffer>) => this.onControlMessage(m.data);
      }
    };
    pc.ontrack = (ev) => {
      // Ask the browser to play frames as soon as they decode (lowest latency).
      // jitterBufferTarget is standard; playoutDelayHint is Chrome's older spelling.
      const receiver = ev.receiver as RTCRtpReceiver & { playoutDelayHint?: number };
      receiver.jitterBufferTarget = 0;
      receiver.playoutDelayHint = 0;
      this.video.srcObject = ev.streams[0] ?? new MediaStream([ev.track]);
      void this.video.play().catch(() => {});
    };
    let everConnected = false;
    pc.onconnectionstatechange = () => {
      const state = pc.connectionState;
      if (state === "connected") {
        everConnected = true;
        this.startTelemetry();
        clearTimeout(this.restartTimer);
        clearTimeout(this.lostTimer);
        this.lostTimer = undefined;
        this.onState({ kind: "live" });
        return;
      }
      if (state !== "disconnected" && state !== "failed") return;
      if (!everConnected) {
        if (state === "failed") {
          this.send("session.end", { reason: "connection_failed" });
          this.end("connection_failed");
        }
        return;
      }
      // Media path lost: ask the agent (the offerer) to restart ICE, and give up after 30 s.
      this.onState({ kind: "reconnecting", detail: "Connection interrupted, reconnecting…" });
      if (this.lostTimer === undefined) {
        this.lostTimer = setTimeout(() => {
          this.send("session.end", { reason: "connection_lost" });
          this.end("connection_lost");
        }, MEDIA_LOSS_TIMEOUT_MS);
      }
      clearTimeout(this.restartTimer);
      this.restartTimer = setTimeout(
        () => this.send("session.ice_restart", {}),
        state === "failed" ? 0 : ICE_RESTART_DELAY_MS,
      );
    };
  }

  /**
   * Once a second: report receive stats (adaptive quality on the agent) and ping (clock sync).
   * Runs whether or not the stats overlay is visible.
   */
  private startTelemetry(): void {
    if (this.telemetryTimer !== undefined) return;
    let tick = 0;
    this.telemetryTimer = setInterval(() => {
      if (this.ctl?.readyState !== "open" || !this.pc) return;
      if (tick++ % 2 === 0) this.ctl.send(JSON.stringify({ type: "ping", t: Date.now() }));
      void this.pc.getStats().then((report) => {
        let rtt: number | undefined;
        let lost = 0;
        let received = 0;
        let fps = 0;
        report.forEach((s: Record<string, unknown>) => {
          if (s.type === "inbound-rtp" && s.kind === "video") {
            lost = Number(s.packetsLost ?? 0);
            received = Number(s.packetsReceived ?? 0);
            fps = Math.round(Number(s.framesPerSecond ?? 0));
          }
          if (s.type === "candidate-pair" && s.state === "succeeded" && s.nominated && typeof s.currentRoundTripTime === "number")
            rtt = Math.round(s.currentRoundTripTime * 1000);
        });
        const prev = this.lastLoss;
        this.lastLoss = { lost, received };
        const dLost = prev ? Math.max(0, lost - prev.lost) : 0;
        const dRecv = prev ? Math.max(0, received - prev.received) : 0;
        const loss = dLost + dRecv > 0 ? dLost / (dLost + dRecv) : 0;
        if (this.ctl?.readyState === "open") this.ctl.send(JSON.stringify({ type: "viewer_stats", rtt_ms: rtt ?? null, loss, fps }));
      });
    }, 1000);
  }

  private onControlMessage(data: string | ArrayBuffer): void {
    if (typeof data === "string") {
      try {
        const msg = JSON.parse(data) as ControlMessage | { type: "pong"; t: number; agent_ms: number };
        if (msg.type === "pong") this.clock.onPong(msg.t, msg.agent_ms, Date.now());
        else this.callbacks.onControl?.(msg);
      } catch {
        // ignore malformed control text
      }
      return;
    }
    const chunk = decodeClipboardChunk(new Uint8Array(data));
    if (!chunk) return;
    const text = this.clipboard.push(chunk.last, chunk.data);
    if (text !== null) this.callbacks.onClipboard?.(text);
  }

  private end(reason: string, message?: string): void {
    if (this.ended) return;
    this.ended = true;
    clearTimeout(this.reconnectTimer);
    clearTimeout(this.restartTimer);
    clearTimeout(this.lostTimer);
    clearInterval(this.telemetryTimer);
    this.pc?.close();
    this.ws?.close(1000, "session over");
    this.video.srcObject = null;
    this.onState({ kind: "ended", reason, message });
  }
}
