# carson user guide

carson lets you see and control your own computers from a web browser. It is free: there is
no account to buy and no subscription. You (or someone you trust) run a small server, and
each computer you want to reach runs `carson-agent`.

## 1. Set up a computer to be reached ("host")

**Mac (Homebrew):**

```bash
brew install shivambisen/carson-remote/carson-agent
carson-agent setup --server https://YOUR-CARSON-SERVER
```

**Mac (download):** unpack `carson-agent-…-macos-arm64.tar.gz` (Apple Silicon) or `…-x86_64`
(Intel), right-click `carson-agent` → **Open** once (it is not notarized), then run the same
`setup` command in Terminal.

**Windows:** unzip `carson-agent-…-windows-x86_64.zip`, then in that folder:

```powershell
powershell -ExecutionPolicy Bypass -File install.ps1 -Server https://YOUR-CARSON-SERVER
```

If SmartScreen warns (unsigned app): **More info → Run anyway**.

`setup` does everything once:

1. **Pairs** the computer with your account. A browser opens with a code filled in: sign in and click
   **Add device**.
2. **Permissions** (Mac): allow **Screen & System Audio Recording** and **Accessibility** for
   carson-agent in System Settings → Privacy & Security, then run `setup` again if asked.
3. **Codec**: on Macs carson uses the built-in hardware encoder. Elsewhere it downloads Cisco's
   free OpenH264 library ("OpenH264 Video Codec provided by Cisco Systems, Inc.").
4. **Starts automatically** at login, in the background.

## 2. Connect from another computer

Open your carson server's address in Chrome, Edge, Firefox or Safari and sign in. Under **My
devices**, click **View** next to a green (online) computer.

- The person at that computer sees **Allow / Deny** (no answer in 30 s means deny), then a red banner
  "*you* are controlling this computer" with a **Disconnect** button for the whole session.
- Click the picture to control it. Untick **Control** for view-only.
- **Copy/paste text:** ⌘/Ctrl+V pastes your clipboard there. Text copied there lands in your clipboard.
- **Fullscreen** captures system shortcuts too (hold Esc to leave).
- With several monitors, pick one in the toolbar.
- **Stats** (Ctrl+Alt+Shift+D) shows fps, bitrate, delay and whether the connection is direct or relayed.

## 3. Unattended access (no one at the computer)

On the host itself (it can never be enabled remotely):

```bash
carson-agent unattended on --pin   # asks for a 6-10 digit PIN
carson-agent unattended off
```

Viewers then type the PIN instead of someone clicking Allow. 5 wrong PINs lock it for 15
minutes. The red banner still shows on the host.

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
| Device shows offline | `carson-agent service status`; `carson-agent service install` to start it; log: `agent.log` in the carson config folder |
| Black screen / "permission" message | Mac: allow Screen Recording for carson-agent, then `carson-agent service install` again (restarts it) |
| Can view but not control | Mac: allow Accessibility for carson-agent |
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
