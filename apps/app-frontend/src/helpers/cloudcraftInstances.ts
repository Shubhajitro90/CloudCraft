export type Loader = 'vanilla' | 'fabric' | 'forge' | 'quilt' | 'neoforge'

export const LOADERS: { id: Loader; label: string; metadataKey: string | null }[] = [
  { id: 'vanilla', label: 'Vanilla', metadataKey: null },
  { id: 'fabric', label: 'Fabric', metadataKey: 'fabric' },
  { id: 'forge', label: 'Forge', metadataKey: 'forge' },
  { id: 'quilt', label: 'Quilt', metadataKey: 'quilt' },
  { id: 'neoforge', label: 'NeoForge', metadataKey: 'neo' },
]

export interface GameVersionManifest {
  versions?: { id: string; type?: string }[]
}

export interface LoaderManifest {
  gameVersions?: { id: string; loaders?: { id: string }[] }[]
}

export interface NewInstanceInput {
  name: string
  gameVersion: string
  loader: Loader
  loaderVersion: string
}

export interface ExistingInstance {
  path: string
  name?: string
}

export const MAX_INSTANCE_NAME_LENGTH = 64
const FORBIDDEN_NAME_CHARACTERS = /[<>:"/\\|?*]/

function hasControlCharacter(value: string): boolean {
  return [...value].some((character) => character.charCodeAt(0) < 32)
}
const RESERVED_NAMES = /^(con|prn|aux|nul|com[1-9]|lpt[1-9])(\..*)?$/i

export function validateInstanceName(name: string, existing: ExistingInstance[]): string {
  const trimmed = name.trim()
  if (!trimmed) return 'Enter a name for the instance.'
  if (trimmed.length > MAX_INSTANCE_NAME_LENGTH) {
    return `Use ${MAX_INSTANCE_NAME_LENGTH} characters or fewer.`
  }
  if (FORBIDDEN_NAME_CHARACTERS.test(trimmed) || hasControlCharacter(trimmed)) {
    return 'The name cannot contain < > : " / \\ | ? * or control characters.'
  }
  if (
    trimmed === '.' ||
    trimmed === '..' ||
    trimmed.endsWith('.') ||
    RESERVED_NAMES.test(trimmed)
  ) {
    return 'That name is not allowed as a folder name.'
  }
  if (existing.some((item) => (item.name ?? '').trim().toLowerCase() === trimmed.toLowerCase())) {
    return 'An instance with that name already exists.'
  }
  return ''
}

export function releaseGameVersions(manifest: GameVersionManifest | null | undefined): string[] {
  return (manifest?.versions ?? []).filter((v) => v.type === 'release').map((v) => v.id)
}

export function compatibleGameVersions(
  loader: Loader,
  gameManifest: GameVersionManifest | null | undefined,
  loaderManifest: LoaderManifest | null | undefined,
): string[] {
  const releases = releaseGameVersions(gameManifest)
  if (loader === 'vanilla') return releases
  const supported = new Set((loaderManifest?.gameVersions ?? []).map((v) => v.id))
  return releases.filter((id) => supported.has(id))
}

export function loaderVersionChoices(
  loader: Loader,
  gameVersion: string,
  loaderManifest: LoaderManifest | null | undefined,
): string[] {
  if (loader === 'vanilla' || !loaderManifest?.gameVersions) return []
  const entries = loaderManifest.gameVersions
  // Fabric and Quilt list one shared loader set under a placeholder entry.
  const entry =
    loader === 'fabric' || loader === 'quilt'
      ? entries[0]
      : entries.find((item) => item.id === gameVersion)
  return (entry?.loaders ?? []).map((item) => item.id)
}

export function validateNewInstance(
  input: NewInstanceInput,
  existing: ExistingInstance[],
  availableGameVersions: string[],
  availableLoaderVersions: string[],
): string[] {
  const issues: string[] = []
  const nameIssue = validateInstanceName(input.name, existing)
  if (nameIssue) issues.push(nameIssue)
  if (!input.gameVersion) issues.push('Choose a Minecraft version.')
  else if (!availableGameVersions.includes(input.gameVersion)) {
    issues.push(`Minecraft ${input.gameVersion} is not available for ${input.loader}.`)
  }
  if (input.loader !== 'vanilla') {
    if (!input.loaderVersion) issues.push('Choose a loader version.')
    else if (!availableLoaderVersions.includes(input.loaderVersion)) {
      issues.push(`Loader version ${input.loaderVersion} is not available.`)
    }
  }
  return issues
}

export interface InstanceDeps {
  create: (
    name: string,
    gameVersion: string,
    loader: string,
    loaderVersion: string | null,
    iconPath: string | null,
    skipInstall: boolean,
  ) => Promise<string>
  install: (path: string, force: boolean) => Promise<unknown>
  remove: (path: string) => Promise<unknown>
  processesFor: (path: string) => Promise<unknown[]>
}

export type CreateOutcome =
  | { kind: 'ready'; path: string }
  | { kind: 'needs_repair'; path: string; message: string }
  | { kind: 'failed'; message: string }

function describe(error: unknown): string {
  if (error instanceof Error) return error.message
  if (typeof error === 'string') return error
  if (error && typeof error === 'object' && 'message' in error) {
    return String((error as { message: unknown }).message)
  }
  return String(error)
}

export async function createAndInstall(
  input: NewInstanceInput,
  deps: InstanceDeps,
  onStage?: (stage: 'creating' | 'installing') => void,
): Promise<CreateOutcome> {
  let path: string
  onStage?.('creating')
  try {
    path = await deps.create(
      input.name.trim(),
      input.gameVersion,
      input.loader,
      input.loader === 'vanilla' ? null : input.loaderVersion,
      null,
      true,
    )
  } catch (error) {
    return { kind: 'failed', message: `Could not create the instance: ${describe(error)}` }
  }
  onStage?.('installing')
  try {
    await deps.install(path, false)
  } catch (error) {
    // The profile exists on disk; keep it so the user can retry without losing the entry.
    return {
      kind: 'needs_repair',
      path,
      message: `The instance was created but installation failed: ${describe(error)}. Use Repair to retry.`,
    }
  }
  return { kind: 'ready', path }
}

export async function deleteInstance(
  instance: ExistingInstance,
  deps: Pick<InstanceDeps, 'remove' | 'processesFor'>,
): Promise<{ deleted: boolean; message: string }> {
  if (!instance.path) return { deleted: false, message: 'No instance selected.' }
  let processes: unknown[]
  try {
    processes = await deps.processesFor(instance.path)
  } catch (error) {
    return { deleted: false, message: `Could not check whether it is running: ${describe(error)}` }
  }
  if (processes.length > 0) {
    return { deleted: false, message: 'Stop the game before deleting this instance.' }
  }
  try {
    await deps.remove(instance.path)
  } catch (error) {
    return { deleted: false, message: `Could not delete the instance: ${describe(error)}` }
  }
  return { deleted: true, message: `${instance.name ?? 'The instance'} was deleted.` }
}

export interface JavaDeps {
  getOptimalJre: (path: string) => Promise<{ parsed_version?: number | string } | null>
  autoInstallJava: (major: number) => Promise<unknown>
}

export async function installRequiredJava(
  instancePath: string,
  deps: JavaDeps,
): Promise<{ installed: boolean; message: string }> {
  let requirement: { parsed_version?: number | string } | null
  try {
    requirement = await deps.getOptimalJre(instancePath)
  } catch (error) {
    return { installed: false, message: `Could not determine the Java version: ${describe(error)}` }
  }
  const major = Number(requirement?.parsed_version)
  if (!Number.isInteger(major) || major <= 0) {
    return { installed: false, message: 'This instance did not report a Java version to install.' }
  }
  try {
    await deps.autoInstallJava(major)
  } catch (error) {
    return { installed: false, message: `Java ${major} could not be installed: ${describe(error)}` }
  }
  return { installed: true, message: `Java ${major} is installed.` }
}

export interface AccountLike {
  id: string
  username?: string
}

export function summarizeAccounts(
  accounts: AccountLike[],
  defaultId: string | null | undefined,
): { selected: AccountLike | null; others: AccountLike[]; needsSignIn: boolean } {
  const selected = accounts.find((account) => account.id === defaultId) ?? null
  return {
    selected,
    others: accounts.filter((account) => account.id !== selected?.id),
    needsSignIn: accounts.length === 0,
  }
}
