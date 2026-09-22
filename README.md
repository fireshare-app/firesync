# Firesync

A tray companion for [Fireshare](https://github.com/fireshare-app/fireshare). Watches folders on
your machine and uploads new clips and screenshots to your instance on its own, with per-folder
rules for where they land and what gets skipped.

Windows and Linux. Built on Tauri v2 — a Rust core that keeps running with the window closed, and
a React UI that only exists while you are looking at it.

> Early development. Nothing here is releasable yet; see [PLAN.md](PLAN.md) for what lands when.

## What it does

- Watches folders and notices new media, waiting for a recording to actually finish before sending
- Remembers everything it has seen, so nothing uploads twice and files that were already there stay
  put unless you ask for them
- Per-folder rules: destination folder, game, size floor and ceiling, video and/or images
- Retries what is worth retrying and gives up loudly on what is not
- Native notifications, switchable off, quiet while a game has the screen
- Updates itself from GitHub Releases, if you let it

## Requirements

A Fireshare instance with upload tokens — Settings → Security → Upload Tokens. Firesync
authenticates with a token rather than your password, so it can be revoked on its own and never
holds your credentials.

## Development

```bash
npm install
npm run tauri dev
```

Needs Node 20+ and a Rust toolchain ([rustup](https://rustup.rs)). On Linux you will also need the
[Tauri prerequisites](https://v2.tauri.app/start/prerequisites/) — GTK 3, WebKitGTK and libxdo.

| Command | What it does |
| --- | --- |
| `npm run tauri dev` | Run the app against the Vite dev server |
| `npm run typecheck` | Type-check the frontend without emitting |
| `npm run build` | Build the frontend |
| `cd src-tauri && cargo check` | Check the Rust core |

### Layout

```
src-tauri/src/     the Rust core — owns all state, outlives the window
  api/             the Fireshare upload-token API
  config.rs        settings and per-folder rules, on disk
  secrets.rs       the upload token, in the OS keychain
  commands.rs      what the webview may call
src/               React + TypeScript
  views/           screens
  lib/ipc.ts       typed wrappers over invoke()
```

The Rust core owns every piece of state. The webview is a view onto it and can be destroyed at any
moment without interrupting an upload.

## Design notes

[STACK.md](STACK.md) — why Tauri, what the upload-token API does and does not give a desktop client,
and the failure modes that shaped the design. [PLAN.md](PLAN.md) — the build order.
