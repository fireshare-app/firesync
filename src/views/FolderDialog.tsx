import { useState } from 'react'
import { open } from '@tauri-apps/plugin-dialog'
import { Select } from '../components/Select'
import {
  asAppError,
  folders as foldersApi,
  type AfterUpload,
  type FolderSummary,
  type MediaKind,
  type UploadOptions,
} from '../lib/ipc'

const MB = 1024 * 1024
const GB = 1024 * MB

/** Blank means "no limit", which is not the same as zero. */
function parseLimit(value: string, unit: number): number | null {
  const trimmed = value.trim()
  if (!trimmed) return null
  const n = Number(trimmed)
  return Number.isFinite(n) && n > 0 ? Math.round(n * unit) : null
}

function formatLimit(bytes: number | null | undefined, unit: number): string {
  if (!bytes) return ''
  const n = bytes / unit
  return Number.isInteger(n) ? String(n) : n.toFixed(1)
}

interface Props {
  /** Editing an existing folder, or undefined when adding one. */
  folder?: FolderSummary
  options: UploadOptions | null
  onClose: () => void
  onSaved: () => void
}

export function FolderDialog({ folder, options, onClose, onSaved }: Props) {
  const editing = Boolean(folder)

  const [path, setPath] = useState(folder?.path ?? '')
  const [subfolders, setSubfolders] = useState(folder?.include_subfolders ?? false)
  const [video, setVideo] = useState(folder ? folder.media.includes('video') : true)
  const [images, setImages] = useState(folder?.media.includes('image') ?? false)
  const [dest, setDest] = useState(folder?.dest_folder ?? options?.default_folder ?? '')
  const [game, setGame] = useState(folder?.game ?? '')
  const [minMb, setMinMb] = useState(formatLimit(folder?.min_size_bytes, MB) || (editing ? '' : '5'))
  const [maxGb, setMaxGb] = useState(formatLimit(folder?.max_size_bytes, GB))
  const [after, setAfter] = useState<AfterUpload>(folder?.after_upload ?? 'keep')
  const [autoSort, setAutoSort] = useState(folder?.auto_sort_by_game ?? true)
  const [uploadExisting, setUploadExisting] = useState(false)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)

  // Which folder list to offer depends on what this folder sends: the server
  // keeps videos and images in separate trees, so a name valid for one is not
  // necessarily valid for the other.
  const folderChoices = images && !video ? options?.folders.image : options?.folders.video
  const imagesUnavailable = images && (options?.folders.image.length ?? 0) === 0

  const ruleList = images && !video ? options?.folder_rules?.image : options?.folder_rules?.video
  const sortedInto = game
    ? ruleList?.find((r) => r.game?.toLowerCase() === game.toLowerCase())?.folder
    : undefined

  async function browse() {
    setError(null)
    try {
      const picked = await open({ directory: true, multiple: false, title: 'Watch a folder' })
      if (typeof picked === 'string') setPath(picked)
    } catch (e) {
      setError(`${asAppError(e).message} You can paste the folder path instead.`)
    }
  }

  async function save(event: React.FormEvent) {
    event.preventDefault()
    setBusy(true)
    setError(null)

    const media: MediaKind[] = [
      ...(video ? (['video'] as MediaKind[]) : []),
      ...(images ? (['image'] as MediaKind[]) : []),
    ]
    const rules = {
      includeSubfolders: subfolders,
      media,
      destFolder: dest.trim() || null,
      game: game.trim() || null,
      minSizeBytes: parseLimit(minMb, MB),
      maxSizeBytes: parseLimit(maxGb, GB),
      afterUpload: after,
      autoSortByGame: autoSort,
    }

    try {
      if (folder) {
        await foldersApi.update(folder.id, rules)
      } else {
        await foldersApi.add({ path: path.trim(), ...rules, uploadExisting })
      }
      onSaved()
      onClose()
    } catch (e) {
      setError(asAppError(e).message)
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="modal" role="dialog" aria-modal="true" aria-label={editing ? 'Folder settings' : 'Add a watched folder'}>
      <form className="modal__box modal__box--narrow" onSubmit={save}>
        <header className="modal__head">
          <div>
            <h2 className="modal__title">{editing ? 'Folder settings' : 'Add a watched folder'}</h2>
            <p className="modal__sub">
              {editing
                ? 'Applies to files from now on. Anything already decided keeps its verdict.'
                : 'These settings apply to every file this folder sends.'}
            </p>
          </div>
        </header>

        <div className="modal__body">
          <div className="field">
            <label className="field__label" htmlFor="fd-path">
              Folder on this machine
            </label>
            {editing ? (
              <p className="field__fixed mono">{path}</p>
            ) : (
              <div className="addbar__row">
                <input
                  id="fd-path"
                  type="text"
                  className="addbar__input mono"
                  placeholder="C:\Users\you\Games\clips"
                  spellCheck={false}
                  autoFocus
                  value={path}
                  onChange={(e) => setPath(e.target.value)}
                />
                <button type="button" className="btn btn--ghost" onClick={browse}>
                  Browse…
                </button>
              </div>
            )}
            {editing && (
              <span className="field__hint">
                The path is the folder's identity — every file it has seen is recorded against it.
                Watching somewhere else means adding a folder.
              </span>
            )}
            <label className="inline-check" htmlFor="fd-sub">
              <input
                id="fd-sub"
                type="checkbox"
                checked={subfolders}
                onChange={(e) => setSubfolders(e.target.checked)}
              />
              Watch subfolders too
            </label>
          </div>

          <div className="row2">
            <div className="field">
              <label className="field__label" htmlFor="fd-dest">
                Fireshare folder
              </label>
              {autoSort ? (
                <p className="field__fixed mono">
                  {sortedInto ?? options?.default_folder ?? 'uploads'}
                </p>
              ) : (
                <Select
                  id="fd-dest"
                  mono
                  value={dest}
                  placeholder={options?.default_folder ?? 'uploads'}
                  options={(folderChoices ?? []).map((f) => ({ value: f, label: f }))}
                  onChange={setDest}
                  customLabel="Use a folder that does not exist yet…"
                />
              )}
              <span className="field__hint">
                {autoSort
                  ? sortedInto
                    ? `Chosen by the game. Fireshare keeps ${game} here.`
                    : game
                      ? `${game} has no folder of its own yet, so uploads use the default.`
                      : 'Pick a game, or turn auto-sort off to choose a folder.'
                  : 'One level only — a slash becomes a dash on the server.'}
              </span>
            </div>

            <div className="field">
              <label className="field__label" htmlFor="fd-game">
                Game <span className="field__optional">(optional)</span>
              </label>
              <Select
                id="fd-game"
                value={game}
                placeholder="No game"
                options={[
                  { value: '', label: 'No game' },
                  ...(options?.games ?? []).map((g) => ({ value: g.name, label: g.name })),
                ]}
                onChange={setGame}
              />
              <span className="field__hint">
                {options
                  ? 'Every game in your library. Firesync never creates new ones.'
                  : 'Connect to load your library’s games.'}
              </span>
              <label className="inline-check" htmlFor="fd-autosort">
                <input
                  id="fd-autosort"
                  type="checkbox"
                  checked={autoSort}
                  onChange={(e) => setAutoSort(e.target.checked)}
                />
                Auto-sort into game folder
              </label>
            </div>
          </div>

          <div className="field">
            <span className="field__label">What to pick up</span>
            <div className="row2">
              <label className={`choice ${video ? 'choice--on' : ''}`} htmlFor="fd-video">
                <input
                  id="fd-video"
                  type="checkbox"
                  checked={video}
                  onChange={(e) => setVideo(e.target.checked)}
                />
                <span>
                  <span className="choice__name">Videos</span>
                  <span className="choice__types mono">mp4 · m4v · mov · webm</span>
                </span>
              </label>
              <label className={`choice ${images ? 'choice--on' : ''}`} htmlFor="fd-images">
                <input
                  id="fd-images"
                  type="checkbox"
                  checked={images}
                  onChange={(e) => setImages(e.target.checked)}
                />
                <span>
                  <span className="choice__name">Images</span>
                  <span className="choice__types mono">png · jpg · jpeg · webp · gif</span>
                </span>
              </label>
            </div>
            {!video && !images && (
              <span className="field__warn">Pick at least one, or nothing will ever upload.</span>
            )}
            {imagesUnavailable && (
              <span className="field__warn">
                This instance has no image directory configured, so images will be refused.
              </span>
            )}
          </div>

          <div className="field">
            <span className="field__label">Size limits</span>
            <div className="row2">
              <div className="limit">
                <label htmlFor="fd-min">Skip anything smaller than</label>
                <span className="limit__input">
                  <input
                    id="fd-min"
                    type="text"
                    inputMode="decimal"
                    value={minMb}
                    onChange={(e) => setMinMb(e.target.value)}
                  />
                  <span>MB</span>
                </span>
                <span className="field__hint">Keeps out stray thumbnails.</span>
              </div>
              <div className="limit">
                <label htmlFor="fd-max">Skip anything larger than</label>
                <span className="limit__input">
                  <input
                    id="fd-max"
                    type="text"
                    inputMode="decimal"
                    value={maxGb}
                    onChange={(e) => setMaxGb(e.target.value)}
                  />
                  <span>GB</span>
                </span>
                <span className="field__hint">Leave blank for no cap.</span>
              </div>
            </div>
          </div>

          <div className="field">
            <label className="field__label" htmlFor="fd-after">
              After a successful upload
            </label>
            <Select
              id="fd-after"
              value={after}
              options={[
                { value: 'keep', label: 'Keep the local file' },
                { value: 'trash', label: 'Move it to the trash' },
                { value: 'delete', label: 'Delete it' },
              ]}
              onChange={(v) => setAfter(v as AfterUpload)}
            />
            {after !== 'keep' && (
              <span className="field__hint">
                Only once Fireshare confirms it has the file. Nothing is removed after a failure.
              </span>
            )}
          </div>

          {!editing && (
            <div className="field">
              <span className="field__label">Files already in this folder</span>
              <label className={`choice ${!uploadExisting ? 'choice--on' : ''}`} htmlFor="fd-skip">
                <input
                  id="fd-skip"
                  type="radio"
                  name="existing"
                  checked={!uploadExisting}
                  onChange={() => setUploadExisting(false)}
                />
                <span>
                  <span className="choice__name">Leave them alone</span>
                  <span className="choice__types">
                    Only files added from now on upload. You can send these later.
                  </span>
                </span>
              </label>
              <label className={`choice ${uploadExisting ? 'choice--on' : ''}`} htmlFor="fd-send">
                <input
                  id="fd-send"
                  type="radio"
                  name="existing"
                  checked={uploadExisting}
                  onChange={() => setUploadExisting(true)}
                />
                <span>
                  <span className="choice__name">Upload all of them now</span>
                  <span className="choice__types">
                    Everything matching the rules above is queued immediately.
                  </span>
                </span>
              </label>
            </div>
          )}

          {error && <div className="banner banner--bad">{error}</div>}
        </div>

        <footer className="modal__foot">
          <span className="spacer" />
          <button type="button" className="btn btn--ghost" onClick={onClose}>
            Cancel
          </button>
          <button
            type="submit"
            className="btn btn--primary"
            disabled={busy || (!editing && !path.trim()) || (!video && !images)}
          >
            {busy ? 'Saving…' : editing ? 'Save changes' : 'Start watching'}
          </button>
        </footer>
      </form>
    </div>
  )
}
