import { useCallback, useEffect, useRef, useState } from 'react'
import { writeText } from '@tauri-apps/plugin-clipboard-manager'
import { openUrl } from '@tauri-apps/plugin-opener'
import {
  api,
  asAppError,
  diagnostics as diagnosticsApi,
  notifications as notificationsApi,
  updates,
  type Connection,
  type Settings,
  type UpdateInfo,
} from '../lib/ipc'
import { BellIcon, CheckIcon, CopyIcon, ExternalLinkIcon, FolderIcon } from '../components/Icons'
import { ReleaseNotes } from './ReleaseNotes'
import { Select } from '../components/Select'

interface ToggleProps {
  id: string
  label: string
  hint?: string
  checked: boolean
  onChange: (next: boolean) => void
}

/** The switch the design draws, built on a real checkbox so it stays focusable. */
function Toggle({ id, label, hint, checked, onChange }: ToggleProps) {
  return (
    <div className="setting">
      <label className="setting__text" htmlFor={id}>
        <span className="setting__label">{label}</span>
        {hint && <span className="setting__hint">{hint}</span>}
      </label>
      <span className="switch">
        <input
          id={id}
          type="checkbox"
          role="switch"
          checked={checked}
          onChange={(e) => onChange(e.target.checked)}
        />
        <span className="switch__track" />
        <span className="switch__knob" />
      </span>
    </div>
  )
}

/** Nothing about the person goes in the URL; the report is pasted in by hand. */
const ISSUES_URL = 'https://github.com/fireshare-app/firesync/issues/new'

interface Props {
  connection: Connection
  onDisconnected: () => void
}

export function SettingsView({ connection, onDisconnected }: Props) {
  const [settings, setSettings] = useState<Settings | null>(null)
  const [atLogin, setAtLogin] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [configPath, setConfigPath] = useState('')
  const [update, setUpdate] = useState<UpdateInfo | null>(null)
  const [checking, setChecking] = useState(false)
  const [updateNote, setUpdateNote] = useState<string | null>(null)
  const [probe, setProbe] = useState<{ text: string; bad: boolean } | null>(null)
  const [notesOpen, setNotesOpen] = useState(false)
  const [logPath, setLogPath] = useState('')
  const [includeServer, setIncludeServer] = useState(false)
  const [copied, setCopied] = useState(false)
  const [troubleNote, setTroubleNote] = useState<string | null>(null)
  const copiedTimer = useRef<ReturnType<typeof setTimeout> | null>(null)

  const load = useCallback(async () => {
    try {
      setSettings(await api.getSettings())
      setAtLogin(await api.launchAtLoginState())
      setConfigPath(await api.configLocation())
      setLogPath(await diagnosticsApi.logLocation())
    } catch (e) {
      setError(asAppError(e).message)
    }
  }, [])

  useEffect(() => {
    void load()
  }, [load])

  useEffect(() => () => {
    if (copiedTimer.current) clearTimeout(copiedTimer.current)
  }, [])

  async function copyDiagnostics() {
    setTroubleNote(null)
    try {
      await writeText(await diagnosticsApi.report(includeServer))
      setCopied(true)
      if (copiedTimer.current) clearTimeout(copiedTimer.current)
      copiedTimer.current = setTimeout(() => setCopied(false), 2000)
    } catch (e) {
      setTroubleNote(asAppError(e).message)
    }
  }

  // Written through immediately rather than behind a Save button: each of these
  // is a single switch whose effect is obvious, and a settings page that can be
  // left in an unsaved state is a settings page that lies.
  async function patch(next: Settings) {
    setSettings(next)
    try {
      await api.saveSettings(next)
    } catch (e) {
      setError(asAppError(e).message)
      void load()
    }
  }

  // Say which of the two silences this is.
  //
  // The toast is sent regardless of the quiet setting — a test that obeys the
  // rule it is meant to be testing around tells you nothing — and the rule's
  // current verdict is reported separately.
  async function runTest() {
    setProbe(null)
    try {
      const r = await notificationsApi.test()
      if (!r.delivered) {
        setProbe({ text: r.error ?? 'The system refused it.', bad: true })
      } else if (r.wouldHold) {
        setProbe({
          text: 'Sent. Something has the screen, so a real one would be held until you alt-tab out.',
          bad: false,
        })
      } else {
        setProbe({
          text: "Sent. If nothing appeared, check this app's notification permission in your OS settings.",
          bad: false,
        })
      }
    } catch (e) {
      setProbe({ text: asAppError(e).message, bad: true })
    }
  }

  if (!settings) {
    return <div className="page">{error && <div className="banner banner--bad">{error}</div>}</div>
  }

  const n = settings.notifications
  const check = connection.check

  return (
    <div className="page">
      {notesOpen && (
        <ReleaseNotes
          current={update?.currentVersion ?? __APP_VERSION__}
          onClose={() => setNotesOpen(false)}
        />
      )}
      <header className="page__head">
        <div className="page__headtext">
          <h1 className="page__title">Settings</h1>
        </div>
      </header>

      {error && <div className="banner banner--bad">{error}</div>}

      <section className="panel">
        <h2 className="panel__title">
          Fireshare instance
          <span className="spacer" />
          <span className="pill pill--ok">Connected as {check.username}</span>
        </h2>
        <div className="settings__conn">
          <div className="field">
            <label className="field__label" htmlFor="c-url">
              Server URL
            </label>
            <input id="c-url" className="mono" type="text" value={connection.serverUrl} readOnly />
          </div>
          <button
            type="button"
            className="btn btn--ghost"
            onClick={() => api.disconnect().then(onDisconnected)}
          >
            Disconnect
          </button>
        </div>
        <p className="field__hint settings__note">
          The upload token is in your OS keychain, never in a config file. Default folder{' '}
          <span className="mono">{check.default_folder ?? 'uploads'}</span> ·{' '}
          {check.images_enabled ? 'images enabled' : 'images not enabled'}
        </p>
      </section>

      <section className="panel">
        <h2 className="panel__title">Notifications</h2>
        <Toggle
          id="n-complete"
          label="When an upload finishes"
          checked={n.on_complete}
          onChange={(v) => patch({ ...settings, notifications: { ...n, on_complete: v } })}
        />
        <Toggle
          id="n-attention"
          label="When an upload needs attention"
          checked={n.on_needs_attention}
          onChange={(v) => patch({ ...settings, notifications: { ...n, on_needs_attention: v } })}
        />
        <Toggle
          id="n-quiet"
          label="Stay quiet while a game has the screen"
          hint="Held back until you alt-tab out, then delivered as one summary. Detected on Windows only."
          checked={n.quiet_in_fullscreen}
          onChange={(v) => patch({ ...settings, notifications: { ...n, quiet_in_fullscreen: v } })}
        />
        <Toggle
          id="n-group"
          label="Group a burst into one notification"
          hint="A folder of clips finishing together becomes one message rather than one each."
          checked={n.group_bursts}
          onChange={(v) => patch({ ...settings, notifications: { ...n, group_bursts: v } })}
        />
        <Toggle
          id="n-copy"
          label="Copy the link when an upload finishes"
          hint="Replaces whatever is on your clipboard with the newest clip's link. Files that were already in your library are left out, so re-sending a folder doesn't keep overwriting it."
          checked={n.copy_link_on_complete}
          onChange={(v) => patch({ ...settings, notifications: { ...n, copy_link_on_complete: v } })}
        />
        <div className="panel__action">
          <button type="button" className="btn btn--ghost btn--icon" onClick={() => void runTest()}>
            <BellIcon />
            Send a test notification
          </button>
          {probe && <span className={`panel__probe ${probe.bad ? 'panel__probe--bad' : ''}`}>{probe.text}</span>}
        </div>
      </section>

      <section className="panel">
        <h2 className="panel__title">
          Updates
          <span className="spacer" />
          <span className="panel__hint mono">
            v{update?.currentVersion ?? __APP_VERSION__}
            {update ? ` · ${update.version} available` : ''}
          </span>
          <button
            type="button"
            className="btn btn--ghost btn--sm"
            onClick={() => setNotesOpen(true)}
          >
            What&rsquo;s new
          </button>
        </h2>
        <Toggle
          id="u-auto"
          label="Install updates automatically"
          hint="Applied once nothing is uploading — never in the middle of a transfer."
          checked={settings.updates.auto_install}
          onChange={(v) => patch({ ...settings, updates: { ...settings.updates, auto_install: v } })}
        />
        <div className="setting">
          <span className="setting__text">
            <span className="setting__label">
              {update ? `Version ${update.version} is ready` : 'Check for updates'}
            </span>
            {updateNote && <span className="setting__hint">{updateNote}</span>}
          </span>
          {update ? (
            <button
              type="button"
              className="btn btn--primary btn--sm"
              onClick={async () => {
                setUpdateNote(null)
                try {
                  if (await updates.blockedByUpload()) {
                    setUpdateNote('An upload is in progress. It will install once that finishes.')
                    return
                  }
                  await updates.install()
                } catch (e) {
                  setUpdateNote(asAppError(e).message)
                }
              }}
            >
              Install and restart
            </button>
          ) : (
            <button
              type="button"
              className="btn btn--ghost btn--sm"
              disabled={checking}
              onClick={async () => {
                setChecking(true)
                setUpdateNote(null)
                try {
                  const found = await updates.check()
                  setUpdate(found)
                  if (!found) setUpdateNote('You are on the latest version.')
                } catch (e) {
                  setUpdateNote(asAppError(e).message)
                } finally {
                  setChecking(false)
                }
              }}
            >
              {checking ? 'Checking…' : 'Check now'}
            </button>
          )}
        </div>
      </section>

      <div className="row2">
        <section className="panel">
          <h2 className="panel__title">Transfers</h2>
          <div className="setting">
            <label className="setting__text" htmlFor="t-conc">
              <span className="setting__label">Uploads at once</span>
              <span className="setting__hint">
                Files, not chunks — one file's chunks always go in order.
              </span>
            </label>
            <Select
              id="t-conc"
              value={String(settings.transfers.max_concurrent)}
              options={[1, 2, 3, 4].map((v) => ({ value: String(v), label: String(v) }))}
              onChange={(v) =>
                patch({
                  ...settings,
                  transfers: { ...settings.transfers, max_concurrent: Number(v) },
                })
              }
            />
          </div>
        </section>

        <section className="panel">
          <h2 className="panel__title">Startup</h2>
          <Toggle
            id="s-login"
            label="Launch at login"
            checked={atLogin}
            onChange={(v) => {
              api
                .setLaunchAtLogin(v)
                // The OS is the authority here, not our config: it can refuse,
                // or somebody can remove the login item behind our back.
                .then(setAtLogin)
                .catch((e) => {
                  setError(asAppError(e).message)
                  void load()
                })
            }}
          />
          <Toggle
            id="s-tray"
            label="Start in the tray"
            hint="Applies when your computer starts it at login. Opening it yourself always shows the window."
            checked={settings.startup.start_in_tray}
            onChange={(v) =>
              patch({ ...settings, startup: { ...settings.startup, start_in_tray: v } })
            }
          />
        </section>
      </div>

      <section className="panel">
        <h2 className="panel__title">Troubleshooting</h2>
        <div className="setting">
          <span className="setting__text">
            <span className="setting__label">Diagnostics</span>
            <span className="setting__hint">
              Version, settings, folder rules, what the queue is doing and the last 200 log lines,
              ready to paste into a bug report. Your home folder and usernames are masked
              {includeServer ? '' : ', and so is your server address'}. The upload token is never
              included.
            </span>
            <label className="inline-check" htmlFor="d-server">
              <input
                id="d-server"
                type="checkbox"
                checked={includeServer}
                onChange={(e) => setIncludeServer(e.target.checked)}
              />
              Include my server address
            </label>
            {troubleNote && <span className="field__warn">{troubleNote}</span>}
          </span>
          <button
            type="button"
            className="btn btn--ghost btn--sm btn--icon"
            onClick={() => void copyDiagnostics()}
          >
            {copied ? <CheckIcon size={14} /> : <CopyIcon size={14} />}
            {copied ? 'Copied' : 'Copy diagnostics'}
          </button>
        </div>
        <div className="setting">
          <span className="setting__text">
            <span className="setting__label">Log files</span>
            {logPath && <span className="setting__hint mono">{logPath}</span>}
            <span className="setting__hint">
              Unlike the copied diagnostics, these are not masked.
            </span>
          </span>
          <button
            type="button"
            className="btn btn--ghost btn--sm btn--icon"
            onClick={() =>
              diagnosticsApi.openLogFolder().catch((e) => setTroubleNote(asAppError(e).message))
            }
          >
            <FolderIcon size={14} />
            Open folder
          </button>
        </div>
        <div className="setting">
          <span className="setting__text">
            <span className="setting__label">Report a problem</span>
            <span className="setting__hint">
              Opens a new issue on GitHub. Paste the diagnostics into it.
            </span>
          </span>
          <button
            type="button"
            className="btn btn--ghost btn--sm btn--icon"
            onClick={() => void openUrl(ISSUES_URL).catch(() => {})}
          >
            <ExternalLinkIcon size={14} />
            Open GitHub
          </button>
        </div>
        {configPath && <p className="mono settings__path">Settings file: {configPath}</p>}
      </section>
    </div>
  )
}
