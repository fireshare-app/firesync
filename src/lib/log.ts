import { error as logError, warn as logWarn } from '@tauri-apps/plugin-log'

type Write = (message: string) => Promise<void>

function describe(value: unknown): string {
  if (value instanceof Error) return value.stack || `${value.name}: ${value.message}`
  if (typeof value === 'string') return value
  try {
    return JSON.stringify(value)
  } catch {
    return String(value)
  }
}

/**
 * Put the window's own errors in the same log file as the core's.
 *
 * A crash in the interface used to reach the devtools console and nowhere else,
 * and nobody running a release build has the devtools open. Console errors and
 * warnings still reach the console as before; they are copied to the log too,
 * along with anything thrown or rejected that nothing caught.
 */
export function forwardToLog(windowLabel: string) {
  const tag = `[${windowLabel} window]`
  const send = (write: Write, parts: unknown[]) => {
    // Logging must never be the thing that breaks. A failed write is dropped.
    void write(`${tag} ${parts.map(describe).join(' ')}`).catch(() => {})
  }

  const original = { error: console.error, warn: console.warn }
  console.error = (...parts: unknown[]) => {
    original.error(...parts)
    send(logError, parts)
  }
  console.warn = (...parts: unknown[]) => {
    original.warn(...parts)
    send(logWarn, parts)
  }

  window.addEventListener('error', (event) =>
    send(logError, [`Uncaught ${describe(event.error ?? event.message)} (${event.filename}:${event.lineno})`]),
  )
  window.addEventListener('unhandledrejection', (event) =>
    send(logError, ['Unhandled rejection:', event.reason]),
  )
}
