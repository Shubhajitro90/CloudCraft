export type CloudStorageState = 'unconfigured' | 'checking' | 'ready' | 'error'

export interface CloudStorageStatus {
  state: CloudStorageState
  message: string
  providerId?: string
  accountLabel?: string
  canUpload: boolean
  canDownload: boolean
  canDelete: boolean
}

export interface CloudBackupRecord {
  id: string
  name: string
  createdAt: string
  sizeBytes?: number
  checksum?: string
}

export interface CloudTransferProgress {
  transferredBytes: number
  totalBytes?: number
  phase: 'preparing' | 'transferring' | 'verifying' | 'complete'
}

export interface CloudStorageProvider {
  getStatus(): Promise<CloudStorageStatus>
  listBackups(): Promise<CloudBackupRecord[]>
  uploadBackup(
    localBackupPath: string,
    onProgress?: (progress: CloudTransferProgress) => void,
  ): Promise<CloudBackupRecord>
  downloadBackup(
    backupId: string,
    destinationParent: string,
    onProgress?: (progress: CloudTransferProgress) => void,
  ): Promise<string>
  deleteBackup(backupId: string): Promise<void>
  cancelTransfer(): Promise<void>
}

/**
 * Boundary for a future cloud-save provider.
 *
 * This implementation deliberately performs no network requests, stores no
 * credentials, and never uploads or downloads world data. A real provider
 * must implement consent, secure credential storage, integrity checks,
 * conflict handling, cancellation, and recovery before replacing this class.
 */
class UnconfiguredCloudStorageProvider implements CloudStorageProvider {
  async getStatus(): Promise<CloudStorageStatus> {
    return {
      state: 'unconfigured',
      message: 'Cloud storage is not configured. World backups stay on this device.',
      canUpload: false,
      canDownload: false,
      canDelete: false,
    }
  }

  async listBackups(): Promise<CloudBackupRecord[]> {
    throw new Error('No cloud storage provider is configured. No remote backups are available.')
  }

  async uploadBackup(
    _localBackupPath: string,
    _onProgress?: (progress: CloudTransferProgress) => void,
  ): Promise<CloudBackupRecord> {
    throw new Error('No cloud storage provider is configured. No files were uploaded.')
  }

  async downloadBackup(
    _backupId: string,
    _destinationParent: string,
    _onProgress?: (progress: CloudTransferProgress) => void,
  ): Promise<string> {
    throw new Error('No cloud storage provider is configured. No files were downloaded.')
  }

  async deleteBackup(_backupId: string): Promise<void> {
    throw new Error('No cloud storage provider is configured. No remote backup was deleted.')
  }

  async cancelTransfer(): Promise<void> {
    // There is no active transfer while no provider is configured.
  }
}

export const cloudStorageProvider: CloudStorageProvider = new UnconfiguredCloudStorageProvider()
