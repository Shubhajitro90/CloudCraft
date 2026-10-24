import assert from 'node:assert/strict'
import { test } from 'node:test'
import {
  applyProcessEvent,
  launchInstance,
  parseMemoryField,
  playability,
  preflightLaunch,
  reconcileRunning,
  stopInstance,
  validateMemory,
} from './cloudcraftLauncher.ts'

const installed = { path: 'a', game_version: '1.21', install_stage: 'installed' }

function makeDeps(overrides = {}) {
  const calls = { run: 0, kill: 0 }
  const processes = { a: [] }
  const deps = {
    getDefaultUser: async () => ({ id: 'u' }),
    getOptimalJre: async () => ({ path: '/java', parsed_version: 21 }),
    testJre: async () => true,
    getSettings: async () => ({ memory: { min: 512, max: 2048 } }),
    getMaxMemoryKib: async () => 8 * 1024 * 1024,
    run: async (path) => {
      calls.run++
      processes[path] = [1]
    },
    kill: async (path) => {
      calls.kill++
      processes[path] = []
    },
    processesFor: async (path) => processes[path] ?? [],
    ...overrides,
  }
  return { deps, calls, processes }
}

test('playability distinguishes installed, installing, and missing', () => {
  assert.equal(playability(installed).canPlay, true)
  assert.equal(playability({ path: 'a', install_stage: 'installing' }).busy, true)
  assert.equal(playability({ path: 'a', install_stage: 'not_installed' }).needsInstall, true)
  assert.equal(playability(null).canPlay, false)
})

test('memory parsing and validation', () => {
  assert.deepEqual(parseMemoryField(''), { value: null, error: '' })
  assert.equal(parseMemoryField('12a').error.length > 0, true)
  assert.deepEqual(validateMemory({ min: 512, max: 2048 }, 8192), [])
  assert.equal(validateMemory({ min: 4096, max: 2048 }, 8192).length, 1)
  assert.equal(validateMemory({ min: 512, max: 16384 }, 8192).length, 1)
  assert.equal(validateMemory({ min: 64, max: 100 }, null).length, 2)
})

test('launch succeeds and the process is tracked', async () => {
  const { deps, calls } = makeDeps()
  const outcome = await launchInstance(installed, deps)
  assert.deepEqual(outcome, { kind: 'launched' })
  assert.equal(calls.run, 1)
  assert.deepEqual(await reconcileRunning(['a'], deps.processesFor), { a: true })
})

test('launch is blocked before running when the instance is not installed', async () => {
  const { deps, calls } = makeDeps()
  const outcome = await launchInstance({ ...installed, install_stage: 'not_installed' }, deps)
  assert.equal(outcome.kind, 'blocked')
  assert.equal(outcome.issues[0].code, 'not_installed')
  assert.equal(calls.run, 0)
})

test('launch is blocked without an account or Java', async () => {
  const noAccount = makeDeps({ getDefaultUser: async () => null })
  const a = await launchInstance(installed, noAccount.deps)
  assert.equal(a.kind, 'blocked')
  assert.ok(a.issues.some((issue) => issue.code === 'no_account'))
  assert.equal(noAccount.calls.run, 0)

  const noJava = makeDeps({ getOptimalJre: async () => null })
  const b = await launchInstance(installed, noJava.deps)
  assert.ok(b.issues.some((issue) => issue.code === 'no_java'))
  assert.equal(noJava.calls.run, 0)
})

test('launch is blocked on invalid memory settings', async () => {
  const { deps, calls } = makeDeps({
    getSettings: async () => ({ memory: { min: 4096, max: 1024 } }),
  })
  const outcome = await launchInstance(installed, deps)
  assert.equal(outcome.kind, 'blocked')
  assert.ok(outcome.issues.some((issue) => issue.code === 'memory'))
  assert.equal(calls.run, 0)
})

test('a second launch of a running instance is refused', async () => {
  const { deps, calls } = makeDeps()
  await launchInstance(installed, deps)
  const second = await launchInstance(installed, deps)
  assert.equal(second.kind, 'blocked')
  assert.ok(second.issues.some((issue) => issue.code === 'already_running'))
  assert.equal(calls.run, 1)
})

test('a failing backend launch is reported, not swallowed', async () => {
  const { deps } = makeDeps({
    run: async () => {
      throw new Error('Java crashed on start')
    },
  })
  const outcome = await launchInstance(installed, deps)
  assert.deepEqual(outcome, { kind: 'failed', message: 'Java crashed on start' })
})

test('a failing preflight dependency is reported as a failure', async () => {
  const { deps } = makeDeps({
    processesFor: async () => {
      throw new Error('process service down')
    },
  })
  const outcome = await launchInstance(installed, deps)
  assert.deepEqual(outcome, { kind: 'failed', message: 'process service down' })
})

test('stop kills a running game and confirms it ended', async () => {
  const { deps, calls } = makeDeps()
  await launchInstance(installed, deps)
  const result = await stopInstance('a', deps)
  assert.deepEqual(result, { stopped: true, message: 'The game was stopped.' })
  assert.equal(calls.kill, 1)
})

test('stop is a no-op when nothing runs and reports kill failures and survivors', async () => {
  const idle = makeDeps()
  assert.equal((await stopInstance('a', idle.deps)).stopped, true)
  assert.equal(idle.calls.kill, 0)

  const failing = makeDeps({
    kill: async () => {
      throw new Error('permission denied')
    },
  })
  await launchInstance(installed, failing.deps)
  assert.deepEqual(await stopInstance('a', failing.deps), {
    stopped: false,
    message: 'permission denied',
  })

  const stubborn = makeDeps({ kill: async () => {} })
  await launchInstance(installed, stubborn.deps)
  assert.equal((await stopInstance('a', stubborn.deps)).stopped, false)
})

test('process events track launch and game exit', () => {
  let running = {}
  running = applyProcessEvent(running, { profile_path_id: 'a', event: 'launched' })
  assert.deepEqual(running, { a: true })
  running = applyProcessEvent(running, { profile_path_id: 'b', event: 'launched' })
  running = applyProcessEvent(running, { profile_path_id: 'a', event: 'finished' })
  assert.deepEqual(running, { b: true })
  assert.deepEqual(applyProcessEvent(running, { profile_path_id: 'b', event: 'other' }), running)
})

test('reconcile recovers running state after a reload and tolerates errors', async () => {
  const result = await reconcileRunning(['a', 'b', 'c'], async (path) => {
    if (path === 'a') return [1]
    if (path === 'b') throw new Error('x')
    return []
  })
  assert.deepEqual(result, { a: true })
})

test('preflight reports every problem at once', async () => {
  const { deps } = makeDeps({ getDefaultUser: async () => null, getOptimalJre: async () => null })
  const issues = await preflightLaunch({ ...installed, install_stage: 'not_installed' }, deps)
  assert.deepEqual(
    issues.map((issue) => issue.code).sort(),
    ['no_account', 'no_java', 'not_installed'].sort(),
  )
})
