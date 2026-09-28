import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from 'react'
import { DotsIcon } from './Icons'

export interface MenuItem {
  key: string
  icon: ReactNode
  label: string
  /** Shown to the right, quieter, for what the item means. */
  hint?: string
  onSelect: () => void
}

interface Props {
  /** A string entry draws a separator. */
  items: (MenuItem | 'separator')[]
  label?: string
}

/**
 * A ⋯ button and the actions behind it, for the ones that deserve a label or
 * are not wanted often enough to sit in a row.
 *
 * It opens upwards when there is no room below inside the scrolling list it
 * sits in, rather than being clipped by it.
 */
export function Menu({ items, label = 'More' }: Props) {
  const [open, setOpen] = useState(false)
  const [upwards, setUpwards] = useState(false)
  const rootRef = useRef<HTMLSpanElement>(null)
  const panelRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    if (!open) return
    const onPointer = (e: MouseEvent) => {
      if (!rootRef.current?.contains(e.target as Node)) setOpen(false)
    }
    document.addEventListener('mousedown', onPointer)
    return () => document.removeEventListener('mousedown', onPointer)
  }, [open])

  // Measured after it has been drawn below, before the frame is painted.
  useLayoutEffect(() => {
    if (!open) return
    const panel = panelRef.current
    if (!panel) return
    const scroller = rootRef.current?.closest('.feed') ?? document.documentElement
    if (panel.getBoundingClientRect().bottom > scroller.getBoundingClientRect().bottom) {
      setUpwards(true)
    }
    panel.querySelector<HTMLButtonElement>('.menu__item')?.focus()
  }, [open])

  function onKeyDown(e: React.KeyboardEvent) {
    const buttons = [...(panelRef.current?.querySelectorAll<HTMLButtonElement>('.menu__item') ?? [])]
    const at = buttons.indexOf(document.activeElement as HTMLButtonElement)
    if (e.key === 'Escape') {
      e.preventDefault()
      setOpen(false)
      rootRef.current?.querySelector<HTMLButtonElement>('.iconbtn')?.focus()
    } else if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault()
      const step = e.key === 'ArrowDown' ? 1 : -1
      buttons[(at + step + buttons.length) % buttons.length]?.focus()
    }
  }

  return (
    <span className="menuwrap" ref={rootRef}>
      <button
        type="button"
        className={`iconbtn ${open ? 'iconbtn--on' : ''}`}
        title={label}
        aria-label={label}
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={() => {
          setUpwards(false)
          setOpen((v) => !v)
        }}
      >
        <DotsIcon />
      </button>
      {open && (
        <div
          className={`menu ${upwards ? 'menu--up' : ''}`}
          role="menu"
          ref={panelRef}
          onKeyDown={onKeyDown}
        >
          {items.map((item, i) =>
            item === 'separator' ? (
              <div key={`sep-${i}`} className="menu__sep" role="separator" />
            ) : (
              <button
                key={item.key}
                type="button"
                role="menuitem"
                className="menu__item"
                onClick={() => {
                  setOpen(false)
                  item.onSelect()
                }}
              >
                {item.icon}
                {item.label}
                {item.hint && <span className="menu__hint">{item.hint}</span>}
              </button>
            ),
          )}
        </div>
      )}
    </span>
  )
}
