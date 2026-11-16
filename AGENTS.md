# AGENTS.md — K2K Packet Desktop

Guidance for any AI agent (Hermes, Claude, Gemini, Codex) working in this repo.
Read this before editing. It is the short version; deeper notes live in the
`k2k-packet-desktop*` skills.

## What this is

A packet-radio (AX.25) terminal for Windows, macOS and Linux. Tauri v2 shell,
Rust backend, **plain JavaScript frontend — no framework, no bundler beyond Vite**.
Speaks KISS and AGWPE to a TNC, implements an AX.25 2.2 connected-mode state
machine, and also runs headless (`--web`) as a browser-served app.

## Layout

| Path | Contents |
|---|---|
| `index.html` | Single page; all UI markup, settings dialog, theme `<option>`s |
| `src/*.js` | Frontend modules (`app.js`, `protocol.js`, `pbbs.js`, `transport.js`, …) |
| `src/styles/main.css` | All CSS, including `[data-theme="…"]` blocks |
| `src-tauri/src/kernel/` | AX.25 state machine, frame encode/decode, NET/ROM decode |
| `src-tauri/src/app/` | Application core, `Out` sink, monitor formatting |
| `src-tauri/src/commands.rs` | Tauri command wrappers |
| `src-tauri/src/web.rs` | Headless browser dispatcher |
| `src-tauri/src/{kiss,agw}.rs` | KISS and AGWPE transports (separate code paths) |
| `tests/frontend/smoke_test.mjs` | Frontend smoke test (jsdom) |
| `packaging/` | Headless armhf build script + systemd unit |

## Commands

```bash
npm ci                                  # install
npm run dev                             # tauri dev (needs a display)
npx vite --host 0.0.0.0 --port 1420     # UI-only preview, no Rust
npm run build                           # vite build -> dist/
npm run test:frontend                   # jsdom smoke test
cd src-tauri && cargo check && cargo test
```

## The gate — run before reporting anything as done

1. `cd src-tauri && cargo check`
2. `cd src-tauri && cargo test` (all green)
3. `node --check` on every changed `.js`, or `npm run build` when modules changed

Report what you actually ran. Never claim a build or behaviour you did not
exercise. If a step was skipped, say so.

## Versioning

The version lives in exactly **two** files: `package.json` and
`src-tauri/Cargo.toml`. `tauri.conf.json` has no `version` key and inherits from
Cargo. Bump both together, or the release artifacts disagree.

## Headless mode (`--web`) — the rule that breaks silently

Every command is split in two:

- `#[tauri::command]` wrapper — exists only for Tauri IPC
- `*_impl(&AppState, Out, …)` body — the real implementation

`web.rs::dispatch` calls the `_impl`. **A command added only as a
`#[tauri::command]` does not exist in browser mode** and the UI fails with
`Unknown command: …`.

- Outward effects go through `app::Out` (`Out::tauri` / `Out::web`), never
  `AppHandle`, inside `_impl` bodies.
- Paths use `storage::app_data_path_static()`.
- `--web` must not construct `tauri::Builder`.
- The frontend talks through `src/transport.js` (`invoke`/`listen`).
- The browser preview shim in `index.html` is opt-in via `?preview`. Never gate
  it on "no `__TAURI_INTERNALS__`" — that is also true in real serve mode.

## Cross-compiling

`src-tauri/.cargo/config.toml` pins the MinGW linker for
`x86_64-pc-windows-gnu`. It is tracked in git — do not delete it.

```bash
npx tauri build --target x86_64-pc-windows-gnu --bundles nsis
```

Cargo builds are **sequential**. Never run the deb and the exe build in
parallel; they fight the Cargo lock.

## Do not

- Build or bump the version unless explicitly asked. "Don't build yet" means
  keep the tree unbuilt and batch further changes.
- Touch production services or the live download page without being asked.
- Reintroduce `AppHandle` into `_impl` bodies.
- Assume a KISS fix covers AGWPE. They are separate paths; both need gating.
- Trust `cargo check` alone after deleting a block — run `git diff` and grep for
  every identifier you removed.

## Known traps

- `pkill -f "k2k-packet-desktop --web"` over SSH kills the SSH session itself.
  Use `pgrep -x k2k-packet-desk` (name truncates at 15 chars) or a bracketed
  pattern.
- A stale Vite dev server on `:1420` serves pre-edit HTML. Check
  `ss -ltnp | grep 1420` before debugging a "missing" change.
- PID `0xCF` carries two different NET/ROM layouts (broadcast vs L3 data).
  Dispatch on the info field's first byte; a stride loop over the wrong layout
  produces plausible garbage, not an error.
- A PID classification can live in more than one place. Grep every gate before
  declaring a fix complete.
- Tauri event payloads are not uniform — several are emitted without `tab`.
  Check the emit site before writing a listener.

## Testing discipline

A regression test is only proven when you have seen it fail. Revert the one line
that fixed the bug, run just the new test, watch it go red, restore, re-run
green. "All tests pass" alone is indistinguishable from a test that asserts
nothing.

## Deploy (only when asked)

Build from **this** repo. The download page and its artifacts live in
`~/projects/packetterm-desktop/downloads/`, served by
`python3 -m http.server 8000 --directory ~/projects/packetterm-desktop/downloads`.
That other clone is a deploy target only — its source tree is stale.

`npx tauri build` names bundles with **spaces**
(`K2K Packet Desktop_<ver>_amd64.deb`) while every page href uses underscores.
Rename on copy, or the links 404 while the build looks successful.

`dist/` is tracked and the build rewrites it — `git add -A` with the release.
