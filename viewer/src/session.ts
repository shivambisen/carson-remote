/**
 * One view-only session with a device: signaling over the carson WebSocket, media over
 * WebRTC. The agent offers (it owns the video track); the browser answers.
 */
import { ClipboardAssembler, decodeClipboardChunk, encode, type InputMessage } from "./input";
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
  | { type: "input_status"; available: boolean; message?: string };

export interface SessionCallbacks {
  onState: (s: SessionState) => void;
  onControl?: (m: ControlMessage) => void;
  /** Text copied on the remote computer. */
  onClipboard?: (text: string) => void;
}

/** Pointer moves are dropped (not queued) when the unreliable channel backs up. */
const MAX_POINTER_BUFFER = 16 * 1024;

export type SessionState =
  | { kind: "connecting"; detail: string }
  | { kind: "live" }
  | { kind: "ended"; reason: string; message?: string };

interface IceServer {
  urls: string[];
  username?: string;
  credential?: string;
}

const REASONS: Record<string, string> = {
  permission_denied: "carson-agent on that computer is missing Screen Recording permission. Grant it in System Settings → Privacy & Security → Screen Recording, then restart the agent.",
  capture_failed: "The other computer could not capture its screen.",
  device_offline: "That device is offline.",
  unknown_device: "That device is not on your account.",
  busy: "Someone is already viewing that device.",
  rate_limited: "Too many connection attempts. Wait a minute and try again.",
  peer_disconnected: "The other computer disconnected.",
  connection_failed: "The direct connection failed. (Relay fallback for strict networks arrives in a later version.)",
  connect_timeout: "The connection timed out.",
  agent_error: "The agent hit an error and ended the session.",
  user: "Session ended.",
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

  constructor(
    private readonly deviceId: string,
    private readonly video: HTMLVideoElement,
    private readonly callbacks: SessionCallbacks,
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

  selectDisplay(index: number): void {
    if (this.ctl?.readyState === "open") this.ctl.send(JSON.stringify({ type: "select_display", index }));
  }

  /** Channel properties as negotiated (diagnostics). */
  channelInfo(): { label: string; ordered: boolean; maxRetransmits: number | null }[] {
    return [this.ptr, this.ctl].filter((c): c is RTCDataChannel => !!c).map((c) => ({ label: c.label, ordered: c.ordered, maxRetransmits: c.maxRetransmits }));
  }

  start(): void {
    this.onState({ kind: "connecting", detail: "Contacting server…" });
    const ws = new WebSocket(signalingUrl(location.origin));
    this.ws = ws;
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
      if (!this.ended && this.pc?.connectionState !== "connected") this.end("peer_disconnected");
    };
  }

  /** The underlying peer connection, for stats. */
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
        this.send("session.request", { device_id: this.deviceId }, this.requestId);
        this.onState({ kind: "connecting", detail: "Asking the device…" });
        return;
      case "turn.creds": {
        if (env.id !== this.requestId) return;
        this.sid = env.sid ?? null;
        const { ice_servers } = env.body as { ice_servers: IceServer[] };
        this.createPeer(ice_servers);
        return;
      }
      case "session.accept":
        if (ours) this.onState({ kind: "connecting", detail: "Connecting…" });
        return;
      case "sdp.offer": {
        if (!ours || !this.pc) return;
        await this.pc.setRemoteDescription(env.body as RTCSessionDescriptionInit);
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

  private createPeer(iceServers: IceServer[]): void {
    const pc = new RTCPeerConnection({ iceServers });
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
    pc.onconnectionstatechange = () => {
      if (pc.connectionState === "connected") this.onState({ kind: "live" });
      else if (pc.connectionState === "failed") {
        this.send("session.end", { reason: "connection_failed" });
        this.end("connection_failed");
      }
    };
  }

  private onControlMessage(data: string | ArrayBuffer): void {
    if (typeof data === "string") {
      try {
        this.callbacks.onControl?.(JSON.parse(data) as ControlMessage);
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
    this.pc?.close();
    this.ws?.close(1000, "session over");
    this.video.srcObject = null;
    this.onState({ kind: "ended", reason, message });
  }
}
