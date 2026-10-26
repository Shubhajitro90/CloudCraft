import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import type { QuotaPolicy, QuotaSnapshot, QuotaWarning } from './cloudQuota'

export const QUOTA_WARNING_EVENT = 'cloudcraft-cloud-quota-warning'

export type CloudSessionState =
  | 'unavailable'
  | 'idle'
  | 'starting'
  | 'ready'
  | 'stopping'
  | 'stopped'
  | 'error'

export interface CloudSessionStatus {
  state: CloudSessionState
  message: string
  sessionId?: string | null
  providerId?: string
  updatedAt?: string | null
  canStart: boolean
  canStop: boolean
  canCancel: boolean
  canReconnect?: boolean
  cleanupRequired: boolean
  /** Both usage counters; `canStart` already accounts for `quota.canLaunch`. */
  quota?: QuotaSnapshot
}

export interface CloudHostConfig {
  host: string
  apiPort: number
  username: string
  certSha256: string
  launchCommand: string
  workingDir: string | null
  moonlightPath: string
  resolution: string | null
  fps: number | null
}

export interface CloudHostView {
  config: CloudHostConfig | null
  unlocked: boolean
  moonlightFound: boolean
  streaming: boolean
  moonlightCandidates: string[]
}

/**
 * Thin client for the Rust session manager and the self-hosted Sunshine
 * provider. All state, validation, timeouts, certificate pinning and cleanup
 * live in the backend; the Sunshine password is sent once to unlock and is
 * never stored or returned.
 */
export const cloudSessionProvider = {
  getStatus: () => invoke<CloudSessionStatus>('cloudcraft_cloud_session_status'),
  startSession: (gameVersion?: string) =>
    invoke<CloudSessionStatus>('cloudcraft_cloud_session_start', {
      gameVersion: gameVersion || null,
    }),
  cancelStart: () => invoke<CloudSessionStatus>('cloudcraft_cloud_session_cancel_start'),
  stopSession: () => invoke<CloudSessionStatus>('cloudcraft_cloud_session_stop'),
  reconnect: () => invoke<CloudSessionStatus>('cloudcraft_cloud_session_reconnect'),
}

export const cloudQuota = {
  get: () => invoke<QuotaSnapshot>('cloudcraft_cloud_quota_status'),
  setPolicy: (policy: QuotaPolicy) =>
    invoke<QuotaSnapshot>('cloudcraft_cloud_quota_set_policy', { policy }),
  /** Each warning is emitted once by the backend ticker, even across restarts. */
  onWarning: (handler: (warnings: QuotaWarning[]) => void): Promise<UnlistenFn> =>
    listen<QuotaWarning[]>(QUOTA_WARNING_EVENT, (event) => handler(event.payload)),
}

export const cloudHost = {
  get: () => invoke<CloudHostView>('cloudcraft_cloud_host_get'),
  probe: (host: string, apiPort: number) =>
    invoke<string>('cloudcraft_cloud_host_probe', { host, apiPort }),
  save: (config: CloudHostConfig) =>
    invoke<CloudHostView>('cloudcraft_cloud_host_save', { config }),
  remove: () => invoke<CloudHostView>('cloudcraft_cloud_host_remove'),
  unlock: (password: string) => invoke<CloudHostView>('cloudcraft_cloud_host_unlock', { password }),
  lock: () => invoke<CloudHostView>('cloudcraft_cloud_host_lock'),
  ping: () => invoke<number>('cloudcraft_cloud_host_ping'),
  logs: () => invoke<string[]>('cloudcraft_cloud_host_logs'),
}

export function cloudSessionStateLabel(state: CloudSessionState): string {
  switch (state) {
    case 'unavailable':
      return 'Not ready'
    case 'idle':
      return 'Ready to start'
    case 'starting':
      return 'Connecting'
    case 'ready':
      return 'Streaming'
    default:
      return state.charAt(0).toUpperCase() + state.slice(1)
  }
}

/** Formats a SHA-256 hex fingerprint as colon-separated byte pairs for comparison. */
export function formatFingerprint(hex: string): string {
  return (hex.match(/.{1,2}/g) || []).join(':').toUpperCase()
}

export function latencyQuality(ms: number | null): 'good' | 'fair' | 'poor' | 'unknown' {
  if (ms === null || !Number.isFinite(ms)) return 'unknown'
  if (ms <= 40) return 'good'
  if (ms <= 100) return 'fair'
  return 'poor'
}
