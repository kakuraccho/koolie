# koolie — notes for agents working on this repo

koolie is a Rust + ratatui terminal console for coding agents running in herdr
(a port of the iOS app Heeler to the terminal). Design doc:
`docs/design.md`.

## Build & test

- cargo is at `~/.cargo/bin` — run `export PATH="$HOME/.cargo/bin:$PATH"` first.
- `cargo build`, `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt`.
- All three must pass before committing.

## Layout

- `src/model.rs` — shared types (Agent, AgentStatus, Inventory). Backends project into these.
- `src/transport/` — `Transport` trait; `local.rs`, `ssh.rs` (system OpenSSH, master + `-O forward`).
- `src/backend/` — `Backend` trait; `herdr_tui.rs` (newline JSON socket API), `herdr_core.rs`.
- `src/transcript/` — Chat: parse Claude Code / Codex transcripts into turns.
- `src/usage/` — token usage from transcripts, price table.
- `src/discover/` — host candidates (ssh config / tailscale / herdr machines) and probe.
- `src/session.rs` — per-host connection loop (connect, subscribe, refetch, backoff).
- `src/app.rs` / `src/ui/` / `src/tui.rs` — TUI state, drawing, terminal loop. `src/watch.rs` — `koolie watch` / `status`.
- `tests/fake_herdr.rs` — end-to-end tests against a fake herdr socket server.
- `docs/reference/herdr-api-schema-0.9.5.json` — herdr's full socket API JSON schema.

## Rules

- Keep public interfaces of other modules stable; change only the module you own.
- Never send input to, close, or modify herdr panes/workspaces you did not create.
  Tests must not depend on a running herdr unless marked `#[ignore]`.
- No network access in tests. No `unwrap()` on data from servers or files in non-test code.
- Unknown JSON fields must be ignored (herdr adds fields over time).

## Manual testing

Run the TUI in a detached tmux session and read it with `tmux capture-pane -p`. For a real herdr
check, create a dedicated workspace (`herdr workspace create --no-focus --label koolie-test`) and
close it afterwards; never type into the user's own panes.
