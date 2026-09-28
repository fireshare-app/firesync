import { useEffect, useRef, useState } from 'react'
import { CheckIcon } from './Icons'
import type { SelectStatus } from './Select'
import type { Tag } from '../lib/ipc'

interface Props {
  /** Fireshare's tags, or null/undefined from a Fireshare that does not offer them. */
  offered: Tag[] | null | undefined
  chosen: number[]
  onChange: (next: number[]) => void
  /** Called as the list opens, to fetch a fresher one. Nothing waits on it. */
  onOpen: () => void
  status: SelectStatus | null
}

function dot(color: string | null | undefined) {
  return <span className="tagchip__dot" style={{ background: color ?? 'var(--text-muted)' }} />
}

/**
 * The tags a folder puts on everything it uploads, as chips, and a list to add
 * them from. The list is Fireshare's, fetched again as it opens.
 */
export function TagPicker({ offered, chosen, onChange, onOpen, status }: Props) {
  const [open, setOpen] = useState(false)
  const rootRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    if (!open) return
    const onPointer = (e: MouseEvent) => {
      if (!rootRef.current?.contains(e.target as Node)) setOpen(false)
    }
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setOpen(false)
    }
    document.addEventListener('mousedown', onPointer)
    document.addEventListener('keydown', onKey)
    return () => {
      document.removeEventListener('mousedown', onPointer)
      document.removeEventListener('keydown', onKey)
    }
  }, [open])

  if (!offered) {
    return (
      <p className="field__fixed">
        Your Fireshare doesn&rsquo;t offer tags to upload tokens yet. Update Fireshare to choose
        them here.
      </p>
    )
  }

  const byId = new Map(offered.map((t) => [t.id, t]))
  const toggle = (id: number) =>
    onChange(chosen.includes(id) ? chosen.filter((x) => x !== id) : [...chosen, id])

  return (
    <div className="tagpick" ref={rootRef}>
      {chosen.map((id) => {
        const tag = byId.get(id)
        return (
          <span key={id} className="tagchip">
            {dot(tag?.color)}
            {tag ? tag.name : <span className="tagchip--gone">Tag no longer available</span>}
            <button
              type="button"
              className="tagchip__x"
              aria-label={`Remove ${tag?.name ?? 'the unavailable tag'}`}
              onClick={() => onChange(chosen.filter((x) => x !== id))}
            >
              ×
            </button>
          </span>
        )
      })}
      <button
        type="button"
        className="tagadd"
        aria-expanded={open}
        onClick={() => {
          const opening = !open
          setOpen(opening)
          if (opening) onOpen()
        }}
      >
        + Add tag
      </button>
      {open && (
        <div className="sel__panel">
          {offered.length === 0 ? (
            <p className="tagpick__empty">Fireshare has no tags yet.</p>
          ) : (
            <div className="sel__list" role="group" aria-label="Tags">
              {offered.map((t) => {
                const on = chosen.includes(t.id)
                return (
                  <button
                    key={t.id}
                    type="button"
                    role="menuitemcheckbox"
                    aria-checked={on}
                    className={`sel__option tagpick__option ${on ? 'sel__option--on' : ''}`}
                    onClick={() => toggle(t.id)}
                  >
                    {dot(t.color)}
                    {t.name}
                    {on && (
                      <span className="sel__note">
                        <CheckIcon size={13} />
                      </span>
                    )}
                  </button>
                )
              })}
            </div>
          )}
          {status && (
            <div
              className={`sel__status ${status.tone === 'warn' ? 'sel__status--warn' : ''}`}
              role="status"
              title={status.detail}
            >
              {status.tone === 'busy' && <span className="spin" aria-hidden="true" />}
              {status.text}
            </div>
          )}
        </div>
      )}
    </div>
  )
}
