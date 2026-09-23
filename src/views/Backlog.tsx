import { useCallback, useEffect, useMemo, useState } from 'react'
import { asAppError, backlog as backlogApi, type BacklogFile, type FolderSummary } from '../lib/ipc'

const MB = 1024 * 1024
const GB = 1024 * MB

function humanSize(bytes: number) {
  if (bytes >= GB) return `${(bytes / GB).toFixed(1)} GB`
  if (bytes >= MB) return `${(bytes / MB).toFixed(1)} MB`
  if (bytes >= 1024) return `${Math.round(bytes / 1024)} KB`
  return `${bytes} bytes`
}

function whenever(unixSeconds: number) {
  if (!unixSeconds) return ''
  return new Date(unixSeconds * 1000).toLocaleDateString(undefined, {
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  })
}

interface Props {
  folder: FolderSummary
  onClose: () => void
  onQueued: () => void
}

export function Backlog({ folder, onClose, onQueued }: Props) {
  const [files, setFiles] = useState<BacklogFile[] | null>(null)
  const [chosen, setChosen] = useState<Set<string>>(new Set())
  const [error, setError] = useState<string | null>(null)
  const [checking, setChecking] = useState(false)
  const [queueing, setQueueing] = useState(false)

  useEffect(() => {
    let live = true
    backlogApi
      .list(folder.id)
      .then((rows) => {
        if (!live) return
        setFiles(rows)
        // Everything the rules would accept starts selected: the common case is
        // "send what you can", and unticking a few beats ticking two hundred.
        setChosen(new Set(rows.filter((r) => !r.excluded).map((r) => r.path)))
      })
      .catch((e) => live && setError(asAppError(e).message))
    return () => {
      live = false
    }
  }, [folder.id])

  // Asked for separately, because it is the slow part — every file is hashed
  // and every hash is a round trip. The list is usable while this fills in.
  const checkLibrary = useCallback(async () => {
    if (!files) return
    setChecking(true)
    setError(null)
    try {
      const candidates = files.filter((f) => !f.excluded).map((f) => f.path)
      const answers = await backlogApi.checkAgainstLibrary(candidates)
      const known = new Map(answers)
      setFiles((prev) =>
        prev?.map((f) =>
          known.has(f.path) ? { ...f, inLibrary: known.get(f.path)! } : f,
        ) ?? prev,
      )
      setChosen((prev) => {
        const next = new Set(prev)
        for (const [path, present] of answers) if (present) next.delete(path)
        return next
      })
    } catch (e) {
      setError(asAppError(e).message)
    } finally {
      setChecking(false)
    }
  }, [files])

  const selectable = useMemo(
    () => (files ?? []).filter((f) => !f.excluded && f.inLibrary !== true),
    [files],
  )
  const totalBytes = useMemo(
    () => (files ?? []).filter((f) => chosen.has(f.path)).reduce((n, f) => n + f.size, 0),
    [files, chosen],
  )

  function toggle(path: string) {
    setChosen((prev) => {
      const next = new Set(prev)
      if (next.has(path)) next.delete(path)
      else next.add(path)
      return next
    })
  }

  async function queue() {
    setQueueing(true)
    setError(null)
    try {
      await backlogApi.queue(folder.id, [...chosen])
      onQueued()
      onClose()
    } catch (e) {
      setError(asAppError(e).message)
    } finally {
      setQueueing(false)
    }
  }

  const allSelected = selectable.length > 0 && selectable.every((f) => chosen.has(f.path))

  return (
    <div className="modal" role="dialog" aria-modal="true" aria-label="Upload files already in this folder">
      <div className="modal__box">
        <header className="modal__head">
          <div>
            <h2 className="modal__title">Upload files already in this folder</h2>
            <p className="modal__sub mono">{folder.path}</p>
          </div>
          <button type="button" className="btn btn--ghost btn--sm" onClick={onClose}>
            Close
          </button>
        </header>

        <div className="modal__bar">
          <label className="modal__all" htmlFor="pick-all">
            <input
              id="pick-all"
              type="checkbox"
              checked={allSelected}
              disabled={selectable.length === 0}
              onChange={(e) =>
                setChosen(e.target.checked ? new Set(selectable.map((f) => f.path)) : new Set())
              }
            />
            Select all
          </label>
          <span className="spacer" />
          <button
            type="button"
            className="btn btn--ghost btn--sm"
            onClick={checkLibrary}
            disabled={checking || !files?.length}
          >
            {checking ? 'Checking the library…' : 'Skip what is already uploaded'}
          </button>
        </div>

        {error && <div className="banner banner--bad">{error}</div>}

        <div className="modal__list">
          {files === null && <p className="panel__empty">Reading the folder…</p>}
          {files?.length === 0 && (
            <p className="panel__empty">
              Nothing is being held back — every file here arrived after the folder was added.
            </p>
          )}
          {files?.map((f) => {
            const blocked = Boolean(f.excluded) || f.inLibrary === true
            return (
              <label
                key={f.path}
                className={`pick ${blocked ? 'pick--blocked' : ''} ${chosen.has(f.path) ? 'pick--on' : ''}`}
              >
                <input
                  type="checkbox"
                  checked={chosen.has(f.path)}
                  disabled={blocked}
                  onChange={() => toggle(f.path)}
                />
                <span className="mono pick__name">{f.name}</span>
                <span className="spacer" />
                <span className="pick__size">{humanSize(f.size)}</span>
                <span className="pick__when">{whenever(f.mtime)}</span>
                <span className="pick__status">
                  {f.inLibrary === true ? 'In library' : (f.excluded ?? 'Ready')}
                </span>
              </label>
            )
          })}
        </div>

        <footer className="modal__foot">
          <span className="modal__count">
            <strong>{chosen.size}</strong> selected · <strong>{humanSize(totalBytes)}</strong> to send
          </span>
          <span className="spacer" />
          <button type="button" className="btn btn--ghost" onClick={onClose}>
            Cancel
          </button>
          <button
            type="button"
            className="btn btn--primary"
            disabled={chosen.size === 0 || queueing}
            onClick={queue}
          >
            {queueing ? 'Queueing…' : `Queue ${chosen.size} file${chosen.size === 1 ? '' : 's'}`}
          </button>
        </footer>
      </div>
    </div>
  )
}
