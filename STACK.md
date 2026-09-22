# Firesync — stack research and architecture notes

A tray companion for [Fireshare](https://github.com/fireshare-app/fireshare) that watches
local folders and pushes new clips and screenshots to your instance using the upload
tokens added in `feat/upload-tokens`.

---

## 1. Recommendation

**Tauri v2 (Rust core + React webview), one repo, GitHub Releases for distribution.**

The deciding argument is the duty cycle. This app is running every hour the machine is on,
and almost all of that time it is doing nothing but holding an inotify/ReadDirectoryChangesW
handle. It shares a machine with the game you are recording. An Electron build of this idles
around 170 MB with a Chromium process tree alive the whole time; the Tauri equivalent idles
around 40 MB, and when the window is closed to the tray the webview is torn down entirely,
leaving only the Rust core resident. For a background uploader aimed at gamers, that is the
whole ballgame.

The cost is Rust. But the Rust surface here is small and it is exactly the part that has to be
correct anyway — the watcher, the queue, the retry state machine, the SQLite ledger. The React
half stays React, which is the same thing the Fireshare client already is, so the UI work
transfers directly.

### The comparison, honestly

| | Tauri v2 | Electron | Wails v3 (Go) | Avalonia (C#) |
|---|---|---|---|---|
| Installed size | ~10–15 MB | ~180–250 MB | ~15–25 MB | ~40–70 MB |
| Idle RAM (tray, window closed) | ~40 MB | ~170 MB | ~50 MB | ~60 MB |
| UI language | React (kept) | React (kept) | React (kept) | XAML |
| Backend language | Rust | Node | Go | C# |
| Tray | `tauri::tray` | built in | built in | built in |
| Auto-update | `tauri-plugin-updater` | `electron-updater` | DIY | Velopack |
| Windows installers | MSI + NSIS | NSIS | NSIS | MSI |
| Linux installers | deb + rpm + AppImage | deb + rpm + AppImage | deb | deb |
| Main risk | Rust ramp-up; WebKitGTK quirks | resource weight | smaller GUI ecosystem | unfamiliar stack |

Electron is the sane fallback if the Rust ramp turns out to be the thing that stalls the
project. `electron-updater` is still the most battle-tested desktop update path in existence,
and nothing else about the design changes — the architecture below ports over with `chokidar`
in place of `notify` and `better-sqlite3` in place of `rusqlite`. Ship weight is the only thing
you give up, and it is a big only.

Wails and Avalonia are both credible and both lose on ecosystem fit: you would be learning a
new stack to land in roughly the same place Tauri gets you, with a hand-rolled updater.

### Crates and plugins

Official Tauri plugins cover: `updater`, `notification`, `autostart`, `single-instance`,
`store`, `sql`, `dialog`, `opener`, `log`, `positioner`. There is **no** official file-watching
plugin, which is fine — the watcher belongs in the always-alive Rust core, not behind a JS
bridge that dies with the window.

- `notify` + `notify-debouncer-full` — cross-platform FS events
- `reqwest` (`multipart`, `stream`) — streaming uploads, no whole-file buffering
- `rusqlite` (bundled SQLite) — the ledger
- `xxhash-rust` (`xxh3`) — matches Fireshare's own `video_id`, see §3
- `keyring` — the token, in the OS credential store
- `tokio` — the queue runtime

Linux tray note: the default backend wants `libayatana-appindicator3` or `libappindicator3`
present at runtime. `tray-icon` 0.25+ ships a `ksni` feature — a pure-Rust StatusNotifierItem
implementation with no appindicator dependency at all. Prefer `ksni` and the `.deb`/`.rpm`
dependency list gets much shorter. GNOME still needs the AppIndicator extension to *show* a
tray at all; that is a GNOME fact, not something the app can fix, and it belongs in the README.

---

## 2. Architecture

```
┌─ Rust core (always alive, survives window close) ────────────┐
│                                                              │
│  Watcher      notify → debounce 2s → stability gate → enqueue│
│  Ledger       SQLite: every file ever seen, and its verdict  │
│  Queue        N workers (default 2), per-folder rules applied│
│  Uploader     reqwest streaming multipart → /api/upload/token│
│  Retry        classified backoff, see §4                     │
│  Tray         status, pause, quick actions                   │
│  Notifier     native toasts, suppressible                    │
│  Updater      signed latest.json from GitHub Releases        │
└──────────────────────────────┬───────────────────────────────┘
                               │ tauri commands + events
┌──────────────────────────────┴───────────────────────────────┐
│  React webview (created on show, destroyed on close)         │
│  Folders · Activity · Settings                               │
└──────────────────────────────────────────────────────────────┘
```

The window is a view onto core state, never the owner of it. Close the window mid-upload and
nothing pauses.

### The ledger

One row per file path ever observed. This is what makes "don't re-upload" and "existing files
are not uploaded unless asked" both fall out of the same mechanism.

```sql
CREATE TABLE files (
  id           INTEGER PRIMARY KEY,
  folder_id    INTEGER NOT NULL,
  path         TEXT NOT NULL,
  size         INTEGER NOT NULL,
  mtime        INTEGER NOT NULL,
  content_hash TEXT,            -- xxh3_128 of first 16 MB, see §3
  state        TEXT NOT NULL,   -- baseline|queued|uploading|done|duplicate|skipped|failed
  reason       TEXT,            -- why skipped or failed, shown in the UI
  attempts     INTEGER DEFAULT 0,
  next_try_at  INTEGER,
  remote_url   TEXT,
  check_sum    TEXT,            -- chunk group id, minted once and persisted
  chunks_total INTEGER,
  chunks_done  TEXT,            -- ACKed indices, so a restart resumes mid-file
  UNIQUE(folder_id, path)
);
```

`check_sum`, `chunks_total` and `chunks_done` are what make resume work at all — the server has
no route that reports which parts it holds, so this table is the only record of progress. See §3.

When a folder is added, every file already in it is inserted as `baseline`. The watcher only
ever queues files it has not seen. "Upload existing files" is a bulk `baseline → queued`
update over a user-chosen subset. Identity is `(path, size, mtime)`, not path alone, so a
recorder that overwrites a filename does not get silently swallowed.

### Detecting that a file is finished

This is the single thing most likely to produce a broken app, and it deserves to be designed
rather than discovered. OBS, ShadowPlay, Medal and friends all create the file first and write
into it for minutes afterward. A naive `on_create → upload` ships truncated clips.

The gate, in order:

1. Debounce the event (2s) — recorders emit a storm of writes.
2. Poll size every 2s; require it unchanged across 3 consecutive polls.
3. On Windows, additionally try `CreateFile` with `dwShareMode = 0`. Success means no other
   process holds the handle. This is the only *real* signal available on Windows; size
   stability alone can be fooled by a stalled encoder.
4. On Linux, prefer the `IN_CLOSE_WRITE` inotify event, which `notify` surfaces — it means
   exactly "the writer closed this file" and needs no polling at all.
5. Watch for rename-on-close: several recorders write `foo.tmp` and rename to `foo.mp4` when
   done. Treat a rename into the folder as a create, and never queue on the temp extension.

Also worth knowing: inotify does not work on network shares or most FUSE mounts, and the
per-user watch limit (`fs.inotify.max_user_watches`, often 8192) is easy to exhaust with a
recursive watch on a deep tree. Both should surface as a real message in the UI rather than
silently watching nothing.

---

## 3. What the API gives you (as of `fbbc646`)

Read of `docs/UploadTokens.md`, `app/server/fireshare/api/upload_tokens.py` and
`app/server/fireshare/api/upload.py` on `feat/upload-tokens`:

`POST /api/upload/token`, bearer header, multipart, one file per request. Fields: `file`,
`title`, `folder`, `game_id` or `game` (name, matched case-insensitively against existing
games — an unknown name is a 400, it never creates one), `tag_ids`. Returns `201 {status:
"accepted", media_type, filename, folder}`. `GET` on the same route validates the token and
returns username, default folder, whether images are on, and both extension allowlists — which
is enough to drive the entire setup screen from one request.

The token route deliberately reuses the browser upload's helpers, so filename sanitising,
folder containment, the extension allowlist, the demo size cap, duplicate rejection and
uploader attribution are literally the same code. That is good news for the client: behaviour
will not drift.

`POST /api/upload/token/chunked` takes one chunk per request in any order, grouped by a
caller-chosen `checkSum`, with `chunkPart`, `totalChunks`, `fileName` and `fileSize` alongside
the same metadata fields. Every request but the last answers `202`; whichever completes the set
reassembles, verifies the result against the declared `fileSize`, and answers exactly as the
single-shot route does. `MAX_CHUNKS` is 20000, so chunk size is a free choice.

`GET /api/upload/token/options` returns `default_folder`, `folders: {video, image}` and the
full game list. Listing games directly rather than through `/api/games` is the right call — the
public route hides games with nothing linked yet, which are exactly the ones an upload might be
first to use, and name resolution already accepts them. A picker built on `/api/games` would
have silently omitted valid choices.

Both upload routes now funnel through `_prepare_upload` / `_finish_upload`, so what they
accept, where they file it and how they attribute it cannot drift. `_positive_int` also means
malformed chunk metadata answers 400 rather than the 500 the browser-facing chunked route gives.

That closes the two gaps that shaped the client. What remains is one gap and four contract
details the client has to get right.

### The chunked contract, from the client side

These follow from `_prepare_upload` running per request, and none of them are obvious from the
route signature.

**Send `folder`, `game`, `title` and `tag_ids` on every chunk, identical each time.**
`_prepare_upload` is called on each request and derives `upload_directory` from `folder` — and
that is where the `.partNNNN` files are written. A single chunk carrying a different folder
drops its part in a different directory, the completing request never sees a full set, and the
upload hangs at 202 forever with no error. This is the sharpest edge in the whole API.

**Upload one file's chunks sequentially.** Chunks may arrive in any order, but if two requests
both observe the full set they will both try to reassemble; the loser finds the parts already
consumed and returns 500. Parallelism belongs across files, not within one. This also gives you
a truthful progress bar for free.

**Resume is your bookkeeping, not the server's.** Part files persist on disk, so re-sending only
the missing indices works — but there is no route that reports which parts the server holds. The
ledger has to remember the `checkSum` and which indices were ACKed. Use a UUID minted once per
queue item and persisted, not a content hash: deterministic across restarts, and no collision if
the same file is queued to two folders.

**A 500 "File size mismatch" is a start-over, not a chunk retry.** Reassembly deletes each part
as it consumes it, so on a size mismatch there is nothing left to resume from. Reset the item's
chunk progress and mint a new `checkSum`.

### The one remaining gap

**The duplicate check still happens after the whole file is on disk.** `_finish_upload` calls
`_reject_duplicate(save_path)`, which hashes the *reassembled* file. So a 5 GB clip you already
have is transferred in full — now across ~55 requests instead of one — and then thrown away with
a 409. The hash is `xxh3_128` over the first 16 MB (`util.video_id`), which the client can
compute locally in milliseconds. A `GET /api/upload/token/exists?video_id=<hex>` doing one
`Video.query.filter_by(video_id=...)` would let Firesync skip duplicates before sending a byte.

This matters most in the manual backfill flow, where the entire point is re-scanning a folder
you may have already uploaded — exactly the case where the 409s arrive in bulk and each one
costs a full transfer.

### Three smaller things

**Destination folders are a single level.** `sanitize_upload_folder` replaces `/` and `\` with
`-`, so `uploads/clips` is filed as `uploads-clips`. The folder picker should offer the flat
names `/options` returns and not imply nesting. (The mockups originally showed `uploads/clips`;
they now show `clips`.)

**`folders.video` and `folders.image` are separate lists**, and `_list_subfolders` returns only
immediate children of each media root. The picker should switch lists based on whether the
watched folder is set to videos or images — a video folder name is not necessarily a valid image
folder name. The default folder is appended to both lists whether or not it exists on disk yet.

**A Fireshare restart wipes in-flight chunk sets, and the client must notice.** `__init__.py`
sweeps every `*.part[0-9][0-9][0-9][0-9]` under the video *and* image roots at startup —
unconditionally, with no age check. (A `/dev/shm` sentinel means a worker respawn skips the
sweep and leaves parts alone; a real restart or reboot does not.)

So a container update mid-upload leaves Firesync holding a ledger that says "chunks 1–18 ACKed"
against a server that has none of them. The client sends 19–34, every one answers 202 because
the set is never complete, and **the upload hangs at 202 forever with no error**.

The defence is cheap and the condition is crisp: *if every index in `1..totalChunks` has been
sent and none of them returned 201 or 409, the server has lost the set.* Discard the
`checkSum`, reset `chunks_done`, and re-send from scratch. Worth building in from the start —
Firesync runs for weeks at a time and the server it talks to will be restarted underneath it.

**nginx still buffers.** `/api/upload/token/chunked` falls under `location ~ /api/.*$`, which
sets `client_max_body_size 0` but leaves `proxy_request_buffering` at its default (on) — so
nginx spools each chunk to its temp directory before Flask sees it. The dedicated
`/api/uploadChunked` block already turns that off. A `location ^~ /api/upload/token` block with
`proxy_request_buffering off` avoids an extra disk write per chunk.

---

## 4. Retry, classified

Blind exponential backoff is wrong here, because several of these failures will never succeed
no matter how long you wait, and retrying them looks like a hung app.

| Response | Verdict | What the app does |
|---|---|---|
| `201` | done | record `remote_url`, notify |
| `409` duplicate | **done, not a failure** | mark `duplicate`, no notification, never retry |
| `400` unsupported type | permanent | mark `skipped`, no notification |
| `400` unknown game | permanent, **user-fixable** | mark `failed`, surface "fix folder settings" |
| `413` over limit | permanent | mark `skipped` with the size reason |
| `503` images off | permanent for images | pause image uploads for that folder, notify once |
| `401` | **stop everything** | pause all queues, notify "reconnect", do not burn retries |
| `429` | retry | honour `Retry-After` exactly; the server's throttle is 30 failures / 300s per IP |
| `202` (chunked) | in progress | record the ACKed index, send the next chunk |
| `500` size mismatch (chunked) | retry **whole file** | parts were consumed; reset progress, new `checkSum` |
| `5xx`, timeout, connection reset | retry | exponential backoff with jitter, 8 attempts, cap 15 min |

On a chunked upload, a retryable failure resumes from the last ACKed index rather than
restarting — that is the whole point of the endpoint, and it only works because the ledger
remembers. The one exception is the size-mismatch 500 above.

Chunk size is a free parameter (`MAX_CHUNKS` is 20000, so even 8 MB chunks cover 160 GB). The
browser client uses 90 MB; for a home upstream that is a lot to lose to one dropped connection.
**32 MB is a better default here** — retries stay cheap, and the per-request overhead is
negligible next to the transfer.

401 deserves the special case: the token is revoked or the account lost `upload` permission,
and the server re-reads the account on every request. Hammering it just trips the throttle and
locks the address out for five minutes.

---

## 5. Updates and distribution

The brief says "GitHub packages" — worth naming the distinction, because it changes the setup:
**GitHub Packages** hosts npm/container/Maven artifacts; **GitHub Releases** hosts installers,
and is what every desktop updater actually reads. Releases is what you want.

`tauri-plugin-updater` 2.10+ (Feb 2026) covers all five bundle types — NSIS, MSI, deb, rpm,
AppImage — which removes the old "Linux users get AppImage or nothing" caveat. The flow:

1. `tauri-action` builds on a tag and uploads installers plus a signed `latest.json`.
2. The app polls that `latest.json` on launch and daily.
3. Signature verification uses a minisign keypair; private key in repo secrets, public key in
   `tauri.conf.json`. **Losing the private key means no existing install can ever update again**
   — back it up somewhere that is not the repo.
4. Auto-install is a user setting. When on, download in the background, apply on next launch,
   and never restart while an upload is in flight (the mockup says this out loud).

Two things that will bite on first release:

- **Windows SmartScreen** flags unsigned installers with a full-screen warning until the binary
  builds reputation. An OV cert is ~$200–400/yr, EV more. Shipping unsigned is survivable for a
  self-hosted-adjacent audience but the README should say so plainly.
- **Linux deb/rpm self-update** works via the plugin but fights the system package manager if
  the user installed from a repo. AppImage is the cleaner self-updating path; ship deb/rpm for
  people who would rather their package manager own it, and let those installs update the
  normal way.

---

## 6. Suggested build order

1. Rust core: config, keyring, `GET /api/upload/token` validation, `/options` fetch. Connect
   screen and the folder/game pickers.
2. Ledger + baseline scan + watcher with the full stability gate. No uploading yet — just prove
   the app notices the right files and ignores the wrong ones.
3. Single-shot uploader + classified retry. This is a working product.
4. Chunked uploader above a size threshold (~200 MB), with resume from the ledger.
5. Tray, notifications, autostart, single-instance.
6. Manual backfill picker.
7. Updater + release pipeline.

Steps 2–4 are the risky ones and they are all in the core. Get them right against a real
instance before any UI polish. Step 4 in particular deserves a deliberate test: kill the app
mid-file, restart, confirm it resumes from the right index rather than re-sending everything.

---

## 7. Open questions

- **Pre-transfer duplicate check**: the last real gap (§3). Cheap to add server-side, and it is
  what makes the backfill flow tolerable. Worth landing before the backfill UI ships.
- **Chunk threshold**: always chunk, or only above a size? Always-chunk is less code and one
  path to test; single-shot is one request instead of several for a 40 MB screenshot burst.
  Leaning always-chunk above ~200 MB, single-shot below.
- **Title templates**: the API takes a `title`. Worth a per-folder pattern
  (`{game} — {date}`), or is filename-derived enough for v1?
- **Delete-after-upload**: tempting for capture folders that fill a drive, and dangerous. If it
  ships, it should require the upload to be confirmed present server-side first, which needs the
  `video_id` lookup above.
- **Tags**: `tag_ids` is accepted, and `/options` returns folders and games but not tags — same
  shape of problem, smaller stakes.
