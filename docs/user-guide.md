# carson user guide

carson lets you see and control your own computers from a web browser. It is free: no
subscription and no paid services. You (or someone you trust) run a small server; each computer
you want to reach runs the carson app.

## 1. Add a computer ("host")

**Windows:** download `carson-setup-…-windows-x86_64.exe` and run it. No admin rights are needed.
If SmartScreen warns (the app isn't signed yet): **More info → Run anyway**.

**Mac:** download `carson-…-macos-arm64.zip` (Apple Silicon) or `…-x86_64` (Intel), unzip, drag
**carson** to Applications, then right-click it → **Open** once (it isn't notarized).

On first launch carson opens your browser: **sign in** (e.g. with Google) and the computer is
added to your account, with no code to type. carson then lives in the menu bar / notification area
and starts at login. On a Mac, allow **Screen & System Audio Recording** and **Accessibility** for
carson when macOS asks (System Settings → Privacy & Security).

The carson menu:

| Item | What it does |
|---|---|
| Status line | Online, connecting, or who is connected right now |
| Sign in… | Add this computer to your account (opens the browser) |
| Open carson website | Your device list |
| Ask before each connection | Show Allow / Deny here for every connection (off by default) |
| Show on-screen indicator | A small "being viewed" pill while connected (off by default) |
| Start at login | Keep this computer reachable after a restart (on after signing in) |
| Sign out of this computer | Forget this computer's identity here (remove it on the website too) |
| Quit carson | Go offline until carson is started again |

**The tray / menu-bar icon always turns orange while someone is connected.** That can't be
turned off: the person at the computer can always tell.

Computers without a browser (servers, scripts) can use the command-line tool instead:
`carson-agent setup --server https://YOUR-CARSON-SERVER`, then enter the code it prints under
**Add with a code** on the website.

## 2. Connect from another computer

Open your carson server's address in Chrome, Edge, Firefox or Safari and sign in with the same
account. Under **Devices**, click **Connect** next to an online computer.

- Click the picture to control it. Turn **Control** off for view-only.
- **Copy/paste text:** ⌘/Ctrl+V pastes your clipboard there. Text copied there lands in your clipboard.
- **Fullscreen** captures system shortcuts too (hold Esc to leave).
- With several monitors, pick one in the toolbar.
- **Stats** (Ctrl+Alt+Shift+D) shows fps, bitrate, delay and whether the connection is direct or relayed.

## 3. Extra protection (optional)

- **Ask before each connection** (carson menu): someone at the computer must click Allow.
- **PIN:** on the host, `carson-agent unattended on --pin` asks viewers for a 6-10 digit PIN.
  5 wrong PINs lock it for 15 minutes. Neither can be changed remotely.

## 4. Security, in plain words

- Video, keyboard and mouse travel **directly and encrypted** between your two computers when
  possible. If a strict network blocks that, they go through your server's relay, still
  encrypted end to end.
- Only computers paired to **your** account show up, and every connection is listed under
  **Activity** (who, when, from where, how it ended).
- **Remove** a device on the devices page to cut it off instantly.
- Your browser checks each connection is signed by that computer's own key. The devices page
  shows it as "key ABCD-…": `carson-agent status` on the computer shows the same value.

## 5. Troubleshooting

| Problem | Fix |
|---|---|
| Device shows offline | Is carson running (icon in the menu bar / tray)? Start it from Applications / the Start menu. Log: `agent.log` in the carson settings folder |
| Black screen / "permission" message | Mac: allow Screen Recording for carson, then Quit and reopen carson |
| Can view but not control | Mac: allow Accessibility for carson |
| macOS asks for permissions again after an update | Expected for free (ad-hoc signed) builds: allow them again |
| Permissions asked again after an update | Expected for free (non-notarized) builds: macOS ties permissions to the exact binary |
| "Security check failed" | The connection was not signed by that computer's key. Do not proceed; re-pair the computer if you replaced it |
| Removed a device by mistake | Run `carson-agent unpair` then `carson-agent setup --server …` on it |

## 6. Uninstall

```bash
carson-agent service uninstall
carson-agent unpair
```

Then delete the binary (`brew uninstall carson-agent` for Homebrew) and remove the device on
the devices page if it is still listed.
