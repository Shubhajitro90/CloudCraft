export type OperationKind =
  | 'backup'
  | 'verify'
  | 'restore'
  | 'delete'
  | 'download'
  | 'import'
  | 'export'
export type OperationState = 'running' | 'completed' | 'cancelled' | 'failed'

export interface OperationSnapshot {
  id: number
  kind: OperationKind
  state: OperationState
  phase: string
  phaseIndex: number
  phaseCount: number
  doneBytes: number
  totalBytes: number
  doneFiles: number
  totalFiles: number
  current: string | null
  cancellable: boolean
  cancelRequested: boolean
  message: string | null
  error: string | null
}

export type OperationOutcome =
  | { kind: 'cancelled'; text: string }
  | { kind: 'failed'; text: string }

export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${Math.max(1, Math.round(bytes / 1024))} KB`
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GB`
}

/**
 * Fraction of the current phase that is done, or null when the backend has not
 * measured any work for it. Callers must show an indeterminate indicator for
 * null instead of inventing a percentage.
 */
export function progressFraction(snapshot: OperationSnapshot | null): number | null {
  if (!snapshot || snapshot.state !== 'running') return null
  if (snapshot.totalBytes > 0) {
    return Math.min(1, Math.max(0, snapshot.doneBytes / snapshot.totalBytes))
  }
  if (snapshot.totalFiles > 0) {
    return Math.min(1, Math.max(0, snapshot.doneFiles / snapshot.totalFiles))
  }
  return null
}

export function progressLabel(snapshot: OperationSnapshot): string {
  const parts = [`Step ${snapshot.phaseIndex} of ${snapshot.phaseCount}: ${snapshot.phase}`]
  if (snapshot.totalBytes > 0) {
    parts.push(`${formatBytes(snapshot.doneBytes)} of ${formatBytes(snapshot.totalBytes)}`)
  }
  if (snapshot.totalFiles > 0) {
    parts.push(`${snapshot.doneFiles} of ${snapshot.totalFiles} files`)
  }
  return parts.join(' · ')
}

export function cancelButtonLabel(snapshot: OperationSnapshot): string {
  if (snapshot.cancelRequested) return 'Cancelling…'
  return snapshot.cancellable ? 'Cancel' : 'Cannot cancel now'
}

/**
 * Decides how to present a rejected operation. A cancellation is reported only
 * when the backend's own record says the operation was cancelled.
 */
export function outcomeFromError(
  last: OperationSnapshot | null,
  thrown: unknown,
  expectedKind: OperationKind,
): OperationOutcome {
  const text = thrown instanceof Error ? thrown.message : String(thrown)
  if (last && last.kind === expectedKind && last.state === 'cancelled') {
    return {
      kind: 'cancelled',
      text: 'Cancelled. Any incomplete files were removed and nothing was changed.',
    }
  }
  return { kind: 'failed', text }
}

/** Typed confirmation must match the exact resource name; no trimming or case folding. */
export function confirmationMatches(expected: string, typed: string): boolean {
  return expected.length > 0 && typed === expected
}

export interface DeletionResultLike {
  complete: boolean
  message: string
  failures: string[]
}

/** Success is shown only for a backend result that reports a complete deletion. */
export function deletionNotice(result: DeletionResultLike): {
  ok: boolean
  text: string
} {
  if (result.complete) return { ok: true, text: result.message }
  const shown = result.failures.slice(0, 3).join('; ')
  const more = result.failures.length > 3 ? ` (+${result.failures.length - 3} more)` : ''
  return { ok: false, text: `${result.message} Remaining problems: ${shown}${more}` }
}
