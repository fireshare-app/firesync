import { useEffect, useRef, useState } from 'react'
import { open } from '@tauri-apps/plugin-dialog'
import { Select } from '../components/Select'
import { TagPicker } from '../components/TagPicker'
import {
  asAppError,
  folders as foldersApi,
  type AfterUpload,
  type FolderSummary,
  type MediaKind,
  type SubfolderGame,
  type SubfolderStatus,
  type TitlePreview,
  type WatchMode,
} from '../lib/ipc'
import { pickerStatus, useUploadOptions } from '../lib/useUploadOptions'

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

/** What a title template can say, in the order the chips offer them. */
const TOKENS = ['{game}', '{date}', '{time}', '{filename}', '{folder}']

/** The name a file would be titled by when there is no template: its stem. */
function stem(name: string) {
  const dot = name.lastIndexOf('.')
  return dot > 0 ? name.slice(0, dot) : name
}

/** The two choices for a subfolder that are not a game. */
const MATCH_BY_NAME = 'auto'
const NO_GAME = 'none'
const GAME = 'game:'

/** What a subfolder's row shows selected: the choice made for it, or none. */
function choiceValue(s: SubfolderStatus) {
  if (s.how === 'chosen' && s.game) return GAME + s.game
  if (s.how === 'none') return NO_GAME
  return MATCH_BY_NAME
}

interface Props {
  /** Editing an existing folder, or undefined when adding one. */
  folder?: FolderSummary
  onClose: () => void
  onSaved: () => void
}

export function FolderDialog({ folder, onClose, onSaved }: Props) {
  const editing = Boolean(folder)

  // The lists this dialog offers are asked for again as it opens, and again as
  // each picker opens: somebody who has just added a game in Fireshare and
  // come here to choose it should find it, without reloading anything.
  const fresh = useUploadOptions()
  const { options, refresh } = fresh
  useEffect(() => {
    void refresh()
  }, [refresh])
  const listStatus = pickerStatus(fresh)

  const [path, setPath] = useState(folder?.path ?? '')
  const [subfolders, setSubfolders] = useState(folder?.include_subfolders ?? false)
  const [perGame, setPerGame] = useState(folder?.game_from_subfolder ?? false)
  const [choices, setChoices] = useState<SubfolderGame[]>(folder?.subfolder_games ?? [])
  const [gameFolders, setGameFolders] = useState<SubfolderStatus[]>(folder?.subfolders ?? [])
  const [video, setVideo] = useState(folder ? folder.media.includes('video') : true)
  const [images, setImages] = useState(folder?.media.includes('image') ?? false)
  const [dest, setDest] = useState(folder?.dest_folder ?? options?.default_folder ?? '')
  const [game, setGame] = useState(folder?.game ?? '')
  const [minMb, setMinMb] = useState(formatLimit(folder?.min_size_bytes, MB) || (editing ? '' : '5'))
  const [maxGb, setMaxGb] = useState(formatLimit(folder?.max_size_bytes, GB))
  const [after, setAfter] = useState<AfterUpload>(folder?.after_upload ?? 'keep')
  const [autoSort, setAutoSort] = useState(folder?.auto_sort_by_game ?? true)
  const [uploadExisting, setUploadExisting] = useState(false)
  const [template, setTemplate] = useState(folder?.title_template ?? '')
  const [preview, setPreview] = useState<TitlePreview | null>(null)
  const [tagIds, setTagIds] = useState<number[]>(folder?.tag_ids ?? [])
  const [watchMode, setWatchMode] = useState<WatchMode>(folder?.watch_mode ?? 'auto')
  const [network, setNetwork] = useState(folder?.network ?? false)
  const titleRef = useRef<HTMLInputElement>(null)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)

  // A new folder starts on the server's default. The list may arrive a moment
  // after the dialog does, so the default is filled in when it lands rather
  // than read once at the first render.
  const defaultFolder = options?.default_folder
  useEffect(() => {
    if (!folder && defaultFolder) setDest((current) => current || defaultFolder)
  }, [folder, defaultFolder])

  // Rendered by the core, the way an upload will be, so the two cannot
  // disagree. Settled typing only.
  useEffect(() => {
    if (!template.trim()) {
      setPreview(null)
      return
    }
    const t = setTimeout(() => {
      foldersApi
        .previewTitle(template, path, game.trim() || null, subfolders, perGame, choices)
        .then(setPreview)
        .catch(() => setPreview(null))
    }, 250)
    return () => clearTimeout(t)
  }, [template, path, game, subfolders, perGame, choices])

  // What each subfolder would send its clips as, from the core, which does
  // the same matching for uploads — so what this shows is what saving means.
  // Asked again as the path, the choices, or the library changes.
  const fetchedAt = fresh.fetchedAt
  useEffect(() => {
    if (!perGame || !path.trim()) {
      setGameFolders([])
      return
    }
    let live = true
    const t = setTimeout(() => {
      foldersApi
        .listSubfolders(path.trim(), choices)
        .then((list) => live && setGameFolders(list))
        .catch(() => live && setGameFolders([]))
    }, 200)
    return () => {
      live = false
      clearTimeout(t)
    }
  }, [perGame, path, choices, fetchedAt])

  /** Record what a subfolder's row was set to. "Match by name" is no record. */
  function choose(subfolder: string, value: string) {
    setChoices((prev) => {
      const rest = prev.filter((c) => c.subfolder.toLowerCase() !== subfolder.toLowerCase())
      if (value === MATCH_BY_NAME) return rest
      return [...rest, { subfolder, game: value === NO_GAME ? null : value.slice(GAME.length) }]
    })
  }

  /** Whether a game a folder names is still in the library, on a list fresh enough to say. */
  function inLibrary(name: string | null) {
    if (!name || !options || fresh.checking || fresh.error) return true
    return options.games.some((g) => g.name.toLowerCase() === name.toLowerCase())
  }

  // For a folder being added: whether "automatically" would mean scanning.
  useEffect(() => {
    if (folder || !path.trim()) return
    const t = setTimeout(() => {
      foldersApi
        .detectNetwork(path.trim())
        .then(setNetwork)
        .catch(() => setNetwork(false))
    }, 300)
    return () => clearTimeout(t)
  }, [folder, path])

  /** Put a token where the cursor is, not at the end. */
  function insertToken(token: string) {
    const input = titleRef.current
    const from = input?.selectionStart ?? template.length
    const to = input?.selectionEnd ?? template.length
    const before = template.slice(0, from)
    const after = template.slice(to)
    // A space either side, unless one is already there, so two tokens never
    // run together into something that reads as one.
    const pad = before && !before.endsWith(' ') ? ' ' : ''
    const padAfter = after && !after.startsWith(' ') ? ' ' : ''
    const next = before + pad + token + padAfter + after
    setTemplate(next)
    requestAnimationFrame(() => {
      const at = (before + pad + token).length
      input?.focus()
      input?.setSelectionRange(at, at)
    })
  }

  const watchHint =
    watchMode === 'scan'
      ? 'Firesync lists the folder every 15 seconds and compares it with what it has already seen.'
      : watchMode === 'events'
        ? 'Files are noticed the moment they appear.'
        : network
          ? 'This folder is on a network drive, where change events can’t be relied on, so Firesync lists it every 15 seconds instead. New clips show up within about 20 seconds.'
          : 'Files are noticed the moment they appear. A folder on a network drive would be checked every 15 seconds instead.'

  // Which folder list to offer depends on what this folder sends: the server
  // keeps videos and images in separate trees, so a name valid for one is not
  // necessarily valid for the other.
  const folderChoices = images && !video ? options?.folders.image : options?.folders.video
  const imagesUnavailable = images && (options?.folders.image.length ?? 0) === 0

  const ruleList = images && !video ? options?.folder_rules?.image : options?.folder_rules?.video
  const sortedInto = game
    ? ruleList?.find((r) => r.game?.toLowerCase() === game.toLowerCase())?.folder
    : undefined

  // A game renamed or deleted in Fireshare. Only said on a list this dialog
  // can trust — just fetched, not mid-check and not a failed refresh — because
  // a stale list would miss a game added since and warn about nothing.
  const gameGone = !perGame && Boolean(game) && !inLibrary(game)

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
      includeSubfolders: subfolders || perGame,
      media,
      destFolder: dest.trim() || null,
      game: game.trim() || null,
      minSizeBytes: parseLimit(minMb, MB),
      maxSizeBytes: parseLimit(maxGb, GB),
      afterUpload: after,
      autoSortByGame: autoSort,
      gameFromSubfolder: perGame,
      // Kept even when the mode is off, so turning it back on finds them.
      subfolderGames: choices,
      titleTemplate: template.trim() || null,
      tagIds,
      watchMode,
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
                checked={subfolders || perGame}
                disabled={perGame}
                onChange={(e) => setSubfolders(e.target.checked)}
              />
              Watch subfolders too
            </label>
            <label className="inline-check" htmlFor="fd-pergame">
              <input
                id="fd-pergame"
                type="checkbox"
                checked={perGame}
                onChange={(e) => setPerGame(e.target.checked)}
              />
              Each subfolder is a game
            </label>
            {perGame && (
              <span className="field__hint">
                For a recorder that keeps a folder per game, such as Segra or ShadowPlay. Each clip
                is tagged with the game named like the folder it&rsquo;s in, and a game you play for
                the first time needs nothing added here. Choose the game yourself for any folder
                that&rsquo;s spelled differently, below.
              </span>
            )}
          </div>

          <div className="field">
            <label className="field__label" htmlFor="fd-watch">
              Noticing new files
            </label>
            <Select
              id="fd-watch"
              value={watchMode}
              options={[
                { value: 'auto', label: 'Automatically', note: network ? 'every 15 s here' : undefined },
                { value: 'events', label: 'As soon as the folder changes' },
                { value: 'scan', label: 'Every 15 seconds' },
              ]}
              onChange={(v) => setWatchMode(v as WatchMode)}
            />
            <span className="field__hint">{watchHint}</span>
            {watchMode === 'events' && network && (
              <span className="field__warn">
                Change events over a network drive can miss files. Choose this only if your NAS is
                known to send them.
              </span>
            )}
          </div>

          <div className="row2">
            <div className="field">
              <label className="field__label" htmlFor="fd-dest">
                Fireshare folder
              </label>
              {autoSort && perGame ? (
                <p className="field__fixed">By game</p>
              ) : autoSort ? (
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
                  onOpen={() => void refresh()}
                  status={listStatus}
                />
              )}
              <span className="field__hint">
                {autoSort && perGame
                  ? `Each clip goes to the folder Fireshare keeps for its game. One whose game has no folder of its own goes to ${dest.trim() || options?.default_folder || 'uploads'}.`
                  : autoSort
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
                Game {!perGame && <span className="field__optional">(optional)</span>}
              </label>
              {perGame ? (
                <>
                  <p className="field__fixed">From each subfolder&rsquo;s name</p>
                  <span className="field__hint">Matched against your library, or chosen below.</span>
                </>
              ) : (
                <>
              <Select
                id="fd-game"
                value={game}
                placeholder="No game"
                options={[
                  { value: '', label: 'No game' },
                  ...(options?.games ?? []).map((g) => ({ value: g.name, label: g.name })),
                ]}
                onChange={setGame}
                onOpen={() => void refresh()}
                status={listStatus}
              />
              {gameGone ? (
                <span className="field__warn">
                  {game} isn&rsquo;t in your library any more. It was renamed or deleted in
                  Fireshare, so uploads from this folder would be refused. Pick another game, or
                  No game.
                </span>
              ) : (
                <span className="field__hint">
                  {options
                    ? 'Every game in your library. Firesync never creates new ones.'
                    : 'Connect to load your library’s games.'}
                </span>
              )}
                </>
              )}
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

          {perGame && (
            <div className="field">
              <span className="field__label">Games by subfolder</span>
              {gameFolders.length === 0 ? (
                <span className="field__hint">
                  {path.trim()
                    ? 'No subfolders here yet. Each one that appears is matched by its name.'
                    : 'Choose the folder above to see its subfolders.'}
                </span>
              ) : (
                <div className="submap">
                  {gameFolders.map((s) => {
                    const chosenGone = s.how === 'chosen' && !inLibrary(s.game)
                    const games = options?.games ?? []
                    return (
                      <div key={s.name} className="submap__row">
                        <span className="submap__name">
                          <span className="mono">{s.name}</span>
                          {!s.present && <span className="submap__gone">not here any more</span>}
                        </span>
                        <Select
                          value={choiceValue(s)}
                          options={[
                            { value: MATCH_BY_NAME, label: 'Match by name', note: s.matched ?? 'no match' },
                            { value: NO_GAME, label: 'No game' },
                            ...games.map((g) => ({ value: GAME + g.name, label: g.name })),
                            // A game chosen here but gone from the library still
                            // reads as itself rather than as nothing.
                            ...(chosenGone && s.game ? [{ value: GAME + s.game, label: s.game }] : []),
                          ]}
                          onChange={(v) => choose(s.name, v)}
                          onOpen={() => void refresh()}
                          status={listStatus}
                        />
                        {s.how === 'unmatched' && (
                          <span className="field__warn submap__note">
                            Nothing in your library is named like this, so its clips wait. Choose a
                            game, or add one in Fireshare.
                          </span>
                        )}
                        {chosenGone && (
                          <span className="field__warn submap__note">
                            {s.game} isn&rsquo;t in your library any more, so uploads from here would
                            be refused.
                          </span>
                        )}
                      </div>
                    )
                  })}
                </div>
              )}
              <span className="field__hint">
                A subfolder named like nothing in your library holds its clips back until you choose
                a game for it or add one in Fireshare; they go on their own once you do.
              </span>
            </div>
          )}

          <div className="field">
            <label className="field__label" htmlFor="fd-title">
              Title <span className="field__optional">(optional)</span>
            </label>
            <input
              id="fd-title"
              ref={titleRef}
              type="text"
              className="mono"
              placeholder="Use the file name"
              spellCheck={false}
              value={template}
              onChange={(e) => setTemplate(e.target.value)}
            />
            <div className="tokens">
              {TOKENS.map((token) => (
                <button key={token} type="button" className="token" onClick={() => insertToken(token)}>
                  {token}
                </button>
              ))}
            </div>
            {template.trim() && preview ? (
              <span className="preview">
                {preview.from ? 'The newest file here would be titled ' : 'An upload would be titled '}
                <strong>{preview.title ?? (preview.from ? stem(preview.from) : 'its file name')}</strong>
                {preview.from && <span className="mono"> · from {preview.from}</span>}
              </span>
            ) : null}
            <span className="field__hint">
              {template.trim()
                ? 'Date and time are when the recording finished, in your time zone. A token with nothing to fill it is dropped along with its separator.'
                : 'Leave it empty and Fireshare titles each upload by its file name.'}
            </span>
          </div>

          <div className="field">
            <span className="field__label">
              Tags <span className="field__optional">(optional)</span>
            </span>
            <TagPicker
              offered={options?.tags}
              chosen={tagIds}
              onChange={setTagIds}
              onOpen={() => void refresh()}
              status={listStatus}
            />
            {options?.tags && (
              <span className="field__hint">
                {tagIds.some((id) => !options.tags!.some((t) => t.id === id))
                  ? 'A tag this folder used is no longer offered by Fireshare: deleted, or now only on private media. It is left off uploads.'
                  : 'Every upload from this folder gets these tags.'}
              </span>
            )}
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
