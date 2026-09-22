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

export interface UploadOptions {
  default_folder: string | null
  folders: { video: string[]; image: string[] }
  games: Game[]
}

export type MediaKind = 'video' | 'image'

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
}
