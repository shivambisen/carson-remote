import { useEffect, useRef, useState, type FormEvent } from "react";
import { ArrowLeft, Gauge, Loader2, Lock, Maximize, Minimize, PhoneOff, ShieldAlert } from "lucide-react";
import { toast } from "sonner";

import { api, type Device } from "@/api";
import { FrameCoalescer, InputRouter, isViewerShortcut } from "@/control";
import { browserPins, checkPin } from "@/identity";
import { normalizePoint, wheelPixels } from "@/input";
import { BARCODE_BITS, BARCODE_BLOCK, decodeBarcode, LatencyMeter } from "@/latency";
import { errorMessage } from "@/lib/errors";
import { navigate } from "@/lib/router";
import { describeReason, ViewerSession, type DisplayDescriptor, type SessionOptions } from "@/session";
import { summarize, type StatsSample } from "@/stats";
import { Centered } from "@/components/shell";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";

/** Looks up the device and checks its key against the one this browser pinned before connecting. */
export function ViewPage({ deviceId, forceRelay }: { deviceId: string; forceRelay: boolean }) {
  const [state, setState] = useState<{ kind: "checking" } | { kind: "missing" } | { kind: "mismatch"; name: string } | { kind: "ready"; device: Device } | { kind: "error"; message: string }>({ kind: "checking" });

  useEffect(() => {
    let alive = true;
    api.devices().then(
      (list) => {
        if (!alive) return;
        const device = list.find((d) => d.id === deviceId);
        if (!device) setState({ kind: "missing" });
        else if (checkPin(browserPins, device.id, device.public_key) === "mismatch") setState({ kind: "mismatch", name: device.name });
        else setState({ kind: "ready", device });
      },
      (e) => alive && setState({ kind: "error", message: errorMessage(e) }),
    );
    return () => {
      alive = false;
    };
  }, [deviceId]);

  if (state.kind === "ready") return <RemoteScreen device={state.device} options={{ forceRelay, publicKey: state.device.public_key }} />;
  if (state.kind === "checking")
    return (
      <div className="grid h-svh place-items-center bg-black">
        <Loader2 className="size-6 animate-spin text-white/60" />
      </div>
    );
  const [title, text] =
    state.kind === "missing"
      ? ["Computer not found", "It may have been removed from your account."]
      : state.kind === "mismatch"
        ? [
            "Security warning",
            `The identity key for "${state.name}" is different from the one this browser saw before. This should never happen for the same computer, so carson is not connecting. If you set that computer up again, it appears as a new device: use that one and remove this entry.`,
          ]
        : ["Something went wrong", state.message];
  return (
    <Centered>
      <Card>
        <CardHeader>
          {state.kind === "mismatch" && <ShieldAlert className="size-8 text-destructive" />}
          <CardTitle>{title}</CardTitle>
          <CardDescription>{text}</CardDescription>
        </CardHeader>
        <CardContent>
          <Button className="w-full" variant="outline" asChild>
            <a href="#/devices">Back to devices</a>
          </Button>
        </CardContent>
      </Card>
    </Centered>
  );
}

type Status = { kind: "connecting" | "reconnecting"; detail: string } | { kind: "live" } | { kind: "ended"; message: string };

function RemoteScreen({ device, options }: { device: Device; options: SessionOptions }) {
  const videoRef = useRef<HTMLVideoElement>(null);
  const stageRef = useRef<HTMLDivElement>(null);
  const sessionRef = useRef<ViewerSession | null>(null);
  const routerRef = useRef<InputRouter | null>(null);
  const controlRef = useRef(true);
  const inputAvailableRef = useRef(false);
  const statsOnRef = useRef(false);

  const [status, setStatus] = useState<Status>({ kind: "connecting", detail: "Connecting…" });
  const [displays, setDisplays] = useState<{ list: DisplayDescriptor[]; current: number }>({ list: [], current: 0 });
  const [pin, setPin] = useState<{ open: boolean; message: string; error: boolean }>({ open: false, message: "", error: false });
  const [pinValue, setPinValue] = useState("");
  const [inputAvailable, setInputAvailable] = useState(false);
  const [control, setControl] = useState(true);
  const [statsOn, setStatsOn] = useState(false);
  const [statsText, setStatsText] = useState("");
  const [fullscreen, setFullscreen] = useState(false);

  controlRef.current = control;
  statsOnRef.current = statsOn;

  useEffect(() => {
    const video = videoRef.current!;
    const stage = stageRef.current!;
    const controlling = () => inputAvailableRef.current && controlRef.current;
    let encoderLine = "";
    let pendingClipboard: string | null = null;

    // Glass-to-glass latency from the test pattern's timestamp barcode (no-op on real screens).
    const latency = new LatencyMeter();
    (window as Window & { carsonLatency?: LatencyMeter }).carsonLatency = latency;
    const strip = new OffscreenCanvas(BARCODE_BITS * BARCODE_BLOCK, BARCODE_BLOCK);
    const stripCtx = strip.getContext("2d", { willReadFrequently: true });
    let stopped = false;
    const onFrame = () => {
      if (stopped) return;
      video.requestVideoFrameCallback(onFrame); // re-arm first: one bad frame must not stop sampling
      const offset = session.clock.offsetMs;
      if (!stripCtx || offset === null || video.videoWidth < strip.width) return;
      try {
        stripCtx.drawImage(video, 0, 0, strip.width, strip.height, 0, 0, strip.width, strip.height);
        const stamp = decodeBarcode(stripCtx.getImageData(0, 0, strip.width, strip.height).data, strip.width);
        if (stamp !== null) latency.add(stamp, Date.now(), offset);
      } catch {
        // frame not drawable yet
      }
    };

    const writeLocalClipboard = (text: string) =>
      navigator.clipboard.writeText(text).then(
        () => toast("Copied from the remote computer"),
        () => (pendingClipboard = text),
      );

    const session = new ViewerSession(
      device.id,
      video,
      {
        onState: (s) => {
          if (s.kind === "connecting" || s.kind === "reconnecting") setStatus({ kind: s.kind, detail: s.detail });
          else if (s.kind === "live") {
            setStatus({ kind: "live" });
            stage.focus();
          } else {
            setPin((p) => ({ ...p, open: false }));
            setStatus({ kind: "ended", message: describeReason(s.reason, s.message) });
          }
        },
        onControl: (m) => {
          if (m.type === "displays") setDisplays({ list: m.displays, current: m.current });
          else if (m.type === "pin_required") {
            setPin({ open: true, message: "This computer requires a PIN.", error: false });
            setPinValue("");
          } else if (m.type === "pin_result") {
            if (m.ok) {
              setPin((p) => ({ ...p, open: false }));
              stage.focus();
            } else if (m.attempts_left > 0) {
              setPin({ open: true, message: `Wrong PIN. ${m.attempts_left} attempt${m.attempts_left === 1 ? "" : "s"} left.`, error: true });
              setPinValue("");
            } else setPin((p) => ({ ...p, open: false })); // the session ends with pin_locked
          } else if (m.type === "encoder_status") {
            encoderLine = `${m.encoder} · target ${(m.bitrate_kbps / 1000).toFixed(1)} Mbps · ${m.fps} fps · ${m.scale_pct}%`;
          } else if (m.type === "input_status") {
            inputAvailableRef.current = m.available;
            setInputAvailable(m.available);
            if (!m.available) toast.warning(`View only: ${m.message ?? "input unavailable"}`);
          }
        },
        onClipboard: (text) => {
          if (document.hasFocus()) void writeLocalClipboard(text);
          else pendingClipboard = text;
        },
      },
      options,
    );
    sessionRef.current = session;
    if ("requestVideoFrameCallback" in video) video.requestVideoFrameCallback(onFrame);

    const router = new InputRouter((m) => session.sendInput(m));
    routerRef.current = router;
    const coalescer = new FrameCoalescer((m) => session.sendInput(m));
    // Always measured at event time, so resizing or fullscreen never shifts the pointer.
    const point = (e: PointerEvent | WheelEvent) => normalizePoint(e.clientX, e.clientY, video.getBoundingClientRect(), video.videoWidth, video.videoHeight);

    // Clicks on glenley's own UI over the picture (PIN box, overlay buttons) stay local.
    const onUi = (e: Event) => (e.target as HTMLElement).closest("[data-ui]") !== null;
    const onMove = (e: PointerEvent) => {
      if (!controlling() || onUi(e)) return;
      const p = point(e);
      if (p) coalescer.pointer(p.x, p.y);
    };
    const onDown = (e: PointerEvent) => {
      if (onUi(e)) return;
      stage.focus();
      if (!controlling()) return;
      const p = point(e);
      if (!p) return;
      e.preventDefault();
      stage.setPointerCapture(e.pointerId); // keep receiving moves while dragging outside
      coalescer.flush();
      // The position goes on the ordered channel with the button, so the click lands exactly here.
      session.sendInput({ type: "pointer_move", ...p }, true);
      session.sendInput({ type: "button", button: e.button, down: true });
    };
    const onUp = (e: PointerEvent) => {
      if (!controlling() || onUi(e)) return;
      coalescer.flush();
      const p = point(e);
      if (p) session.sendInput({ type: "pointer_move", ...p }, true);
      session.sendInput({ type: "button", button: e.button, down: false });
    };
    const onWheel = (e: WheelEvent) => {
      if (!controlling()) return;
      e.preventDefault();
      const { dx, dy } = wheelPixels(e, stage.clientHeight);
      coalescer.addWheel(dx, dy);
    };
    const onKey = (e: KeyboardEvent) => {
      if (onUi(e)) return; // e.g. the PIN never reaches the remote
      if (isViewerShortcut(e)) {
        if (e.type === "keydown") setStatsOn((v) => !v);
        e.preventDefault();
        return;
      }
      if (!controlling()) return;
      if (router.key(e)) e.preventDefault();
    };
    const onPaste = (e: ClipboardEvent) => {
      if (!controlling()) return;
      e.preventDefault();
      router.paste(e.clipboardData?.getData("text/plain") ?? "");
    };
    const release = () => controlling() && router.releaseAll();
    const onVisibility = () => document.hidden && release();
    const onFocus = () => {
      if (pendingClipboard === null) return;
      const text = pendingClipboard;
      pendingClipboard = null;
      void writeLocalClipboard(text);
    };
    const onFullscreen = () => {
      setFullscreen(document.fullscreenElement === stage);
      stage.focus(); // entering/leaving fullscreen can drop focus, which would stop the keyboard
    };
    const noMenu = (e: Event) => e.preventDefault();

    stage.addEventListener("pointermove", onMove);
    stage.addEventListener("pointerdown", onDown);
    stage.addEventListener("pointerup", onUp);
    stage.addEventListener("wheel", onWheel, { passive: false });
    stage.addEventListener("keydown", onKey);
    stage.addEventListener("keyup", onKey);
    stage.addEventListener("paste", onPaste);
    stage.addEventListener("blur", release);
    stage.addEventListener("contextmenu", noMenu);
    document.addEventListener("visibilitychange", onVisibility);
    document.addEventListener("fullscreenchange", onFullscreen);
    window.addEventListener("focus", onFocus);

    let prev: StatsSample | null = null;
    const timer = setInterval(() => {
      const pc = session.peer;
      if (!pc || !statsOnRef.current) return;
      void pc.getStats().then((report) => {
        const { stats: st, sample } = summarize(report as unknown as Parameters<typeof summarize>[0], prev, performance.now());
        prev = sample;
        setStatsText(
          `${st.width}×${st.height} @ ${st.fps} fps\n` +
            `${(st.kbps / 1000).toFixed(2)} Mbps\n` +
            `RTT ${st.rttMs ?? "–"} ms · jitter buf ${st.jitterBufferMs ?? "–"} ms\n` +
            `decode ${st.decodeMs ?? "–"} ms · lost ${st.packetsLost}\n` +
            `path ${st.path} (${st.candidateType})\n` +
            (encoderLine ? `${encoderLine}\n` : "") +
            (latency.count > 0 ? `glass-to-glass p50 ${Math.round(latency.percentile(50) ?? 0)} ms · p95 ${Math.round(latency.percentile(95) ?? 0)} ms (${latency.count})\n` : "") +
            `input ${controlling() ? "on" : "off"}`,
        );
      });
    }, 1000);

    (window as Window & { carsonSession?: ViewerSession }).carsonSession = session; // diagnostics
    session.start();

    return () => {
      stopped = true;
      clearInterval(timer);
      release();
      stage.removeEventListener("pointermove", onMove);
      stage.removeEventListener("pointerdown", onDown);
      stage.removeEventListener("pointerup", onUp);
      stage.removeEventListener("wheel", onWheel);
      stage.removeEventListener("keydown", onKey);
      stage.removeEventListener("keyup", onKey);
      stage.removeEventListener("paste", onPaste);
      stage.removeEventListener("blur", release);
      stage.removeEventListener("contextmenu", noMenu);
      document.removeEventListener("visibilitychange", onVisibility);
      document.removeEventListener("fullscreenchange", onFullscreen);
      window.removeEventListener("focus", onFocus);
      session.stop();
      sessionRef.current = null;
    };
  }, [device.id]); // eslint-disable-line react-hooks/exhaustive-deps -- options are fixed per device

  const toggleControl = (on: boolean) => {
    setControl(on);
    if (!on) routerRef.current?.releaseAll();
  };
  const toggleFullscreen = () => {
    const stage = stageRef.current!;
    if (document.fullscreenElement) return void document.exitFullscreen().catch(() => {});
    void stage.requestFullscreen().then(() => {
      // In fullscreen, Chrome lets us capture system keys (Esc must be held to exit).
      const kb = (navigator as Navigator & { keyboard?: { lock?: () => Promise<void> } }).keyboard;
      void kb?.lock?.().catch(() => {});
      stage.focus();
    }, () => {});
  };
  const submitPin = (e: FormEvent) => {
    e.preventDefault();
    if (/^\d{6,10}$/.test(pinValue)) sessionRef.current?.sendPin(pinValue);
    else setPin((p) => ({ ...p, message: "The PIN is 6 to 10 digits.", error: true }));
  };
  const disconnect = () => navigate("#/devices");

  const badge =
    status.kind === "live" ? (
      <Badge variant="success">
        <span className="size-1.5 rounded-full bg-success" /> Live
      </Badge>
    ) : status.kind === "ended" ? (
      <Badge variant="secondary">Ended</Badge>
    ) : (
      <Badge variant="outline" className="border-white/20 text-white/80">
        <Loader2 className="animate-spin" /> {status.kind === "connecting" ? "Connecting" : "Reconnecting"}
      </Badge>
    );

  return (
    <div className="dark flex h-svh flex-col overflow-hidden bg-neutral-950 text-neutral-100">
      <header className="flex h-12 shrink-0 items-center gap-2 border-b border-white/10 px-2 sm:gap-3 sm:px-3">
        <Button variant="ghost" size="icon-sm" className="text-white/80 hover:bg-white/10 hover:text-white" onClick={disconnect} aria-label="Back to devices">
          <ArrowLeft />
        </Button>
        <span className="min-w-0 truncate text-sm font-medium">{device.name}</span>
        {badge}
        <div className="min-w-0 flex-1" />
        {displays.list.length > 1 && (
          <Select value={String(displays.current)} onValueChange={(v) => sessionRef.current?.selectDisplay(Number(v))}>
            <SelectTrigger size="sm" className="border-white/15 bg-white/5 text-white">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {displays.list.map((d) => (
                <SelectItem key={d.index} value={String(d.index)}>
                  {d.name} · {d.width}×{d.height}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        )}
        <Tooltip>
          <TooltipTrigger asChild>
            <div className="flex shrink-0 items-center gap-2 px-1 sm:px-2">
              <Switch id="control" checked={control && inputAvailable} disabled={!inputAvailable} onCheckedChange={toggleControl} />
              <Label htmlFor="control" className="hidden text-sm text-white/80 sm:flex">
                Control
              </Label>
            </div>
          </TooltipTrigger>
          <TooltipContent>{inputAvailable ? "Send your mouse and keyboard to this computer" : "View only on this computer"}</TooltipContent>
        </Tooltip>
        <Tooltip>
          <TooltipTrigger asChild>
            <Button variant="ghost" size="icon-sm" className={statsOn ? "bg-white/10 text-white" : "text-white/70 hover:bg-white/10 hover:text-white"} onClick={() => setStatsOn((v) => !v)} aria-label="Stats">
              <Gauge />
            </Button>
          </TooltipTrigger>
          <TooltipContent>Connection stats (Ctrl+Alt+Shift+D)</TooltipContent>
        </Tooltip>
        <Tooltip>
          <TooltipTrigger asChild>
            <Button variant="ghost" size="icon-sm" className="text-white/70 hover:bg-white/10 hover:text-white" onClick={toggleFullscreen} aria-label="Fullscreen">
              {fullscreen ? <Minimize /> : <Maximize />}
            </Button>
          </TooltipTrigger>
          <TooltipContent>{fullscreen ? "Exit fullscreen (hold Esc)" : "Fullscreen"}</TooltipContent>
        </Tooltip>
        <Button size="sm" variant="destructive" className="shrink-0" onClick={disconnect} aria-label="Disconnect">
          <PhoneOff /> <span className="hidden sm:inline">Disconnect</span>
        </Button>
      </header>

      <div ref={stageRef} tabIndex={0} className="relative flex min-h-0 min-w-0 flex-1 items-center justify-center overflow-hidden bg-black outline-none">
        <video ref={videoRef} autoPlay playsInline muted className="h-full w-full object-contain" />

        {statsOn && (
          <pre className="pointer-events-none absolute top-3 left-3 rounded-md bg-black/70 px-3 py-2 font-mono text-xs leading-relaxed text-emerald-300">{statsText || "Collecting…"}</pre>
        )}

        {status.kind !== "live" && !pin.open && (
          <div data-ui className={`absolute inset-0 flex flex-col items-center justify-center gap-4 p-6 text-center ${status.kind === "reconnecting" ? "bg-black/40" : "bg-black/70"}`}>
            {status.kind === "ended" ? (
              <>
                <p className="max-w-md text-white/90">{status.message}</p>
                <Button variant="secondary" asChild>
                  <a href="#/devices">Back to devices</a>
                </Button>
              </>
            ) : (
              <>
                <Loader2 className="size-7 animate-spin text-white/70" />
                <p className="text-sm text-white/80">{status.detail}</p>
              </>
            )}
          </div>
        )}

        {pin.open && (
          <div data-ui className="absolute inset-0 grid place-items-center bg-black/70 p-6">
            <Card className="w-full max-w-xs">
              <CardHeader className="items-center text-center">
                <Lock className="mx-auto size-6 text-primary" />
                <CardTitle>Enter PIN</CardTitle>
                <CardDescription className={pin.error ? "text-destructive" : ""}>{pin.message}</CardDescription>
              </CardHeader>
              <CardContent>
                <form className="grid gap-3" onSubmit={submitPin}>
                  <Input autoFocus type="password" inputMode="numeric" autoComplete="off" maxLength={10} placeholder="PIN" aria-label="PIN" className="text-center tracking-widest" value={pinValue} onChange={(e) => setPinValue(e.target.value)} />
                  <Button type="submit">Unlock</Button>
                </form>
              </CardContent>
            </Card>
          </div>
        )}
      </div>
    </div>
  );
}
