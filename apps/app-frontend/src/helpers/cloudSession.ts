import { invoke } from '@tauri-apps/api/core'

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
  cleanupRequired: boolean
}

/**
 * Thin client for the Rust session manager. All state, validation, timeouts and
 * cleanup live in the backend; this module only forwards commands and returns the
 * backend's snapshot. Until a real provider is integrated the backend reports
 * `unavailable` and never creates remote resources.
 */
export const cloudSessionProvider = {
  getStatus: () => invoke<CloudSessionStatus>('cloudcraft_cloud_session_status'),
  startSession: (gameVersion?: string) =>
    invoke<CloudSessionStatus>('cloudcraft_cloud_session_start', {
      gameVersion: gameVersion || null,
    }),
  cancelStart: () => invoke<CloudSessionStatus>('cloudcraft_cloud_session_cancel_start'),
  stopSession: () => invoke<CloudSessionStatus>('cloudcraft_cloud_session_stop'),
}

export function cloudSessionStateLabel(state: CloudSessionState): string {
  switch (state) {
    case 'unavailable':
      return 'Not configured'
    case 'idle':
      return 'Ready to start'
    default:
      return state.charAt(0).toUpperCase() + state.slice(1)
  }
}
