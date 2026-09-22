# Firesync — build plan

Companion to [STACK.md](STACK.md), which carries the stack decision and the API findings this
plan is built on. This document is the order of work.

---

## Assumptions

Stated so they are easy to argue with rather than buried in the phases.

1. **Firesync targets stock `feat/upload-tokens`.** Every phase below works against the branch
   as it stands today. The Fireshare-side fixes in Phase 0 make the client better, not
   possible — so they run as a parallel track and never block client work.
2. **Chunk above 200 MB, single-shot below**, at 32 MB per chunk. Two code paths, but the small
   path is the common one for screenshots and short clips and it is one request instead of
   seven. Revisit if the two paths start to drift.
3. **No delete-after-upload in 1.0.** It needs a server-side confirmation that the file landed,
   which needs the `video_id` lookup (Phase 0d). Shipping it on optimism risks eating clips.
4. **Titles come from the filename in 1.0.** Templates are a small feature on top and not worth
   holding the release for.
5. **One repo**, `fireshare-app/firesync`, separate from the server so release cadence is its
   own. Updates come from its GitHub Releases.

---

## Repository layout

```
firesync/
├─ src-tauri/
│  ├─ tauri.conf.json          bundle targets, updater endpoint + pubkey
│  ├─ Cargo.toml
│  └─ src/
│     ├─ main.rs               entry, tray-first (no window on launch)
│     ├─ config.rs             settings + folder rules, serde, on disk
│     ├─ secrets.rs            token in the OS keychain, never in config
│     ├─ api/
│     │  ├─ client.rs          reqwest client, base URL, auth header
│     │  ├─ discovery.rs       GET /api/upload/token, /options
│     │  └─ upload.rs          single-shot + chunked
│     ├─ ledger/
│     │  ├─ mod.rs             rusqlite, migrations
│     │  └─ queries.rs
│     ├─ watcher/
│     │  ├─ mod.rs             notify + notify-debouncer-full
│     │  └─ settle.rs          the "is it finished" gate
│     ├─ queue/
│     │  ├─ mod.rs             workers, concurrency, pause
│     │  ├─ rules.rs           extension / size / subfolder filters
│     │  └─ retry.rs           classification + backoff
│     ├─ tray.rs
│     ├─ notify.rs             native toasts, suppression
│     ├─ updater.rs
│     └─ commands.rs           the tauri::command surface
├─ src/                        React + TS + Vite
│  ├─ views/  Connect · Folders · Activity · Settings
│  ├─ components/
│  └─ lib/ipc.ts               typed wrappers over invoke()
└─ .github/workflows/release.yml
```

The Rust core owns all state. The webview is a view onto it and may be destroyed at any time
without stopping an upload.

---

## Phase 0 — Fireshare side (parallel track, different repo)

Ordered by damage prevented, not by effort. All four are small.

| | Change | Why | Size |
|---|---|---|---|
| **0a** | Reassemble to `{checkSum}.assembling`, `os.rename` into place after the size check; add `*.assembling` to the startup sweep glob | A SIGKILL mid-reassembly currently leaves a truncated `.mp4` that `bulk-import` ingests within 5 minutes. Silent library corruption. | S |
| **0b** | 202 body reports which indices the server holds | Makes a swept set detectable on the next chunk instead of after re-sending everything; gives real resume to a client that lost its ledger | S |
| **0c** | Startup sweep keyed on mtime (24h) rather than unconditional | A restart stops destroying in-flight uploads entirely — the client resumes instead of restarting the file | S |
| **0d** | `GET /api/upload/token/exists?video_id=<hex>` | Skip duplicates before transferring. Matters most for backfill, where bulk 409s are the expected case. Also the precondition for delete-after-upload. | S |

0a should land regardless of whether Firesync ever ships. 0d gates the backfill phase being
pleasant rather than possible.

---

## Phase 1 — Skeleton and connection

Scaffold Tauri v2 + React + TS. Config file, keychain, and the two discovery endpoints. The
Connect screen from the mockups, working against a real instance.

**Build**
- `create-tauri-app`, plugins: `store`, `single-instance`, `autostart`, `notification`,
  `updater`, `opener`, `log`.
- `config.rs` — settings and folder rules, serialised beside the app data dir.
- `secrets.rs` — token via `keyring`. It never touches the config file.
- `api/client.rs` — base URL normalisation (trailing slash, scheme), `Authorization: Bearer`.
- `api/discovery.rs` — `GET /api/upload/token` for validation, `GET /api/upload/token/options`
  for folders and games. Cache `/options` and refresh on window open.
- Connect view + the Settings connection panel.

**Done when**
- [ ] URL + token → username, default folder, images flag, extension lists on screen.
- [ ] A bad token gives a specific message, not "request failed".
- [ ] `/options` populates real folder and game pickers; the folder list switches between
      `folders.video` and `folders.image`.
- [ ] Relaunch keeps the connection; the token is in the keychain and appears nowhere on disk.
- [ ] Folder field rejects `/` with an inline note, since the server flattens it to `-`.

---

## Phase 2 — Ledger and watcher, with nothing uploading

The highest-risk phase, and it deserves to be provable before anything sends a byte. Build a
debug view that lists every file the watcher has seen and the verdict it reached, with the
reason. Ship it hidden behind a dev flag; it stays useful forever.

**Build**
- `ledger/` — rusqlite, migration runner, the `files` table from STACK.md §2.
- Baseline scan: adding a folder inserts everything already present as `baseline`.
- `watcher/mod.rs` — `notify` + `notify-debouncer-full`, one watcher per folder, recursive
  when the folder says so.
- `watcher/settle.rs` — the gate: debounce 2s → size stable across 3 × 2s polls → on Windows
  a `CreateFile` share-mode-0 probe, on Linux prefer `IN_CLOSE_WRITE` and skip the polling.
  Rename-into-folder counts as a create; temp extensions never queue.
- `queue/rules.rs` — extension allowlist (from `/options`), min/max size, subfolder toggle.

**Test harness worth writing here**, because you will use it every phase after: a script that
creates a file and grows it in chunks over ~30s, to exercise the settle gate without needing
OBS running.

**Done when**
- [ ] A growing file is not queued until it stops growing — verified with the harness and once
      for real with OBS or ShadowPlay.
- [ ] A recorder that writes `foo.tmp` then renames to `foo.mp4` is caught, once, on the rename.
- [ ] A 2 MB file under a 5 MB floor lands as `skipped` with the reason stored.
- [ ] Adding a folder with 200 existing files queues none of them.
- [ ] App restart re-queues nothing and duplicates no rows.
- [ ] Exhausting `fs.inotify.max_user_watches` surfaces a real message rather than silently
      watching nothing.

---

## Phase 3 — Single-shot upload and the retry state machine

The first phase that produces a working product.

**Build**
- `api/upload.rs` — streaming multipart via `reqwest`, no whole-file buffering.
- `queue/mod.rs` — N workers (default 2), pause/resume per folder and globally.
- `queue/retry.rs` — the classification table from STACK.md §4. Exponential backoff with
  jitter, 8 attempts, 15 min cap.
- Folders and Activity views wired to live core events.

**Done when** — every row of the taxonomy produced deliberately and handled correctly:
- [ ] `201` → `done`, `remote_url` stored.
- [ ] `409` → `duplicate`, no notification, never retried.
- [ ] `400` unknown game → `failed`, surfaced as "fix folder settings", not retried.
- [ ] `413` / unsupported type / `503` images-off → permanent, with the right reason.
- [ ] `401` → every queue pauses, one notification, no retry storm and no throttle trip.
- [ ] `429` → honours `Retry-After` exactly.
- [ ] Network pulled mid-upload → backs off, then completes on its own.

---

## Phase 4 — Chunked upload and resume

The phase most likely to hide a bug that only appears in the field. Budget real time for it.

**Build**
- Chunked path above 200 MB, 32 MB chunks, sequential within a file (parallelism across files
  only — two chunks racing to be last both reassemble and the loser 500s).
- `checkSum` minted once per queue item and persisted; `chunks_done` updated on each ACK.
- **Every chunk carries identical `folder`/`game`/`title`/`tag_ids`.** A differing `folder`
  scatters part files and strands the set at 202 forever.
- Exhaustion guard: all indices sent, no `201`/`409` → the server lost the set. New `checkSum`,
  start over.
- If the server sends an informative 202 (Phase 0b), detect the shortfall immediately instead.
- `500 File size mismatch` → full restart, not a chunk retry; the parts were consumed.

**Done when**
- [ ] A 3 GB file uploads and appears in the library.
- [ ] Kill the app mid-file, relaunch → resumes at the correct index, does not re-send.
- [ ] **Restart Fireshare mid-file → the client notices the swept set and restarts the file
      cleanly.** Without Phase 0c this is the scenario that hangs; it must be scripted, not
      tested by hand once.
- [ ] Pause mid-file, resume an hour later → completes.
- [ ] Two large files queued → they upload one after another, not interleaved chunk-wise.

---

## Phase 5 — Tray, notifications, lifecycle

**Build**
- `tray.rs` — status line, progress, pause all, open, quit. Prefer `tray-icon`'s `ksni`
  feature on Linux so the `.deb`/`.rpm` do not depend on libayatana-appindicator.
- `notify.rs` — per-event toggles, burst grouping, suppression while a game holds the screen
  (foreground-window check on Windows; on Linux, best-effort via the active window's fullscreen
  state, and say so in the UI rather than pretending it is exact).
- Autostart, start-in-tray, single-instance, close-to-tray.

**Done when**
- [ ] Window closed → uploads continue, tray still reports progress.
- [ ] Second launch focuses the running instance instead of starting a second one.
- [ ] Notifications off → genuinely silent.
- [ ] Fullscreen game → held, then delivered as one summary on alt-tab.
- [ ] Tray appears on Windows 11, KDE, and GNOME-with-AppIndicator; the README states the GNOME
      extension requirement plainly.

---

## Phase 6 — Backfill picker

The existing-files dialog from the mockups: list, filter by the folder's own rules, select,
bulk `baseline → queued`.

Much better with Phase 0d — without it, a folder you already uploaded transfers in full and
409s file by file. If 0d has not landed, default the filter to "files that match this folder's
rules" and warn on selections over some size.

**Done when**
- [ ] 200 files selectable and queued without freezing the UI.
- [ ] Files already in the library are shown as such and not selectable.
- [ ] Files excluded by size or type are shown greyed with the reason.

---

## Phase 7 — Updater and release pipeline

**Build**
- minisign keypair. Private key in repo secrets, public key in `tauri.conf.json`.
  **Back the private key up somewhere that is not the repo** — losing it means no existing
  install can ever update again.
- `.github/workflows/release.yml` on tag: build NSIS + MSI + deb + rpm + AppImage, upload with
  a signed `latest.json`.
- In-app: check on launch and daily, stable/pre-release channels, auto-install toggle,
  and never restart while an upload is in flight.

**Done when**
- [ ] Install 1.0.0, publish 1.0.1, the app offers and installs it — verified on Windows and on
      at least one Linux bundle type.
- [ ] Auto-install off → it only notifies.
- [ ] An update arriving mid-upload waits.
- [ ] README says plainly that installers are unsigned and SmartScreen will warn, until/unless
      a cert is bought.

---

## Phase 8 — Hardening before 1.0

- Log rotation, and a "copy diagnostics" button that gathers version, instance URL (token
  redacted), queue counts and the last N log lines.
- Network shares and FUSE mounts: inotify does not work there. Detect and fall back to polling,
  or refuse the folder with an honest message.
- Large-queue behaviour: 1000+ files should not stall the UI or the core.
- First-run on a machine with no keychain daemon (some minimal Linux setups) — fail with a
  message, not a panic.

---

## Sequencing notes

Phases 1→4 are strictly ordered; each depends on the one before. 5, 6 and 7 are independent of
each other and can be reordered to taste — though 7 is worth doing before you have users, since
shipping an app that cannot update itself means every later fix needs a manual reinstall.

The two places to slow down are **Phase 2's settle gate** and **Phase 4's resume**. Both fail in
ways that look like "it works on my machine": the gate only misbehaves with a real recorder
writing a real file, and resume only misbehaves when something restarts at the wrong moment.
Both need the scripted tests listed above, not a manual pass.

---

## Open questions

Carried forward from STACK.md §7, plus what this plan raises:

- **Phase 0 ordering** — is 0a going into Fireshare now, or should Firesync assume it never
  lands? The plan works either way; 0a is the one I would not leave undone.
- **Chunk threshold** — 200 MB is a guess. Worth measuring against a real instance once Phase 3
  exists: if single-shot at 500 MB is reliable over your WAN, raise it and keep one code path
  warm.
- **Pre-release channel** — worth having from 1.0, or added when there is someone to test it?
