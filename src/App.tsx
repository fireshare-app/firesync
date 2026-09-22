import { useEffect, useState } from 'react'
import { Connect } from './views/Connect'
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

  if (phase.status === 'loading') {
    return <div className="shell" />
  }

  if (phase.status === 'disconnected') {
    return <Connect onConnected={(connection) => setPhase({ status: 'connected', connection })} />
  }

  const { connection } = phase
  return (
    <div className="shell">
      <span className="shell__badge">
        <span className="dot" />
        Connected to <span className="mono">{new URL(connection.serverUrl).host}</span> as{' '}
        {connection.check.username}
      </span>
      <p className="shell__note">
        Watched folders, activity and settings land here next. Discovery is live —{' '}
        {options ? (
          <>
            this instance offers {options.folders.video.length} video folder
            {options.folders.video.length === 1 ? '' : 's'} and {options.games.length} game
            {options.games.length === 1 ? '' : 's'}.
          </>
        ) : (
          <>loading the folder and game lists…</>
        )}
      </p>
      <button
        type="button"
        className="btn btn--ghost"
        onClick={() => api.disconnect().then(() => setPhase({ status: 'disconnected' }))}
      >
        Disconnect
      </button>
    </div>
  )
}
