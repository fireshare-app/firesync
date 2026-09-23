import { useEffect, useState } from 'react'
import { getCurrentWindow } from '@tauri-apps/api/window'
import logo from '../assets/logo.png'

/**
 * The window's own title bar, because the design draws one.
 *
 * With `decorations: false` the OS supplies no chrome at all, so everything a
 * frame normally does has to exist here: the drag region, the controls, and —
 * the one that bites — a close button that hides to the tray rather than
 * quitting, matching what the real frame's close button did.
 */
export function TitleBar() {
  const [maximized, setMaximized] = useState(false)

  useEffect(() => {
    const win = getCurrentWindow()
    win.isMaximized().then(setMaximized).catch(() => {})
    const unlisten = win.onResized(() => {
      win.isMaximized().then(setMaximized).catch(() => {})
    })
    return () => {
      void unlisten.then((fn) => fn())
    }
  }, [])

  const win = getCurrentWindow()

  return (
    <div className="titlebar" data-tauri-drag-region>
      <img className="titlebar__logo" src={logo} alt="" width={14} height={14} data-tauri-drag-region />
      <span className="titlebar__name" data-tauri-drag-region>
        Firesync
      </span>
      <span className="spacer" data-tauri-drag-region />

      <button
        type="button"
        className="wbtn"
        aria-label="Minimise"
        onClick={() => void win.minimize()}
      >
        <svg width="11" height="11" viewBox="0 0 11 11" aria-hidden="true">
          <path d="M1 5.5h9" stroke="currentColor" strokeWidth="1.2" fill="none" />
        </svg>
      </button>

      <button
        type="button"
        className="wbtn"
        aria-label={maximized ? 'Restore' : 'Maximise'}
        onClick={() => void win.toggleMaximize()}
      >
        <svg width="11" height="11" viewBox="0 0 11 11" aria-hidden="true">
          <rect
            x="1.4"
            y="1.4"
            width="8.2"
            height="8.2"
            stroke="currentColor"
            strokeWidth="1.2"
            fill="none"
          />
        </svg>
      </button>

      <button
        type="button"
        className="wbtn wbtn--close"
        aria-label="Close to tray"
        // Hide, not close: uploads keep running and the tray keeps reporting.
        onClick={() => void win.hide()}
      >
        <svg width="11" height="11" viewBox="0 0 11 11" aria-hidden="true">
          <path d="M1.5 1.5l8 8M9.5 1.5l-8 8" stroke="currentColor" strokeWidth="1.2" fill="none" />
        </svg>
      </button>
    </div>
  )
}
