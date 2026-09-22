import { useCallback, useEffect, useState } from 'react'
import { open } from '@tauri-apps/plugin-dialog'
import { listen } from '@tauri-apps/api/event'
import {
  activity,
  asAppError,
  folders as foldersApi,
  queue as queueApi,
  type Decision,
  type FileRow,
  type FolderSummary,
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

interface Props {
  options: UploadOptions | null
}

export function Folders({ options }: Props) {
  const [list, setList] = useState<FolderSummary[]>([])
  const [decisions, setDecisions] = useState<Decision[]>([])
  const [recent, setRecent] = useState<FileRow[]>([])
  const [problems, setProblems] = useState<string[]>([])
  const [status, setStatus] = useState<QueueStatus | null>(null)
  const [uploads, setUploads] = useState<UploadEvent[]>([])
  const [error, setError] = useState<string | null>(null)
  const [adding, setAdding] = useState(false)

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

  // Decisions are pushed, not polled: a clip can settle minutes after the write
  // that triggered the wait, long after any request the UI made would return.
  useEffect(() => {
    const stop = listen<Decision>('firesync://decision', (event) => {
      setDecisions((prev) => [event.payload, ...prev].slice(0, 50))
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
      setUploads((prev) => [event.payload, ...prev].slice(0, 50))
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

  async function addFolder() {
    setError(null)
    const picked = await open({ directory: true, multiple: false, title: 'Watch a folder' })
    if (typeof picked !== 'string') return
    setAdding(true)
    try {
      await foldersApi.add({
        path: picked,
        includeSubfolders: false,
        media: ['video'] as MediaKind[],
        destFolder: options?.default_folder ?? null,
        minSizeBytes: 5 * MB,
        uploadExisting: false,
      })
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
        <button type="button" className="btn btn--primary" onClick={addFolder} disabled={adding}>
          {adding ? 'Adding…' : 'Add folder'}
        </button>
      </header>

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

            <div className="card__foot">
              <span>
                <strong>{countOf(folder, 'queued')}</strong> queued
              </span>
              <span>
                <strong>{countOf(folder, 'skipped')}</strong> skipped
              </span>
              <span>
                <strong>{folder.baselineCount}</strong> already here
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
        {decisions.length === 0 && recent.length === 0 ? (
          <p className="panel__empty">
            Nothing yet. Drop a file into a watched folder and it appears here once it stops being
            written.
          </p>
        ) : (
          <ul className="rows">
            {uploads.map((u, i) => (
              <li key={`up-${u.id}-${u.state}-${i}`} className="row">
                <span className={`badge badge--${u.state}`}>{u.state}</span>
                <span className="mono row__path">{u.path.split(/[\\/]/).pop()}</span>
                <span className="spacer" />
                {u.landedAs && <span className="row__meta mono">{u.landedAs}</span>}
                <span className="row__meta">{humanSize(u.size)}</span>
                {u.reason && <span className="row__reason">{u.reason}</span>}
              </li>
            ))}
            {decisions.map((d, i) => (
              <li key={`${d.path}-${d.at}-${i}`} className="row">
                <span className={`badge badge--${d.outcome}`}>{d.outcome}</span>
                <span className="mono row__path">{d.path.split(/[\\/]/).pop()}</span>
                <span className="spacer" />
                <span className="row__meta">{humanSize(d.size)}</span>
                {d.reason && <span className="row__reason">{d.reason}</span>}
              </li>
            ))}
            {recent
              .filter((r) => !decisions.some((d) => d.path === r.path))
              .slice(0, 20)
              .map((r) => (
                <li key={r.id} className="row row--dim">
                  <span className={`badge badge--${r.state}`}>{r.state}</span>
                  <span className="mono row__path">{r.path.split(/[\\/]/).pop()}</span>
                  <span className="spacer" />
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
