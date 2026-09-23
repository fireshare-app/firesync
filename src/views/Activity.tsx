import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { listen } from '@tauri-apps/api/event'
import { writeText } from '@tauri-apps/plugin-clipboard-manager'
import { openUrl } from '@tauri-apps/plugin-opener'
import {
  AlertCircleIcon,
  ArrowUpIcon,
  CheckCircleIcon,
  CheckIcon,
  ClockIcon,
  CopyIcon,
  ExternalLinkIcon,
  LinkIcon,
  PauseIcon,
  RetryIcon,
} from '../components/Icons'
import { humanRate } from '../lib/format'
import {
  activity as activityApi,
  asAppError,
  folders as foldersApi,
  queue as queueApi,
  type ActivityRow,
  type FileState,
  type FolderSummary,
  type QueueStatus,
  type UploadEvent,
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
  if (!unixSeconds) return ''
  const seconds = Math.max(0, Math.floor(Date.now() / 1000) - unixSeconds)
  if (seconds < 60) return 'just now'
  const minutes = Math.floor(seconds / 60)
  if (minutes < 60) return `${minutes} minute${minutes === 1 ? '' : 's'} ago`
  const hours = Math.floor(minutes / 60)
  if (hours < 24) return `${hours} hour${hours === 1 ? '' : 's'} ago`
  const days = Math.floor(hours / 24)
  return `${days} day${days === 1 ? '' : 's'} ago`
}

function basename(path: string) {
  return path.split(/[\\/]/).pop() ?? path
}

type Filter = 'all' | 'progress' | 'attention' | 'done'

const NEEDS_ATTENTION: FileState[] = ['failed']
const IN_PROGRESS: FileState[] = ['queued', 'uploading']
const FINISHED: FileState[] = ['done', 'duplicate', 'skipped']

function iconFor(state: FileState, inFlight: boolean) {
  if (inFlight) return <ArrowUpIcon />
  switch (state) {
    case 'queued':
      return <ClockIcon />
    case 'uploading':
      return <ArrowUpIcon />
    case 'failed':
      return <AlertCircleIcon />
    case 'duplicate':
      return <CopyIcon />
    case 'skipped':
      return <ClockIcon />
    default:
      return <CheckCircleIcon />
  }
}

export function Activity() {
  const [rows, setRows] = useState<ActivityRow[]>([])
  const [folders, setFolders] = useState<FolderSummary[]>([])
  const [status, setStatus] = useState<QueueStatus | null>(null)
  const [sending, setSending] = useState<Record<string, { fraction: number; rate: number | null }>>(
    {},
  )
  const [landings, setLandings] = useState<Record<string, string>>({})
  const [filter, setFilter] = useState<Filter>('all')
  const [copied, setCopied] = useState<number | null>(null)
  const [error, setError] = useState<string | null>(null)
  const copyTimer = useRef<ReturnType<typeof setTimeout> | null>(null)

  const refresh = useCallback(async () => {
    try {
      setRows(await activityApi.recent(200))
      setStatus(await queueApi.status())
      setFolders(await foldersApi.list())
    } catch (e) {
      setError(asAppError(e).message)
    }
  }, [])

  useEffect(() => {
    void refresh()
    const t = setInterval(() => void refresh(), 5000)
    return () => clearInterval(t)
  }, [refresh])

  useEffect(() => {
    const stop = listen<UploadEvent>('firesync://upload', (event) => {
      const { path, state, sent, size, bytesPerSecond, landedAs, removedLocal } = event.payload
      // Progress arrives twice a second and does not change the ledger row, so
      // it updates in place rather than triggering a reread.
      if (state === 'uploading') {
        setSending((prev) => ({
          ...prev,
          [path]: { fraction: size > 0 ? sent / size : 0, rate: bytesPerSecond },
        }))
        return
      }
      setSending((prev) => {
        const next = { ...prev }
        delete next[path]
        return next
      })
      const note = [landedAs, removedLocal].filter(Boolean).join(' · ')
      if (note) setLandings((prev) => ({ ...prev, [path]: note }))
      void refresh()
    })
    return () => {
      void stop.then((fn) => fn())
    }
  }, [refresh])

  useEffect(() => {
    const stop = listen('firesync://decision', () => void refresh())
    return () => {
      void stop.then((fn) => fn())
    }
  }, [refresh])

  useEffect(() => () => {
    if (copyTimer.current) clearTimeout(copyTimer.current)
  }, [])

  const copyLink = useCallback(async (id: number, link: string) => {
    try {
      await writeText(link)
      setCopied(id)
      if (copyTimer.current) clearTimeout(copyTimer.current)
      copyTimer.current = setTimeout(() => setCopied(null), 1600)
    } catch (e) {
      setError(asAppError(e).message)
    }
  }, [])

  const folderName = useCallback(
    (id: string) => {
      const folder = folders.find((f) => f.id === id)
      return folder ? basename(folder.path) : null
    },
    [folders],
  )

  const counts = useMemo(
    () => ({
      attention: rows.filter((r) => NEEDS_ATTENTION.includes(r.state)).length,
    }),
    [rows],
  )

  const visible = useMemo(() => {
    switch (filter) {
      case 'progress':
        return rows.filter((r) => IN_PROGRESS.includes(r.state))
      case 'attention':
        return rows.filter((r) => NEEDS_ATTENTION.includes(r.state))
      case 'done':
        return rows.filter((r) => FINISHED.includes(r.state))
      default:
        return rows
    }
  }, [rows, filter])

  const lifetime = useMemo(() => {
    const done = rows.filter((r) => r.state === 'done')
    return { files: done.length, bytes: done.reduce((n, r) => n + r.size, 0) }
  }, [rows])

  return (
    <div className="page page--tall">
      <header className="page__head">
        <h1 className="page__title">Activity</h1>
        <span className="spacer" />
        <button
          type="button"
          className="btn btn--ghost btn--icon"
          onClick={() =>
            (status?.paused ? queueApi.resume() : queueApi.pause()).then(refresh).catch(() => {})
          }
        >
          <PauseIcon />
          {status?.paused ? 'Resume' : 'Pause all'}
        </button>
        <button
          type="button"
          className="btn btn--primary btn--icon"
          disabled={!status?.failed}
          onClick={() => queueApi.retryFailed().then(refresh).catch(() => {})}
        >
          <RetryIcon />
          Retry failed
        </button>
      </header>

      {error && <div className="banner banner--bad">{error}</div>}
      {status?.paused && (
        <div className="banner banner--bad">{status.pauseReason ?? 'Uploads are paused.'}</div>
      )}

      <div className="tabs">
        {(
          [
            ['all', 'All', 0],
            ['progress', 'In progress', 0],
            ['attention', 'Needs attention', counts.attention],
            ['done', 'Completed', 0],
          ] as [Filter, string, number][]
        ).map(([key, label, badge]) => (
          <button
            key={key}
            type="button"
            className={`tab ${filter === key ? 'tab--on' : ''}`}
            onClick={() => setFilter(key)}
          >
            {label}
            {badge > 0 && <span className="tab__badge">{badge}</span>}
          </button>
        ))}
      </div>

      <div className="feed">
        {visible.length === 0 && (
          <p className="panel__empty">
            Nothing here yet. Drop a file into a watched folder and it appears once it stops being
            written.
          </p>
        )}
        {visible.map((r) => {
          const flight = sending[r.path]
          const inFlight = flight !== undefined
          const state = inFlight ? 'uploading' : r.state
          return (
            <div key={r.id} className={`arow arow--${state}`}>
              <span className={`arow__icon arow__icon--${state}`}>{iconFor(r.state, inFlight)}</span>
              <span className="mono arow__name" title={r.path}>
                {basename(r.path)}
              </span>
              {folderName(r.folderId) && (
                <span className="arow__folder">{folderName(r.folderId)}</span>
              )}
              <span className="spacer" />
              {inFlight ? (
                <>
                  {flight.rate !== null && flight.rate > 0 && (
                    <span className="arow__rate mono">{humanRate(flight.rate)}</span>
                  )}
                  <span className="arow__bar">
                    <span
                      className="arow__bar-fill"
                      style={{ width: `${Math.min(100, Math.round(flight.fraction * 100))}%` }}
                    />
                  </span>
                  <span className="arow__pct">
                    {Math.min(100, Math.round(flight.fraction * 100))}%
                  </span>
                </>
              ) : (
                <>
                  {landings[r.path] && (
                    <span className="arow__meta mono arow__landed" title={landings[r.path]}>
                      {landings[r.path]}
                    </span>
                  )}
                  {r.reason && (
                    <span className="arow__reason" title={r.reason}>
                      {r.reason}
                    </span>
                  )}
                  <span className="arow__meta">{humanSize(r.size)}</span>
                  <span className="arow__when">{ago(r.updatedAt)}</span>
                  {r.link && (
                    <span className="arow__actions">
                      <button
                        type="button"
                        className="iconbtn"
                        title={copied === r.id ? 'Link copied' : 'Copy link'}
                        aria-label="Copy link"
                        onClick={() => void copyLink(r.id, r.link!)}
                      >
                        {copied === r.id ? <CheckIcon /> : <LinkIcon />}
                      </button>
                      <button
                        type="button"
                        className="iconbtn"
                        title="Open in Fireshare"
                        aria-label="Open in Fireshare"
                        onClick={() => void openUrl(r.link!).catch(() => {})}
                      >
                        <ExternalLinkIcon />
                      </button>
                    </span>
                  )}
                </>
              )}
            </div>
          )
        })}
      </div>

      <footer className="feedfoot">
        <span>
          <strong className="num--blue">{status?.uploading ?? 0}</strong> uploading
        </span>
        <span>
          <strong>{status?.queued ?? 0}</strong> queued
        </span>
        <span>
          <strong className={status?.failed ? 'num--bad' : ''}>{status?.failed ?? 0}</strong> need
          attention
        </span>
        <span className="spacer" />
        <span className="feedfoot__total">
          {humanSize(lifetime.bytes)} uploaded · {lifetime.files} file
          {lifetime.files === 1 ? '' : 's'}
        </span>
      </footer>
    </div>
  )
}
