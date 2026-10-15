# carson protocol fixtures

Language-neutral source of truth for the wire protocol (see `docs/carson-design.md` §6 and §8).
Each implementation (Go server, Rust agent, TS viewer) has a test asserting it matches these files.

- `message-types.txt`: signaling `type` values, one per line, in canonical order.
