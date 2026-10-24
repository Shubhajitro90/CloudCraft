import { invoke } from '@tauri-apps/api/core'

import type { OperationSnapshot } from './cloudcraftOperations'

export type LocalServerLifecycle = 'stopped' | 'starting' | 'running' | 'stopping' | 'failed'

export interface LocalServerConfig {
  serverDir: string
  serverJar: string
  javaExecutable: string
  name: string
  gameVersion: string
  memoryMb: number
  maxPlayers: number
  port: number
  acceptEula: boolean
}

export interface LocalServerStatus {
  state: LocalServerLifecycle
  message: string
  config?: LocalServerConfig
  serverDir?: string
  name?: string
  gameVersion?: string
  port?: number
  pid?: number
  reachable: boolean
  connectionAddress?: string
  canStart: boolean
  canStop: boolean
  canForceStop: boolean
  startedAt?: string
  lastError?: string
}

export async function configureLocalServer(config: LocalServerConfig) {
  return (await invoke('cloudcraft_server_configure', { config })) as LocalServerStatus
}

export async function loadLocalServer(serverDir: string) {
  return (await invoke('cloudcraft_server_load', { serverDir })) as LocalServerStatus
}

export async function startLocalServer(serverDir: string) {
  return (await invoke('cloudcraft_server_start', { serverDir })) as LocalServerStatus
}

export async function getLocalServerStatus() {
  return (await invoke('cloudcraft_server_status')) as LocalServerStatus
}

export async function stopLocalServer() {
  return (await invoke('cloudcraft_server_stop')) as LocalServerStatus
}

export async function forceStopLocalServer() {
  return (await invoke('cloudcraft_server_force_stop')) as LocalServerStatus
}

export async function getLocalServerLogs() {
  return (await invoke('cloudcraft_server_logs')) as string[]
}

export interface ServerBackupSummary {
  id: string
  levelName: string
  createdAt: string
  totalBytes: number
  fileCount: number
}

export interface ServerBackupVerifyReport {
  id: string
  ok: boolean
  checkedFiles: number
  problems: string[]
}

export async function createServerBackup(serverDir: string) {
  return (await invoke('cloudcraft_server_backup_create', { serverDir })) as ServerBackupSummary
}

export async function listServerBackups(serverDir: string) {
  return (await invoke('cloudcraft_server_backup_list', { serverDir })) as ServerBackupSummary[]
}

export async function verifyServerBackup(serverDir: string, backupId: string) {
  return (await invoke('cloudcraft_server_backup_verify', {
    serverDir,
    backupId,
  })) as ServerBackupVerifyReport
}

/** Restores into a new folder and returns its name; the current world is untouched. */
export async function restoreServerBackup(serverDir: string, backupId: string) {
  return (await invoke('cloudcraft_server_backup_restore', { serverDir, backupId })) as string
}

export type { OperationSnapshot } from './cloudcraftOperations'

export async function getOperationStatus() {
  return (await invoke('cloudcraft_operation_status')) as OperationSnapshot | null
}

/** Rejects when the running operation is past the point where cancelling is safe. */
export async function cancelOperation() {
  return (await invoke('cloudcraft_operation_cancel')) as OperationSnapshot
}

export interface ServerDeletionPlan {
  serverName: string
  levelName: string | null
  levelNameError: string | null
  worldExists: boolean
  worldFiles: number
  worldBytes: number
  worldBlocker: string | null
  backups: ServerBackupSummary[]
  backupBytes: number
  configurationRemoves: string[]
  configurationRetains: string[]
  blockedReason: string | null
}

export type ServerDeleteTarget =
  | { kind: 'configuration' }
  | { kind: 'world' }
  | { kind: 'backup'; id: string }

export interface ServerDeletionResult {
  target: string
  name: string
  complete: boolean
  deletedFiles: number
  deletedBytes: number
  failures: string[]
  retained: string[]
  message: string
}

export async function getServerDeletionPlan(serverDir: string) {
  return (await invoke('cloudcraft_server_delete_plan', { serverDir })) as ServerDeletionPlan
}

/** Deletes exactly one confirmed resource. The result comes from the backend, never assumed. */
export async function deleteServerResource(
  serverDir: string,
  target: ServerDeleteTarget,
  confirmation: string,
) {
  return (await invoke('cloudcraft_server_delete', {
    serverDir,
    target,
    confirmation,
  })) as ServerDeletionResult
}
