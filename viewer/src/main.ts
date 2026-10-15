import "./style.css";
import { api, ApiError, loginUrl, type Device, type User } from "./api";
import { h } from "./dom";
import { osLabel, timeAgo } from "./format";
import { formatCode, isCompleteCode } from "./pairing";
import { FrameCoalescer, InputRouter, isViewerShortcut } from "./control";
import { normalizePoint, wheelPixels } from "./input";
import { describeReason, ViewerSession } from "./session";
import { PresenceClient, type ConnectionStatus } from "./signaling";
import { summarize, type StatsSample } from "./stats";

const app = document.querySelector<HTMLElement>("#app")!;
let presence: PresenceClient | null = null;
let viewing: { session: ViewerSession; timer: ReturnType<typeof setInterval>; cleanup: () => void } | null = null;
let user: User | null = null;

type Route = { page: "login" | "devices" | "pair" | "view"; params: URLSearchParams; deviceId?: string };

function parseRoute(): Route {
  const [path = "", query = ""] = location.hash.replace(/^#/, "").split("?");
  const params = new URLSearchParams(query);
  const view = /^\/view\/([A-Za-z0-9_]+)$/.exec(path);
  if (view) return { page: "view", params, deviceId: view[1] };
  const page = path === "/login" ? "login" : path === "/pair" ? "pair" : "devices";
  return { page, params };
}

function render(...nodes: Node[]): void {
  app.replaceChildren(...nodes);
}

function header(): HTMLElement {
  return h(
    "header",
    { class: "bar" },
    h("a", { class: "brand", href: "#/devices" }, "carson"),
    user &&
      h(
        "div",
        { class: "who" },
        h("span", {}, user.email),
        h("button", { class: "link", onclick: () => void logout() }, "Sign out"),
      ),
  );
}

const ERRORS: Record<string, string> = {
  invalid_state: "Your sign-in expired or was already used. Please try again.",
  access_denied: "Sign-in was cancelled.",
  unverified_email: "Your account needs a verified email address.",
  login_failed: "Sign-in failed. Please try again.",
};

async function showLogin(params: URLSearchParams): Promise<void> {
  stopPresence();
  const returnTo = params.get("next") ?? "/#/devices";
  const { providers, dev } = await api.providers();
  const error = params.get("error");
  const email = h("input", { type: "email", name: "email", placeholder: "you@example.com", required: true, autocomplete: "email" });
  const devForm =
    dev &&
    h(
      "form",
      {
        class: "stack",
        onsubmit: (e: Event) => {
          e.preventDefault();
          void api.devLogin(email.value).then(() => (location.hash = hashOf(returnTo)), showError);
        },
      },
      h("p", { class: "muted" }, "Dev mode: sign in with any email (no verification)."),
      email,
      h("button", { type: "submit" }, "Sign in (dev)"),
    );
  render(
    header(),
    h(
      "main",
      { class: "card narrow" },
      h("h1", {}, "Sign in to carson"),
      error && h("p", { class: "error" }, ERRORS[error] ?? "Sign-in failed."),
      ...providers.map((p) => h("a", { class: "button", href: loginUrl(p, returnTo) }, `Continue with ${p === "github" ? "GitHub" : "Google"}`)),
      providers.length === 0 && !dev ? h("p", { class: "error" }, "No sign-in method is configured on this server.") : null,
      devForm,
    ),
  );
}

/** "/#/pair?x=1" → "#/pair?x=1"; anything without a hash goes to the device list. */
function hashOf(returnTo: string): string {
  const i = returnTo.indexOf("#");
  return i >= 0 ? returnTo.slice(i) : "#/devices";
}

function statusLabel(s: ConnectionStatus): string {
  return s === "live" ? "Live" : s === "connecting" ? "Connecting…" : "Reconnecting…";
}

function deviceRow(d: Device): HTMLElement {
  return h(
    "li",
    { class: "device" },
    h("span", { class: `dot ${d.online ? "on" : "off"}`, title: d.online ? "Online" : "Offline" }),
    h(
      "div",
      { class: "grow" },
      h("div", { class: "name" }, d.name),
      h(
        "div",
        { class: "muted" },
        `${osLabel(d.os)} · ${d.online ? "Online" : `Last seen ${timeAgo(d.last_seen_at)}`}${d.agent_version ? ` · agent ${d.agent_version}` : ""}`,
      ),
    ),
    d.online && h("a", { class: "button small", href: `#/view/${encodeURIComponent(d.id)}` }, "View"),
  );
}

function showDevices(): void {
  const list = h("ul", { class: "devices" });
  const status = h("span", { class: "status" }, statusLabel("connecting"));
  const empty = h(
    "div",
    { class: "empty" },
    h("p", {}, "No devices yet. On the computer you want to reach, run:"),
    h("pre", {}, `carson-agent pair --server ${location.origin}`),
    h("p", {}, "then enter the code it shows on the ", h("a", { href: "#/pair" }, "pair page"), "."),
  );
  render(
    header(),
    h(
      "main",
      { class: "card" },
      h("div", { class: "row" }, h("h1", { class: "grow" }, "My devices"), status, h("a", { class: "button small", href: "#/pair" }, "Pair a device")),
      list,
      empty,
    ),
  );
  stopPresence();
  presence = new PresenceClient(
    (s) => {
      if (s.user) user = s.user;
      list.replaceChildren(...s.devices.map(deviceRow));
      empty.hidden = s.devices.length > 0;
    },
    (s) => {
      status.textContent = statusLabel(s);
      status.className = `status ${s}`;
    },
    () => (location.hash = "#/login?next=" + encodeURIComponent("/#/devices")),
  );
  presence.start();
}

function showView(deviceId: string): void {
  stopPresence();
  stopViewing();
  const video = h("video", { autoplay: true, playsinline: true, muted: true, class: "screen" });
  const overlay = h("div", { class: "overlay" }, "Connecting…");
  const stats = h("pre", { class: "stats", hidden: true });
  const toast = h("div", { class: "toast", hidden: true });
  // The stage takes keyboard focus while controlling; clicking it focuses it.
  const stage = h("div", { class: "stage", tabindex: "0" }, video, overlay, stats, toast);
  const displaySelect = h("select", { class: "small", hidden: true, title: "Display" });
  const controlToggle = h("input", { type: "checkbox", checked: true });
  const controlLabel = h("label", { class: "toggle", title: "Send mouse and keyboard to the remote computer" }, controlToggle, " Control");
  const toggleStats = () => (stats.hidden = !stats.hidden);
  const fullscreen = () => {
    if (document.fullscreenElement) return void document.exitFullscreen().catch(() => {});
    void stage.requestFullscreen().then(() => {
      // In fullscreen, Chrome lets us capture system keys (Esc must be held to exit).
      const kb = (navigator as Navigator & { keyboard?: { lock?: () => Promise<void> } }).keyboard;
      void kb?.lock?.().catch(() => {});
      stage.focus();
    }, () => {});
  };
  render(
    h(
      "div",
      { class: "viewer" },
      h(
        "header",
        { class: "bar" },
        h("a", { class: "brand", href: "#/devices", onclick: () => stopViewing() }, "← carson"),
        h(
          "div",
          { class: "row" },
          h("span", { class: "status", id: "view-status" }, "Connecting"),
          displaySelect,
          controlLabel,
          h("button", { class: "secondary small", onclick: toggleStats, title: "Ctrl+Alt+Shift+D" }, "Stats"),
          h("button", { class: "secondary small", onclick: fullscreen }, "Fullscreen"),
          h("button", { class: "small", onclick: () => { stopViewing(); location.hash = "#/devices"; } }, "Disconnect"),
        ),
      ),
      stage,
    ),
  );
  const statusEl = document.querySelector<HTMLElement>("#view-status")!;
  let inputAvailable = false;
  let pendingClipboard: string | null = null;
  const showToast = (text: string) => {
    toast.textContent = text;
    toast.hidden = false;
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => (toast.hidden = true), 2500);
  };
  let toastTimer: ReturnType<typeof setTimeout> | undefined;
  const writeLocalClipboard = (text: string) =>
    navigator.clipboard.writeText(text).then(() => showToast("Copied from remote computer"), () => (pendingClipboard = text));

  const session = new ViewerSession(deviceId, video, {
    onState: (s) => {
      if (s.kind === "connecting") {
        overlay.hidden = false;
        overlay.textContent = s.detail;
        statusEl.textContent = "Connecting";
      } else if (s.kind === "live") {
        overlay.hidden = true;
        statusEl.textContent = "Live";
        statusEl.className = "status live";
        stage.focus();
      } else {
        overlay.hidden = false;
        overlay.replaceChildren(
          h("p", {}, describeReason(s.reason, s.message)),
          h("a", { class: "button small", href: "#/devices" }, "Back to devices"),
        );
        statusEl.textContent = "Ended";
        statusEl.className = "status";
      }
    },
    onControl: (m) => {
      if (m.type === "displays") {
        displaySelect.replaceChildren(
          ...m.displays.map((d) => h("option", { value: String(d.index), selected: d.index === m.current }, `${d.name} · ${d.width}×${d.height}`)),
        );
        displaySelect.hidden = m.displays.length < 2;
      } else if (m.type === "input_status") {
        inputAvailable = m.available;
        controlToggle.disabled = !m.available;
        controlToggle.checked = m.available;
        controlLabel.title = m.available ? "Send mouse and keyboard to the remote computer" : m.message ?? "Input unavailable";
        if (!m.available) showToast(`View only: ${m.message ?? "input unavailable"}`);
      }
    },
    onClipboard: (text) => {
      if (document.hasFocus()) void writeLocalClipboard(text);
      else pendingClipboard = text;
    },
  });
  displaySelect.addEventListener("change", () => session.selectDisplay(Number(displaySelect.value)));

  const controlling = () => inputAvailable && controlToggle.checked;
  const router = new InputRouter((m) => session.sendInput(m));
  const coalescer = new FrameCoalescer((m) => session.sendInput(m));
  controlToggle.addEventListener("change", () => {
    if (!controlToggle.checked) router.releaseAll();
  });
  const point = (e: PointerEvent | WheelEvent) =>
    normalizePoint(e.clientX, e.clientY, video.getBoundingClientRect(), video.videoWidth, video.videoHeight);

  stage.addEventListener("pointermove", (e) => {
    if (!controlling()) return;
    const p = point(e);
    if (p) coalescer.pointer(p.x, p.y);
  });
  stage.addEventListener("pointerdown", (e) => {
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
  });
  stage.addEventListener("pointerup", (e) => {
    if (!controlling()) return;
    coalescer.flush();
    const p = point(e);
    if (p) session.sendInput({ type: "pointer_move", ...p }, true);
    session.sendInput({ type: "button", button: e.button, down: false });
  });
  stage.addEventListener("contextmenu", (e) => e.preventDefault());
  stage.addEventListener(
    "wheel",
    (e) => {
      if (!controlling()) return;
      e.preventDefault();
      const { dx, dy } = wheelPixels(e, stage.clientHeight);
      coalescer.addWheel(dx, dy);
    },
    { passive: false },
  );
  const onKey = (e: KeyboardEvent) => {
    if (isViewerShortcut(e)) {
      if (e.type === "keydown") toggleStats();
      e.preventDefault();
      return;
    }
    if (!controlling()) return;
    if (router.key(e)) e.preventDefault();
  };
  stage.addEventListener("keydown", onKey);
  stage.addEventListener("keyup", onKey);
  stage.addEventListener("paste", (e) => {
    if (!controlling()) return;
    e.preventDefault();
    router.paste(e.clipboardData?.getData("text/plain") ?? "");
  });
  stage.addEventListener("blur", () => controlling() && router.releaseAll());
  const onVisibility = () => document.hidden && controlling() && router.releaseAll();
  const onFocus = () => {
    if (pendingClipboard !== null) {
      const text = pendingClipboard;
      pendingClipboard = null;
      void writeLocalClipboard(text);
    }
  };
  document.addEventListener("visibilitychange", onVisibility);
  window.addEventListener("focus", onFocus);

  let prev: StatsSample | null = null;
  const timer = setInterval(() => {
    const pc = session.peer;
    if (!pc || stats.hidden) return;
    void pc.getStats().then((report) => {
      const { stats: st, sample } = summarize(report as unknown as Parameters<typeof summarize>[0], prev, performance.now());
      prev = sample;
      stats.textContent =
        `${st.width}x${st.height} @ ${st.fps} fps\n` +
        `${(st.kbps / 1000).toFixed(2)} Mbps\n` +
        `RTT ${st.rttMs ?? "–"} ms · jitter buf ${st.jitterBufferMs ?? "–"} ms\n` +
        `decode ${st.decodeMs ?? "–"} ms · lost ${st.packetsLost}\n` +
        `path ${st.path} (${st.candidateType})\n` +
        `input ${controlling() ? "on" : "off"}`;
    });
  }, 1000);
  viewing = {
    session,
    timer,
    cleanup: () => {
      document.removeEventListener("visibilitychange", onVisibility);
      window.removeEventListener("focus", onFocus);
      if (controlling()) router.releaseAll();
    },
  };
  (window as Window & { carsonSession?: ViewerSession }).carsonSession = session; // diagnostics
  session.start();
}

function stopViewing(): void {
  if (!viewing) return;
  viewing.cleanup();
  clearInterval(viewing.timer);
  viewing.session.stop();
  viewing = null;
}


function showPair(params: URLSearchParams): void {
  stopPresence();
  const input = h("input", {
    name: "code",
    placeholder: "BCDF-GHJK",
    autocomplete: "off",
    autocapitalize: "characters",
    spellcheck: "false",
    inputmode: "text",
    "aria-label": "Pairing code",
  });
  input.value = formatCode(params.get("code") ?? "");
  input.addEventListener("input", () => (input.value = formatCode(input.value)));
  const result = h("div", { class: "stack" });
  const form = h(
    "form",
    {
      class: "stack",
      onsubmit: (e: Event) => {
        e.preventDefault();
        if (!isCompleteCode(input.value)) return showError(new Error("Enter the 8-letter code shown by carson-agent."), result);
        api.pairLookup(input.value).then((p) => confirm(p.name, p.os), (err) => showError(err, result));
      },
    },
    h("p", {}, "Enter the code shown by ", h("code", {}, "carson-agent pair"), " on the computer you want to add."),
    input,
    h("button", { type: "submit" }, "Continue"),
  );
  const confirm = (name: string, os: string) => {
    form.hidden = true;
    result.replaceChildren(
      h("p", {}, "Add ", h("strong", {}, name), ` (${osLabel(os)}) to your account?`),
      h("p", { class: "muted" }, "Anyone signed in to this account will be able to request access to it. Only approve a code you started yourself."),
      h(
        "div",
        { class: "row" },
        h("button", { onclick: () => api.pairApprove(input.value).then(() => (location.hash = "#/devices"), (err) => showError(err, result)) }, "Add device"),
        h("button", { class: "secondary", onclick: () => showPair(new URLSearchParams()) }, "Cancel"),
      ),
    );
  };
  render(header(), h("main", { class: "card narrow" }, h("h1", {}, "Pair a device"), form, result));
  input.focus();
}

function showError(err: unknown, into?: HTMLElement): void {
  const msg =
    err instanceof ApiError
      ? err.code === "invalid_code"
        ? "That code is invalid or has expired. Start pairing again on the device."
        : err.code === "rate_limited"
          ? "Too many attempts. Wait a minute and try again."
          : err.message
      : err instanceof Error
        ? err.message
        : String(err);
  const p = h("p", { class: "error", role: "alert" }, msg);
  if (into) into.replaceChildren(p);
  else app.prepend(p);
}

function stopPresence(): void {
  presence?.stop();
  presence = null;
}

async function logout(): Promise<void> {
  await api.logout().catch(() => {});
  user = null;
  location.hash = "#/login";
}

async function route(): Promise<void> {
  const r = parseRoute();
  if (r.page !== "view") stopViewing();
  if (r.page === "login") return showLogin(r.params);
  if (!user) {
    try {
      user = await api.me();
    } catch (err) {
      if (err instanceof ApiError && err.status === 401) {
        location.hash = "#/login?next=" + encodeURIComponent("/" + (location.hash || "#/devices"));
        return;
      }
      throw err;
    }
  }
  if (r.page === "pair") showPair(r.params);
  else if (r.page === "view" && r.deviceId) showView(r.deviceId);
  else showDevices();
}

window.addEventListener("hashchange", () => void route().catch(showError));
void route().catch(showError);
