/**
 * One view-only session with a device: signaling over the carson WebSocket, media over
 * WebRTC. The agent offers (it owns the video track); the browser answers.
 */
import { PROTOCOL_VERSION, signalingUrl, type Envelope } from "./signaling";

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

  constructor(
    private readonly deviceId: string,
    private readonly video: HTMLVideoElement,
    private readonly onState: (s: SessionState) => void,
  ) {}

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

  private end(reason: string, message?: string): void {
    if (this.ended) return;
    this.ended = true;
    this.pc?.close();
    this.ws?.close(1000, "session over");
    this.video.srcObject = null;
    this.onState({ kind: "ended", reason, message });
  }
}
