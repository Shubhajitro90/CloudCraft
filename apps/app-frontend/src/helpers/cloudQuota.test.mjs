import assert from 'node:assert/strict'
import { test } from 'node:test'
import {
  CLOUD_BACKENDS,
  formatDuration,
  formatMoment,
  mergeAlerts,
  mostUrgentWarning,
  resetScheduleLabel,
  sourceLabel,
  usagePercent,
  usageTone,
} from './cloudQuota.ts'

test('formatDuration renders hours, minutes and seconds', () => {
  assert.equal(formatDuration(0), '0 s')
  assert.equal(formatDuration(42), '42 s')
  assert.equal(formatDuration(5 * 60), '5 min')
  assert.equal(formatDuration(12 * 3600), '12 h')
  assert.equal(formatDuration(2 * 3600 + 5 * 60 + 9), '2 h 5 min')
  assert.equal(formatDuration(-10), '0 s')
  assert.equal(formatDuration(null), '—')
})

test('usagePercent clamps and tolerates bad input', () => {
  assert.equal(usagePercent(15 * 3600, 30 * 3600), 50)
  assert.equal(usagePercent(40, 30), 100)
  assert.equal(usagePercent(-1, 30), 0)
  assert.equal(usagePercent(10, 0), 0)
})

test('usageTone follows the warning thresholds', () => {
  assert.equal(usageTone(null), 'ok')
  assert.equal(usageTone(2 * 3600), 'ok')
  assert.equal(usageTone(30 * 60), 'low')
  assert.equal(usageTone(5 * 60), 'critical')
})

test('source and reset labels never imply provider data for estimates', () => {
  assert.equal(sourceLabel('localEstimate'), 'Local estimate')
  assert.equal(sourceLabel('provider'), 'Provider-reported')
  assert.equal(resetScheduleLabel({ resetWeekday: 5, resetHourUtc: 0 }), 'Saturday 00:00 UTC')
})

test('formatMoment rejects missing and invalid timestamps', () => {
  assert.equal(formatMoment(null), '—')
  assert.equal(formatMoment('not a date'), '—')
  assert.notEqual(formatMoment('2024-01-06T00:00:00+00:00', 'en-US'), '—')
})

test('mostUrgentWarning prefers critical warnings', () => {
  const info = { id: 'a', level: 'info' }
  const critical = { id: 'b', level: 'critical' }
  assert.equal(mostUrgentWarning([info, critical]).id, 'b')
  assert.equal(mostUrgentWarning([]), null)
})

test('mergeAlerts ignores duplicates and keeps the latest five', () => {
  const make = (id) => ({ id, level: 'info' })
  const merged = mergeAlerts([make('1')], [make('1'), make('2')])
  assert.deepEqual(
    merged.map((w) => w.id),
    ['1', '2'],
  )
  const many = mergeAlerts([], ['1', '2', '3', '4', '5', '6'].map(make))
  assert.equal(many.length, 5)
  assert.equal(many[0].id, '2')
})

test('Kaggle is never presented as a usable backend', () => {
  const kaggle = CLOUD_BACKENDS.find((b) => b.id === 'kaggle')
  assert.ok(kaggle)
  assert.equal(kaggle.status, 'policyBlocked')
  assert.ok(kaggle.sources.some((s) => s.url === 'https://www.kaggle.com/aup'))
  assert.equal(CLOUD_BACKENDS.filter((b) => b.status === 'available').length, 1)
})
