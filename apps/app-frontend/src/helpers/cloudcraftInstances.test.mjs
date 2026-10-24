import assert from 'node:assert/strict'
import { test } from 'node:test'
import {
  compatibleGameVersions,
  createAndInstall,
  deleteInstance,
  installRequiredJava,
  loaderVersionChoices,
  summarizeAccounts,
  validateInstanceName,
  validateNewInstance,
} from './cloudcraftInstances.ts'

const gameManifest = {
  versions: [
    { id: '1.21.1', type: 'release' },
    { id: '1.20.1', type: 'release' },
    { id: '24w01a', type: 'snapshot' },
  ],
}
const forgeManifest = {
  gameVersions: [{ id: '1.20.1', loaders: [{ id: '47.2.0' }, { id: '47.1.0' }] }],
}
const fabricManifest = {
  gameVersions: [{ id: '${modrinth.gameVersion}', loaders: [{ id: '0.16.0' }] }],
}

function deps(overrides = {}) {
  const calls = []
  return {
    calls,
    create: async (...args) => {
      calls.push(['create', ...args])
      return '/profiles/new'
    },
    install: async (...args) => {
      calls.push(['install', ...args])
    },
    remove: async (path) => {
      calls.push(['remove', path])
    },
    processesFor: async () => [],
    ...overrides,
  }
}

test('name validation rejects empty, unsafe, reserved and duplicate names', () => {
  const existing = [{ path: '/a', name: 'Survival' }]
  assert.equal(validateInstanceName('Fresh start', existing), '')
  assert.match(validateInstanceName('   ', existing), /Enter a name/)
  assert.match(validateInstanceName('a/b', existing), /cannot contain/)
  assert.match(validateInstanceName('..', existing), /not allowed/)
  assert.match(validateInstanceName('CON', existing), /not allowed/)
  assert.match(validateInstanceName('trailing.', existing), /not allowed/)
  assert.match(validateInstanceName('survival', existing), /already exists/)
  assert.match(validateInstanceName('x'.repeat(65), existing), /64 characters/)
})

test('versions are limited to releases the loader supports', () => {
  assert.deepEqual(compatibleGameVersions('vanilla', gameManifest, null), ['1.21.1', '1.20.1'])
  assert.deepEqual(compatibleGameVersions('forge', gameManifest, forgeManifest), ['1.20.1'])
  assert.deepEqual(compatibleGameVersions('forge', gameManifest, null), [])
})

test('loader versions come from the right manifest entry', () => {
  assert.deepEqual(loaderVersionChoices('forge', '1.20.1', forgeManifest), ['47.2.0', '47.1.0'])
  assert.deepEqual(loaderVersionChoices('forge', '1.21.1', forgeManifest), [])
  assert.deepEqual(loaderVersionChoices('fabric', '1.21.1', fabricManifest), ['0.16.0'])
  assert.deepEqual(loaderVersionChoices('vanilla', '1.21.1', fabricManifest), [])
})

test('new instance validation reports every problem', () => {
  const ok = { name: 'Pack', gameVersion: '1.20.1', loader: 'forge', loaderVersion: '47.2.0' }
  assert.deepEqual(validateNewInstance(ok, [], ['1.20.1'], ['47.2.0']), [])
  const bad = { name: '', gameVersion: '1.19', loader: 'forge', loaderVersion: '' }
  const issues = validateNewInstance(bad, [], ['1.20.1'], ['47.2.0'])
  assert.equal(issues.length, 3)
  const vanilla = { name: 'V', gameVersion: '1.20.1', loader: 'vanilla', loaderVersion: '' }
  assert.deepEqual(validateNewInstance(vanilla, [], ['1.20.1'], []), [])
})

test('create then install succeeds and reports each stage', async () => {
  const d = deps()
  const stages = []
  const outcome = await createAndInstall(
    { name: ' Pack ', gameVersion: '1.20.1', loader: 'forge', loaderVersion: '47.2.0' },
    d,
    (stage) => stages.push(stage),
  )
  assert.deepEqual(outcome, { kind: 'ready', path: '/profiles/new' })
  assert.deepEqual(stages, ['creating', 'installing'])
  assert.deepEqual(d.calls[0], ['create', 'Pack', '1.20.1', 'forge', '47.2.0', null, true])
  assert.deepEqual(d.calls[1], ['install', '/profiles/new', false])
})

test('vanilla creation passes no loader version', async () => {
  const d = deps()
  await createAndInstall(
    { name: 'V', gameVersion: '1.21.1', loader: 'vanilla', loaderVersion: 'x' },
    d,
  )
  assert.equal(d.calls[0][4], null)
})

test('a failed create does not attempt to install', async () => {
  const d = deps({
    create: async () => {
      throw new Error('disk full')
    },
  })
  const outcome = await createAndInstall(
    { name: 'P', gameVersion: '1.20.1', loader: 'vanilla', loaderVersion: '' },
    d,
  )
  assert.equal(outcome.kind, 'failed')
  assert.match(outcome.message, /disk full/)
  assert.equal(
    d.calls.some((call) => call[0] === 'install'),
    false,
  )
})

test('a failed install keeps the instance and points to repair', async () => {
  const d = deps({
    install: async () => {
      throw new Error('network down')
    },
  })
  const outcome = await createAndInstall(
    { name: 'P', gameVersion: '1.20.1', loader: 'vanilla', loaderVersion: '' },
    d,
  )
  assert.equal(outcome.kind, 'needs_repair')
  assert.equal(outcome.path, '/profiles/new')
  assert.match(outcome.message, /network down/)
  assert.equal(
    d.calls.some((call) => call[0] === 'remove'),
    false,
  )
})

test('delete refuses a running instance and reports failures', async () => {
  const running = deps({ processesFor: async () => [{}] })
  const refused = await deleteInstance({ path: '/p', name: 'P' }, running)
  assert.equal(refused.deleted, false)
  assert.equal(running.calls.length, 0)

  const unknown = deps({
    processesFor: async () => {
      throw new Error('boom')
    },
  })
  assert.equal((await deleteInstance({ path: '/p' }, unknown)).deleted, false)

  const failing = deps({
    remove: async () => {
      throw new Error('locked')
    },
  })
  const failed = await deleteInstance({ path: '/p' }, failing)
  assert.match(failed.message, /locked/)

  const ok = deps()
  assert.equal((await deleteInstance({ path: '/p', name: 'P' }, ok)).deleted, true)
  assert.deepEqual(ok.calls, [['remove', '/p']])
  assert.equal((await deleteInstance({ path: '' }, ok)).deleted, false)
})

test('java install uses the version the instance requires', async () => {
  const installed = []
  const result = await installRequiredJava('/p', {
    getOptimalJre: async () => ({ parsed_version: 21 }),
    autoInstallJava: async (major) => installed.push(major),
  })
  assert.deepEqual(result, { installed: true, message: 'Java 21 is installed.' })
  assert.deepEqual(installed, [21])
})

test('java install reports missing requirement and install failure', async () => {
  const none = await installRequiredJava('/p', {
    getOptimalJre: async () => null,
    autoInstallJava: async () => {},
  })
  assert.equal(none.installed, false)
  const failing = await installRequiredJava('/p', {
    getOptimalJre: async () => ({ parsed_version: '17' }),
    autoInstallJava: async () => {
      throw new Error('offline')
    },
  })
  assert.equal(failing.installed, false)
  assert.match(failing.message, /Java 17 could not be installed: offline/)
  const lookup = await installRequiredJava('/p', {
    getOptimalJre: async () => {
      throw new Error('bad profile')
    },
    autoInstallJava: async () => {},
  })
  assert.match(lookup.message, /bad profile/)
})

test('account summary selects the default and flags a missing sign-in', () => {
  const accounts = [{ id: 'a' }, { id: 'b' }]
  const summary = summarizeAccounts(accounts, 'b')
  assert.equal(summary.selected.id, 'b')
  assert.deepEqual(summary.others, [{ id: 'a' }])
  assert.equal(summary.needsSignIn, false)
  assert.equal(summarizeAccounts([], undefined).needsSignIn, true)
  assert.equal(summarizeAccounts(accounts, 'zzz').selected, null)
})
