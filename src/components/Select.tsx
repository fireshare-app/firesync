import { useEffect, useId, useRef, useState } from 'react'

export interface Option {
  value: string
  label: string
  /** Shown to the right of the label, for a hint or a type list. */
  note?: string
}

/** A line under the open list, about the list itself. */
export interface SelectStatus {
  text: string
  /** `busy` spins while something is being checked; `warn` is a problem. */
  tone?: 'busy' | 'warn'
  /** The full story, for the tooltip, when `text` is the short version. */
  detail?: string
}

interface Props {
  value: string
  options: Option[]
  onChange: (next: string) => void
  id?: string
  placeholder?: string
  mono?: boolean
  /** Offer a free-text entry as the last item, for a name that does not exist yet. */
  customLabel?: string
  /**
   * Called as the list opens, for a caller that wants to fetch a fresher one.
   * The list opens on what it already has either way; nothing waits on this.
   */
  onOpen?: () => void
  /** Shown under the list while it is open. */
  status?: SelectStatus | null
}

/**
 * A dropdown that matches the rest of the app.
 *
 * A native `select` cannot be styled past its closed state — the open list is
 * drawn by the OS, in the OS's colours, which is exactly what looked wrong
 * against this palette. So this is a real listbox: a button, a panel, and the
 * keyboard behaviour a select would have given for free, which is the price of
 * controlling how it looks.
 */
export function Select({
  value,
  options,
  onChange,
  id,
  placeholder = 'Select…',
  mono = false,
  customLabel,
  onOpen,
  status,
}: Props) {
  const [open, setOpen] = useState(false)
  // The highlighted option, by value rather than position. A list that
  // refreshes while open can gain an entry above it, and an index would then
  // quietly point at a different option from the one somebody was about to
  // pick.
  const [activeValue, setActiveValue] = useState<string | null>(null)
  const [custom, setCustom] = useState(false)
  const [draft, setDraft] = useState('')
  const rootRef = useRef<HTMLDivElement>(null)
  const listRef = useRef<HTMLUListElement>(null)
  const listId = useId()

  const selected = options.find((o) => o.value === value)
  // A value that is not in the list is still a real choice — a folder that only
  // exists on the server, say — so it shows as itself rather than as nothing.
  const label = selected?.label ?? (value || placeholder)

  useEffect(() => {
    if (!open) return
    const onPointer = (e: MouseEvent) => {
      if (!rootRef.current?.contains(e.target as Node)) setOpen(false)
    }
    document.addEventListener('mousedown', onPointer)
    return () => document.removeEventListener('mousedown', onPointer)
  }, [open])

  const found = options.findIndex((o) => o.value === activeValue)
  const active = found >= 0 ? found : 0

  useEffect(() => {
    if (!open) return
    listRef.current?.children[active]?.scrollIntoView({ block: 'nearest' })
  }, [open, active])

  function openList() {
    setActiveValue(selected ? value : (options[0]?.value ?? null))
    setOpen(true)
    onOpen?.()
  }

  function moveTo(index: number) {
    const option = options[Math.max(0, Math.min(options.length - 1, index))]
    if (option) setActiveValue(option.value)
  }

  function choose(next: string) {
    onChange(next)
    setOpen(false)
    setCustom(false)
  }

  function onKeyDown(e: React.KeyboardEvent) {
    if (!open) {
      if (e.key === 'Enter' || e.key === ' ' || e.key === 'ArrowDown') {
        e.preventDefault()
        openList()
      }
      return
    }
    switch (e.key) {
      case 'Escape':
        e.preventDefault()
        setOpen(false)
        break
      case 'ArrowDown':
        e.preventDefault()
        moveTo(active + 1)
        break
      case 'ArrowUp':
        e.preventDefault()
        moveTo(active - 1)
        break
      case 'Home':
        e.preventDefault()
        moveTo(0)
        break
      case 'End':
        e.preventDefault()
        moveTo(options.length - 1)
        break
      case 'Enter':
      case ' ':
        e.preventDefault()
        if (options[active]) choose(options[active].value)
        break
    }
  }

  return (
    <div className="sel" ref={rootRef}>
      <button
        type="button"
        id={id}
        className={`sel__button ${mono ? 'mono' : ''} ${open ? 'sel__button--open' : ''}`}
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-controls={open ? listId : undefined}
        onClick={() => (open ? setOpen(false) : openList())}
        onKeyDown={onKeyDown}
      >
        <span className={`sel__label ${!selected && !value ? 'sel__label--empty' : ''}`}>
          {label}
        </span>
        <svg className="sel__chevron" width="11" height="11" viewBox="0 0 12 12" aria-hidden="true">
          <path
            d="M2.5 4.5L6 8l3.5-3.5"
            stroke="currentColor"
            strokeWidth="1.5"
            fill="none"
            strokeLinecap="round"
            strokeLinejoin="round"
          />
        </svg>
      </button>

      {open && (
        <div className="sel__panel">
          <ul className="sel__list" role="listbox" id={listId} ref={listRef} tabIndex={-1}>
            {options.map((o, i) => (
              <li
                key={o.value}
                role="option"
                aria-selected={o.value === value}
                className={`sel__option ${i === active ? 'sel__option--active' : ''} ${
                  o.value === value ? 'sel__option--on' : ''
                }`}
                onMouseEnter={() => setActiveValue(o.value)}
                onMouseDown={(e) => {
                  e.preventDefault()
                  choose(o.value)
                }}
              >
                <span className={mono ? 'mono' : ''}>{o.label}</span>
                {o.note && <span className="sel__note">{o.note}</span>}
              </li>
            ))}
          </ul>

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

          {customLabel &&
            (custom ? (
              <form
                className="sel__custom"
                onSubmit={(e) => {
                  e.preventDefault()
                  if (draft.trim()) choose(draft.trim())
                }}
              >
                <input
                  autoFocus
                  type="text"
                  className={mono ? 'mono' : ''}
                  value={draft}
                  placeholder="Name it"
                  onChange={(e) => setDraft(e.target.value)}
                  onKeyDown={(e) => e.key === 'Escape' && setCustom(false)}
                />
                <button type="submit" className="btn btn--primary btn--sm" disabled={!draft.trim()}>
                  Use
                </button>
              </form>
            ) : (
              <button type="button" className="sel__customopen" onMouseDown={(e) => {
                e.preventDefault()
                setCustom(true)
                setDraft('')
              }}>
                {customLabel}
              </button>
            ))}
        </div>
      )}
    </div>
  )
}
