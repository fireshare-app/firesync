import { useState } from 'react'
import logo from '../assets/logo.png'
import { api, asAppError, type AppError, type Connection } from '../lib/ipc'

const CheckIcon = () => (
  <svg width="17" height="17" viewBox="0 0 18 18" aria-hidden="true" style={{ flexShrink: 0 }}>
    <circle cx="9" cy="9" r="6.8" stroke="currentColor" strokeWidth="1.5" fill="none" />
    <path
      d="M5.9 9.2l2.1 2.1 4.1-4.4"
      stroke="currentColor"
      strokeWidth="1.7"
      fill="none"
      strokeLinecap="round"
      strokeLinejoin="round"
    />
  </svg>
)

const AlertIcon = () => (
  <svg width="17" height="17" viewBox="0 0 18 18" aria-hidden="true" style={{ flexShrink: 0 }}>
    <circle cx="9" cy="9" r="6.8" stroke="currentColor" strokeWidth="1.5" fill="none" />
    <path d="M9 5.4v4M9 12.1v.1" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" />
  </svg>
)

/** Headline for a failure, so the body text stays the specific part. */
const HEADLINES: Record<AppError['kind'], string> = {
  bad_url: 'Check the address',
  unreachable: 'Could not reach that instance',
  not_fireshare: "That does not look like Fireshare's API",
  token_rejected: 'That token was not accepted',
  throttled: 'Too many attempts',
  server: 'The server had a problem',
  keychain: 'Could not use the system keychain',
  storage: 'Could not save your settings',
  not_connected: 'Not connected yet',
}

interface Props {
  onConnected: (connection: Connection) => void
}

export function Connect({ onConnected }: Props) {
  const [url, setUrl] = useState('')
  const [token, setToken] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<AppError | null>(null)
  const [result, setResult] = useState<Connection | null>(null)

  const canSubmit = url.trim().length > 0 && token.trim().length > 0 && !busy

  async function test(event: React.FormEvent) {
    event.preventDefault()
    setBusy(true)
    setError(null)
    setResult(null)
    try {
      setResult(await api.connect(url, token))
    } catch (e) {
      setError(asAppError(e))
    } finally {
      setBusy(false)
    }
  }

  return (
    <form className="connect" onSubmit={test}>
      <div className="connect__rail">
        <div className="connect__brand">
          <img src={logo} alt="" width={28} height={28} />
          <span>Firesync</span>
        </div>
        <p className="connect__tagline">
          Drop a clip in a watched folder. It lands in Fireshare before you finish the next round.
        </p>
        <div className="spacer" />
        <div className="connect__steps">
          <div className="connect__step connect__step--active">
            <span className="connect__step-num">1</span>
            <span>Connect your instance</span>
          </div>
          <div className="connect__step">
            <span className="connect__step-num">2</span>
            <span>Pick folders to watch</span>
          </div>
          <div className="connect__step">
            <span className="connect__step-num">3</span>
            <span>Forget it is running</span>
          </div>
        </div>
      </div>

      <div className="connect__form">
        <h1 className="connect__title">Connect to your instance</h1>
        <p className="connect__subtitle">
          Firesync signs in with an upload token, so it never holds your password.
        </p>

        <div className="field">
          <label className="field__label" htmlFor="server">
            Server URL
          </label>
          <input
            id="server"
            type="text"
            autoFocus
            spellCheck={false}
            placeholder="https://fireshare.example.com"
            value={url}
            disabled={busy}
            onChange={(e) => setUrl(e.target.value)}
          />
        </div>

        <div className="field">
          <label className="field__label" htmlFor="token">
            Upload token
          </label>
          <input
            id="token"
            type="password"
            spellCheck={false}
            placeholder="fsk_…"
            value={token}
            disabled={busy}
            onChange={(e) => setToken(e.target.value)}
          />
          <span className="field__hint">
            In Fireshare: Settings → Security → Upload Tokens → Create token. The secret is shown
            once.
          </span>
        </div>

        {error && (
          <div className="result result--bad" role="alert">
            <div className="result__head">
              <AlertIcon />
              <span>{HEADLINES[error.kind] ?? 'Something went wrong'}</span>
            </div>
            <p className="result__detail">{error.message}</p>
          </div>
        )}

        {result && (
          <div className="result result--ok">
            <div className="result__head">
              <CheckIcon />
              <span>Token works — signed in as {result.check.username}</span>
            </div>
            <div className="chips">
              {result.check.default_folder && (
                <span className="chip">
                  Default folder <span className="mono">{result.check.default_folder}</span>
                </span>
              )}
              <span className="chip">
                Images {result.check.images_enabled ? 'enabled' : 'not enabled'}
              </span>
              {result.check.supported_video_types.length > 0 && (
                <span className="chip chip--mono">
                  {result.check.supported_video_types.join(' ')}
                </span>
              )}
              {result.check.images_enabled && result.check.supported_image_types.length > 0 && (
                <span className="chip chip--mono">
                  {result.check.supported_image_types.join(' ')}
                </span>
              )}
            </div>
          </div>
        )}

        <div className="spacer" />

        <div className="actions">
          <span className="field__hint">Token kept in your OS keychain.</span>
          <div className="spacer" />
          {result ? (
            <>
              <button type="submit" className="btn btn--ghost" disabled={busy}>
                Test again
              </button>
              <button type="button" className="btn btn--primary" onClick={() => onConnected(result)}>
                Continue
              </button>
            </>
          ) : (
            <button type="submit" className="btn btn--primary" disabled={!canSubmit}>
              {busy ? 'Testing…' : 'Test connection'}
            </button>
          )}
        </div>
      </div>
    </form>
  )
}
