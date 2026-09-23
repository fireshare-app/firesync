import { useCallback, useEffect, useState } from 'react'
import logo from './assets/logo.png'
import { ActivityIcon, FolderIcon, GearIcon } from './components/Icons'
import { Connect } from './views/Connect'
import { Folders } from './views/Folders'
import { Activity } from './views/Activity'
import { SettingsView } from './views/SettingsView'
import {
  api,
  asAppError,
  folders as foldersApi,
  queue as queueApi,
  type Connection,
  type UploadOptions,
} from './lib/ipc'

type Phase =
  | { status: 'loading' }
  | { status: 'disconnected' }
  | { status: 'connected'; connection: Connection }

type Tab = 'folders' | 'activity' | 'settings'

const VERSION = '0.1.0'

export default function App() {
  const [phase, setPhase] = useState<Phase>({ status: 'loading' })
  const [tab, setTab] = useState<Tab>('folders')
  const [options, setOptions] = useState<UploadOptions | null>(null)
  const [folderCount, setFolderCount] = useState(0)
  const [attention, setAttention] = useState(0)

  // A stored connection is re-checked on launch rather than trusted: the token
  // may have been revoked, or the account may have lost its upload permission,
  // since the last run. Either way the server decides, not the config file.
  useEffect(() => {
    api
      .connectionStatus()
      .then((connection) =>
        setPhase(connection ? { status: 'connected', connection } : { status: 'disconnected' }),
      )
      .catch(() => setPhase({ status: 'disconnected' }))
  }, [])

  useEffect(() => {
    if (phase.status !== 'connected') return
    api
      .uploadOptions()
      .then(setOptions)
      .catch((e) => console.warn('could not load upload options:', asAppError(e).message))
  }, [phase.status])

  // The sidebar counts live here so they are the same whichever view is open.
  const refreshCounts = useCallback(async () => {
    try {
      setFolderCount((await foldersApi.list()).length)
      const status = await queueApi.status()
      setAttention(status.failed + status.uploading + status.queued)
    } catch {
      // A count that cannot be read is not worth surfacing; the views that
      // depend on the data report their own failures.
    }
  }, [])

  useEffect(() => {
    if (phase.status !== 'connected') return
    void refreshCounts()
    const t = setInterval(() => void refreshCounts(), 4000)
    return () => clearInterval(t)
  }, [phase.status, refreshCounts])

  if (phase.status === 'loading') return <div className="boot" />

  if (phase.status === 'disconnected') {
    return <Connect onConnected={(connection) => setPhase({ status: 'connected', connection })} />
  }

  const { connection } = phase

  return (
    <div className="app">
      <aside className="sidebar">
        <div className="sidebar__brand">
          <img src={logo} alt="" width={20} height={20} />
          <span className="sidebar__name">Firesync</span>
          <span className="sidebar__version mono">{VERSION}</span>
        </div>

        <nav className="sidebar__nav">
          <button
            type="button"
            className={`navitem ${tab === 'folders' ? 'navitem--on' : ''}`}
            onClick={() => setTab('folders')}
          >
            <FolderIcon />
            Watched folders
            {folderCount > 0 && <span className="navitem__count">{folderCount}</span>}
          </button>
          <button
            type="button"
            className={`navitem ${tab === 'activity' ? 'navitem--on' : ''}`}
            onClick={() => setTab('activity')}
          >
            <ActivityIcon />
            Activity
            {attention > 0 && <span className="navitem__badge">{attention}</span>}
          </button>
          <button
            type="button"
            className={`navitem ${tab === 'settings' ? 'navitem--on' : ''}`}
            onClick={() => setTab('settings')}
          >
            <GearIcon />
            Settings
          </button>
        </nav>

        <span className="spacer" />

        <div className="conncard">
          <div className="conncard__head">
            <span className="dot dot--ok" />
            Connected
          </div>
          <div className="conncard__host mono">{new URL(connection.serverUrl).host}</div>
          <div className="conncard__user">
            signed in as <strong>{connection.check.username}</strong>
          </div>
        </div>
      </aside>

      <main className="main">
        {tab === 'folders' && <Folders options={options} onChanged={refreshCounts} />}
        {tab === 'activity' && <Activity />}
        {tab === 'settings' && (
          <SettingsView
            connection={connection}
            onDisconnected={() => setPhase({ status: 'disconnected' })}
          />
        )}
      </main>
    </div>
  )
}
