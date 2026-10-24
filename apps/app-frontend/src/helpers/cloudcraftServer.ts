import { invoke } from '@tauri-apps/api/core'

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
