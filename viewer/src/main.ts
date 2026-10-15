export {};

const statusEl = document.querySelector<HTMLSpanElement>("#server-status");

async function checkServer(): Promise<void> {
  if (!statusEl) return;
  try {
    const res = await fetch("/v1/version");
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    const body = (await res.json()) as { version: string };
    statusEl.textContent = `online (${body.version})`;
  } catch (err) {
    statusEl.textContent = `unreachable (${String(err)})`;
  }
}

void checkServer();
