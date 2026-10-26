#!/usr/bin/env node
// Validates CloudCraft installers before they are uploaded or attached to a
// draft release. Uses only Node built-ins so it runs on every CI runner.
//
//   node validate-installers.mjs collect --dir <dist> --version <x.y.z> --sums <SHA256SUMS-*.txt>
//   node validate-installers.mjs verify  --dir <dist> --version <x.y.z> [--require-platforms]
//   node validate-installers.mjs tag     --version <x.y.z> --tag <ref name>

import { createHash } from 'node:crypto'
import {
  closeSync,
  openSync,
  readdirSync,
  readFileSync,
  readSync,
  statSync,
  writeFileSync,
} from 'node:fs'
import { join } from 'node:path'
import { pathToFileURL } from 'node:url'

export const CHECKSUM_FILE = 'SHA256SUMS.txt'
const MIN_INSTALLER_BYTES = 1024 * 1024
const MAX_INSTALLER_BYTES = 1024 * 1024 * 1024

// Each type is identified by its file-format signature, not only its name.
export const INSTALLER_TYPES = [
  {
    ext: '-setup.exe',
    platform: 'windows',
    check: (head) => head.subarray(0, 2).toString('latin1') === 'MZ',
  },
  {
    ext: '.msi',
    platform: 'windows',
    check: (head) => head.subarray(0, 8).equals(Buffer.from('d0cf11e0a1b11ae1', 'hex')),
  },
  {
    ext: '.AppImage',
    platform: 'linux',
    // ELF executable carrying the AppImage type-2 magic at offset 8.
    check: (head) =>
      head.subarray(0, 4).equals(Buffer.from([0x7f, 0x45, 0x4c, 0x46])) &&
      head.subarray(8, 11).equals(Buffer.from([0x41, 0x49, 0x02])),
  },
  {
    ext: '.deb',
    platform: 'linux',
    check: (head) => head.subarray(0, 21).toString('latin1') === '!<arch>\ndebian-binary',
  },
  {
    ext: '.rpm',
    platform: 'linux',
    check: (head) => head.subarray(0, 4).equals(Buffer.from('edabeedb', 'hex')),
  },
  // UDIF images end with a 512-byte "koly" trailer.
  {
    ext: '.dmg',
    platform: 'macos',
    check: (_head, tail) => tail.subarray(0, 4).toString('latin1') === 'koly',
  },
]

export const REQUIRED_PLATFORM_TYPES = ['-setup.exe', '.msi', '.AppImage', '.deb', '.dmg']

const SAFE_NAME = /^[A-Za-z0-9][A-Za-z0-9._+-]{0,199}$/

export function installerType(name) {
  return INSTALLER_TYPES.find((type) => name.endsWith(type.ext)) ?? null
}

export function validateVersion(version) {
  if (!/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(String(version))) {
    throw new Error(`Invalid application version ${JSON.stringify(version)}.`)
  }
}

/** A release tag must be exactly `v<version>` from tauri.conf.json. */
export function validateTag(tag, version) {
  validateVersion(version)
  if (tag !== `v${version}`) {
    throw new Error(
      `Tag ${JSON.stringify(tag)} does not match the application version ${version}; expected v${version}. Bump apps/app/tauri.conf.json or fix the tag.`,
    )
  }
}

function readEdges(path, size) {
  const fd = openSync(path, 'r')
  try {
    const head = Buffer.alloc(Math.min(64, size))
    readSync(fd, head, 0, head.length, 0)
    const tail = Buffer.alloc(Math.min(512, size))
    readSync(fd, tail, 0, tail.length, size - tail.length)
    return { head, tail }
  } finally {
    closeSync(fd)
  }
}

export function sha256File(path) {
  return createHash('sha256').update(readFileSync(path)).digest('hex')
}

/**
 * Checks one installer's name, version, size and format signature.
 * `limits` exists so tests can use small fixtures.
 */
export function validateInstaller(dir, name, version, limits = {}) {
  const min = limits.min ?? MIN_INSTALLER_BYTES
  const max = limits.max ?? MAX_INSTALLER_BYTES
  if (!SAFE_NAME.test(name)) throw new Error(`Unsafe installer file name ${JSON.stringify(name)}.`)
  const type = installerType(name)
  if (!type) throw new Error(`${name} is not a recognised installer type.`)
  if (!name.includes(version))
    throw new Error(`${name} does not carry the release version ${version}.`)
  const path = join(dir, name)
  const stat = statSync(path)
  if (!stat.isFile()) throw new Error(`${name} is not a regular file.`)
  if (stat.size < min)
    throw new Error(`${name} is only ${stat.size} bytes; the build is incomplete.`)
  if (stat.size > max)
    throw new Error(`${name} is ${stat.size} bytes, above the ${max}-byte limit.`)
  const { head, tail } = readEdges(path, stat.size)
  if (!type.check(head, tail))
    throw new Error(`${name} does not have a valid ${type.ext} file signature.`)
  return { name, size: stat.size, platform: type.platform, sha256: sha256File(path) }
}

export function parseSums(text) {
  const entries = new Map()
  for (const [index, raw] of text.split('\n').entries()) {
    const line = raw.replace(/\r$/, '')
    if (!line.trim()) continue
    const match = /^([0-9a-f]{64}) [ *](.+)$/.exec(line)
    if (!match) throw new Error(`${CHECKSUM_FILE} line ${index + 1} is malformed.`)
    const [, hash, name] = match
    if (entries.has(name)) throw new Error(`${name} is listed more than once in ${CHECKSUM_FILE}.`)
    entries.set(name, hash)
  }
  if (entries.size === 0) throw new Error(`${CHECKSUM_FILE} lists no installers.`)
  return entries
}

export function formatSums(results) {
  return (
    [...results]
      .sort((a, b) => a.name.localeCompare(b.name))
      .map((r) => `${r.sha256}  ${r.name}`)
      .join('\n') + '\n'
  )
}

function listDir(dir) {
  return readdirSync(dir).filter((name) => !name.startsWith('.'))
}

/** Validates every installer in `dir`; nothing else may be present. */
export function collect(dir, version, limits) {
  const names = listDir(dir).filter((name) => !/^SHA256SUMS(-.+)?\.txt$/.test(name))
  if (names.length === 0) throw new Error('No installers were produced.')
  return names.map((name) => validateInstaller(dir, name, version, limits))
}

/**
 * Verifies a merged release directory: SHA256SUMS.txt lists exactly the
 * installers present, every hash matches, and (optionally) every platform is present.
 */
export function verify(dir, version, { requirePlatforms = false, limits } = {}) {
  const sums = parseSums(readFileSync(join(dir, CHECKSUM_FILE), 'utf8'))
  const present = listDir(dir).filter((name) => name !== CHECKSUM_FILE)
  const stray = present.filter((name) => !sums.has(name))
  if (stray.length) throw new Error(`Files not covered by ${CHECKSUM_FILE}: ${stray.join(', ')}`)
  const missing = [...sums.keys()].filter((name) => !present.includes(name))
  if (missing.length)
    throw new Error(`Listed in ${CHECKSUM_FILE} but missing: ${missing.join(', ')}`)
  const results = present.map((name) => {
    const result = validateInstaller(dir, name, version, limits)
    if (result.sha256 !== sums.get(name)) throw new Error(`Checksum mismatch for ${name}.`)
    return result
  })
  if (requirePlatforms) {
    const absent = REQUIRED_PLATFORM_TYPES.filter(
      (ext) => !results.some((r) => r.name.endsWith(ext)),
    )
    if (absent.length)
      throw new Error(`The release is missing required installer types: ${absent.join(', ')}`)
  }
  return results
}

function parseArgs(argv) {
  const [command, ...rest] = argv
  const options = {}
  for (let i = 0; i < rest.length; i++) {
    const key = rest[i]
    if (!key.startsWith('--')) throw new Error(`Unexpected argument ${key}`)
    const next = rest[i + 1]
    if (next === undefined || next.startsWith('--')) options[key.slice(2)] = true
    else options[key.slice(2)] = rest[++i]
  }
  return { command, options }
}

export function main(argv) {
  const { command, options } = parseArgs(argv)
  const version = options.version
  validateVersion(version)
  if (command === 'tag') {
    validateTag(options.tag, version)
    console.log(`Tag ${options.tag} matches version ${version}.`)
  } else if (command === 'collect') {
    if (!options.dir || !options.sums) throw new Error('collect needs --dir and --sums')
    const results = collect(options.dir, version)
    writeFileSync(join(options.dir, options.sums), formatSums(results))
    for (const r of results) console.log(`OK ${r.name} ${r.size} bytes ${r.sha256}`)
  } else if (command === 'verify') {
    if (!options.dir) throw new Error('verify needs --dir')
    const results = verify(options.dir, version, {
      requirePlatforms: Boolean(options['require-platforms']),
    })
    for (const r of results) console.log(`VERIFIED ${r.name} ${r.sha256}`)
  } else {
    throw new Error(`Unknown command ${command}; use collect, verify or tag.`)
  }
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  try {
    main(process.argv.slice(2))
  } catch (error) {
    console.error(`::error::${error.message}`)
    process.exit(1)
  }
}
