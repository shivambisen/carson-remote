import "./style.css";
import { api, ApiError, loginUrl, type Device, type User } from "./api";
import { h } from "./dom";
import { osLabel, timeAgo } from "./format";
import { formatCode, isCompleteCode } from "./pairing";
import { PresenceClient, type ConnectionStatus } from "./signaling";

const app = document.querySelector<HTMLElement>("#app")!;
let presence: PresenceClient | null = null;
let user: User | null = null;

type Route = { page: "login" | "devices" | "pair"; params: URLSearchParams };

function parseRoute(): Route {
  const [path = "", query = ""] = location.hash.replace(/^#/, "").split("?");
  const page = path === "/login" ? "login" : path === "/pair" ? "pair" : "devices";
  return { page, params: new URLSearchParams(query) };
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
  else showDevices();
}

window.addEventListener("hashchange", () => void route().catch(showError));
void route().catch(showError);
