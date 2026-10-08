import { invoke } from '@tauri-apps/api/core'

/** Mirrors AppError in src-tauri/src/error.rs. */
export type AppErrorKind =
  | 'bad_url'
  | 'unreachable'
  | 'not_fireshare'
  | 'token_rejected'
  | 'throttled'
  | 'server'
  | 'keychain'
  | 'storage'
  | 'not_connected'

export interface AppError {
  kind: AppErrorKind
  message: string
}

/** A rejected invoke carries the serialised AppError, not an Error instance. */
export function asAppError(e: unknown): AppError {
  if (e && typeof e === 'object' && 'kind' in e && 'message' in e) {
    return e as AppError
  }
  return { kind: 'server', message: String(e) }
}

export interface TokenCheck {
  ok: boolean
  username: string
  default_folder: string | null
  images_enabled: boolean
  supported_video_types: string[]
  supported_image_types: string[]
}

export interface Connection {
  serverUrl: string
  check: TokenCheck
  /** False when these are the server's last answers rather than fresh ones. */
  verified: boolean
  /** Why the server could not be re-checked, when it could not. */
  problem: string | null
}

export interface Game {
  id: number
  name: string
  steamgriddb_id: number | null
}

export interface FolderRule {
  folder: string
  game_id: number | null
  game: string | null
}

/** A Fireshare tag an upload may name. */
export interface Tag {
  id: number
  name: string
  /** A hex colour, when it has one. */
  color: string | null
}

export interface UploadOptions {
  default_folder: string | null
  folders: { video: string[]; image: string[] }
  games: Game[]
  /** Missing on a Fireshare too old to offer tags to upload tokens. */
  tags?: Tag[] | null
  /** Which folder each game's media belongs in, as Fireshare's scanner reads it. */
  folder_rules: { video: FolderRule[]; image: FolderRule[] }
}

/**
 * What Fireshare last said it will accept, and how recently.
 *
 * The core holds this, not the window, and refreshes it whenever something is
 * about to rely on it. A failed refresh keeps the old list and says why.
 */
export interface OptionsSnapshot {
  options: UploadOptions | null
  /** Unix seconds of Fireshare's last answer. */
  fetchedAt: number | null
  /** Why the latest refresh failed, until one succeeds. */
  error: string | null
}

export const uploadOptions = {
  /** Whatever the core holds right now. Never touches the network. */
  cached: () => invoke<OptionsSnapshot>('upload_options'),
  /** Ask Fireshare again. Resolves with the list either way; see `error`. */
  refresh: () => invoke<OptionsSnapshot>('refresh_options'),
}

export type MediaKind = 'video' | 'image'

/** What happens to the local file once the server confirms it has it. */
export type AfterUpload = 'keep' | 'trash' | 'delete'

/** A subfolder's game, chosen by hand. `game` null: sent without a game. */
export interface SubfolderGame {
  subfolder: string
  game: string | null
}

/**
 * How a subfolder's game was settled on: chosen in the settings, matched by
 * its name, chosen to have none, or neither — in which case its clips wait.
 */
export type SubfolderHow = 'chosen' | 'matched' | 'none' | 'unmatched'

export interface SubfolderStatus {
  name: string
  /** The game its clips are sent as, when that is settled. */
  game: string | null
  how: SubfolderHow
  /** What matching by name gives, whatever was chosen. */
  matched: string | null
  /** Still on disk. A choice for a subfolder that has gone is kept and shown. */
  present: boolean
}

export interface WatchedFolder {
  id: string
  path: string
  enabled: boolean
  include_subfolders: boolean
  media: MediaKind[]
  dest_folder: string | null
  game: string | null
  min_size_bytes: number | null
  max_size_bytes: number | null
  after_upload: AfterUpload
  auto_sort_by_game: boolean
  /** Each clip's game comes from the subfolder it is in. Implies subfolders. */
  game_from_subfolder: boolean
  subfolder_games: SubfolderGame[]
  /** e.g. `{game} — {date}`. Null keeps the file name as the title. */
  title_template: string | null
  tag_ids: number[]
  watch_mode: WatchMode
}

/** How a folder notices new files: change events, or a timed scan. */
export type WatchMode = 'auto' | 'events' | 'scan'

export interface Settings {
  server_url: string | null
  folders: WatchedFolder[]
  notifications: {
    on_complete: boolean
    on_needs_attention: boolean
    quiet_in_fullscreen: boolean
    group_bursts: boolean
    copy_link_on_complete: boolean
  }
  transfers: {
    max_concurrent: number
    /** Bytes per second, shared by every upload, or null for none. */
    speed_cap: number | null
    while_playing: 'full' | 'limit' | 'pause'
    /** Bytes per second while playing, when `while_playing` is `limit`. */
    while_playing_cap: number | null
  }
  startup: { launch_at_login: boolean; start_in_tray: boolean }
  updates: { auto_install: boolean }
}

export const api = {
  connect: (url: string, token: string) => invoke<Connection>('connect', { url, token }),
  connectionStatus: () => invoke<Connection | null>('connection_status'),
  disconnect: () => invoke<void>('disconnect'),
  getSettings: () => invoke<Settings>('get_settings'),
  saveSettings: (settings: Settings) => invoke<void>('save_settings', { settings }),
  configLocation: () => invoke<string>('config_location'),
  setLaunchAtLogin: (enabled: boolean) => invoke<boolean>('set_launch_at_login', { enabled }),
  launchAtLoginState: () => invoke<boolean>('launch_at_login_state'),
}

// --- Phase 2: ledger + watcher -------------------------------------------

export type FileState =
  | 'baseline'
  | 'queued'
  | 'uploading'
  | 'done'
  | 'duplicate'
  | 'skipped'
  | 'failed'

export interface FileRow {
  id: number
  folderId: string
  path: string
  size: number
  mtime: number
  state: FileState
  reason: string | null
  attempts: number
  /** Fireshare's id for these bytes, once it has been computed. */
  contentHash: string | null
  /** When a file waiting to retry is next due, in unix seconds. */
  nextTryAt: number | null
  observedAt: number
  updatedAt: number
}

export interface ActivityRow extends FileRow {
  /**
   * The page this landed on in Fireshare, when there is one. Built in Rust,
   * where the server address and the extension lists already live.
   */
  link: string | null
}

export interface FolderSummary extends WatchedFolder {
  counts: [string, number][]
  /** Media files in the folder right now, counted from disk. */
  presentCount: number
  /** Unix seconds when something from this folder last reached the server. */
  lastUploadAt: number | null
  /** Files held for review rather than uploaded: turned up during a pause, say. */
  held: number
  /** Why they were held, when every one shares a reason. */
  heldReason: string | null
  availability: 'watching' | 'unavailable' | 'paused'
  /** Why this folder is not being watched, when it is not. */
  problem: string | null
  /** Seconds between scans, for a folder scanned rather than watched. */
  scannedEvery: number | null
  /** Whether it is on a network drive, for the dialog to explain "automatically". */
  network: boolean
  /** Each subfolder and its game, for a folder that keeps one per game. Empty otherwise. */
  subfolders: SubfolderStatus[]
}

export interface FolderRules {
  includeSubfolders?: boolean
  media?: MediaKind[]
  destFolder?: string | null
  game?: string | null
  minSizeBytes?: number | null
  maxSizeBytes?: number | null
  afterUpload?: AfterUpload
  autoSortByGame?: boolean
  gameFromSubfolder?: boolean
  subfolderGames?: SubfolderGame[]
  titleTemplate?: string | null
  tagIds?: number[]
  watchMode?: WatchMode
}

/** What a folder's newest file would be titled. */
export interface TitlePreview {
  /** Null: Fireshare would title it by its file name. */
  title: string | null
  /** The file the preview was made from, or null for a made-up example. */
  from: string | null
}

export interface NewFolder extends FolderRules {
  path: string
  uploadExisting?: boolean
}

/** Pushed from Rust when the watcher settles on a file. */
export interface Decision {
  folderId: string
  path: string
  outcome: 'queued' | 'skipped' | 'requeued' | 'unchanged' | 'held'
  reason: string | null
  size: number
  at: number
}

export const folders = {
  list: () => invoke<FolderSummary[]>('list_folders'),
  add: (folder: NewFolder) => invoke<FolderSummary>('add_folder', { folder }),
  remove: (id: string) => invoke<void>('remove_folder', { id }),
  setEnabled: (id: string, enabled: boolean) =>
    invoke<void>('set_folder_enabled', { id, enabled }),
  setAfterUpload: (id: string, afterUpload: AfterUpload) =>
    invoke<void>('set_folder_after_upload', { id, afterUpload }),
  update: (id: string, rules: FolderRules) => invoke<void>('update_folder', { id, rules }),
  detectNetwork: (path: string) => invoke<boolean>('detect_network', { path }),
  /**
   * The subfolders of a folder, added or not, each with the game it would send
   * its clips as given these choices. The core does the matching, the same
   * way it does for an upload, so this cannot disagree with what is sent.
   */
  listSubfolders: (path: string, subfolderGames: SubfolderGame[]) =>
    invoke<SubfolderStatus[]>('list_subfolders', { path, subfolderGames }),
  previewTitle: (
    template: string,
    path: string,
    game: string | null,
    includeSubfolders: boolean,
    gameFromSubfolder: boolean,
    subfolderGames: SubfolderGame[],
  ) =>
    invoke<TitlePreview>('preview_title', {
      template,
      path,
      game,
      includeSubfolders,
      gameFromSubfolder,
      subfolderGames,
    }),
  uploadExisting: (folderId: string, paths: string[]) =>
    invoke<number>('upload_existing', { folderId, paths }),
}

export type ActivityTab = 'all' | 'progress' | 'attention' | 'finished'

/** Counted in the ledger under the same filter as the page, not from the page. */
export interface ActivityCounts {
  all: number
  progress: number
  attention: number
  finished: number
  uploadedFiles: number
  uploadedBytes: number
}

export interface ActivityPage {
  rows: ActivityRow[]
  counts: ActivityCounts
}

export interface ActivityQuery {
  tab: ActivityTab
  /** Part of a file name. */
  name?: string | null
  folderId?: string | null
  limit: number
}

/**
 * The feed, and what can be done to one file in it. Each action answers false
 * when the file was no longer in a state it applies to — it finished, say —
 * which is a reason to refresh rather than an error.
 */
export const activity = {
  page: (query: ActivityQuery) => invoke<ActivityPage>('activity_page', { query }),
  retry: (id: number) => invoke<boolean>('retry_file', { id }),
  skip: (id: number) => invoke<boolean>('skip_file', { id }),
  stop: (id: number) => invoke<boolean>('stop_upload', { id }),
  uploadAnyway: (id: number) => invoke<boolean>('upload_anyway', { id }),
  reveal: (id: number) => invoke<void>('reveal_file', { id }),
}

/** What a test notification found out. */
export interface NotificationProbe {
  /** The platform accepted the toast. It may still be hidden downstream. */
  delivered: boolean
  error: string | null
  /** Something owns the screen right now. */
  screenBusy: boolean
  /** A real notification arriving this second would have been held back. */
  wouldHold: boolean
}

/** One published release, for the "what's new" panel. */
export interface Release {
  version: string
  name: string
  notes: string
  publishedAt: string | null
  url: string
  prerelease: boolean
}

export const releases = {
  history: (limit = 15) => invoke<Release[]>('release_history', { limit }),
}

export const diagnostics = {
  /**
   * Everything a bug report needs, as text to paste. Home folder, usernames and
   * (unless asked for) the server address are masked; the token never appears.
   */
  report: (includeServer: boolean) => invoke<string>('diagnostics_report', { includeServer }),
  logLocation: () => invoke<string>('log_location'),
  openLogFolder: () => invoke<void>('open_log_dir'),
}

export const notifications = {
  test: () => invoke<NotificationProbe>('test_notification'),
}

// --- Phase 3: the upload queue -------------------------------------------

/** Pushed from Rust as each upload resolves. */
export interface UploadEvent {
  id: number
  path: string
  size: number
  /** Bytes handed to the socket so far, on an `uploading` event. */
  sent: number
  state: 'uploading' | 'done' | 'duplicate' | 'failed' | 'waiting' | 'paused' | 'skipped'
  reason: string | null
  url: string | null
  landedAs: string | null
  removedLocal: string | null
  /** Current upload speed in bytes per second, on an `uploading` event. */
  bytesPerSecond: number | null
  /** Why it is going slower than it could: the speed limit, or a game. */
  limit: 'limit' | 'playing' | null
}

export interface QueueStatus {
  paused: boolean
  /** Waiting because a game has the screen. Not a pause, and has no Resume. */
  held: boolean
  pauseReason: string | null
  queued: number
  uploading: number
  failed: number
}

export const queue = {
  status: () => invoke<QueueStatus>('queue_status'),
  pause: () => invoke<void>('pause_queue'),
  resume: () => invoke<void>('resume_queue'),
  retryFailed: () => invoke<number>('retry_failed'),
}

// --- Phase 7: updates -----------------------------------------------------

export interface UpdateInfo {
  version: string
  currentVersion: string
  notes: string | null
  date: string | null
}

export const updates = {
  check: () => invoke<UpdateInfo | null>('check_for_updates'),
  install: () => invoke<void>('install_update'),
  blockedByUpload: () => invoke<boolean>('update_blocked_by_upload'),
}

// --- Phase 6: the backlog -------------------------------------------------

export interface BacklogFile {
  path: string
  name: string
  size: number
  mtime: number
  /** Why this folder's rules would exclude it, if they would. */
  excluded: string | null
  /** Filled in once the library has been asked. */
  inLibrary: boolean | null
  /** Why it was held for review, for a file that did not predate the folder. */
  held: string | null
}

export const backlog = {
  list: (folderId: string) => invoke<BacklogFile[]>('list_backlog', { folderId }),
  checkAgainstLibrary: (paths: string[]) =>
    invoke<[string, boolean][]>('check_backlog_against_library', { paths }),
  queue: (folderId: string, paths: string[]) =>
    invoke<number>('queue_backlog', { folderId, paths }),
}

/** A finished upload with a page to open, for the tray. */
export interface RecentLink {
  id: number
  name: string
  link: string
  /** When it finished, in unix seconds. */
  at: number
}

export const tray = {
  recentLinks: (limit = 3) => invoke<RecentLink[]>('recent_links', { limit }),
  openMain: () => invoke<void>('open_main_window'),
  openSettings: () => invoke<void>('open_main_at', { tab: 'settings' }),
  quit: () => invoke<void>('quit_app'),
}
