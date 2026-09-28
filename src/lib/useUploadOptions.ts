import { useCallback, useEffect, useState } from 'react'
import { listen } from '@tauri-apps/api/event'
import type { SelectStatus } from '../components/Select'
import { uploadOptions as optionsApi, type OptionsSnapshot } from './ipc'

export interface FreshOptions {
  options: OptionsSnapshot['options']
  fetchedAt: number | null
  error: string | null
  /** A refresh this view asked for is still out. */
  checking: boolean
  /** Ask Fireshare again. Never makes anybody wait: the list already shown stays. */
  refresh: () => Promise<void>
}

/**
 * Fireshare's folders, games and folder rules, kept fresh.
 *
 * The core holds the list; this reads it, hears whenever it changes, and asks
 * for a newer one when a view is about to rely on it. It replaced a one-shot
 * fetch that lasted the whole session — the window is hidden rather than
 * closed, so a game added in Fireshare never appeared without a reload.
 */
export function useUploadOptions(): FreshOptions {
  const [snapshot, setSnapshot] = useState<OptionsSnapshot | null>(null)
  const [pending, setPending] = useState(0)

  useEffect(() => {
    let live = true
    optionsApi
      .cached()
      .then((s) => live && setSnapshot(s))
      .catch(() => {})
    // Pushed after every refresh, whoever asked — the queue's own refresh
    // included, so a dialog left open still catches up.
    const stop = listen<OptionsSnapshot>('firesync://options', (event) => setSnapshot(event.payload))
    return () => {
      live = false
      void stop.then((fn) => fn())
    }
  }, [])

  const refresh = useCallback(async () => {
    setPending((n) => n + 1)
    try {
      setSnapshot(await optionsApi.refresh())
    } catch {
      // Only fails when there is no connection to ask with, and then there is
      // nothing a picker could do about it either.
    } finally {
      setPending((n) => n - 1)
    }
  }, [])

  return {
    options: snapshot?.options ?? null,
    fetchedAt: snapshot?.fetchedAt ?? null,
    error: snapshot?.error ?? null,
    checking: pending > 0,
    refresh,
  }
}

/**
 * When a list is from, the way somebody would say it.
 *
 * Its spaces are made unbreakable: a time is one thing to read, and the footer
 * it sits in is narrow enough that "2:12" and "PM" would otherwise land on
 * separate lines.
 */
function whenFrom(unixSeconds: number) {
  const then = new Date(unixSeconds * 1000)
  const time = then.toLocaleTimeString([], { hour: 'numeric', minute: '2-digit' })
  const said =
    then.toDateString() === new Date().toDateString()
      ? time
      : `${then.toLocaleDateString([], { month: 'short', day: 'numeric' })}, ${time}`
  return said.replace(/\s/g, ' ')
}

/**
 * What a picker's footer says about the list above it.
 *
 * Nothing at all when the list is current: the footer is only there while a
 * check is running, or when one has failed and the list may be out of date.
 */
export function pickerStatus(fresh: FreshOptions): SelectStatus | null {
  if (fresh.checking) return { text: 'Checking Fireshare…', tone: 'busy' }
  if (fresh.error) {
    return {
      text: fresh.fetchedAt
        ? `Couldn’t check with Fireshare. List from ${whenFrom(fresh.fetchedAt)}.`
        : 'Couldn’t check with Fireshare.',
      tone: 'warn',
      detail: fresh.error,
    }
  }
  return null
}
