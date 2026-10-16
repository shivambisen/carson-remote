# End-to-end test plan

Automated checks run on every push (`.github/workflows/ci.yml`). The manual matrix below is for
releases, where real hardware, networks and OS permission dialogs are involved.

## A. Automated (CI and `make test`)

| Area | What is covered | Where |
|---|---|---|
| Auth | OIDC (fake issuer, nonce, PKCE), GitHub OAuth, single-use state, unverified email, origin/CSRF | `server/internal/auth` |
| Pairing & devices | code guessing limits, expiry, Ed25519 challenge, cross-account isolation, revoke | `server/internal/devices`, `store` |
| Signaling | presence, heartbeats, relay authorization, resume (tokens, expiry, theft), floods, audit rows | `server/internal/signaling` |
| TURN | coturn REST credentials vs an independent HMAC vector | `server/internal/turn` |
| Protocol | shared fixtures: message types, device auth, input bytes, DTLS binding | `proto/*` (checked by Go, Rust, TS) |
| Media | WebRTC loopback: H.264 over real ICE/DTLS, data channels, input, PIN gate, ICE restart | `carson-agent` `peer::tests` |
| Encoders | VideoToolbox (on macOS runners), OpenH264, Cisco binary hash check | `carson-encode` |
| Input | key maps, multi-click, drag, modifier sync, stuck-key release | `carson-input` |
| Viewer | input encoding, letterboxing, paste ordering, stats, identity verification, pinning, latency maths | `viewer/src/*.test.ts` |
| Pairing + presence end to end | real server + real agent binaries | `scripts/e2e-pairing.sh` |

## B. Manual release checklist (two physical machines)

Use a host (Mac or Windows) and a viewer (any OS, Chrome), on **different networks** where
noted. Record pass/fail and versions.

### Install and pairing
1. Host: install via Homebrew (Mac) / zip + `install.ps1` (Windows). `carson-agent setup --server …`.
2. Browser opens with the code. Approve. The host appears under My devices, online, with the same
   key id as `carson-agent status`.
3. Reboot the host and log in: device comes back online without action (service).

### Permissions (Mac)
4. First connection without Screen Recording → viewer shows the permission message. Grant → works.
5. Without Accessibility → viewer shows "View only". Grant → control works.

### Consent
6. Attended: Allow → banner appears before any picture; Deny → viewer "declined"; ignore 30 s → "no one answered".
7. Banner **Disconnect** ends the session on both sides.
8. `unattended on --pin`: wrong PIN shows attempts left; 5 wrong → lockout message; right PIN → picture.
   Banner still shown.

### Control
9. Mouse: click, double-click (select a word), drag a window, right-click menu, scroll (trackpad + wheel).
10. Keyboard: typing, Shift/Caps, shortcuts (host's own, e.g. Ctrl+C on Windows), arrows, F-keys.
11. Focus loss mid-keypress (Cmd+Tab away while holding a key) → no stuck key on the host.
12. Clipboard both ways, including non-ASCII text and >100 KB.
13. Two monitors: switch in the toolbar; clicks land on the right monitor (incl. negative coordinates).
14. Fullscreen + keyboard lock (Chrome): Cmd/Alt+Tab goes to the host.

### Networks
15. Same Wi-Fi: Stats shows `path direct`.
16. Different networks (phone hotspot for the viewer): connects; note direct vs relay.
17. Strict network (or `?relay=1`): `path relay`, still smooth.
18. Switch the viewer's Wi-Fi mid-session → "Reconnecting…", recovers within ~5 s, same session.
19. Kill the host's network for 10 s → recovers; for 40 s → session ends ("connection lost").
20. Restart the server mid-session: video continues P2P. Expect the session to end on next signaling need
    (sessions are in memory). Viewer can reconnect.

### Security
21. Second account cannot see or connect to the first account's devices.
22. **Remove** a device → host disconnects within ~1 s, the agent stops with the re-pair message.
23. Activity page lists sign-in, pairing, each connection (IP, browser, duration), removal.
24. Change the pinned key in the browser's localStorage → "Security warning", no connection.

### Performance
25. `carson-agent run --test-pattern`, Stats open: record glass-to-glass p50/p95 and fps,
    direct and via relay. Targets: p50 < 100 ms, 30 fps.
26. Real screen: drag a window quickly: smooth, no smearing longer than one keyframe interval.
27. Throttle the network (Chrome DevTools or macOS Network Link Conditioner, 1 Mbps / 5% loss): quality steps
    down (Stats shows lower target/fps/scale), stays usable, recovers after removing the limit.

### Uninstall
28. `carson-agent service uninstall && carson-agent unpair` → no login item / task left, device removed after
    revoking on the devices page.
