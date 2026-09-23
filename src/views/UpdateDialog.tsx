import { useEffect, useState } from 'react'
import { asAppError, updates, type UpdateInfo } from '../lib/ipc'

/** Release notes arrive as markdown; only the list matters here. */
function bullets(notes: string | null): string[] {
  if (!notes) return []
  return notes
    .split('\n')
    .map((line) => line.replace(/^\s*[-*]\s+/, '').trim())
    .filter((line) => line.length > 0 && !line.startsWith('#'))
    .slice(0, 8)
}

interface Props {
  update: UpdateInfo
  onClose: () => void
}

export function UpdateDialog({ update, onClose }: Props) {
  const [blocked, setBlocked] = useState(false)
  const [installing, setInstalling] = useState(false)
  const [error, setError] = useState<string | null>(null)

  // Checked when the dialog opens and again while it is open: an upload may
  // start or finish while somebody is reading the notes.
  useEffect(() => {
    const poll = () => updates.blockedByUpload().then(setBlocked).catch(() => {})
    poll()
    const t = setInterval(poll, 3000)
    return () => clearInterval(t)
  }, [])

  const notes = bullets(update.notes)

  return (
    <div className="modal" role="dialog" aria-modal="true" aria-label={`Firesync ${update.version}`}>
      <div className="modal__box modal__box--narrow">
        <header className="modal__head updhead">
          <span className="updhead__mark" aria-hidden="true">
            <svg width="20" height="20" viewBox="0 0 18 18">
              <path
                d="M9 12.6V4M5.2 7.8L9 4l3.8 3.8"
                stroke="#fff"
                strokeWidth="1.8"
                fill="none"
                strokeLinecap="round"
                strokeLinejoin="round"
              />
            </svg>
          </span>
          <div>
            <h2 className="modal__title">Firesync {update.version} is ready</h2>
            <p className="modal__sub">
              You are on <span className="mono">{update.currentVersion}</span>
              {update.date ? ` · released ${update.date.slice(0, 10)}` : ''}
            </p>
          </div>
        </header>

        <div className="modal__body">
          {notes.length > 0 ? (
            <section className="notes">
              <h3 className="notes__title">What changed</h3>
              <ul className="notes__list">
                {notes.map((line, i) => (
                  <li key={i}>{line}</li>
                ))}
              </ul>
            </section>
          ) : (
            <p className="panel__empty">This release came without notes.</p>
          )}

          {blocked && (
            <div className="banner banner--warn">
              An upload is in flight. Firesync will finish it before restarting — nothing is lost by
              installing now, it just waits.
            </div>
          )}
          {error && <div className="banner banner--bad">{error}</div>}
        </div>

        <footer className="modal__foot">
          <span className="spacer" />
          <button type="button" className="btn btn--ghost" onClick={onClose}>
            Later
          </button>
          <button
            type="button"
            className="btn btn--primary"
            disabled={installing || blocked}
            onClick={async () => {
              setInstalling(true)
              setError(null)
              try {
                await updates.install()
              } catch (e) {
                setError(asAppError(e).message)
                setInstalling(false)
              }
            }}
          >
            {installing ? 'Installing…' : blocked ? 'Waiting for the upload' : 'Install and restart'}
          </button>
        </footer>
      </div>
    </div>
  )
}
