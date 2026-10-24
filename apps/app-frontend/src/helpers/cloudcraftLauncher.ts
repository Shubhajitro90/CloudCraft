export type InstallStage = 'installed' | 'installing' | 'pack_installing' | 'not_installed' | string

export interface LauncherInstance {
  path: string
  name?: string
  game_version?: string
  install_stage?: InstallStage
}

export interface MemorySettings {
  min: number | null
  max: number | null
}

export interface Playability {
  canPlay: boolean
  needsInstall: boolean
  busy: boolean
  reason: string
}

export function playability(instance: LauncherInstance | null | undefined): Playability {
  const stage = instance?.install_stage
  if (!instance?.path) {
    return { canPlay: false, needsInstall: false, busy: false, reason: 'No instance selected.' }
  }
  if (stage === 'installed') return { canPlay: true, needsInstall: false, busy: false, reason: '' }
  if (stage === 'installing' || stage === 'pack_installing') {
    return {
      canPlay: false,
      needsInstall: false,
      busy: true,
      reason: 'This instance is still installing.',
    }
  }
  return {
    canPlay: false,
    needsInstall: true,
    busy: false,
    reason: 'This instance is not installed. Install or repair it first.',
  }
}

export function parseMemoryField(value: string): { value: number | null; error: string } {
  const trimmed = value.trim()
  if (trimmed === '') return { value: null, error: '' }
  if (!/^\d+$/.test(trimmed)) return { value: null, error: 'Use a whole number of MB.' }
  return { value: Number(trimmed), error: '' }
}

export function validateMemory(memory: MemorySettings, systemMaxMb: number | null): string[] {
  const issues: string[] = []
  const { min, max } = memory
  if (min !== null && min < 256) issues.push('Minimum memory must be at least 256 MB.')
  if (max !== null && max < 512) issues.push('Maximum memory must be at least 512 MB.')
  if (min !== null && max !== null && min > max) {
    issues.push('Minimum memory cannot exceed maximum memory.')
  }
  if (systemMaxMb !== null && systemMaxMb > 0) {
    if (max !== null && max > systemMaxMb) {
      issues.push(`Maximum memory exceeds the ${systemMaxMb} MB available on this device.`)
    }
    if (min !== null && min > systemMaxMb) {
      issues.push(`Minimum memory exceeds the ${systemMaxMb} MB available on this device.`)
    }
  }
  return issues
}

export interface JavaRequirement {
  parsed_version?: number | string
  path?: string
}

export interface LauncherDeps {
  getDefaultUser: () => Promise<unknown>
  getOptimalJre: (path: string) => Promise<JavaRequirement | null>
  testJre: (path: string, major: number, minor: number) => Promise<boolean>
  getSettings: () => Promise<{ memory?: MemorySettings } | null>
  getMaxMemoryKib: () => Promise<number>
  run: (path: string) => Promise<unknown>
  kill: (path: string) => Promise<unknown>
  processesFor: (path: string) => Promise<unknown[]>
}

export type PreflightIssue = {
  code: 'not_installed' | 'installing' | 'no_account' | 'no_java' | 'memory' | 'already_running'
  message: string
}

export async function preflightLaunch(
  instance: LauncherInstance,
  deps: LauncherDeps,
): Promise<PreflightIssue[]> {
  const issues: PreflightIssue[] = []
  const state = playability(instance)
  if (state.needsInstall) issues.push({ code: 'not_installed', message: state.reason })
  if (state.busy) issues.push({ code: 'installing', message: state.reason })

  if ((await deps.processesFor(instance.path)).length > 0) {
    issues.push({ code: 'already_running', message: 'This instance is already running.' })
  }

  const user = await deps.getDefaultUser().catch(() => null)
  if (!user) {
    issues.push({
      code: 'no_account',
      message: 'Sign in with a Microsoft account that owns Minecraft before launching.',
    })
  }

  const java = await deps.getOptimalJre(instance.path).catch(() => null)
  if (!java?.path) {
    issues.push({
      code: 'no_java',
      message:
        `No compatible Java runtime was found for Minecraft ${instance.game_version ?? ''}. Install one from Performance settings.`.replace(
          '  ',
          ' ',
        ),
    })
  }

  const settings = await deps.getSettings().catch(() => null)
  if (settings?.memory) {
    const systemKib = await deps.getMaxMemoryKib().catch(() => 0)
    const memoryIssues = validateMemory(
      settings.memory,
      systemKib ? Math.floor(systemKib / 1024) : null,
    )
    for (const message of memoryIssues) issues.push({ code: 'memory', message })
  }
  return issues
}

export type LaunchOutcome =
  | { kind: 'launched' }
  | { kind: 'blocked'; issues: PreflightIssue[] }
  | { kind: 'failed'; message: string }

export function describeError(error: unknown): string {
  if (error instanceof Error) return error.message
  if (typeof error === 'string') return error
  if (error && typeof error === 'object' && 'message' in error) {
    return String((error as { message: unknown }).message)
  }
  return String(error)
}

export async function launchInstance(
  instance: LauncherInstance,
  deps: LauncherDeps,
): Promise<LaunchOutcome> {
  let issues: PreflightIssue[]
  try {
    issues = await preflightLaunch(instance, deps)
  } catch (error) {
    return { kind: 'failed', message: describeError(error) }
  }
  if (issues.length > 0) return { kind: 'blocked', issues }
  try {
    await deps.run(instance.path)
  } catch (error) {
    return { kind: 'failed', message: describeError(error) }
  }
  return { kind: 'launched' }
}

export async function stopInstance(
  path: string,
  deps: Pick<LauncherDeps, 'kill' | 'processesFor'>,
): Promise<{ stopped: boolean; message: string }> {
  if ((await deps.processesFor(path)).length === 0) {
    return { stopped: true, message: 'The game is not running.' }
  }
  try {
    await deps.kill(path)
  } catch (error) {
    return { stopped: false, message: describeError(error) }
  }
  const remaining = await deps.processesFor(path)
  return remaining.length === 0
    ? { stopped: true, message: 'The game was stopped.' }
    : { stopped: false, message: 'The game process is still running.' }
}

export interface ProcessEvent {
  profile_path_id: string
  event: string
  uuid?: string
  message?: string
}

export type RunningMap = Record<string, true>

export function applyProcessEvent(running: RunningMap, event: ProcessEvent): RunningMap {
  const next = { ...running }
  if (event.event === 'launched') next[event.profile_path_id] = true
  else if (event.event === 'finished') delete next[event.profile_path_id]
  return next
}

export async function reconcileRunning(
  paths: string[],
  processesFor: (path: string) => Promise<unknown[]>,
): Promise<RunningMap> {
  const result: RunningMap = {}
  await Promise.all(
    paths.map(async (path) => {
      const processes = await processesFor(path).catch(() => [])
      if (processes.length > 0) result[path] = true
    }),
  )
  return result
}
