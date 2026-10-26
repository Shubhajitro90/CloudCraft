import assert from 'node:assert/strict'
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { test } from 'node:test'

import {
  collect,
  formatSums,
  parseSums,
  validateInstaller,
  validateTag,
  verify,
} from './validate-installers.mjs'

const VERSION = '0.9.204'
const LIMITS = { min: 16, max: 1 << 20 }

function pad(buffer, size = 2048) {
  return Buffer.concat([buffer, Buffer.alloc(Math.max(0, size - buffer.length), 0x20)])
}

const fixtures = {
  [`CloudCraft_${VERSION}_x64-setup.exe`]: pad(Buffer.from('MZ')),
  [`CloudCraft_${VERSION}_x64_en-US.msi`]: pad(Buffer.from('d0cf11e0a1b11ae1', 'hex')),
  [`CloudCraft_${VERSION}_amd64.AppImage`]: pad(
    Buffer.from([0x7f, 0x45, 0x4c, 0x46, 2, 1, 1, 0, 0x41, 0x49, 0x02]),
  ),
  [`CloudCraft_${VERSION}_amd64.deb`]: pad(Buffer.from('!<arch>\ndebian-binary   ')),
  [`CloudCraft-${VERSION}-1.x86_64.rpm`]: pad(Buffer.from('edabeedb', 'hex')),
  [`CloudCraft_${VERSION}_universal.dmg`]: Buffer.concat([
    Buffer.alloc(1536, 1),
    pad(Buffer.from('koly'), 512),
  ]),
}

function dist(files = fixtures) {
  const dir = mkdtempSync(join(tmpdir(), 'cloudcraft-dist-'))
  for (const [name, data] of Object.entries(files)) writeFileSync(join(dir, name), data)
  return dir
}

test('accepts real installer signatures and writes sorted checksums', () => {
  const dir = dist()
  try {
    const results = collect(dir, VERSION, LIMITS)
    assert.equal(results.length, 6)
    const text = formatSums(results)
    assert.equal(parseSums(text).size, 6)
    writeFileSync(join(dir, 'SHA256SUMS.txt'), text)
    assert.equal(verify(dir, VERSION, { requirePlatforms: true, limits: LIMITS }).length, 6)
  } finally {
    rmSync(dir, { recursive: true })
  }
})

test('rejects a file whose content does not match its extension', () => {
  const name = `CloudCraft_${VERSION}_x64_en-US.msi`
  const dir = dist({ [name]: pad(Buffer.from('<html>404</html>')) })
  try {
    assert.throws(() => validateInstaller(dir, name, VERSION, LIMITS), /valid \.msi file signature/)
  } finally {
    rmSync(dir, { recursive: true })
  }
})

test('rejects truncated, empty and wrongly versioned installers', () => {
  const dir = dist({
    [`CloudCraft_${VERSION}_amd64.deb`]: Buffer.from('!<arch>\n'),
    [`CloudCraft_1.0.0_amd64.AppImage`]: fixtures[`CloudCraft_${VERSION}_amd64.AppImage`],
  })
  try {
    assert.throws(
      () => validateInstaller(dir, `CloudCraft_${VERSION}_amd64.deb`, VERSION, LIMITS),
      /incomplete/,
    )
    assert.throws(
      () => validateInstaller(dir, 'CloudCraft_1.0.0_amd64.AppImage', VERSION, LIMITS),
      /does not carry the release version/,
    )
  } finally {
    rmSync(dir, { recursive: true })
  }
})

test('rejects unsafe or unknown file names', () => {
  const dir = dist({})
  try {
    assert.throws(() => validateInstaller(dir, '../evil.msi', VERSION, LIMITS), /Unsafe/)
    assert.throws(
      () => validateInstaller(dir, `CloudCraft_${VERSION}.sh`, VERSION, LIMITS),
      /not a recognised/,
    )
  } finally {
    rmSync(dir, { recursive: true })
  }
})

test('collect fails when no installer was produced', () => {
  const dir = dist({})
  try {
    assert.throws(() => collect(dir, VERSION, LIMITS), /No installers/)
  } finally {
    rmSync(dir, { recursive: true })
  }
})

test('verify detects tampering, stray files, missing files and missing platforms', () => {
  const dir = dist()
  try {
    const results = collect(dir, VERSION, LIMITS)
    writeFileSync(join(dir, 'SHA256SUMS.txt'), formatSums(results))

    const exe = `CloudCraft_${VERSION}_x64-setup.exe`
    writeFileSync(join(dir, exe), pad(Buffer.from('MZ tampered')))
    assert.throws(() => verify(dir, VERSION, { limits: LIMITS }), /Checksum mismatch/)
    writeFileSync(join(dir, exe), fixtures[exe])

    writeFileSync(join(dir, 'notes.txt'), 'x')
    assert.throws(() => verify(dir, VERSION, { limits: LIMITS }), /not covered/)
    rmSync(join(dir, 'notes.txt'))

    rmSync(join(dir, `CloudCraft_${VERSION}_universal.dmg`))
    assert.throws(() => verify(dir, VERSION, { limits: LIMITS }), /missing/)
    writeFileSync(
      join(dir, 'SHA256SUMS.txt'),
      formatSums(results.filter((r) => !r.name.endsWith('.dmg'))),
    )
    assert.equal(verify(dir, VERSION, { limits: LIMITS }).length, 5)
    assert.throws(
      () => verify(dir, VERSION, { requirePlatforms: true, limits: LIMITS }),
      /missing required installer types: \.dmg/,
    )
  } finally {
    rmSync(dir, { recursive: true })
  }
})

test('checksum file must be well formed and unique', () => {
  const hash = 'a'.repeat(64)
  assert.throws(() => parseSums(''), /lists no installers/)
  assert.throws(() => parseSums(`${hash.slice(1)}  a.msi\n`), /malformed/)
  assert.throws(() => parseSums(`${hash}  a.msi\n${hash}  a.msi\n`), /more than once/)
  assert.equal(parseSums(`${hash} *a.msi\r\n`).get('a.msi'), hash)
})

test('release tag must equal v + application version', () => {
  validateTag(`v${VERSION}`, VERSION)
  assert.throws(() => validateTag('v0.9.205', VERSION), /does not match/)
  assert.throws(() => validateTag(VERSION, VERSION), /expected v0\.9\.204/)
  assert.throws(() => validateTag('v1', 'not-a-version'), /Invalid application version/)
})
