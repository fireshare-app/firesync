import { useEffect, useState } from 'react'
import logo from './assets/logo.png'
import { Connect } from './views/Connect'
import { Folders } from './views/Folders'
import { api, asAppError, type Connection, type UploadOptions } from './lib/ipc'

type Phase =
  | { status: 'loading' }
  | { status: 'disconnected' }
  | { status: 'connected'; connection: Connection }

export default function App() {
  const [phase, setPhase] = useState<Phase>({ status: 'loading' })
  const [options, setOptions] = useState<UploadOptions | null>(null)

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

  if (phase.status === 'loading') return <div className="shell" />

  if (phase.status === 'disconnected') {
    return <Connect onConnected={(connection) => setPhase({ status: 'connected', connection })} />
  }

  const { connection } = phase
  return (
    <div className="app">
      <aside className="sidebar">
        <div className="sidebar__brand">
          <img src={logo} alt="" width={22} height={22} />
          <span>Firesync</span>
        </div>
        <nav className="sidebar__nav">
          <span className="sidebar__item sidebar__item--active">Watched folders</span>
        </nav>
        <span className="spacer" />
        <div className="sidebar__conn">
          <span className="sidebar__conn-head">
            <span className="dot dot--ok" />
            Connected
          </span>
          <span className="mono sidebar__host">{new URL(connection.serverUrl).host}</span>
          <span className="sidebar__user">
            as <strong>{connection.check.username}</strong>
          </span>
          <button
            type="button"
            className="btn btn--ghost btn--sm"
            onClick={() => api.disconnect().then(() => setPhase({ status: 'disconnected' }))}
          >
            Disconnect
          </button>
        </div>
      </aside>
      <main className="main">
        <Folders options={options} />
      </main>
    </div>
  )
}
