import { useCallback, useEffect, useRef, useState } from 'react'
import { getCurrentWindow, LogicalSize } from '@tauri-apps/api/window'
import { listen } from '@tauri-apps/api/event'
import logo from '../assets/logo.png'
import {
  GearIcon,
  PauseIcon,
  PlayIcon,
  RetryIcon,
  UploadIcon,
} from '../components/Icons'
import {
  api,
  asAppError,
  queue as queueApi,
  tray as trayApi,
  type QueueStatus,
  type Settings,
  type UploadEvent,
} from '../lib/ipc'

/**
 * The tray panel the design draws.
 *
 * It is a window rather than a menu because a tray menu is drawn by the OS on
 * every platform and takes no styling at all — no panel colour, no progress
 * bar, no badges. The cost is doing what a menu does for free: this closes
 * itself when it loses focus, since a window will otherwise sit there.
 */
export function TrayPanel() {
  const [status, setStatus] = useState<QueueStatus | null>(null)
  const [settings, setSettings] = useState<Settings | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [sending, setSending] = useState<{ fraction: number; name: string } | null>(null)
  const panelRef = useRef<HTMLDivElement>(null)

  const refresh = useCallback(async () => {
    try {
      setStatus(await queueApi.status())
      setSettings(await api.getSettings())
    } catch (e) {
      setError(asAppError(e).message)
    }
  }, [])

  useEffect(() => {
    void refresh()
    const t = setInterval(() => void refresh(), 1500)
    return () => clearInterval(t)
  }, [refresh])

  // The real transfer, not a guess from queue counts: events are app-wide, so
  // this window hears the same progress the main one does.
  useEffect(() => {
    const stop = listen<UploadEvent>('firesync://upload', (event) => {
      const { state, sent, size, path } = event.payload
      if (state === 'uploading' && size > 0) {
        setSending({ fraction: sent / size, name: path.split(/[\\/]/).pop() ?? '' })
      } else {
        setSending(null)
      }
    })
    return () => {
      void stop.then((fn) => fn())
    }
  }, [])

  // The window is sized to its contents rather than to a guess. A menu is
  // exactly as tall as what is in it, and this one's height changes with the
  // progress bar appearing and going away.
  useEffect(() => {
    const el = panelRef.current
    if (!el) return
    const fit = () => {
      const height = Math.ceil(el.getBoundingClientRect().height)
      if (height > 0) void getCurrentWindow().setSize(new LogicalSize(320, height))
    }
    fit()
    const observer = new ResizeObserver(fit)
    observer.observe(el)
    return () => observer.disconnect()
  }, [])

  // Closing on blur is what makes it behave like a menu rather than a window
  // somebody has to dismiss.
  useEffect(() => {
    const win = getCurrentWindow()
    const unlisten = win.onFocusChanged(({ payload: focused }) => {
      if (!focused) void win.hide()
    })
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') void win.hide()
    }
    window.addEventListener('keydown', onKey)
    return () => {
      void unlisten.then((fn) => fn())
      window.removeEventListener('keydown', onKey)
    }
  }, [])

  const notificationsOn = Boolean(
    settings && (settings.notifications.on_complete || settings.notifications.on_needs_attention),
  )

  const total = (status?.uploading ?? 0) + (status?.queued ?? 0)
  const line = status?.paused
    ? 'Paused'
    : status?.uploading
      ? total > (status?.uploading ?? 0)
        ? `Uploading ${status.uploading} of ${total}`
        : `Uploading ${status.uploading}`
      : status?.queued
        ? `${status.queued} queued`
        : 'Up to date'


  async function hideThen(fn: () => Promise<unknown>) {
    await fn()
    void getCurrentWindow().hide()
  }

  return (
    <div className="tp" ref={panelRef}>
      <div className="tp__head">
        <img src={logo} alt="" width={22} height={22} className="tp__logo" />
        <div className="tp__headtext">
          <span className="tp__name">Firesync</span>
          <span className={`tp__status ${status?.failed ? 'tp__status--bad' : ''}`}>
            {line}
            {status?.failed ? ` · ${status.failed} need attention` : ''}
          </span>
        </div>
      </div>

      {!status?.paused && sending && (
        <div className="tp__bar">
          <div
            className="tp__bar-fill"
            style={{ width: `${Math.min(100, Math.round(sending.fraction * 100))}%` }}
          />
        </div>
      )}

      {error && <div className="tp__error">{error}</div>}

      <div className="tp__sep" />

      <button className="tp__item tp__item--primary" onClick={() => hideThen(trayApi.openMain)}>
        <span className="tp__icon">
          <UploadIcon size={16} />
        </span>
        Open Firesync
      </button>

      <button
        className="tp__item"
        onClick={() => (status?.paused ? queueApi.resume() : queueApi.pause()).then(refresh)}
      >
        <span className="tp__icon">{status?.paused ? <PlayIcon size={16} /> : <PauseIcon size={16} />}</span>
        {status?.paused ? 'Resume uploads' : 'Pause all uploads'}
      </button>

      <button
        className="tp__item"
        disabled={!status?.failed}
        onClick={() => queueApi.retryFailed().then(refresh)}
      >
        <span className="tp__icon">
          <RetryIcon size={16} />
        </span>
        Retry failed
        {status?.failed ? <span className="tp__count">{status.failed}</span> : null}
      </button>

      <div className="tp__sep" />

      <button
        className="tp__item"
        onClick={async () => {
          if (!settings) return
          const on = !notificationsOn
          await api.saveSettings({
            ...settings,
            notifications: {
              ...settings.notifications,
              on_complete: on,
              on_needs_attention: on,
            },
          })
          void refresh()
        }}
      >
        <span className="tp__icon">
          <svg width="16" height="16" viewBox="0 0 18 18" aria-hidden="true">
            <path
              d="M4.4 7.6a4.6 4.6 0 019.2 0c0 3.3 1.2 4.4 1.2 4.4H3.2s1.2-1.1 1.2-4.4z"
              stroke="currentColor"
              strokeWidth="1.4"
              fill="none"
              strokeLinejoin="round"
            />
            <path
              d="M7.6 14.5a1.6 1.6 0 002.8 0"
              stroke="currentColor"
              strokeWidth="1.4"
              fill="none"
              strokeLinecap="round"
            />
          </svg>
        </span>
        Notifications
        <span className="tp__value">{notificationsOn ? 'On' : 'Off'}</span>
      </button>

      <button className="tp__item" onClick={() => hideThen(trayApi.openSettings)}>
        <span className="tp__icon">
          <GearIcon size={16} />
        </span>
        Settings
      </button>

      <div className="tp__sep" />

      <button className="tp__item tp__item--quit" onClick={() => trayApi.quit()}>
        <span className="tp__icon">
          <svg width="16" height="16" viewBox="0 0 18 18" aria-hidden="true">
            <path
              d="M7 3.4H4.6c-.7 0-1.2.5-1.2 1.2v8.8c0 .7.5 1.2 1.2 1.2H7"
              stroke="currentColor"
              strokeWidth="1.4"
              fill="none"
              strokeLinecap="round"
            />
            <path
              d="M11 12.2L14.2 9 11 5.8M14.2 9H7"
              stroke="currentColor"
              strokeWidth="1.4"
              fill="none"
              strokeLinecap="round"
              strokeLinejoin="round"
            />
          </svg>
        </span>
        Quit Firesync
      </button>
    </div>
  )
}
