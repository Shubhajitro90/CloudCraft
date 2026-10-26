/**
 * Pure, dependency-free presentation logic for the cloud usage policy.
 * All accounting and enforcement live in the Rust ledger
 * (`cloudcraft_cloud_quota.rs`); this file only formats its snapshots.
 */

export type UsageSource = 'localEstimate' | 'provider'
export type LimitKind = 'session' | 'weekly'
export type WarningLevel = 'info' | 'warning' | 'critical'
export type EndReason =
  | 'stopped'
  | 'sessionLimit'
  | 'weeklyAllowance'
  | 'startFailed'
  | 'cancelled'
  | 'launcherClosed'
  | 'superseded'

export interface QuotaPolicy {
  maxSessionSecs: number
  weeklyAllowanceSecs: number
  resetWeekday: number
  resetHourUtc: number
  sessionWarningSecs: number[]
  weeklyWarningSecs: number[]
}

export interface QuotaWarning {
  id: string
  kind: LimitKind
  level: WarningLevel
  remainingSecs: number
  message: string
}

export interface QuotaSnapshot {
  policy: QuotaPolicy
  source: UsageSource
  sourceNote: string
  sessionActive: boolean
  sessionElapsedSecs: number | null
  sessionRemainingSecs: number | null
  sessionDeadline: string | null
  bindingLimit: LimitKind | null
  weeklyUsedSecs: number
  weeklyRemainingSecs: number
  weekStartedAt: string
  nextResetAt: string
  canLaunch: boolean
  blockReason: string | null
  warnings: QuotaWarning[]
  lastSessionEnd: {
    sessionId: string
    endedAt: string
    reason: EndReason
    countedSecs: number
    message: string
  } | null
  notice: string | null
}

const WEEKDAYS = ['Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday', 'Sunday']

export function formatDuration(totalSecs: number | null | undefined): string {
  if (totalSecs === null || totalSecs === undefined || !Number.isFinite(totalSecs)) return '—'
  const secs = Math.max(0, Math.floor(totalSecs))
  const hours = Math.floor(secs / 3600)
  const minutes = Math.floor((secs % 3600) / 60)
  if (hours === 0 && minutes === 0) return `${secs} s`
  if (hours === 0) return `${minutes} min`
  if (minutes === 0) return `${hours} h`
  return `${hours} h ${minutes} min`
}

/** Clamped 0–100 share of `limit` that `used` represents. */
export function usagePercent(used: number, limit: number): number {
  if (!Number.isFinite(used) || !Number.isFinite(limit) || limit <= 0) return 0
  return Math.min(100, Math.max(0, Math.round((used / limit) * 100)))
}

export function usageTone(remainingSecs: number | null): 'ok' | 'low' | 'critical' {
  if (remainingSecs === null) return 'ok'
  if (remainingSecs <= 5 * 60) return 'critical'
  if (remainingSecs <= 30 * 60) return 'low'
  return 'ok'
}

export function sourceLabel(source: UsageSource): string {
  return source === 'provider' ? 'Provider-reported' : 'Local estimate'
}

export function resetScheduleLabel(policy: QuotaPolicy): string {
  const day = WEEKDAYS[policy.resetWeekday] ?? 'Unknown day'
  return `${day} ${String(policy.resetHourUtc).padStart(2, '0')}:00 UTC`
}

/** Formats an RFC 3339 timestamp in the user's locale, or a dash when absent/invalid. */
export function formatMoment(iso: string | null | undefined, locale?: string): string {
  if (!iso) return '—'
  const date = new Date(iso)
  if (Number.isNaN(date.getTime())) return '—'
  return date.toLocaleString(locale, {
    weekday: 'short',
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  })
}

/** The most urgent warning, for a persistent banner. */
export function mostUrgentWarning(warnings: QuotaWarning[]): QuotaWarning | null {
  const rank: Record<WarningLevel, number> = { critical: 0, warning: 1, info: 2 }
  return [...warnings].sort((a, b) => rank[a.level] - rank[b.level])[0] ?? null
}

/** Merges newly announced warnings into a dismissible list without duplicates. */
export function mergeAlerts(existing: QuotaWarning[], incoming: QuotaWarning[]): QuotaWarning[] {
  const seen = new Set(existing.map((w) => w.id))
  return [...existing, ...incoming.filter((w) => !seen.has(w.id))].slice(-5)
}

export type BackendStatus = 'available' | 'policyBlocked' | 'notIntegrated'

export interface CloudBackend {
  id: string
  name: string
  status: BackendStatus
  summary: string
  sources: { label: string; url: string }[]
  checkedOn: string
}

/**
 * Compute backends CloudCraft evaluated. Only a computer the user controls is
 * usable; the others are listed with the exact reason so the UI never implies
 * they work.
 */
export const CLOUD_BACKENDS: CloudBackend[] = [
  {
    id: 'self-hosted-sunshine',
    name: 'Your own computer (Sunshine)',
    status: 'available',
    summary:
      'A PC you control runs Minecraft and Sunshine; Moonlight on this device streams it. This is the supported backend.',
    sources: [{ label: 'Sunshine', url: 'https://github.com/LizardByte/Sunshine' }],
    checkedOn: '2026-10-26',
  },
  {
    id: 'kaggle',
    name: 'Kaggle Notebooks',
    status: 'policyBlocked',
    summary:
      "Kaggle's Acceptable Use Policy forbids using its resources for activity unrelated to ML data science, server farming, and circumvention tools. A streamed Minecraft client is a game, not ML work, and notebooks accept no inbound connections for a stream, so CloudCraft does not start Kaggle sessions. The 12-hour session and 30-hour weekly figures from Kaggle's documentation are kept only as CloudCraft's own usage limits.",
    sources: [
      { label: 'Kaggle Acceptable Use Policy', url: 'https://www.kaggle.com/aup' },
      { label: 'Kaggle notebook specifications', url: 'https://www.kaggle.com/docs/notebooks' },
    ],
    checkedOn: '2026-10-26',
  },
]

export function backendStatusLabel(status: BackendStatus): string {
  switch (status) {
    case 'available':
      return 'Supported'
    case 'policyBlocked':
      return 'Not permitted by provider'
    default:
      return 'Not integrated'
  }
}
