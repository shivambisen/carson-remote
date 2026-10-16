# Benchmarks

## Method

- **Glass-to-glass latency** (shown in the viewer's Stats overlay): the agent's test pattern
  (`carson-agent run --test-pattern`) stamps its clock into every frame as a barcode. The viewer
  reads it from each *presented* frame (`requestVideoFrameCallback`) and aligns the two clocks
  with NTP-style ping/pong over the data channel (best-RTT sample). This includes capture, encode,
  packetization, network, jitter buffer, decode and presentation. It excludes the monitor's own
  scan-out (~half a refresh).
- **Encoder speed:** `cargo test -p carson-encode --release -- --ignored --nocapture encode_speed`.

## Results (2026-10-16, Apple Silicon MacBook, macOS 26, Chrome, one machine, direct path)

| Metric | Result | Target |
|---|---|---|
| Encode 1280×720, VideoToolbox (hardware) | **5.0 ms/frame** | |
| Encode 1280×720, OpenH264 (software) | 10.6 ms/frame | |
| Frame rate, release build, test pattern | 28–29 fps sent / 27 received | 30–60 |
| Glass-to-glass p50 / p95 | **45 ms / 48 ms** | < 100 ms |
| Real screen 1440×900 (M3, software encoder) | 29 fps | |
| Via TURN relay on LAN (M5) | 30 fps, RTT 1 ms | |

Caveats: agent and viewer on the same machine (no real network latency), and the viewer ran in
an embedded browser pane that throttles rendering when unfocused (few latency samples). Earlier runs
under heavy background load (Spotlight indexing, load average 19) dropped to ~21 fps for both
encoders. Re-measure on two machines with `docs/e2e-test-plan.md` §B.

## Adaptive quality

The viewer reports RTT/loss/fps every second. The agent cuts the bitrate by 30% on congestion
(loss above 3%, or RTT 150 ms over baseline) and probes +10% after 3 clean seconds, between
300 kbps and the configured maximum (default 4 Mbps). As the bitrate falls it steps
30 fps/100% → 30/75% → 20/75% → 15/50%. Unit-tested in `agent/crates/carson-agent/src/abr.rs`.
Not yet measured under emulated loss (netem).
