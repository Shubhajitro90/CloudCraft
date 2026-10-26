/** Pure, dependency-free logic for instance editing, exit reporting and cloud host forms. */

export interface ProcessExit {
  uuid?: string
  profile_path: string
  start_time: string
  end_time: string
  exit_code: number | null
  success: boolean
  stopped_by_user: boolean
}

export type ExitKind = 'clean' | 'stopped' | 'crashed'

export interface ExitSummary {
  kind: ExitKind
  title: string
  detail: string
}

function formatDuration(startIso: string, endIso: string): string {
  const ms = Date.parse(endIso) - Date.parse(startIso)
  if (!Number.isFinite(ms) || ms < 0) return ''
  const seconds = Math.round(ms / 1000)
  if (seconds < 60) return `${seconds}s`
  const minutes = Math.floor(seconds / 60)
  if (minutes < 60) return `${minutes}m ${seconds % 60}s`
  return `${Math.floor(minutes / 60)}h ${minutes % 60}m`
}

export function describeExit(exit: ProcessExit): ExitSummary {
  const played = formatDuration(exit.start_time, exit.end_time)
  const after = played ? ` after ${played}` : ''
  if (exit.stopped_by_user) {
    return { kind: 'stopped', title: 'Stopped', detail: `You stopped the game${after}.` }
  }
  if (exit.success) {
    return {
      kind: 'clean',
      title: 'Exited normally',
      detail: `Minecraft closed${after} (exit code 0).`,
    }
  }
  const code =
    exit.exit_code === null
      ? 'was terminated by the operating system'
      : `exited with code ${exit.exit_code}`
  return {
    kind: 'crashed',
    title: 'Crashed',
    detail: `Minecraft ${code}${after}. Check the crash report and logs, then try Repair or a different Java runtime.`,
  }
}

/** Splits JVM arguments on whitespace, honouring single and double quotes. */
export function parseJvmArgs(input: string): { args: string[]; error: string } {
  if (/[\r\n\0]/.test(input)) return { args: [], error: 'Put all arguments on one line.' }
  const args: string[] = []
  let current = ''
  let quote: string | null = null
  let started = false
  for (const char of input) {
    if (quote) {
      if (char === quote) quote = null
      else current += char
    } else if (char === '"' || char === "'") {
      quote = char
      started = true
    } else if (/\s/.test(char)) {
      if (started) args.push(current)
      current = ''
      started = false
    } else {
      current += char
      started = true
    }
  }
  if (quote) return { args: [], error: 'A quote is not closed.' }
  if (started) args.push(current)
  if (args.length > 64) return { args: [], error: 'Use at most 64 arguments.' }
  if (args.some((arg) => arg.length > 512)) return { args: [], error: 'An argument is too long.' }
  return { args, error: '' }
}

export function formatJvmArgs(args: string[] | null | undefined): string {
  return (args || [])
    .map((arg) => (/[\s"']/.test(arg) ? `"${arg.replace(/"/g, '')}"` : arg))
    .join(' ')
}

export interface InstanceEditForm {
  name: string
  memoryOverride: boolean
  maxMemoryMb: string
  javaPath: string
  jvmArgs: string
}

export interface InstanceEdit {
  name: string
  memory: { maximum: number } | null
  java_path: string | null
  extra_launch_args: string[] | null
}

export function buildInstanceEdit(
  form: InstanceEditForm,
  systemMaxMb: number | null,
): { edit: InstanceEdit | null; errors: string[] } {
  const errors: string[] = []
  const name = form.name.trim()
  if (!name) errors.push('Enter a name.')
  else if (name.length > 64) errors.push('Use at most 64 characters for the name.')
  else if (/[\\/:*?"<>|\0]/.test(name)) errors.push('The name cannot contain \\ / : * ? " < > |.')

  let memory: InstanceEdit['memory'] = null
  if (form.memoryOverride) {
    const value = Number(form.maxMemoryMb.trim())
    if (!/^\d+$/.test(form.maxMemoryMb.trim()) || value < 512) {
      errors.push('Maximum memory must be a whole number of at least 512 MB.')
    } else if (systemMaxMb && value > systemMaxMb) {
      errors.push(`Maximum memory cannot exceed this computer's ${systemMaxMb} MB.`)
    } else {
      memory = { maximum: value }
    }
  }

  const javaPath = form.javaPath.trim()
  if (javaPath && !/^([a-zA-Z]:[\\/]|\/)/.test(javaPath)) {
    errors.push('The Java path must be an absolute path to a java executable.')
  }

  const parsed = parseJvmArgs(form.jvmArgs)
  if (parsed.error) errors.push(`JVM arguments: ${parsed.error}`)

  if (errors.length) return { edit: null, errors }
  return {
    edit: {
      name,
      memory,
      java_path: javaPath || null,
      extra_launch_args: parsed.args.length ? parsed.args : null,
    },
    errors,
  }
}

export interface CloudHostForm {
  host: string
  apiPort: string
  username: string
  launchCommand: string
  workingDir: string
  moonlightPath: string
  resolution: string
  fps: string
}

export function validateCloudHostForm(form: CloudHostForm): string[] {
  const errors: string[] = []
  const host = form.host.trim()
  if (!host) errors.push('Enter the remote computer hostname or IP address.')
  else if (host.includes('://') || host.includes('/'))
    errors.push('Enter only the hostname, without https:// or a path.')
  else if (!/^[A-Za-z0-9._:-]+$/.test(host) || host.startsWith('-'))
    errors.push('The hostname contains unsupported characters.')
  const port = Number(form.apiPort)
  if (!/^\d+$/.test(form.apiPort.trim()) || port < 1 || port > 65535)
    errors.push('The Sunshine API port must be between 1 and 65535.')
  if (!form.username.trim()) errors.push('Enter the Sunshine username.')
  if (!form.launchCommand.trim())
    errors.push('Enter the command that starts Minecraft on the host.')
  if (/[\r\n]/.test(form.launchCommand)) errors.push('The launch command must be one line.')
  if (!/^([a-zA-Z]:[\\/]|\/)/.test(form.moonlightPath.trim()))
    errors.push('Enter the absolute path of the Moonlight program on this device.')
  if (form.resolution.trim() && !/^\d{3,4}x\d{3,4}$/.test(form.resolution.trim()))
    errors.push('Resolution must look like 1920x1080.')
  if (form.fps.trim()) {
    const fps = Number(form.fps)
    if (!/^\d+$/.test(form.fps.trim()) || fps < 30 || fps > 240)
      errors.push('Frame rate must be between 30 and 240.')
  }
  return errors
}
