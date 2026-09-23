import { useEffect, useId, useRef, useState } from 'react'

export interface Option {
  value: string
  label: string
  /** Shown to the right of the label, for a hint or a type list. */
  note?: string
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
}: Props) {
  const [open, setOpen] = useState(false)
  const [active, setActive] = useState(0)
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

  useEffect(() => {
    if (!open) return
    const index = options.findIndex((o) => o.value === value)
    setActive(index >= 0 ? index : 0)
  }, [open, options, value])

  useEffect(() => {
    if (!open) return
    listRef.current?.children[active]?.scrollIntoView({ block: 'nearest' })
  }, [open, active])

  function choose(next: string) {
    onChange(next)
    setOpen(false)
    setCustom(false)
  }

  function onKeyDown(e: React.KeyboardEvent) {
    if (!open) {
      if (e.key === 'Enter' || e.key === ' ' || e.key === 'ArrowDown') {
        e.preventDefault()
        setOpen(true)
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
        setActive((i) => Math.min(options.length - 1, i + 1))
        break
      case 'ArrowUp':
        e.preventDefault()
        setActive((i) => Math.max(0, i - 1))
        break
      case 'Home':
        e.preventDefault()
        setActive(0)
        break
      case 'End':
        e.preventDefault()
        setActive(options.length - 1)
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
        onClick={() => setOpen((v) => !v)}
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
                onMouseEnter={() => setActive(i)}
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
