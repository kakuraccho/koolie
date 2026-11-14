# koolie — notes for agents working on this repo

koolie is a Rust + ratatui terminal console for coding agents running in herdr
(the iOS app Heeler, brought to the terminal). User docs are in Japanese.

- `README.md` — what it does, keys, CLI, config.
- `docs/design.md` — architecture and the decisions behind it. Read before changing structure.
- `docs/development.md` — tests, manual testing in tmux, real-herdr checks, review rules.
- `docs/status.md` — what is verified on real machines, known limits, next steps. Keep it current.
- `docs/notes/` — dated development notes (history; do not rewrite old ones).
- `LICENSE` (MIT); `THIRD_PARTY_NOTICES.md` + `licenses/` — anything copied or ported from other
  projects is listed there, and a ported file says so in its header comment.

## Build & test

- cargo is at `~/.cargo/bin` — run `export PATH="$HOME/.cargo/bin:$PATH"` first. MSRV 1.89.
- `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt` must all pass before committing.
- UI changes: also run it (`scripts/tui-drive.sh`, `scripts/tiny-sizes.sh`); unit tests miss focus and layout bugs.

## Layout

- `src/model.rs` — shared types (Agent, AgentStatus, Inventory) and console order.
- `src/transport/` — `Transport` trait; `local.rs`, `ssh.rs` (system OpenSSH: master + `-O forward`).
- `src/backend/` — `Backend` trait; `herdr_tui.rs` (newline JSON), `herdr_core.rs` + `herdr_core/`
  (JSON-RPC plain mode; never starts a core), `jsonl.rs` (line framing).
- `src/session.rs` — per-host loop: connect, subscribe, refetch snapshot, backoff.
- `src/discover/` — host candidates (ssh config / tailscale / herdr machines) and the read-only `probe.sh`.
- `src/transcript/` — Chat: Claude Code / Codex transcripts into turns; local or remote reads.
- `src/usage/` — token usage and cost; `pricing.json` (+ `SUPPLEMENT` in `prices.rs`).
- `src/app.rs` (state, keys), `src/ui/` (drawing, `text.rs` editor), `src/tui.rs` (terminal loop, attach).
- `src/notify.rs`, `src/watch.rs` (`watch` / `status`), `src/config.rs`, `src/demo.rs`, `src/main.rs` (CLI).
- `tests/fake_herdr.rs` — integration tests against a fake herdr socket server.
- `docs/reference/herdr-api-schema-0.9.5.json` — herdr's socket API schema.

## Rules

- Never send input to, close, or modify herdr panes/workspaces you did not create. Never run
  `herdr server stop`. Tests must not need a running herdr unless marked `#[ignore]`.
- No network access in tests. No `unwrap()` on data from servers or files in non-test code.
- Unknown JSON fields must be ignored (herdr adds fields over time).
- Remote shell scripts take values as positional arguments, never spliced into the script.
- User-facing text is Japanese.
- A bug fix comes with a test that fails without the fix — check that it does.
