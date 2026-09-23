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

export interface UploadOptions {
  default_folder: string | null
  folders: { video: string[]; image: string[] }
  games: Game[]
  /** Which folder each game's media belongs in, as Fireshare's scanner reads it. */
  folder_rules: { video: FolderRule[]; image: FolderRule[] }
}

export type MediaKind = 'video' | 'image'

/** What happens to the local file once the server confirms it has it. */
export type AfterUpload = 'keep' | 'trash' | 'delete'

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
}

export interface Settings {
  server_url: string | null
  folders: WatchedFolder[]
  notifications: {
    on_complete: boolean
    on_needs_attention: boolean
    quiet_in_fullscreen: boolean
    group_bursts: boolean
  }
  transfers: { max_concurrent: number; speed_cap: number | null }
  startup: { launch_at_login: boolean; start_in_tray: boolean }
  updates: { auto_install: boolean; prerelease: boolean }
}

export const api = {
  connect: (url: string, token: string) => invoke<Connection>('connect', { url, token }),
  connectionStatus: () => invoke<Connection | null>('connection_status'),
  disconnect: () => invoke<void>('disconnect'),
  uploadOptions: () => invoke<UploadOptions>('upload_options'),
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
  uploadExisting: (folderId: string, paths: string[]) =>
    invoke<number>('upload_existing', { folderId, paths }),
  problems: () => invoke<string[]>('watcher_problems'),
}

export const activity = {
  recent: (limit = 200) => invoke<ActivityRow[]>('recent_activity', { limit }),
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
  state: 'uploading' | 'done' | 'duplicate' | 'failed' | 'waiting' | 'paused'
  reason: string | null
  url: string | null
  landedAs: string | null
  removedLocal: string | null
  /** Current upload speed in bytes per second, on an `uploading` event. */
  bytesPerSecond: number | null
}

export interface QueueStatus {
  paused: boolean
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
}

export const backlog = {
  list: (folderId: string) => invoke<BacklogFile[]>('list_backlog', { folderId }),
  checkAgainstLibrary: (paths: string[]) =>
    invoke<[string, boolean][]>('check_backlog_against_library', { paths }),
  queue: (folderId: string, paths: string[]) =>
    invoke<number>('queue_backlog', { folderId, paths }),
}

export const tray = {
  openMain: () => invoke<void>('open_main_window'),
  openSettings: () => invoke<void>('open_main_at', { tab: 'settings' }),
  quit: () => invoke<void>('quit_app'),
}
