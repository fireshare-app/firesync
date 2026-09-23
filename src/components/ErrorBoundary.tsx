import { Component, type ErrorInfo, type ReactNode } from 'react'

interface Props {
  children: ReactNode
}

interface State {
  error: Error | null
}

/**
 * Turns a render crash into something a person can read and report.
 *
 * Without this, a throw anywhere in the tree unmounts everything and leaves the
 * window blank — no text, nothing clickable, and no way to tell a crash apart
 * from a hang or a window that failed to paint. That is a bad state to be in and
 * a worse one to describe over a bug report.
 */
export class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null }

  static getDerivedStateFromError(error: Error): State {
    return { error }
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    console.error('firesync: the interface crashed', error, info.componentStack)
  }

  render() {
    if (!this.state.error) return this.props.children

    return (
      <div className="crash">
        <h1 className="crash__title">Something in the window broke</h1>
        <p className="crash__body">
          Uploads are not affected — the queue runs outside this window and carries on. Reopening
          from the tray icon is usually enough.
        </p>
        <pre className="crash__detail mono">{this.state.error.message}</pre>
        <button type="button" className="btn btn--primary" onClick={() => location.reload()}>
          Reload the window
        </button>
      </div>
    )
  }
}
