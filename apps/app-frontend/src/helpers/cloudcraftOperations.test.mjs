import assert from 'node:assert/strict'
import { test } from 'node:test'
import {
  cancelButtonLabel,
  confirmationMatches,
  deletionNotice,
  formatBytes,
  outcomeFromError,
  progressFraction,
  progressLabel,
} from './cloudcraftOperations.ts'

const snapshot = (overrides = {}) => ({
  id: 1,
  kind: 'backup',
  state: 'running',
  phase: 'Copying world',
  phaseIndex: 2,
  phaseCount: 3,
  doneBytes: 0,
  totalBytes: 0,
  doneFiles: 0,
  totalFiles: 0,
  current: null,
  cancellable: true,
  cancelRequested: false,
  message: null,
  error: null,
  ...overrides,
})

test('progress is derived only from measured bytes or files', () => {
  assert.equal(progressFraction(null), null)
  assert.equal(progressFraction(snapshot()), null)
  assert.equal(progressFraction(snapshot({ doneBytes: 25, totalBytes: 100 })), 0.25)
  assert.equal(progressFraction(snapshot({ doneFiles: 1, totalFiles: 4 })), 0.25)
})

test('progress never exceeds 100 percent when the world grows during a copy', () => {
  assert.equal(progressFraction(snapshot({ doneBytes: 150, totalBytes: 100 })), 1)
})

test('finished operations report no live progress', () => {
  for (const state of ['completed', 'cancelled', 'failed']) {
    assert.equal(progressFraction(snapshot({ state, doneBytes: 5, totalBytes: 10 })), null)
  }
})

test('labels describe the real step and counters', () => {
  const label = progressLabel(
    snapshot({ doneBytes: 1024 * 1024, totalBytes: 4 * 1024 * 1024, doneFiles: 2, totalFiles: 8 }),
  )
  assert.equal(label, 'Step 2 of 3: Copying world · 1.0 MB of 4.0 MB · 2 of 8 files')
  assert.equal(
    progressLabel(snapshot({ phase: 'Scanning world', phaseIndex: 1 })),
    'Step 1 of 3: Scanning world',
  )
})

test('cancel button reflects whether cancelling is currently safe', () => {
  assert.equal(cancelButtonLabel(snapshot()), 'Cancel')
  assert.equal(cancelButtonLabel(snapshot({ cancellable: false })), 'Cannot cancel now')
  assert.equal(cancelButtonLabel(snapshot({ cancelRequested: true })), 'Cancelling…')
})

test('cancellation is reported only when the backend recorded it', () => {
  const cancelled = snapshot({ state: 'cancelled' })
  assert.equal(outcomeFromError(cancelled, new Error('x'), 'backup').kind, 'cancelled')
  assert.equal(outcomeFromError(cancelled, new Error('x'), 'restore').kind, 'failed')
  const failed = outcomeFromError(snapshot({ state: 'failed' }), 'disk full', 'backup')
  assert.deepEqual(failed, { kind: 'failed', text: 'disk full' })
  assert.equal(outcomeFromError(null, 'boom', 'backup').text, 'boom')
})

test('typed confirmation must match exactly', () => {
  assert.equal(confirmationMatches('world', 'world'), true)
  for (const typed of ['', 'World', 'world ', ' world', 'worl']) {
    assert.equal(confirmationMatches('world', typed), false)
  }
  assert.equal(confirmationMatches('', ''), false)
})

test('deletion success is shown only for a complete backend result', () => {
  const ok = deletionNotice({ complete: true, message: 'Deleted world.', failures: [] })
  assert.deepEqual(ok, { ok: true, text: 'Deleted world.' })
  const partial = deletionNotice({
    complete: false,
    message: 'Only partly done.',
    failures: ['a: locked', 'b: locked', 'c: locked', 'd: locked'],
  })
  assert.equal(partial.ok, false)
  assert.match(partial.text, /a: locked; b: locked; c: locked \(\+1 more\)/)
})

test('byte formatting', () => {
  assert.equal(formatBytes(10), '10 B')
  assert.equal(formatBytes(2048), '2 KB')
  assert.equal(formatBytes(5 * 1024 * 1024), '5.0 MB')
  assert.equal(formatBytes(3 * 1024 * 1024 * 1024), '3.00 GB')
})
