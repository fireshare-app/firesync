import { useCallback, useEffect, useState } from 'react'
import { listen } from '@tauri-apps/api/event'
import {
  PauseIcon,
  PencilIcon,
  PlayIcon,
  PlusIcon,
  TrashIcon,
  UploadIcon,
  WarningTriangleIcon,
} from '../components/Icons'
import { Backlog } from './Backlog'
import { FolderDialog } from './FolderDialog'
import {
  asAppError,
  folders as foldersApi,
  type FolderSummary,
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

function ago(unixSeconds: number) {
  if (!unixSeconds) return null
  const seconds = Math.max(0, Math.floor(Date.now() / 1000) - unixSeconds)
  if (seconds < 90) return 'just now'
  const minutes = Math.floor(seconds / 60)
  if (minutes < 60) return `${minutes} minutes ago`
  const hours = Math.floor(minutes / 60)
  if (hours < 24) return `${hours} hour${hours === 1 ? '' : 's'} ago`
  return `${Math.floor(hours / 24)} days ago`
}

function countOf(folder: FolderSummary, state: string) {
  return folder.counts.find(([s]) => s === state)?.[1] ?? 0
}

function sizeRule(folder: FolderSummary) {
  const min = folder.min_size_bytes
  const max = folder.max_size_bytes
  if (min && max) return `${humanSize(min)} – ${humanSize(max)}`
  if (min) return `Over ${humanSize(min)}`
  if (max) return `Under ${humanSize(max)}`
  return null
}

interface Props {
  options: UploadOptions | null
  onChanged?: () => void
}

/**
 * An upload in flight, keyed by the file's path.
 *
 * Keyed by path rather than by folder because the queue sends several files at
 * once: a per-folder key meant each tick overwrote the last, and the card
 * flickered between whichever files happened to report most recently.
 */
interface InFlight {
  folderId: string
  name: string
  fraction: number
}

export function Folders({ options, onChanged }: Props) {
  const [list, setList] = useState<FolderSummary[]>([])
  const [problems, setProblems] = useState<string[]>([])
  const [error, setError] = useState<string | null>(null)
  const [editing, setEditing] = useState<FolderSummary | 'new' | null>(null)
  const [backlogFor, setBacklogFor] = useState<FolderSummary | null>(null)
  const [inFlight, setInFlight] = useState<Record<string, InFlight>>({})

  const refresh = useCallback(async () => {
    try {
      setList(await foldersApi.list())
      setProblems(await foldersApi.problems())
      onChanged?.()
    } catch (e) {
      setError(asAppError(e).message)
    }
  }, [onChanged])

  useEffect(() => {
    void refresh()
  }, [refresh])

  // The design shows the file being sent on the card it came from, so progress
  // is tracked per folder here rather than only in the activity feed.
  useEffect(() => {
    const stop = listen<UploadEvent>('firesync://upload', (event) => {
      const { path, state, sent, size } = event.payload
      const folder = list.find((f) => path.startsWith(f.path))
      if (!folder) return
      if (state === 'uploading') {
        setInFlight((prev) => ({
          ...prev,
          [path]: {
            folderId: folder.id,
            name: path.split(/[\\/]/).pop() ?? path,
            fraction: size > 0 ? sent / size : 0,
          },
        }))
        return
      }
      setInFlight((prev) => {
        const next = { ...prev }
        delete next[path]
        return next
      })
      void refresh()
    })
    return () => {
      void stop.then((fn) => fn())
    }
  }, [list, refresh])

  useEffect(() => {
    const stop = listen('firesync://decision', () => void refresh())
    return () => {
      void stop.then((fn) => fn())
    }
  }, [refresh])

  return (
    <div className="page">
      <header className="page__head">
        <div className="page__headtext">
          <h1 className="page__title">Watched folders</h1>
          <p className="page__sub">
            New clips and screenshots dropped in these folders get uploaded on their own.
          </p>
        </div>
        <button
          type="button"
          className="btn btn--primary btn--icon"
          onClick={() => setEditing('new')}
        >
          <PlusIcon />
          Add folder
        </button>
      </header>

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
        {list.map((folder) => {
          const flights = Object.entries(inFlight)
            .filter(([, f]) => f.folderId === folder.id)
            .sort(([a], [b]) => a.localeCompare(b))
          const live = flights.length > 0
          const failed = countOf(folder, 'failed')
          const lastUpload = ago(folder.lastUploadAt ?? 0)
          return (
            <article key={folder.id} className={`fcard ${live ? 'fcard--live' : ''}`}>
              <div className="fcard__head">
                <span
                  className={`dot ${!folder.enabled ? 'dot--off' : live ? 'dot--live' : 'dot--ok'}`}
                />
                <span className="mono fcard__path">{folder.path}</span>
                <span className={`pill ${!folder.enabled ? '' : live ? 'pill--live' : 'pill--ok'}`}>
                  {!folder.enabled ? 'Paused' : live ? 'Uploading' : 'Watching'}
                </span>
                <span className="spacer" />
                <button
                  type="button"
                  className="iconbtn"
                  aria-label={folder.enabled ? 'Pause this folder' : 'Resume this folder'}
                  onClick={() =>
                    foldersApi.setEnabled(folder.id, !folder.enabled).then(refresh).catch(() => {})
                  }
                >
                  {folder.enabled ? <PauseIcon /> : <PlayIcon />}
                </button>
                <button
                  type="button"
                  className="iconbtn"
                  aria-label="Folder settings"
                  onClick={() => setEditing(folder)}
                >
                  <PencilIcon />
                </button>
                {folder.presentCount > 0 && (
                  <button
                    type="button"
                    className="iconbtn"
                    aria-label="Upload files already in this folder"
                    onClick={() => setBacklogFor(folder)}
                  >
                    <UploadIcon />
                  </button>
                )}
                <button
                  type="button"
                  className="iconbtn"
                  aria-label="Stop watching this folder"
                  onClick={() => foldersApi.remove(folder.id).then(refresh).catch(() => {})}
                >
                  <TrashIcon />
                </button>
              </div>

              <div className="chips chips--tight">
                {folder.dest_folder && <span className="tag mono">{folder.dest_folder}</span>}
                {folder.game && <span className="tag tag--game">{folder.game}</span>}
                <span className="tag">
                  {folder.media.includes('image') && folder.media.includes('video')
                    ? 'Video + images'
                    : folder.media.includes('image')
                      ? 'Images only'
                      : 'Video only'}
                </span>
                {sizeRule(folder) && <span className="tag">{sizeRule(folder)}</span>}
                {folder.include_subfolders && <span className="tag">Subfolders</span>}
                {folder.after_upload !== 'keep' && (
                  <span className="tag">
                    {folder.after_upload === 'trash' ? 'Trash after upload' : 'Delete after upload'}
                  </span>
                )}
              </div>

              {flights.length > 0 ? (
                <div className="fcard__flights">
                  {flights.map(([path, flight]) => {
                    const pct = Math.min(100, Math.round(flight.fraction * 100))
                    return (
                      <div key={path} className="fcard__progress">
                        <div className="fcard__progresshead">
                          <span className="mono">{flight.name}</span>
                          <span className="spacer" />
                          <span>{pct}%</span>
                        </div>
                        <div className="fcard__bar">
                          <div className="fcard__bar-fill" style={{ width: `${pct}%` }} />
                        </div>
                      </div>
                    )
                  })}
                </div>
              ) : failed > 0 ? (
                <div className="fcard__warn">
                  <WarningTriangleIcon />
                  <span>
                    {failed} upload{failed === 1 ? '' : 's'} need{failed === 1 ? 's' : ''} attention.
                  </span>
                </div>
              ) : null}

              <div className="fcard__foot">
                <span>
                  <strong>{countOf(folder, 'done')}</strong> uploaded
                </span>
                <span>
                  <strong>{countOf(folder, 'queued')}</strong> queued
                </span>
                <span>
                  <strong>{countOf(folder, 'skipped')}</strong> skipped
                </span>
                <span>
                  <strong>{folder.presentCount}</strong> here now
                </span>
                <span className="spacer" />
                {lastUpload && <span>Last upload {lastUpload}</span>}
              </div>
            </article>
          )
        })}
      </div>

      {editing && (
        <FolderDialog
          folder={editing === 'new' ? undefined : editing}
          options={options}
          onClose={() => setEditing(null)}
          onSaved={refresh}
        />
      )}

      {backlogFor && (
        <Backlog folder={backlogFor} onClose={() => setBacklogFor(null)} onQueued={refresh} />
      )}
    </div>
  )
}
