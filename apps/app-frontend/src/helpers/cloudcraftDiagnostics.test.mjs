import assert from 'node:assert/strict'
import { test } from 'node:test'
import {
  buildInstanceEdit,
  describeExit,
  formatJvmArgs,
  parseJvmArgs,
  validateCloudHostForm,
} from './cloudcraftDiagnostics.ts'

const base = {
  profile_path: 'a',
  start_time: '2026-10-26T10:00:00Z',
  end_time: '2026-10-26T10:05:30Z',
}

test('describeExit separates user stop, clean exit and crash', () => {
  assert.equal(
    describeExit({ ...base, exit_code: null, success: false, stopped_by_user: true }).kind,
    'stopped',
  )
  const clean = describeExit({ ...base, exit_code: 0, success: true, stopped_by_user: false })
  assert.equal(clean.kind, 'clean')
  assert.match(clean.detail, /5m 30s/)
  const crash = describeExit({
    ...base,
    exit_code: -1073740791,
    success: false,
    stopped_by_user: false,
  })
  assert.equal(crash.kind, 'crashed')
  assert.match(crash.detail, /code -1073740791/)
  const signal = describeExit({ ...base, exit_code: null, success: false, stopped_by_user: false })
  assert.match(signal.detail, /terminated by the operating system/)
})

test('parseJvmArgs honours quotes and rejects unsafe input', () => {
  assert.deepEqual(parseJvmArgs('-XX:+UseG1GC  "-Dfoo=a b" \'-Dbar=c\'').args, [
    '-XX:+UseG1GC',
    '-Dfoo=a b',
    '-Dbar=c',
  ])
  assert.deepEqual(parseJvmArgs('').args, [])
  assert.match(parseJvmArgs('"unterminated').error, /quote/)
  assert.match(parseJvmArgs('a\nb').error, /one line/)
  assert.match(parseJvmArgs(Array(65).fill('-x').join(' ')).error, /64/)
  assert.deepEqual(parseJvmArgs(formatJvmArgs(['-Da=b c', '-X'])).args, ['-Da=b c', '-X'])
})

test('buildInstanceEdit validates and clears overrides', () => {
  const form = {
    name: ' Survival ',
    memoryOverride: false,
    maxMemoryMb: '',
    javaPath: '',
    jvmArgs: '',
  }
  assert.deepEqual(buildInstanceEdit(form, 16384).edit, {
    name: 'Survival',
    memory: null,
    java_path: null,
    extra_launch_args: null,
  })
  const tuned = buildInstanceEdit(
    {
      ...form,
      memoryOverride: true,
      maxMemoryMb: '6144',
      javaPath: '/usr/bin/java',
      jvmArgs: '-XX:+UseZGC',
    },
    16384,
  )
  assert.deepEqual(tuned.edit.memory, { maximum: 6144 })
  assert.deepEqual(tuned.edit.extra_launch_args, ['-XX:+UseZGC'])
  assert.equal(buildInstanceEdit({ ...form, name: 'a/b' }, null).edit, null)
  assert.equal(
    buildInstanceEdit({ ...form, memoryOverride: true, maxMemoryMb: '99999' }, 8192).edit,
    null,
  )
  assert.equal(buildInstanceEdit({ ...form, javaPath: 'java' }, null).edit, null)
})

test('validateCloudHostForm reports each problem', () => {
  const good = {
    host: 'gaming-pc.tail1234.ts.net',
    apiPort: '47990',
    username: 'admin',
    launchCommand: 'C:\\launcher.exe',
    workingDir: '',
    moonlightPath: '/usr/bin/moonlight',
    resolution: '1920x1080',
    fps: '60',
  }
  assert.deepEqual(validateCloudHostForm(good), [])
  const bad = validateCloudHostForm({
    ...good,
    host: 'https://x',
    apiPort: '0',
    moonlightPath: 'moonlight',
    resolution: 'big',
    fps: '1000',
  })
  assert.equal(bad.length, 5)
})
