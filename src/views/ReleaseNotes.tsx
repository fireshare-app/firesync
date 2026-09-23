import { useEffect, useState } from 'react'
import { openUrl } from '@tauri-apps/plugin-opener'
import { asAppError, releases as releasesApi, type Release } from '../lib/ipc'
import { ExternalLinkIcon } from '../components/Icons'

/**
 * Release notes are written as a markdown list, which is all this needs to
 * render: a line that begins with a bullet marker is one, and anything else is
 * a paragraph. A markdown library would be several hundred kilobytes to handle
 * cases these notes do not contain.
 */
function lines(notes: string): { bullet: boolean; text: string }[] {
  return notes
    .split('\n')
    .map((line) => line.trim())
    .filter((line) => line.length > 0 && !line.startsWith('#'))
    .map((line) => {
      const bullet = /^[-*]\s+/.test(line)
      return { bullet, text: bullet ? line.replace(/^[-*]\s+/, '') : line }
    })
}

function when(iso: string | null) {
  if (!iso) return ''
  const d = new Date(iso)
  if (Number.isNaN(d.getTime())) return ''
  return d.toLocaleDateString(undefined, { year: 'numeric', month: 'long', day: 'numeric' })
}

interface Props {
  current: string
  onClose: () => void
}

export function ReleaseNotes({ current, onClose }: Props) {
  const [list, setList] = useState<Release[] | null>(null)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    releasesApi
      .history(15)
      .then(setList)
      .catch((e) => setError(asAppError(e).message))
  }, [])

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose()
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [onClose])

  return (
    <div className="modal" role="dialog" aria-modal="true" aria-label="Release notes">
      <div className="modal__box modal__box--narrow">
        <header className="modal__head">
          <div>
            <h2 className="modal__title">What&rsquo;s new</h2>
            <p className="modal__sub">
              You are running <span className="mono">v{current}</span>
            </p>
          </div>
          <span className="spacer" />
          <button type="button" className="btn btn--ghost btn--sm" onClick={onClose}>
            Close
          </button>
        </header>

        <div className="modal__body notes__scroll">
          {error && <div className="banner banner--bad">{error}</div>}
          {!list && !error && <p className="panel__empty">Fetching the release notes&hellip;</p>}
          {list?.length === 0 && <p className="panel__empty">No releases published yet.</p>}

          {list?.map((r) => {
            const running = r.version === current
            return (
              <section key={r.version} className={`rel ${running ? 'rel--running' : ''}`}>
                <header className="rel__head">
                  <span className="rel__version mono">v{r.version}</span>
                  {running && <span className="rel__badge">Running</span>}
                  {r.prerelease && <span className="rel__badge rel__badge--pre">Pre-release</span>}
                  <span className="spacer" />
                  <span className="rel__date">{when(r.publishedAt)}</span>
                  <button
                    type="button"
                    className="iconbtn"
                    title="Open on GitHub"
                    aria-label={`Open v${r.version} on GitHub`}
                    onClick={() => void openUrl(r.url).catch(() => {})}
                  >
                    <ExternalLinkIcon />
                  </button>
                </header>
                {r.notes ? (
                  <ul className="rel__list">
                    {lines(r.notes).map((line, i) => (
                      <li key={i} className={line.bullet ? '' : 'rel__para'}>
                        {line.text}
                      </li>
                    ))}
                  </ul>
                ) : (
                  <p className="rel__none">No notes were written for this release.</p>
                )}
              </section>
            )
          })}
        </div>
      </div>
    </div>
  )
}
