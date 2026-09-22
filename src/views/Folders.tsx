import { useCallback, useEffect, useState } from 'react'
import { open } from '@tauri-apps/plugin-dialog'
import { listen } from '@tauri-apps/api/event'
import {
  activity,
  asAppError,
  folders as foldersApi,
  queue as queueApi,
  type FileRow,
  type FolderSummary,
  type AfterUpload,
  type MediaKind,
  type QueueStatus,
  type UploadEvent,
  type UploadOptions,
} from '../lib/ipc'

const MB = 1024 * 1024
const GB = 1024 * MB

function humanSize(bytes: number) {
  if (bytes >= GB) return `${(bytes / GB).toFixed(1)} GB`
  if (bytes >= MB) return `${(bytes / MB).toFixed(1)} MB`
  if (bytes >= 1024) return `${Math.round(bytes / 1024)} KB`
  return `${bytes} bytes`
}

function countOf(folder: FolderSummary, state: string) {
  return folder.counts.find(([s]) => s === state)?.[1] ?? 0
}

function basename(path: string) {
  return path.split(/[\\/]/).pop() ?? path
}

interface Props {
  options: UploadOptions | null
}

export function Folders({ options }: Props) {
  const [list, setList] = useState<FolderSummary[]>([])
  const [recent, setRecent] = useState<FileRow[]>([])
  const [problems, setProblems] = useState<string[]>([])
  const [status, setStatus] = useState<QueueStatus | null>(null)
  const [landings, setLandings] = useState<Record<string, string>>({})
  const [error, setError] = useState<string | null>(null)
  const [adding, setAdding] = useState(false)
  const [composing, setComposing] = useState(false)
  const [draftPath, setDraftPath] = useState('')

  // Same filename in two watched folders is ordinary — a move between them
  // produces exactly that — so each row says which folder it belongs to.
  const folderName = useCallback(
    (id: string) => {
      const folder = list.find((f) => f.id === id)
      return folder ? basename(folder.path) : null
    },
    [list],
  )

  const refresh = useCallback(async () => {
    try {
      setList(await foldersApi.list())
      setRecent(await activity.recent(40))
      setProblems(await foldersApi.problems())
      setStatus(await queueApi.status())
    } catch (e) {
      setError(asAppError(e).message)
    }
  }, [])

  useEffect(() => {
    void refresh()
  }, [refresh])

  // A decision can land minutes after the write that triggered the wait, long
  // after any request the UI made would have returned — so it is pushed. The
  // payload is not rendered directly; the ledger is the one source that knows a
  // file's current state, and this just tells us to read it again.
  useEffect(() => {
    const stop = listen('firesync://decision', () => {
      void refresh()
    })
    return () => {
      void stop.then((fn) => fn())
    }
  }, [refresh])

  // Upload outcomes are pushed as they resolve; a queue that only updated when
  // the window was open would be a queue nobody could trust.
  useEffect(() => {
    const stop = listen<UploadEvent>('firesync://upload', (event) => {
      const { path, landedAs, removedLocal } = event.payload
      const note = [landedAs, removedLocal].filter(Boolean).join(' · ')
      if (note) setLandings((prev) => ({ ...prev, [path]: note }))
      void refresh()
    })
    return () => {
      void stop.then((fn) => fn())
    }
  }, [refresh])

  // A backoff resolves on a timer with no event behind it, so the counts need a
  // slow tick to stay honest while files are waiting.
  useEffect(() => {
    const t = setInterval(() => void refresh(), 5000)
    return () => clearInterval(t)
  }, [refresh])

  async function browse() {
    setError(null)
    try {
      const picked = await open({ directory: true, multiple: false, title: 'Watch a folder' })
      if (typeof picked === 'string') setDraftPath(picked)
    } catch (e) {
      // The picker can be unavailable or restricted — on macOS an unsigned dev
      // build has no TCC identity, so protected folders are simply disabled.
      // Typing a path still works, so say so rather than dead-ending.
      setError(
        `${asAppError(e).message} You can paste the folder path instead.`,
      )
    }
  }

  async function commit() {
    const path = draftPath.trim()
    if (!path) return
    setError(null)
    setAdding(true)
    try {
      await foldersApi.add({
        path,
        includeSubfolders: false,
        media: ['video'] as MediaKind[],
        destFolder: options?.default_folder ?? null,
        minSizeBytes: 5 * MB,
        uploadExisting: false,
      })
      setDraftPath('')
      setComposing(false)
      await refresh()
    } catch (e) {
      setError(asAppError(e).message)
    } finally {
      setAdding(false)
    }
  }

  return (
    <div className="page">
      <header className="page__head">
        <div>
          <h1 className="page__title">Watched folders</h1>
          <p className="page__sub">
            New clips dropped in these folders are picked up on their own. Files already there stay
            put until you ask for them.
          </p>
        </div>
        <button
          type="button"
          className="btn btn--primary"
          onClick={() => setComposing((v) => !v)}
          disabled={adding}
        >
          Add folder
        </button>
      </header>

      {composing && (
        <form
          className="addbar"
          onSubmit={(e) => {
            e.preventDefault()
            void commit()
          }}
        >
          <label className="addbar__label" htmlFor="newpath">
            Folder on this machine
          </label>
          <div className="addbar__row">
            <input
              id="newpath"
              type="text"
              className="addbar__input mono"
              placeholder="/Users/you/Movies/clips"
              spellCheck={false}
              autoFocus
              value={draftPath}
              onChange={(e) => setDraftPath(e.target.value)}
            />
            <button type="button" className="btn btn--ghost" onClick={browse}>
              Browse…
            </button>
            <button type="submit" className="btn btn--primary" disabled={adding || !draftPath.trim()}>
              {adding ? 'Adding…' : 'Watch it'}
            </button>
          </div>
          <span className="addbar__hint">
            Paste a path if the picker will not let you reach the folder — a network share, or a
            location macOS has not granted access to.
          </span>
        </form>
      )}

      {status?.paused && (
        <div className="banner banner--bad banner--row">
          <span>{status.pauseReason ?? 'Uploads are paused.'}</span>
          <span className="spacer" />
          <button
            type="button"
            className="btn btn--ghost btn--sm"
            onClick={() => queueApi.resume().then(refresh)}
          >
            Resume
          </button>
        </div>
      )}

      {status && !status.paused && (status.queued > 0 || status.uploading > 0 || status.failed > 0) && (
        <div className="statusbar">
          <span>
            <strong>{status.uploading}</strong> uploading
          </span>
          <span>
            <strong>{status.queued}</strong> queued
          </span>
          {status.failed > 0 && (
            <span className="statusbar__bad">
              <strong>{status.failed}</strong> need attention
            </span>
          )}
          <span className="spacer" />
          <button
            type="button"
            className="btn btn--ghost btn--sm"
            onClick={() => queueApi.pause().then(refresh)}
          >
            Pause all
          </button>
          {status.failed > 0 && (
            <button
              type="button"
              className="btn btn--primary btn--sm"
              onClick={() => queueApi.retryFailed().then(refresh)}
            >
              Retry failed
            </button>
          )}
        </div>
      )}

      {error && <div className="banner banner--bad">{error}</div>}
      {problems.map((p) => (
        <div key={p} className="banner banner--warn">
          {p}
        </div>
      ))}

      {list.length === 0 && !error && (
        <div className="empty">
          <p>No folders watched yet. Add the folder your recorder writes into.</p>
        </div>
      )}

      <div className="cards">
        {list.map((folder) => (
          <article key={folder.id} className="card">
            <div className="card__head">
              <span className={`dot ${folder.enabled ? 'dot--ok' : 'dot--off'}`} />
              <span className="mono card__path">{folder.path}</span>
              <span className={`pill ${folder.enabled ? 'pill--ok' : ''}`}>
                {folder.enabled ? 'Watching' : 'Paused'}
              </span>
              <span className="spacer" />
              <button
                type="button"
                className="btn btn--ghost btn--sm"
                onClick={() =>
                  foldersApi.setEnabled(folder.id, !folder.enabled).then(refresh).catch(() => {})
                }
              >
                {folder.enabled ? 'Pause' : 'Resume'}
              </button>
              <button
                type="button"
                className="btn btn--ghost btn--sm"
                onClick={() => foldersApi.remove(folder.id).then(refresh).catch(() => {})}
              >
                Remove
              </button>
            </div>

            <div className="chips chips--tight">
              {folder.dest_folder && <span className="tag mono">{folder.dest_folder}</span>}
              {folder.game && <span className="tag tag--game">{folder.game}</span>}
              <span className="tag">{folder.media.includes('image') ? 'Video + images' : 'Video only'}</span>
              {folder.min_size_bytes && (
                <span className="tag">Over {humanSize(folder.min_size_bytes)}</span>
              )}
              {folder.include_subfolders && <span className="tag">Subfolders</span>}
            </div>

            <div className="card__after">
              <label htmlFor={`after-${folder.id}`}>After a successful upload</label>
              <select
                id={`after-${folder.id}`}
                value={folder.after_upload}
                onChange={(e) =>
                  foldersApi
                    .setAfterUpload(folder.id, e.target.value as AfterUpload)
                    .then(refresh)
                    .catch((err) => setError(asAppError(err).message))
                }
              >
                <option value="keep">Keep the local file</option>
                <option value="trash">Move it to the trash</option>
                <option value="delete">Delete it</option>
              </select>
              {folder.after_upload !== 'keep' && (
                <span className="card__after-note">
                  Only once Fireshare confirms it has the file.
                </span>
              )}
            </div>

            <div className="card__foot">
              <span>
                <strong>{countOf(folder, 'done')}</strong> uploaded
              </span>
              <span>
                <strong>{countOf(folder, 'queued')}</strong> queued
              </span>
              <span>
                <strong>{countOf(folder, 'skipped')}</strong> skipped
              </span>
              {countOf(folder, 'failed') > 0 && (
                <span className="card__foot-bad">
                  <strong>{countOf(folder, 'failed')}</strong> need attention
                </span>
              )}
              <span>
                <strong>{folder.presentCount}</strong>{' '}
                {folder.presentCount === 1 ? 'file here' : 'files here'}
              </span>
            </div>
          </article>
        ))}
      </div>

      <section className="panel">
        <h2 className="panel__title">
          Activity
          <span className="panel__hint">
            what the watcher settled on and what the server said — live
          </span>
        </h2>
        {recent.length === 0 ? (
          <p className="panel__empty">
            Nothing yet. Drop a file into a watched folder and it appears here once it stops being
            written.
          </p>
        ) : (
          <ul className="rows">
            {recent.slice(0, 25).map((r) => (
              <li key={r.id} className="row">
                <span className={`badge badge--${r.state}`}>{r.state}</span>
                <span className="mono row__path">{basename(r.path)}</span>
                {folderName(r.folderId) && (
                  <span className="row__folder">{folderName(r.folderId)}</span>
                )}
                <span className="spacer" />
                {landings[r.path] && (
                  <span className="row__meta mono">{landings[r.path]}</span>
                )}
                <span className="row__meta">{humanSize(r.size)}</span>
                {r.reason && <span className="row__reason">{r.reason}</span>}
              </li>
            ))}
          </ul>
        )}
      </section>
    </div>
  )
}
