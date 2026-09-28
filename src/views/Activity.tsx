import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from 'react'
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
  FolderIcon,
  LinkIcon,
  PauseIcon,
  PlayIcon,
  RetryIcon,
  SearchIcon,
  SkipIcon,
  StopIcon,
  UploadIcon,
} from '../components/Icons'
import { Menu, type MenuItem } from '../components/Menu'
import { Select } from '../components/Select'
import { humanRate } from '../lib/format'
import {
  activity as activityApi,
  asAppError,
  folders as foldersApi,
  queue as queueApi,
  type ActivityCounts,
  type ActivityRow,
  type ActivityTab,
  type FileState,
  type FolderSummary,
  type QueueStatus,
  type UploadEvent,
} from '../lib/ipc'

const MB = 1024 * 1024
const GB = 1024 * MB

/** Rows asked for at a time; "show more" asks for this many more. */
const PAGE = 100

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

const TABS: [ActivityTab, string][] = [
  ['all', 'All'],
  ['progress', 'In progress'],
  ['attention', 'Needs attention'],
  ['finished', 'Completed'],
]

interface Action {
  key: string
  title: string
  icon: ReactNode
  run: () => void
}

export function Activity() {
  const [rows, setRows] = useState<ActivityRow[]>([])
  const [counts, setCounts] = useState<ActivityCounts | null>(null)
  const [folders, setFolders] = useState<FolderSummary[]>([])
  const [status, setStatus] = useState<QueueStatus | null>(null)
  const [sending, setSending] = useState<
    Record<string, { fraction: number; rate: number | null; limit: UploadEvent['limit'] }>
  >({})
  const [landings, setLandings] = useState<Record<string, string>>({})
  const [tab, setTab] = useState<ActivityTab>('all')
  const [typed, setTyped] = useState('')
  const [name, setName] = useState('')
  const [folderId, setFolderId] = useState('')
  const [limit, setLimit] = useState(PAGE)
  const [copied, setCopied] = useState<number | null>(null)
  const [error, setError] = useState<string | null>(null)
  const copyTimer = useRef<ReturnType<typeof setTimeout> | null>(null)

  // A query per settled word rather than per keystroke.
  useEffect(() => {
    const t = setTimeout(() => setName(typed.trim()), 200)
    return () => clearTimeout(t)
  }, [typed])

  // A different question starts from the top of its answer.
  useEffect(() => setLimit(PAGE), [tab, name, folderId])

  const refresh = useCallback(async () => {
    try {
      const page = await activityApi.page({
        tab,
        name: name || null,
        folderId: folderId || null,
        limit,
      })
      setRows(page.rows)
      setCounts(page.counts)
      setStatus(await queueApi.status())
      setError(null)
    } catch (e) {
      setError(asAppError(e).message)
    }
  }, [tab, name, folderId, limit])

  useEffect(() => {
    void refresh()
    const t = setInterval(() => void refresh(), 5000)
    return () => clearInterval(t)
  }, [refresh])

  // For the folder chips and the filter. Folders change rarely, and listing
  // them counts every file on disk, so this is not on the refresh timer.
  useEffect(() => {
    foldersApi
      .list()
      .then(setFolders)
      .catch(() => {})
  }, [])

  useEffect(() => {
    const stop = listen<UploadEvent>('firesync://upload', (event) => {
      const { path, state, sent, size, bytesPerSecond, landedAs, removedLocal, limit } = event.payload
      // Progress arrives twice a second and does not change the ledger row, so
      // it updates in place rather than triggering a reread.
      if (state === 'uploading') {
        setSending((prev) => ({
          ...prev,
          [path]: { fraction: size > 0 ? sent / size : 0, rate: bytesPerSecond, limit },
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

  /** Do something to one file, then show what it came to. */
  const act = useCallback(
    async (run: () => Promise<unknown>) => {
      try {
        await run()
      } catch (e) {
        setError(asAppError(e).message)
      }
      void refresh()
    },
    [refresh],
  )

  const folderName = useCallback(
    (id: string) => {
      const folder = folders.find((f) => f.id === id)
      return folder ? basename(folder.path) : null
    },
    [folders],
  )

  const folderOptions = useMemo(
    () => [
      { value: '', label: 'All folders' },
      ...folders.map((f) => ({ value: f.id, label: basename(f.path) })),
    ],
    [folders],
  )

  const inTab = counts ? counts[tab] : 0

  function actionsFor(r: ActivityRow, inFlight: boolean): { inline: Action[]; menu: MenuItem[] } {
    const reveal: MenuItem = {
      key: 'reveal',
      icon: <FolderIcon size={15} />,
      label: 'Show in folder',
      onSelect: () => void act(() => activityApi.reveal(r.id)),
    }
    const skip: MenuItem = {
      key: 'skip',
      icon: <SkipIcon />,
      label: 'Skip this file',
      hint: 'won’t upload',
      onSelect: () => void act(() => activityApi.skip(r.id)),
    }
    const now = Date.now() / 1000

    if (inFlight || r.state === 'uploading') {
      return {
        inline: [
          {
            key: 'stop',
            title: 'Stop. You can upload it later from where it got to.',
            icon: <StopIcon />,
            run: () => void act(() => activityApi.stop(r.id)),
          },
        ],
        menu: [reveal],
      }
    }
    switch (r.state) {
      case 'failed':
        return {
          inline: [
            { key: 'retry', title: 'Retry', icon: <RetryIcon />, run: () => void act(() => activityApi.retry(r.id)) },
          ],
          menu: [skip, reveal],
        }
      case 'queued': {
        const waiting = r.nextTryAt !== null && r.nextTryAt > now
        return {
          inline: waiting
            ? [
                {
                  key: 'now',
                  title: 'Try now instead of waiting',
                  icon: <PlayIcon />,
                  run: () => void act(() => activityApi.retry(r.id)),
                },
              ]
            : [],
          menu: [skip, reveal],
        }
      }
      case 'skipped':
        return {
          inline: [
            {
              key: 'anyway',
              title: 'Upload anyway',
              icon: <UploadIcon />,
              run: () => void act(() => activityApi.uploadAnyway(r.id)),
            },
          ],
          menu: [reveal],
        }
      default:
        return {
          inline: r.link
            ? [
                {
                  key: 'copy',
                  title: copied === r.id ? 'Link copied' : 'Copy link',
                  icon: copied === r.id ? <CheckIcon /> : <LinkIcon />,
                  run: () => void copyLink(r.id, r.link!),
                },
                {
                  key: 'open',
                  title: 'Open in Fireshare',
                  icon: <ExternalLinkIcon />,
                  run: () => void openUrl(r.link!).catch(() => {}),
                },
              ]
            : [],
          menu: [reveal],
        }
    }
  }

  return (
    <div className="page page--tall">
      <header className="page__head">
        <h1 className="page__title">Activity</h1>
        <div className="page__tools">
          <label className="search">
            <SearchIcon />
            <input
              type="text"
              placeholder="Search file names"
              aria-label="Search file names"
              spellCheck={false}
              value={typed}
              onChange={(e) => setTyped(e.target.value)}
            />
          </label>
          <div className="page__filter">
            <Select value={folderId} placeholder="All folders" options={folderOptions} onChange={setFolderId} />
          </div>
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
        </div>
      </header>

      {error && <div className="banner banner--bad">{error}</div>}
      {status?.paused && (
        <div className="banner banner--bad">{status.pauseReason ?? 'Uploads are paused.'}</div>
      )}
      {!status?.paused && status?.held && (
        <div className="banner banner--info">
          <PauseIcon />
          <span>
            A game has the screen, so uploads are waiting. They carry on as soon as you tab out.
          </span>
        </div>
      )}

      <div className="tabs">
        {TABS.map(([key, label]) => {
          const count = counts?.[key] ?? 0
          return (
            <button
              key={key}
              type="button"
              className={`tab ${tab === key ? 'tab--on' : ''}`}
              onClick={() => setTab(key)}
            >
              {label}
              {key === 'attention' ? (
                count > 0 && <span className="tab__badge">{count.toLocaleString()}</span>
              ) : (
                <span className="tab__count">{count.toLocaleString()}</span>
              )}
            </button>
          )
        })}
      </div>

      <div className="feed">
        {rows.length === 0 && (
          <p className="panel__empty">
            {name
              ? `No files match “${name}”.`
              : 'Nothing here yet. Drop a file into a watched folder and it appears once it stops being written.'}
          </p>
        )}
        {rows.map((r) => {
          const flight = sending[r.path]
          const inFlight = flight !== undefined
          const state = inFlight ? 'uploading' : r.state
          const { inline, menu } = actionsFor(r, inFlight)
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
                    <span className={`arow__rate mono ${flight.limit === 'playing' ? 'arow__slow' : ''}`}>
                      {humanRate(flight.rate)}
                      {flight.limit === 'limit' && ' · limited'}
                      {flight.limit === 'playing' && ' · slowed while you play'}
                    </span>
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
                </>
              )}
              <span className="arow__actions arow__actions--wide">
                {inline.map((a) => (
                  <button
                    key={a.key}
                    type="button"
                    className="iconbtn"
                    title={a.title}
                    aria-label={a.title}
                    onClick={a.run}
                  >
                    {a.icon}
                  </button>
                ))}
                <Menu items={menu.length > 1 ? [menu[0], 'separator', ...menu.slice(1)] : menu} />
              </span>
            </div>
          )
        })}
        {rows.length > 0 && rows.length < inTab && (
          <div className="feed__more">
            <button type="button" className="btn btn--ghost btn--sm" onClick={() => setLimit((n) => n + PAGE)}>
              Show {Math.min(PAGE, inTab - rows.length).toLocaleString()} more
            </button>
            <span>
              Showing {rows.length.toLocaleString()} of {inTab.toLocaleString()}
            </span>
          </div>
        )}
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
          {humanSize(counts?.uploadedBytes ?? 0)} uploaded ·{' '}
          {(counts?.uploadedFiles ?? 0).toLocaleString()} file
          {counts?.uploadedFiles === 1 ? '' : 's'}
        </span>
      </footer>
    </div>
  )
}
