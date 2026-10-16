import { useEffect, useState, type ReactNode } from "react";
import { ArrowRight, Check, Code2, Download, KeyRound, Laptop, Lock, MonitorSmartphone, MousePointer2, Radio, ShieldCheck, Zap } from "lucide-react";

import { MARK_PATH } from "./logo";
import { APP_URL, currentUAData, detectPlatform, DOWNLOADS, refinePlatform, SOURCE_URL, type Platform } from "./downloads";

function usePlatform(): Platform {
  const [platform, setPlatform] = useState<Platform>(() => detectPlatform(navigator.userAgent, currentUAData()));
  useEffect(() => {
    void refinePlatform(platform, currentUAData()).then(setPlatform);
  }, []); // eslint-disable-line react-hooks/exhaustive-deps -- once, on load
  return platform;
}

function Mark({ className }: { className?: string }) {
  return (
    <svg viewBox="0 0 100 100" className={className} aria-hidden="true">
      <path fill="currentColor" fillRule="evenodd" d={MARK_PATH} />
    </svg>
  );
}

function AppleLogo({ className = "size-4" }: { className?: string }) {
  return (
    <svg viewBox="0 0 24 24" fill="currentColor" className={className} aria-hidden="true">
      <path d="M16.37 12.66c-.02-2.3 1.88-3.4 1.97-3.46-1.07-1.57-2.74-1.78-3.33-1.8-1.42-.14-2.77.83-3.49.83-.72 0-1.83-.81-3.01-.79-1.55.02-2.98.9-3.78 2.29-1.61 2.8-.41 6.94 1.16 9.21.77 1.11 1.68 2.36 2.88 2.31 1.16-.05 1.59-.75 2.99-.75 1.4 0 1.79.75 3.01.72 1.24-.02 2.03-1.13 2.79-2.25.88-1.29 1.24-2.53 1.26-2.6-.03-.01-2.42-.93-2.45-3.71zM14.08 5.9c.64-.77 1.07-1.85.95-2.92-.92.04-2.03.61-2.69 1.38-.59.68-1.11 1.77-.97 2.82 1.02.08 2.07-.52 2.71-1.28z" />
    </svg>
  );
}

function WindowsLogo({ className = "size-4" }: { className?: string }) {
  return (
    <svg viewBox="0 0 24 24" fill="currentColor" className={className} aria-hidden="true">
      <path d="M3 5.5 10.5 4.5v7H3zM11.5 4.4 21 3v8.5h-9.5zM3 12.5h7.5v7L3 18.5zM11.5 12.5H21V21l-9.5-1.4z" />
    </svg>
  );
}

function osIcon(p: Platform, className?: string) {
  return p === "windows" ? <WindowsLogo className={className} /> : <AppleLogo className={className} />;
}

/** The primary call to action for this visitor's computer. */
function DownloadButton({ platform, size = "lg" }: { platform: Platform; size?: "sm" | "lg" }) {
  if (platform === "other") {
    return (
      <a href="#download" className={btn("dark", size)}>
        <Download className="size-4" /> Download for desktop
      </a>
    );
  }
  const d = DOWNLOADS[platform];
  return (
    <a href={d.href} className={btn(size === "sm" ? "accent" : "dark", size)}>
      {osIcon(platform)} Download for {platform === "windows" ? "Windows" : "Mac"}
    </a>
  );
}

function btn(kind: "dark" | "accent" | "ghost", size: "sm" | "lg" = "lg"): string {
  const base = "inline-flex items-center justify-center gap-2 whitespace-nowrap rounded-xl font-medium transition-all active:translate-y-px";
  const sizes = size === "lg" ? "px-7 py-3.5 text-[15px]" : "px-4 py-2 text-sm";
  const kinds = {
    dark: "bg-ink text-white shadow-[0_1px_2px_rgba(61,61,61,.7),0_6px_16px_-4px_rgba(0,0,0,.3)] hover:bg-ink/90",
    accent: "bg-accent text-white shadow-[0_1px_2px_rgba(180,80,0,.5),0_4px_12px_rgba(249,115,22,.25)] hover:bg-[#ea6a10]",
    ghost: "bg-white/60 text-ink chip-shadow hover:bg-white",
  };
  return `${base} ${sizes} ${kinds[kind]}`;
}

function Nav({ platform }: { platform: Platform }) {
  return (
    <header className="sticky top-0 z-50 border-b border-transparent bg-paper/75 backdrop-blur-md">
      <div className="mx-auto flex h-16 max-w-6xl items-center gap-8 px-5">
        <a href="#" className="flex items-center gap-2 text-[17px] font-semibold tracking-tight">
          <Mark className="size-7" /> glenley
        </a>
        <nav className="hidden items-center gap-7 text-[15px] text-ink/80 md:flex">
          <a href="#how" className="hover:text-ink">How it works</a>
          <a href="#private" className="hover:text-ink">Privacy</a>
          <a href="#pricing" className="hover:text-ink">Pricing</a>
          <a href="#download" className="hover:text-ink">Download</a>
          <a href={SOURCE_URL} className="hover:text-ink">GitHub</a>
        </nav>
        <div className="flex-1" />
        {platform === "other" ? (
          // Phones and tablets can't host glenley, but they can connect.
          <a href={APP_URL} className={btn("accent", "sm")}>
            Connect <ArrowRight className="size-4" />
          </a>
        ) : (
          <>
            <a href={APP_URL} className="hidden text-[15px] font-medium text-ink/80 hover:text-ink sm:inline">
              Connect
            </a>
            <DownloadButton platform={platform} size="sm" />
          </>
        )}
      </div>
    </header>
  );
}

/** A floating feature chip around the hero. */
function Chip({ icon, label, className }: { icon: ReactNode; label: string; className: string }) {
  return (
    <div className={`pointer-events-none absolute hidden flex-col items-center gap-2 lg:flex ${className}`}>
      <div className="grid size-12 place-items-center rounded-xl bg-paper text-ink/70 chip-shadow">{icon}</div>
      <span className="text-xs text-muted">{label}</span>
    </div>
  );
}

/** A stylised remote session: someone else's desktop inside your browser. */
function SessionMock() {
  return (
    <div className="mx-auto w-full max-w-4xl rounded-[20px] bg-white/70 p-2.5 soft ring-1 ring-black/5">
      <div className="overflow-hidden rounded-[14px] bg-screen">
        <div className="flex h-10 items-center gap-2 border-b border-white/10 px-4 text-[13px] text-white/80">
          <span className="hidden size-2.5 rounded-full bg-white/15 sm:block" />
          <span className="hidden size-2.5 rounded-full bg-white/15 sm:block" />
          <span className="hidden size-2.5 rounded-full bg-white/15 sm:block" />
          <span className="font-medium whitespace-nowrap sm:ml-3">Studio PC</span>
          <span className="ml-2 hidden items-center gap-1.5 rounded-md bg-emerald-400/15 px-2 py-0.5 text-[11px] text-emerald-300 sm:inline-flex">
            <span className="size-1.5 rounded-full bg-emerald-300" /> Live · direct · 42 ms
          </span>
          <span className="ml-2 inline-flex size-2 rounded-full bg-emerald-300 sm:hidden" aria-label="Live" />
          <div className="flex-1" />
          <span className="rounded-md bg-red-500/90 px-2.5 py-1 text-[11px] font-medium text-white">Disconnect</span>
        </div>
        <div className="relative aspect-[16/8.5] bg-gradient-to-br from-[#1d2a44] via-[#2a2f4f] to-[#3b2a3f]">
          <div className="absolute top-[12%] left-[8%] w-[46%] rounded-lg bg-white/95 p-3 shadow-2xl">
            <div className="mb-2 flex gap-1.5">
              <span className="size-2 rounded-full bg-red-400" />
              <span className="size-2 rounded-full bg-amber-400" />
              <span className="size-2 rounded-full bg-emerald-400" />
            </div>
            <div className="space-y-1.5">
              <div className="h-2 w-3/4 rounded bg-ink/15" />
              <div className="h-2 w-full rounded bg-ink/10" />
              <div className="h-2 w-5/6 rounded bg-ink/10" />
              <div className="h-2 w-2/3 rounded bg-ink/10" />
              <div className="mt-3 h-16 rounded bg-gradient-to-r from-accent/30 to-accent/5" />
            </div>
          </div>
          <div className="absolute right-[9%] bottom-[16%] w-[34%] rounded-lg bg-[#0f1117]/90 p-3 font-mono text-[10px] leading-relaxed text-emerald-300/90 shadow-2xl ring-1 ring-white/10">
            <div>$ npm run build</div>
            <div className="text-white/50">✓ built in 412ms</div>
            <div>$ git push</div>
          </div>
          <MousePointer2 className="absolute top-[52%] left-[49%] size-6 fill-white text-ink drop-shadow" />
        </div>
      </div>
    </div>
  );
}

function Hero({ platform }: { platform: Platform }) {
  const other = platform === "windows" ? "mac-arm" : "windows";
  return (
    <section className="relative overflow-hidden">
      <div className="dots absolute inset-0 [mask-image:radial-gradient(ellipse_at_center,black_35%,transparent_75%)]" />
      <Chip icon={<Lock className="size-5" />} label="Encrypted" className="top-[18%] left-[9%]" />
      <Chip icon={<KeyRound className="size-5" />} label="No codes" className="top-[54%] left-[5%]" />
      <Chip icon={<Zap className="size-5" />} label="Low latency" className="top-[20%] right-[9%]" />
      <Chip icon={<Code2 className="size-5" />} label="Open source" className="top-[52%] right-[5%]" />
      <div className="relative mx-auto max-w-6xl px-5 pt-20 pb-16 text-center sm:pt-28">
        <h1 className="font-serif text-[clamp(3rem,9vw,7rem)] leading-[1.02] tracking-[-0.035em]">
          <span className="text-faint">Your computer,</span>
          <br />
          <span>anywhere.</span>
        </h1>
        <p className="mx-auto mt-7 max-w-xl text-[17px] leading-relaxed text-muted sm:text-lg">
          glenley lets you see and control your own Mac or PC from any browser. Install it once, sign in, and it's always a click away. Encrypted end to end. ₹99 once, yours for life.
        </p>
        <div className="mt-9 flex flex-col items-center justify-center gap-4 sm:flex-row">
          <DownloadButton platform={platform} />
          <a href={APP_URL} className="group inline-flex items-center gap-1.5 px-3 py-3 text-[15px] font-medium text-ink/80 hover:text-ink">
            Connect to a computer <ArrowRight className="size-4 transition-transform group-hover:translate-x-0.5" />
          </a>
        </div>
        <p className="mt-5 text-[13px] text-faint">
          {platform === "other" ? (
            "Available for macOS and Windows"
          ) : (
            <>
              Also for{" "}
              <a href={DOWNLOADS[other].href} className="underline decoration-faint/50 underline-offset-4 hover:text-ink">
                {other === "windows" ? "Windows" : "macOS"}
              </a>
              {platform === "mac-arm" && (
                <>
                  {" · "}
                  <a href={DOWNLOADS["mac-intel"].href} className="underline decoration-faint/50 underline-offset-4 hover:text-ink">
                    Intel Mac
                  </a>
                </>
              )}
              {" · "}₹99 once, no subscription
            </>
          )}
        </p>
      </div>
      <div className="relative px-5 pb-24">
        <SessionMock />
      </div>
    </section>
  );
}

function Eyebrow({ children }: { children: ReactNode }) {
  return <p className="text-xs font-semibold tracking-[0.14em] text-accent-deep uppercase">{children}</p>;
}

function Steps({ steps }: { steps: { title: string; text: ReactNode }[] }) {
  return (
    <ol className="space-y-6">
      {steps.map((s, i) => (
        <li key={s.title} className="flex gap-4">
          <span className="grid size-8 shrink-0 place-items-center rounded-full bg-white font-mono text-sm text-ink chip-shadow">{i + 1}</span>
          <div>
            <p className="font-medium">{s.title}</p>
            <p className="mt-1 text-[15px] leading-relaxed text-muted">{s.text}</p>
          </div>
        </li>
      ))}
    </ol>
  );
}

/** What the glenley tray menu looks like on the computer being shared. */
function TrayMock() {
  const row = (label: string, extra?: ReactNode, dim?: boolean) => (
    <div className={`flex items-center justify-between rounded-md px-3 py-1.5 ${dim ? "text-white/40" : "text-white/85"}`}>
      <span>{label}</span>
      {extra}
    </div>
  );
  const tick = <Check className="size-3.5 text-white/70" />;
  return (
    <div className="rounded-[20px] bg-white/70 p-2.5 soft ring-1 ring-black/5">
      <div className="rounded-[14px] bg-gradient-to-br from-[#2b3350] to-[#45324a] p-6 sm:p-10">
        <div className="ml-auto w-full max-w-[290px] rounded-xl bg-[#1f2128]/95 p-1.5 text-[13px] shadow-2xl ring-1 ring-white/10 backdrop-blur">
          {row("Studio PC · online", <span className="size-2 rounded-full bg-emerald-400" />)}
          <div className="my-1 h-px bg-white/10" />
          {row("Open glenley website")}
          <div className="my-1 h-px bg-white/10" />
          {row("Ask before each connection", undefined)}
          {row("Show on-screen indicator")}
          {row("Start at login", tick)}
          <div className="my-1 h-px bg-white/10" />
          {row("Sign out of this computer", undefined, true)}
          {row("Quit glenley")}
        </div>
      </div>
    </div>
  );
}

/** The device list at app.glenley.in. */
function DevicesMock() {
  const device = (name: string, os: string, online: boolean) => (
    <div className="flex items-center gap-3 px-4 py-3">
      <div className="relative grid size-9 place-items-center rounded-lg bg-paper text-ink/60">
        {os === "Windows" ? <WindowsLogo /> : <AppleLogo />}
        <span className={`absolute -right-0.5 -bottom-0.5 size-2.5 rounded-full border-2 border-white ${online ? "bg-emerald-500" : "bg-faint/60"}`} />
      </div>
      <div className="min-w-0 flex-1">
        <p className="text-sm font-medium">{name}</p>
        <p className="text-xs text-muted">{online ? `Online · ${os}` : `Offline · ${os} · 2 h ago`}</p>
      </div>
      <span className={`rounded-lg px-3 py-1.5 text-xs font-medium ${online ? "bg-ink text-white" : "bg-paper text-faint"}`}>Connect</span>
    </div>
  );
  return (
    <div className="rounded-[20px] bg-white/70 p-2.5 soft ring-1 ring-black/5">
      <div className="overflow-hidden rounded-[14px] bg-white">
        <div className="flex h-10 items-center gap-2 border-b border-line px-4 text-xs text-faint">
          <span className="size-2.5 rounded-full bg-line" />
          <span className="size-2.5 rounded-full bg-line" />
          <span className="size-2.5 rounded-full bg-line" />
          <span className="mx-auto rounded-md bg-paper px-3 py-1 font-mono">app.glenley.in</span>
        </div>
        <div className="p-5">
          <p className="font-serif text-2xl">Devices</p>
          <p className="mb-4 text-xs text-muted">Computers on your account</p>
          <div className="divide-y divide-line rounded-xl ring-1 ring-line">
            {device("Studio PC", "Windows", true)}
            {device("MacBook Air", "macOS", true)}
            {device("Office iMac", "macOS", false)}
          </div>
        </div>
      </div>
    </div>
  );
}

function HowItWorks({ platform }: { platform: Platform }) {
  const [tab, setTab] = useState<"share" | "connect">("share");
  const tabBtn = (id: "share" | "connect", icon: ReactNode, label: string) => (
    <button
      type="button"
      onClick={() => setTab(id)}
      className={`inline-flex items-center gap-2 rounded-lg px-4 py-2 text-sm transition-all ${tab === id ? "bg-white text-ink chip-shadow" : "text-muted hover:text-ink"}`}
      aria-pressed={tab === id}
    >
      {icon}
      {label}
    </button>
  );
  return (
    <section id="how" className="scroll-mt-20 border-t border-line/70 py-24 sm:py-32">
      <div className="mx-auto max-w-6xl px-5">
        <div className="mx-auto max-w-2xl text-center">
          <h2 className="font-serif text-[clamp(2.4rem,5.5vw,4.2rem)] leading-[1.05] tracking-[-0.03em]">
            Two steps.
            <br />
            <span className="text-faint">No codes, no setup.</span>
          </h2>
          <p className="mt-6 text-[17px] leading-relaxed text-muted">Install glenley on the computer you want to reach. Then open glenley in any browser, on any other computer, and click Connect.</p>
        </div>
        <div className="mt-10 flex justify-center">
          <div className="inline-flex gap-1 rounded-xl bg-line/60 p-1">
            {tabBtn("share", <Laptop className="size-4" />, "Share a computer")}
            {tabBtn("connect", <MonitorSmartphone className="size-4" />, "Connect to one")}
          </div>
        </div>
        <div className="mt-14 grid items-center gap-12 lg:grid-cols-[1fr_1.15fr]">
          {tab === "share" ? (
            <>
              <div>
                <Eyebrow>On the computer to reach</Eyebrow>
                <h3 className="mt-3 font-serif text-4xl leading-tight tracking-[-0.02em]">Install once. It stays reachable.</h3>
                <div className="mt-8">
                  <Steps
                    steps={[
                      { title: "Download and open glenley", text: "A tiny app for Mac or Windows. No admin rights needed on Windows." },
                      { title: "Sign in with Google", text: "Your browser opens. Sign in and the computer is added to your account. There's no code to type." },
                      { title: "That's it", text: "glenley sits quietly in the menu bar or tray and starts with the computer. Its icon lights up whenever someone is connected." },
                    ]}
                  />
                </div>
                <div className="mt-9">
                  <DownloadButton platform={platform} />
                </div>
              </div>
              <TrayMock />
            </>
          ) : (
            <>
              <div>
                <Eyebrow>From anywhere</Eyebrow>
                <h3 className="mt-3 font-serif text-4xl leading-tight tracking-[-0.02em]">Any browser is your remote.</h3>
                <div className="mt-8">
                  <Steps
                    steps={[
                      { title: "Open app.glenley.in", text: "In Chrome, Edge, Safari or Firefox, on any computer. Nothing to install here." },
                      { title: "Sign in with the same account", text: "Every computer you've added shows up, with a green dot when it's online." },
                      { title: "Click Connect", text: "See the screen, use the mouse and keyboard, copy and paste text both ways, go fullscreen." },
                    ]}
                  />
                </div>
                <div className="mt-9">
                  <a href={APP_URL} className={btn("dark")}>
                    Open glenley <ArrowRight className="size-4" />
                  </a>
                </div>
              </div>
              <DevicesMock />
            </>
          )}
        </div>
      </div>
    </section>
  );
}

function Feature({ id, eyebrow, title, text, points, visual, flip }: { id?: string; eyebrow: string; title: ReactNode; text: string; points: string[]; visual: ReactNode; flip?: boolean }) {
  return (
    <div id={id} className="grid scroll-mt-24 items-center gap-12 border-t border-line/70 py-20 lg:grid-cols-2 lg:gap-20">
      <div className={flip ? "lg:order-2" : ""}>
        <Eyebrow>{eyebrow}</Eyebrow>
        <h3 className="mt-3 font-serif text-[clamp(2.2rem,4.5vw,3.4rem)] leading-[1.05] tracking-[-0.025em]">{title}</h3>
        <p className="mt-5 max-w-md text-[16px] leading-relaxed text-muted">{text}</p>
        <div className="mt-6 flex flex-wrap gap-2">
          {points.map((p) => (
            <span key={p} className="rounded-lg bg-white/70 px-3 py-1.5 text-[13px] text-ink/80 chip-shadow">
              {p}
            </span>
          ))}
        </div>
      </div>
      <div className={flip ? "lg:order-1" : ""}>{visual}</div>
    </div>
  );
}

function Panel({ children }: { children: ReactNode }) {
  return (
    <div className="rounded-[20px] bg-white/70 p-2.5 soft ring-1 ring-black/5">
      <div className="rounded-[14px] bg-screen p-7 font-mono text-[13px] leading-7 text-white/80">{children}</div>
    </div>
  );
}

function Features() {
  return (
    <section className="mx-auto max-w-6xl px-5 pb-12">
      <Feature
        id="private"
        eyebrow="Private"
        title={
          <>
            Your screen never
            <br />
            <span className="text-faint">touches our server.</span>
          </>
        }
        text="Video, keyboard and mouse go straight between your two computers, encrypted. The server only introduces them. Each computer signs its connections with its own key, so nobody can pretend to be it."
        points={["End-to-end encrypted", "Only your account", "Activity log", "Remove a device instantly"]}
        visual={
          <Panel>
            <div className="flex items-center gap-2 text-emerald-300">
              <ShieldCheck className="size-4" /> Connection verified
            </div>
            <div className="text-white/40">device key&nbsp; 092e-l5wR-KoZ-xpO9</div>
            <div className="text-white/40">signature&nbsp;&nbsp; ✓ matches Studio PC</div>
            <div className="text-white/40">path&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;&nbsp; direct · DTLS-SRTP</div>
            <div className="mt-3 text-white/60">server sees&nbsp; who · when · from where</div>
            <div className="text-white/60">server never sees&nbsp; your screen</div>
          </Panel>
        }
      />
      <Feature
        flip
        eyebrow="Fast"
        title={
          <>
            Feels like
            <br />
            <span className="text-faint">you're sitting there.</span>
          </>
        }
        text="Hardware video encoding on the Mac, adaptive quality that follows your connection, and a direct path between computers whenever the network allows. We measured about 45 ms from screen to screen on the same network."
        points={["Hardware encoding", "Adaptive quality", "Up to 30 fps", "Multiple monitors"]}
        visual={
          <Panel>
            <div className="flex items-center gap-2 text-amber-300">
              <Radio className="size-4" /> Live stats
            </div>
            <div>1920×1080 @ 30 fps · 3.8 Mbps</div>
            <div>glass-to-glass p50 45 ms · p95 48 ms</div>
            <div>encoder VideoToolbox (hardware)</div>
            <div className="text-white/40">path direct (host)</div>
          </Panel>
        }
      />
      <Feature
        id="pricing"
        eyebrow="Pricing"
        title={
          <>
            ₹99. Once.
            <br />
            <span className="text-faint">Yours for life.</span>
          </>
        }
        text="No subscription and no renewals. Pay once with UPI, a card or netbanking, and use glenley on every computer you own, with every update. glenley is also open source under the Apache 2.0 license."
        points={["One-time payment", "All your computers", "All future updates", "No ads, no tracking"]}
        visual={<PriceCard />}
      />
    </section>
  );
}

function PriceCard() {
  return (
    <div className="rounded-[20px] bg-white/70 p-2.5 soft ring-1 ring-black/5">
      <div className="rounded-[14px] bg-white p-8">
        <p className="text-sm font-medium text-muted">glenley · lifetime</p>
        <p className="mt-3 font-serif text-6xl tracking-[-0.03em]">
          ₹99 <span className="font-sans text-base text-muted">once</span>
        </p>
        <ul className="mt-6 space-y-2.5 text-[15px]">
          {["Every computer you own", "Mac and Windows apps", "Connect from any browser", "End-to-end encrypted", "All future updates"].map((t) => (
            <li key={t} className="flex items-center gap-2.5">
              <Check className="size-4 text-accent" /> {t}
            </li>
          ))}
        </ul>
        <a href={APP_URL} className={`${btn("dark", "sm")} mt-7 w-full`}>
          Get glenley <ArrowRight className="size-4" />
        </a>
        <p className="mt-3 text-center text-xs text-faint">Sign in, then pay with UPI, card or netbanking</p>
      </div>
    </div>
  );
}

function DownloadCard({ platform, current }: { platform: "mac-arm" | "windows"; current: Platform }) {
  const d = DOWNLOADS[platform];
  const isCurrent = current === platform || (platform === "mac-arm" && current === "mac-intel");
  return (
    <div className={`flex flex-col rounded-2xl bg-white/80 p-7 soft ring-1 ${isCurrent ? "ring-accent/40" : "ring-black/5"}`}>
      <div className="flex items-center gap-3">
        <span className="grid size-11 place-items-center rounded-xl bg-paper text-ink">{osIcon(platform, "size-5")}</span>
        <div>
          <p className="font-medium">{platform === "windows" ? "Windows" : "macOS"}</p>
          <p className="text-[13px] text-muted">{d.detail}</p>
        </div>
        {isCurrent && <span className="ml-auto shrink-0 whitespace-nowrap rounded-md bg-accent/10 px-2 py-1 text-[11px] font-medium text-accent-deep">Your system</span>}
      </div>
      <div className="mt-6 flex flex-wrap items-center gap-3">
        <a href={d.href} className={btn(isCurrent ? "dark" : "ghost", "sm")}>
          <Download className="size-4" /> Download
        </a>
        {platform === "mac-arm" && (
          <a href={DOWNLOADS["mac-intel"].href} className="text-[13px] text-muted underline decoration-faint/50 underline-offset-4 hover:text-ink">
            Intel Mac version
          </a>
        )}
      </div>
      <p className="mt-5 text-[12px] leading-relaxed text-faint">
        {platform === "windows"
          ? "Windows may say it protected your PC, because the app isn't signed yet: choose More info → Run anyway."
          : "Unzip and move glenley to Applications. The first time, right-click it and choose Open. Allow Screen Recording and Accessibility when asked."}
      </p>
    </div>
  );
}

function DownloadSection({ platform }: { platform: Platform }) {
  return (
    <section id="download" className="scroll-mt-20 border-t border-line/70 py-24 sm:py-32">
      <div className="mx-auto max-w-4xl px-5">
        <div className="text-center">
          <h2 className="font-serif text-[clamp(2.4rem,5.5vw,4.2rem)] leading-[1.05] tracking-[-0.03em]">Get glenley</h2>
          <p className="mx-auto mt-5 max-w-lg text-[17px] text-muted">Install it on each computer you want to reach. To connect, you only need a browser.</p>
        </div>
        <div className="mt-12 grid gap-5 sm:grid-cols-2">
          <DownloadCard platform="mac-arm" current={platform} />
          <DownloadCard platform="windows" current={platform} />
        </div>
        <p className="mt-8 text-center text-[13px] text-faint">
          Linux hosts are coming. Already installed?{" "}
          <a href={APP_URL} className="text-ink underline underline-offset-4">
            Connect now
          </a>
        </p>
      </div>
    </section>
  );
}

function Faq() {
  const qa: [string, string][] = [
    ["Does the person at the computer know I'm connected?", "Yes. glenley's icon in the menu bar or tray always changes while someone is connected. You can also turn on an on-screen indicator, or ask for approval before every connection."],
    ["Who can connect to my computer?", "Only the account it was added to. You can remove a computer from your account at any time, and every connection is listed under Activity."],
    ["How much does it cost?", "₹99, once. There's no subscription: one payment gives your account glenley on all your computers, with every future update. Payments are handled by Razorpay (UPI, cards, netbanking)."],
    ["Is glenley open source?", "Yes, under the Apache 2.0 license. The video codec on Windows is Cisco's free OpenH264 (OpenH264 Video Codec provided by Cisco Systems, Inc.)."],
    ["What if my network is strict?", "glenley connects computers directly. Most home and mobile networks work. Some office networks block direct connections; a relay for those is on the way."],
  ];
  return (
    <section className="border-t border-line/70 py-24">
      <div className="mx-auto max-w-3xl px-5">
        <h2 className="text-center font-serif text-4xl tracking-[-0.02em]">Questions</h2>
        <div className="mt-10 divide-y divide-line">
          {qa.map(([q, a]) => (
            <details key={q} className="group py-5">
              <summary className="flex cursor-pointer list-none items-center justify-between gap-4 font-medium">
                {q}
                <span className="text-xl text-faint transition-transform group-open:rotate-45">+</span>
              </summary>
              <p className="mt-3 text-[15px] leading-relaxed text-muted">{a}</p>
            </details>
          ))}
        </div>
      </div>
    </section>
  );
}

function Footer() {
  return (
    <footer className="border-t border-line/70">
      <div className="mx-auto flex max-w-6xl flex-col gap-4 px-5 py-10 text-[13px] text-muted sm:flex-row sm:items-center">
        <span className="flex items-center gap-2 font-medium text-ink">
          <Mark className="size-5" /> glenley
        </span>
        <span>₹99 once · open source (Apache-2.0)</span>
        <div className="flex-1" />
        <a href={APP_URL} className="hover:text-ink">Connect</a>
        <a href="#download" className="hover:text-ink">Download</a>
        <a href={SOURCE_URL} className="hover:text-ink">GitHub</a>
      </div>
    </footer>
  );
}

export function App() {
  const platform = usePlatform();
  return (
    <>
      <Nav platform={platform} />
      <main>
        <Hero platform={platform} />
        <HowItWorks platform={platform} />
        <Features />
        <DownloadSection platform={platform} />
        <Faq />
      </main>
      <Footer />
    </>
  );
}
