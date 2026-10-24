import { ref } from 'vue'

import { cancelOperation, getOperationStatus } from './cloudcraftServer'
import {
  type OperationKind,
  type OperationOutcome,
  type OperationSnapshot,
  outcomeFromError,
} from './cloudcraftOperations'

const POLL_INTERVAL_MS = 400

// Shared by every server panel so they all agree on whether the single backend
// operation slot is busy.
const snapshot = ref<OperationSnapshot | null>(null)
const running = ref(false)
const revision = ref(0)
const cancelError = ref('')
let timer: number | undefined
let adopted = false

async function poll() {
  try {
    snapshot.value = await getOperationStatus()
  } catch {
    return
  }
  if (adopted && snapshot.value?.state !== 'running') {
    adopted = false
    stopPolling()
    running.value = false
    revision.value += 1
  }
}

function startPolling() {
  if (timer === undefined) timer = window.setInterval(poll, POLL_INTERVAL_MS)
}

function stopPolling() {
  if (timer !== undefined) {
    window.clearInterval(timer)
    timer = undefined
  }
}

export type OperationRun<T> = { ok: true; value: T } | { ok: false; outcome: OperationOutcome }

/** Picks up an operation that is already running, for example after a page reload. */
async function attach() {
  await poll()
  if (snapshot.value?.state === 'running' && !running.value) {
    adopted = true
    running.value = true
    startPolling()
  }
}

async function run<T>(kind: OperationKind, task: () => Promise<T>): Promise<OperationRun<T>> {
  running.value = true
  cancelError.value = ''
  startPolling()
  void poll()
  try {
    const value = await task()
    await poll()
    return { ok: true, value }
  } catch (caught) {
    await poll()
    return { ok: false, outcome: outcomeFromError(snapshot.value, caught, kind) }
  } finally {
    stopPolling()
    running.value = false
    revision.value += 1
  }
}

async function cancel() {
  cancelError.value = ''
  try {
    snapshot.value = await cancelOperation()
  } catch (caught) {
    cancelError.value = caught instanceof Error ? caught.message : String(caught)
  }
}

export function useServerOperation() {
  return { snapshot, running, revision, cancelError, attach, run, cancel }
}
