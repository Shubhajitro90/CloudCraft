<script setup>
import { computed, onMounted, ref, watch } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { open as openDialog } from '@tauri-apps/plugin-dialog'
import { RouterLink, useRouter } from 'vue-router'
import { useBreadcrumbs } from '@/store/breadcrumbs'
import { list, run } from '@/helpers/profile'
import { get as getSettings, set as setSettings } from '@/helpers/settings'
import { cloudSessionProvider } from '@/helpers/cloudSession'
import { cloudStorageProvider } from '@/helpers/cloudStorage'
import {
  CompassIcon,
  DownloadIcon,
  FolderOpenIcon,
  GlobeIcon,
  LibraryIcon,
  PlayIcon,
  SettingsIcon,
} from '@modrinth/assets'

const breadcrumbs = useBreadcrumbs()
const router = useRouter()
breadcrumbs.setRootContext({ name: 'CloudCraft', link: '/cloudcraft' })

const activeSection = ref('overview')
const serverAddress = ref('')
const copied = ref(false)
const saveName = ref('')
const saveNotes = ref('')
const instances = ref([])
const instancesLoading = ref(true)
const cloudInstancePath = ref('')
const cloudSessionBusy = ref(false)
const cloudSessionError = ref('')
const cloudStorageStatus = ref({
  state: 'unconfigured',
  message: 'Checking cloud storage provider status…',
  canUpload: false,
  canDownload: false,
  canDelete: false,
})
const instanceError = ref('')
const launchingPath = ref('')
const launchError = ref('')
const launcherSettings = ref(null)
const memoryMin = ref('')
const memoryMax = ref('')
const settingsLoading = ref(false)
const settingsMessage = ref('')
const settingsError = ref('')
function readLocalList(key) {
  try {
    const value = JSON.parse(localStorage.getItem(key) || '[]')
    return Array.isArray(value) ? value : []
  } catch {
    return []
  }
}

const savedAddresses = ref(readLocalList('cloudcraft.server-addresses'))
const backupHistory = ref(readLocalList('cloudcraft.save-reminders'))
const worldBackupHistory = ref(readLocalList('cloudcraft.world-backups'))
const cloudSessionStatus = ref({
  state: 'unavailable',
  message: 'Checking cloud gaming provider status…',
  canStart: false,
  canStop: false,
})
const backupBusy = ref(false)
const backupError = ref('')
const backupMessage = ref('')
watch(
  backupHistory,
  (entries) => {
    try {
      localStorage.setItem('cloudcraft.save-reminders', JSON.stringify(entries))
    } catch (error) {
      console.error('Could not save reminder list locally', error)
    }
  },
  { deep: true },
)
watch(
  worldBackupHistory,
  (entries) => {
    try {
      localStorage.setItem('cloudcraft.world-backups', JSON.stringify(entries))
    } catch (error) {
      console.error('Could not save world backup history locally', error)
    }
  },
  { deep: true },
)

const sections = [
  { id: 'overview', label: 'Overview' },
  { id: 'cloud', label: 'Cloud' },
  { id: 'instances', label: 'Instances' },
  { id: 'mods', label: 'Mods & modpacks' },
  { id: 'performance', label: 'Performance' },
  { id: 'servers', label: 'Servers' },
  { id: 'saves', label: 'Saves & backups' },
]

async function loadInstances() {
  instancesLoading.value = true
  instanceError.value = ''
  try {
    const profiles = await list()
    instances.value = Array.isArray(profiles) ? profiles : Object.values(profiles || {})
    if (!cloudInstancePath.value) {
      cloudInstancePath.value =
        instances.value.find((instance) => instance.install_stage === 'installed')?.path || ''
    }
  } catch (error) {
    instanceError.value = String(error?.message || error)
  } finally {
    instancesLoading.value = false
  }
}

async function launchInstance(instance) {
  if (!instance?.path || launchingPath.value) return
  launchingPath.value = instance.path
  launchError.value = ''
  try {
    await run(instance.path)
  } catch (error) {
    launchError.value = String(error?.message || error)
  } finally {
    launchingPath.value = ''
  }
}

async function loadCloudSessionStatus() {
  try {
    cloudSessionStatus.value = await cloudSessionProvider.getStatus()
  } catch (error) {
    cloudSessionStatus.value = {
      state: 'error',
      message: String(error?.message || error),
      canStart: false,
      canStop: false,
    }
  }
}

async function loadCloudStorageStatus() {
  try {
    cloudStorageStatus.value = await cloudStorageProvider.getStatus()
  } catch (error) {
    cloudStorageStatus.value = {
      state: 'error',
      message: String(error?.message || error),
      canUpload: false,
      canDownload: false,
      canDelete: false,
    }
  }
}

async function startCloudSession() {
  const instance = instances.value.find((item) => item.path === cloudInstancePath.value)
  if (
    !instance ||
    instance.install_stage !== 'installed' ||
    !cloudSessionStatus.value.canStart ||
    cloudSessionBusy.value
  ) {
    return
  }

  cloudSessionBusy.value = true
  cloudSessionError.value = ''
  try {
    cloudSessionStatus.value = await cloudSessionProvider.startSession({
      instancePath: instance.path,
      gameVersion: instance.game_version,
    })
  } catch (error) {
    cloudSessionError.value = String(error?.message || error)
  } finally {
    cloudSessionBusy.value = false
  }
}

async function stopCloudSession() {
  const sessionId = cloudSessionStatus.value.sessionId
  if (!sessionId || !cloudSessionStatus.value.canStop || cloudSessionBusy.value) return

  cloudSessionBusy.value = true
  cloudSessionError.value = ''
  try {
    cloudSessionStatus.value = await cloudSessionProvider.stopSession(sessionId)
  } catch (error) {
    cloudSessionError.value = String(error?.message || error)
  } finally {
    cloudSessionBusy.value = false
  }
}

async function cancelCloudSessionStart() {
  if (cloudSessionStatus.value.state !== 'starting' || cloudSessionBusy.value) return

  cloudSessionBusy.value = true
  cloudSessionError.value = ''
  try {
    await cloudSessionProvider.cancelStart()
    await loadCloudSessionStatus()
  } catch (error) {
    cloudSessionError.value = String(error?.message || error)
  } finally {
    cloudSessionBusy.value = false
  }
}

async function loadLauncherSettings() {
  settingsLoading.value = true
  settingsError.value = ''
  try {
    launcherSettings.value = await getSettings()
    memoryMin.value = String(launcherSettings.value?.memory?.min ?? '')
    memoryMax.value = String(launcherSettings.value?.memory?.max ?? '')
  } catch (error) {
    settingsError.value = String(error?.message || error)
  } finally {
    settingsLoading.value = false
  }
}

async function saveMemorySettings() {
  if (!launcherSettings.value) return
  const min = memoryMin.value === '' ? null : Number(memoryMin.value)
  const max = Number(memoryMax.value)
  const maxSetting = 4_294_967_295
  if (
    (min !== null && (!Number.isInteger(min) || min < 256 || min > maxSetting)) ||
    !Number.isInteger(max) ||
    max < 512 ||
    max > maxSetting ||
    (min !== null && min > max)
  ) {
    settingsError.value =
      'Enter whole-number memory values in MB. Minimum must be at least 256 MB and cannot exceed maximum.'
    return
  }
  settingsLoading.value = true
  settingsError.value = ''
  settingsMessage.value = ''
  try {
    // Re-read the full object before saving so unrelated settings changed elsewhere
    // are not overwritten by an older copy held by this page.
    const currentSettings = await getSettings()
    const updated = {
      ...currentSettings,
      memory: { ...currentSettings.memory, min, max },
    }
    await setSettings(updated)
    launcherSettings.value = updated
    settingsMessage.value = 'Launcher memory settings saved.'
  } catch (error) {
    settingsError.value = String(error?.message || error)
  } finally {
    settingsLoading.value = false
  }
}

function openInstance(instance) {
  if (instance?.path) router.push(`/instance/${encodeURIComponent(instance.path)}`)
}

onMounted(() => {
  loadInstances()
  loadLauncherSettings()
  loadCloudSessionStatus()
  loadCloudStorageStatus()
})
const hasServerAddress = computed(() => serverAddress.value.trim().length > 0)

function rememberServer() {
  const address = serverAddress.value.trim()
  if (!address) return
  try {
    if (!savedAddresses.value.includes(address)) {
      savedAddresses.value = [address, ...savedAddresses.value].slice(0, 10)
      localStorage.setItem('cloudcraft.server-addresses', JSON.stringify(savedAddresses.value))
    }
    serverAddress.value = address
  } catch (error) {
    console.error('Could not save server address locally', error)
  }
}

function removeServerAddress(address) {
  savedAddresses.value = savedAddresses.value.filter((item) => item !== address)
  try {
    localStorage.setItem('cloudcraft.server-addresses', JSON.stringify(savedAddresses.value))
  } catch (error) {
    console.error('Could not update server addresses locally', error)
  }
}

async function copyAddress() {
  if (!serverAddress.value.trim()) return
  try {
    await navigator.clipboard.writeText(serverAddress.value.trim())
    copied.value = true
    window.setTimeout(() => (copied.value = false), 1800)
  } catch {
    copied.value = false
  }
}

async function chooseDirectory(title) {
  const selection = await openDialog({ directory: true, multiple: false, title })
  return Array.isArray(selection) ? selection[0] || null : selection
}

function formatBackupTimestamp(value) {
  const timestamp = new Date(value)
  return Number.isNaN(timestamp.getTime()) ? 'Unknown date' : timestamp.toLocaleString()
}

async function createWorldBackup() {
  backupBusy.value = true
  backupError.value = ''
  backupMessage.value = ''
  try {
    const sourcePath = await chooseDirectory('Choose the Minecraft world folder to back up')
    if (!sourcePath) return
    const backupRoot = await chooseDirectory('Choose a destination folder for the world backup')
    if (!backupRoot) return
    const destination = String(await invoke('cloudcraft_backup_world', { sourcePath, backupRoot }))
    const name = destination.split(/[\\/]/).filter(Boolean).pop() || 'World backup'
    worldBackupHistory.value = [
      {
        name,
        path: destination,
        createdAt: new Date().toISOString(),
      },
      ...worldBackupHistory.value,
    ].slice(0, 50)
    backupMessage.value = `World backup created: ${destination}`
  } catch (error) {
    backupError.value = String(error?.message || error)
  } finally {
    backupBusy.value = false
  }
}

async function restoreWorldBackup(backupEntry = null) {
  backupBusy.value = true
  backupError.value = ''
  backupMessage.value = ''
  try {
    const backupPath =
      backupEntry?.path || (await chooseDirectory('Choose a CloudCraft world backup folder'))
    if (!backupPath) return
    const destinationParent = await chooseDirectory(
      'Choose the folder where the restored world should be created',
    )
    if (!destinationParent) return
    const destination = String(
      await invoke('cloudcraft_restore_world', { backupPath, destinationParent }),
    )
    backupMessage.value = `World restored to: ${destination}`
  } catch (error) {
    backupError.value = String(error?.message || error)
  } finally {
    backupBusy.value = false
  }
}

function exportSaveManifest() {
  const manifest = {
    format: 'cloudcraft-save-manifest',
    version: 1,
    createdAt: new Date().toISOString(),
    note: 'This file contains only the label and notes entered here. It does not contain Minecraft worlds, launcher instances, credentials, or cloud-hosted data.',
    entries: backupHistory.value,
  }
  const blob = new Blob([JSON.stringify(manifest, null, 2)], { type: 'application/json' })
  const url = URL.createObjectURL(blob)
  const anchor = document.createElement('a')
  anchor.href = url
  anchor.download = 'cloudcraft-save-manifest.json'
  anchor.click()
  URL.revokeObjectURL(url)
}

function addSaveNote() {
  const name = saveName.value.trim()
  if (!name) return
  const entry = { name, notes: saveNotes.value.trim(), createdAt: new Date().toISOString() }
  backupHistory.value = [entry, ...backupHistory.value].slice(0, 50)
  saveName.value = ''
  saveNotes.value = ''
}
</script>

<template>
  <main class="cloudcraft-page p-6 md:p-8 flex flex-col gap-6">
    <header class="flex flex-col gap-4">
      <div class="flex flex-wrap items-center justify-between gap-4">
        <div>
          <p class="eyebrow m-0 mb-2">YOUR MINECRAFT SPACE</p>
          <h1 class="m-0 text-3xl md:text-4xl font-bold tracking-tight">CloudCraft</h1>
          <p class="mt-2 mb-0 text-secondary max-w-2xl">
            Your Minecraft setup, profiles, mods, and play sessions — in one place.
          </p>
        </div>
        <span class="status-pill"><span class="status-dot"></span> Launcher workspace</span>
      </div>
      <nav class="section-tabs" aria-label="CloudCraft sections">
        <button
          v-for="section in sections"
          :key="section.id"
          type="button"
          :class="{ selected: activeSection === section.id }"
          @click="activeSection = section.id"
        >
          {{ section.label }}
        </button>
      </nav>
    </header>

    <template v-if="activeSection === 'overview'">
      <section class="hero-panel">
        <div class="hero-copy">
          <p class="eyebrow m-0 mb-3">READY WHEN YOU ARE</p>
          <h2 class="m-0 text-2xl md:text-3xl font-bold">Your next world starts here.</h2>
          <p class="mt-3 mb-5 text-secondary max-w-xl">
            CloudCraft keeps the real launcher close at hand. Create an instance, browse modpacks,
            or keep a handy server address without pretending a remote game host is already
            connected.
          </p>
          <div class="flex flex-wrap gap-3">
            <RouterLink class="primary-action" to="/library"
              >Open instances <span aria-hidden="true">↗</span></RouterLink
            >
            <RouterLink class="secondary-action" to="/browse/modpack"
              >Explore modpacks <span aria-hidden="true">↗</span></RouterLink
            >
          </div>
        </div>
        <div class="hero-orbit" aria-hidden="true">
          <div class="orbit-ring ring-one"></div>
          <div class="orbit-ring ring-two"></div>
          <div class="orbit-core">CC</div>
          <span class="orbit-star star-one">✦</span><span class="orbit-star star-two">✧</span>
        </div>
      </section>

      <section class="grid grid-cols-1 md:grid-cols-2 xl:grid-cols-4 gap-4">
        <button class="feature-card text-left" type="button" @click="activeSection = 'instances'">
          <span class="feature-icon"><PlayIcon /></span
          ><span class="feature-title">Play your instances</span>
          <span class="feature-copy"
            >{{ instances.length }} profile{{ instances.length === 1 ? '' : 's' }} found in your
            existing launcher library.</span
          >
          <span class="feature-link">Open profiles <span aria-hidden="true">→</span></span>
        </button>
        <button class="feature-card text-left" type="button" @click="activeSection = 'mods'">
          <span class="feature-icon"><CompassIcon /></span
          ><span class="feature-title">Mods & modpacks</span>
          <span class="feature-copy"
            >Browse real Modrinth content and add it through the existing launcher flow.</span
          >
          <span class="feature-link">Explore content <span aria-hidden="true">→</span></span>
        </button>
        <button class="feature-card text-left" type="button" @click="activeSection = 'servers'">
          <span class="feature-icon"><GlobeIcon /></span
          ><span class="feature-title">Server shortcuts</span>
          <span class="feature-copy">Save an address for a server you already run or use.</span>
          <span class="feature-link">Manage addresses <span aria-hidden="true">→</span></span>
        </button>
        <button class="feature-card text-left" type="button" @click="activeSection = 'performance'">
          <span class="feature-icon"><SettingsIcon /></span
          ><span class="feature-title">Performance</span>
          <span class="feature-copy"
            >Adjust the launcher's actual Minecraft memory allocation.</span
          >
          <span class="feature-link">Tune memory <span aria-hidden="true">→</span></span>
        </button>
        <button class="feature-card text-left" type="button" @click="activeSection = 'cloud'">
          <span class="feature-icon">☁</span>
          <span class="feature-title">Cloud connections</span>
          <span class="feature-copy">Manage Kaggle compute and TeraBox storage connection setup.</span>
          <span class="feature-link">Open cloud <span aria-hidden="true">→</span></span>
        </button>
        <button class="feature-card text-left" type="button" @click="activeSection = 'saves'">
          <span class="feature-icon"><FolderOpenIcon /></span
          ><span class="feature-title">Save workspace</span>
          <span class="feature-copy">Keep backup reminders and export your reminder list.</span>
          <span class="feature-link">Open save tools <span aria-hidden="true">→</span></span>
        </button>
        <RouterLink class="feature-card" to="/library">
          <span class="feature-icon"><LibraryIcon /></span
          ><span class="feature-title">Your instances</span>
          <span class="feature-copy"
            >Use the existing launcher library and its real installed profiles.</span
          >
          <span class="feature-link">Open library <span aria-hidden="true">→</span></span>
        </RouterLink>
      </section>

      <section class="notice-panel">
        <div class="notice-symbol">i</div>
        <div>
          <h3 class="m-0 text-base font-semibold">Transparent by design</h3>
          <p class="m-0 mt-1 text-sm text-secondary">
            Remote cloud gaming and hosted multiplayer servers need a real provider integration.
            Neither is presented as active until one is configured.
          </p>
        </div>
      </section>
      <section class="content-panel">
        <div class="panel-heading">
          <div>
            <p class="eyebrow m-0 mb-2">REMOTE CLIENT</p>
            <h3 class="m-0 text-lg font-semibold">Cloud gaming</h3>
          </div>
          <span class="status-pill muted">
            {{
              cloudSessionStatus.state === 'unavailable'
                ? 'Not configured'
                : cloudSessionStatus.state
            }}
          </span>
        </div>
        <p class="mt-3 text-secondary">{{ cloudSessionStatus.message }}</p>
        <p class="m-0 text-sm text-secondary">
          The provider boundary defines unavailable, starting, ready, stopping, stopped, and error
          states. No remote machine is started and no provider credentials are configured.
        </p>
        <div class="cloud-controls">
          <label for="cloud-instance">Instance for a future remote session</label>
          <select id="cloud-instance" v-model="cloudInstancePath" :disabled="cloudSessionBusy">
            <option value="">Select an installed instance</option>
            <option
              v-for="instance in instances.filter((item) => item.install_stage === 'installed')"
              :key="instance.path"
              :value="instance.path"
            >
              {{ instance.name || instance.path }} ·
              {{ instance.game_version || 'version unknown' }}
            </option>
          </select>
          <div class="flex flex-wrap gap-2">
            <button
              v-if="cloudSessionStatus.canStart"
              class="primary-action"
              type="button"
              :disabled="cloudSessionBusy || !cloudInstancePath"
              @click="startCloudSession"
            >
              {{ cloudSessionBusy ? 'Working…' : 'Start cloud session' }}
            </button>
            <button
              v-if="cloudSessionStatus.canStop && cloudSessionStatus.sessionId"
              class="secondary-action"
              type="button"
              :disabled="cloudSessionBusy"
              @click="stopCloudSession"
            >
              Stop cloud session
            </button>
            <button
              v-if="cloudSessionStatus.state === 'starting'"
              class="secondary-action"
              type="button"
              :disabled="cloudSessionBusy"
              @click="cancelCloudSessionStart"
            >
              Cancel startup
            </button>
            <button
              class="secondary-action"
              type="button"
              :disabled="cloudSessionBusy"
              @click="loadCloudSessionStatus"
            >
              Refresh status
            </button>
          </div>
          <p v-if="cloudSessionError" class="inline-error" role="alert">{{ cloudSessionError }}</p>
        </div>
      </section>
    </template>

    <template v-else-if="activeSection === 'cloud'">
      <section class="content-panel">
        <div class="panel-heading">
          <div>
            <p class="eyebrow m-0 mb-2">CLOUD CONNECTIONS</p>
            <h2 class="m-0 text-2xl font-bold">Your cloud, one place</h2>
          </div>
          <span class="status-pill muted">2 providers · setup required</span>
        </div>
        <p class="mt-3 text-secondary">
          CloudCraft separates remote compute from file storage. Kaggle may provide notebook compute;
          TeraBox is a storage service. Neither connection is active until a supported authentication
          flow is configured and verified.
        </p>
        <div class="grid grid-cols-1 xl:grid-cols-2 gap-4 mt-5">
          <article class="provider-card">
            <div class="provider-card-heading">
              <span class="provider-logo kaggle-logo" aria-hidden="true">K</span>
              <div class="min-w-0 flex-1">
                <h3 class="m-0 text-lg font-semibold">Kaggle Compute</h3>
                <p class="m-0 mt-1 text-sm text-secondary">Potential remote runtime</p>
              </div>
              <span class="connection-state">
                <span class="connection-dot disconnected"></span>Not connected
              </span>
            </div>
            <p class="mt-4 text-sm text-secondary">
              Official Kaggle APIs support notebook-related operations and OAuth 2.0 for registered
              third-party apps. That does not by itself provide a supported interactive game-streaming
              service. CloudCraft has no registered OAuth client or verified remote game runtime yet.
            </p>
            <ul class="provider-facts">
              <li>
                Authentication: requires a registered OAuth client or a securely handled API token.
              </li>
              <li>Compute: notebook runtime availability and limits are controlled by Kaggle.</li>
              <li>Game streaming: not available in this build.</li>
            </ul>
            <a
              class="secondary-action"
              href="https://www.kaggle.com/docs/api"
              target="_blank"
              rel="noopener noreferrer"
            >
              Open Kaggle API documentation ↗
            </a>
            <a
              class="provider-text-link"
              href="https://www.kaggle.com/account"
              target="_blank"
              rel="noopener noreferrer"
            >
              Open Kaggle account settings ↗
            </a>
          </article>
          <article class="provider-card">
            <div class="provider-card-heading">
              <span class="provider-logo terabox-logo" aria-hidden="true">T</span>
              <div class="min-w-0 flex-1">
                <h3 class="m-0 text-lg font-semibold">TeraBox Storage</h3>
                <p class="m-0 mt-1 text-sm text-secondary">Potential world-backup storage</p>
              </div>
              <span class="connection-state">
                <span class="connection-dot disconnected"></span>Not connected
              </span>
            </div>
            <p class="mt-4 text-sm text-secondary">
              CloudCraft can only connect through a supported TeraBox integration. The official
              Open Platform requires an application to be registered and authorized; this project
              does not currently have the required app credentials or a verified upload/download client.
            </p>
            <ul class="provider-facts">
              <li>Authentication: requires an authorized TeraBox Open Platform app.</li>
              <li>Storage: account quota and file access cannot be verified until connected.</li>
              <li>World sync: not available in this build; local backups remain on this device.</li>
            </ul>
            <a
              class="secondary-action"
              href="https://www.terabox.com/integrations/docs?lang=en"
              target="_blank"
              rel="noopener noreferrer"
            >
              Open TeraBox integration docs ↗
            </a>
            <a
              class="provider-text-link"
              href="https://teraabox.com/developers"
              target="_blank"
              rel="noopener noreferrer"
            >
              Open TeraBox developer portal ↗
            </a>
          </article>
        </div>
      </section>
      <section class="notice-panel">
        <div class="notice-symbol">i</div>
        <div>
          <h3 class="m-0 text-base font-semibold">Why there is no “Connect” switch yet</h3>
          <p class="m-0 mt-1 text-sm text-secondary">
            A real login needs registered provider app credentials, a secure desktop OAuth callback,
            protected token storage, and API verification. A green status would be misleading before
            those steps work. CloudCraft never asks you to paste your provider password into this page.
          </p>
        </div>
      </section>
      <section class="content-panel">
        <h3 class="m-0 text-lg font-semibold">What must be true before cloud play works</h3>
        <div class="definition-grid">
          <div>
            <span class="definition-tag">COMPUTE</span>
            <p class="text-sm text-secondary">
              A supported remote runtime must start the graphical Minecraft client, expose a secure
              low-latency video/input stream, and stop reliably when you exit.
            </p>
          </div>
          <div>
            <span class="definition-tag">STORAGE</span>
            <p class="text-sm text-secondary">
              A verified file API must upload and download complete world folders with integrity
              checks, cancellation, conflict handling, and recovery.
            </p>
          </div>
        </div>
      </section>
    </template>

    <template v-else-if="activeSection === 'instances'">
      <section class="content-panel">
        <div class="panel-heading">
          <div>
            <p class="eyebrow m-0 mb-2">YOUR LIBRARY</p>
            <h2 class="m-0 text-2xl font-bold">Minecraft instances</h2>
          </div>
          <div class="flex flex-wrap gap-2">
            <button
              class="secondary-action"
              type="button"
              :disabled="instancesLoading"
              @click="loadInstances"
            >
              Refresh
            </button>
            <RouterLink class="primary-action" to="/library">Manage library ↗</RouterLink>
          </div>
        </div>
        <p class="mt-3 text-secondary">
          These profiles come from the existing launcher backend. Launching uses the normal launcher
          process and account flow.
        </p>
        <p v-if="instanceError" class="inline-error">{{ instanceError }}</p>
        <p v-if="launchError" class="inline-error">{{ launchError }}</p>
        <div v-if="instancesLoading" class="empty-state">
          <span class="text-secondary">Loading your real profiles…</span>
        </div>
        <div v-else-if="instances.length" class="instance-grid">
          <article v-for="instance in instances" :key="instance.path" class="instance-card">
            <div class="instance-icon">{{ (instance.name || 'M').slice(0, 1).toUpperCase() }}</div>
            <div class="min-w-0 flex-1">
              <h3 class="m-0 font-semibold truncate">{{ instance.name || instance.path }}</h3>
              <p class="m-0 mt-1 text-sm text-secondary">
                {{ instance.loader || 'Vanilla' }} · Minecraft
                {{ instance.game_version || 'version unknown' }}
              </p>
              <p class="m-0 mt-1 text-xs text-secondary">
                {{
                  instance.install_stage === 'installed'
                    ? 'Ready to play'
                    : 'Needs installation or repair'
                }}
              </p>
            </div>
            <div class="flex flex-wrap gap-2">
              <button
                class="primary-action"
                type="button"
                :disabled="instance.install_stage !== 'installed' || !!launchingPath"
                @click="launchInstance(instance)"
              >
                {{ launchingPath === instance.path ? 'Launching…' : 'Play' }}
              </button>
              <button class="secondary-action" type="button" @click="openInstance(instance)">
                Details
              </button>
            </div>
          </article>
        </div>
        <div v-else class="empty-state">
          <span class="feature-icon"><LibraryIcon /></span><strong>No instances found</strong
          ><span class="text-sm text-secondary"
            >Create one with the launcher's existing instance wizard.</span
          ><RouterLink class="primary-action" to="/library">Open library</RouterLink>
        </div>
      </section>
    </template>

    <template v-else-if="activeSection === 'mods'">
      <section class="content-panel">
        <p class="eyebrow m-0 mb-2">MODRINTH DISCOVERY</p>
        <h2 class="m-0 text-2xl font-bold">Make Minecraft yours</h2>
        <p class="mt-3 text-secondary">
          Use the launcher's existing content browser for actual project search, version
          compatibility, installation, and profile targeting.
        </p>
        <div class="grid grid-cols-1 md:grid-cols-2 gap-4 mt-5">
          <RouterLink class="feature-card" to="/browse/mod"
            ><span class="feature-icon"><CompassIcon /></span
            ><span class="feature-title">Browse mods</span
            ><span class="feature-copy"
              >Find supported mods and install them to an instance using the existing flow.</span
            ><span class="feature-link">Browse mods ↗</span></RouterLink
          >
          <RouterLink class="feature-card" to="/browse/modpack"
            ><span class="feature-icon"><LibraryIcon /></span
            ><span class="feature-title">Explore modpacks</span
            ><span class="feature-copy"
              >Discover complete modpacks from the real content catalogue.</span
            ><span class="feature-link">Browse modpacks ↗</span></RouterLink
          >
          <RouterLink class="feature-card" to="/browse/resourcepack"
            ><span class="feature-icon"><DownloadIcon /></span
            ><span class="feature-title">Resource packs</span
            ><span class="feature-copy"
              >Find visual customizations through the existing browser.</span
            ><span class="feature-link">Browse packs ↗</span></RouterLink
          >
          <RouterLink class="feature-card" to="/browse/shader"
            ><span class="feature-icon"><CompassIcon /></span
            ><span class="feature-title">Shaders</span
            ><span class="feature-copy">Discover shaders in the supported content catalogue.</span
            ><span class="feature-link">Browse shaders ↗</span></RouterLink
          >
        </div>
      </section>
    </template>

    <template v-else-if="activeSection === 'performance'">
      <section class="content-panel">
        <div class="panel-heading">
          <div>
            <p class="eyebrow m-0 mb-2">GAME TUNING</p>
            <h2 class="m-0 text-2xl font-bold">Memory allocation</h2>
          </div>
          <span class="status-pill muted">Live launcher setting</span>
        </div>
        <p class="mt-3 text-secondary">
          These values are read from and saved to the launcher's global settings. Values are in MB.
          More RAM is not always faster; leave room for Windows and other apps.
        </p>
        <p class="mt-2 text-sm text-secondary">
          Java installations, default instance options, appearance, and privacy settings remain
          available in the existing Settings dialog from the gear icon in the sidebar.
        </p>
        <div v-if="settingsLoading" class="empty-state">
          <span class="text-secondary">Loading launcher settings…</span>
        </div>
        <form v-else class="memory-form" @submit.prevent="saveMemorySettings">
          <label for="memory-min"
            >Minimum memory (MB) <span class="text-secondary">— optional</span></label
          >
          <input
            id="memory-min"
            v-model="memoryMin"
            type="number"
            min="256"
            step="256"
            placeholder="Leave blank for launcher default"
          />
          <label for="memory-max">Maximum memory (MB)</label>
          <input id="memory-max" v-model="memoryMax" type="number" min="512" step="256" required />
          <div class="memory-presets">
            <button class="secondary-action" type="button" @click="memoryMax = '4096'">4 GB</button>
            <button class="secondary-action" type="button" @click="memoryMax = '6144'">6 GB</button>
            <button class="secondary-action" type="button" @click="memoryMax = '8192'">8 GB</button>
          </div>
          <button
            class="primary-action self-start"
            type="submit"
            :disabled="settingsLoading || !launcherSettings"
          >
            Save memory settings
          </button>
        </form>
        <p v-if="settingsMessage" class="success-message">{{ settingsMessage }}</p>
        <p v-if="settingsError" class="inline-error">{{ settingsError }}</p>
      </section>
      <section class="notice-panel">
        <div class="notice-symbol">i</div>
        <div>
          <h3 class="m-0 text-base font-semibold">Performance without fake promises</h3>
          <p class="m-0 mt-1 text-sm text-secondary">
            CloudCraft does not claim a guaranteed FPS multiplier or silently install optimization
            mods. Browse compatible mods and manage them through the existing Modrinth flow.
          </p>
          <RouterLink class="feature-link inline-block mt-3" to="/browse/mod"
            >Explore performance mods ↗</RouterLink
          >
        </div>
      </section>
    </template>

    <template v-else-if="activeSection === 'servers'">
      <section class="content-panel">
        <div class="panel-heading">
          <div>
            <p class="eyebrow m-0 mb-2">MULTIPLAYER</p>
            <h2 class="m-0 text-2xl font-bold">Server shortcuts</h2>
          </div>
          <span class="status-pill muted">No hosting provider connected</span>
        </div>
        <p class="text-secondary mt-3">
          Keep a server address handy. This does not create or host a Minecraft server; it stores
          the address locally in this app's web storage.
        </p>
        <form class="server-form" @submit.prevent="rememberServer">
          <label for="server-address">Server address</label>
          <div class="flex flex-col sm:flex-row gap-2">
            <input
              id="server-address"
              v-model="serverAddress"
              autocomplete="off"
              placeholder="play.example.net or host:25565"
            />
            <button class="primary-action" type="submit" :disabled="!hasServerAddress">
              Save address
            </button>
          </div>
        </form>
        <div v-if="savedAddresses.length" class="reminder-list">
          <div v-for="address in savedAddresses" :key="address" class="reminder-row">
            <button class="text-left flex-1 min-w-0" type="button" @click="serverAddress = address">
              <strong class="break-all">{{ address }}</strong>
              <span class="block text-xs text-secondary mt-1">Saved on this device</span>
            </button>
            <button
              class="text-sm text-secondary hover:text-contrast"
              type="button"
              @click="removeServerAddress(address)"
            >
              Remove
            </button>
          </div>
        </div>
        <div v-if="hasServerAddress" class="saved-address">
          <div>
            <span class="small-label">CURRENT ADDRESS</span>
            <p class="m-0 mt-1 font-semibold break-all">{{ serverAddress.trim() }}</p>
          </div>
          <button class="secondary-action" type="button" @click="copyAddress">
            {{ copied ? 'Copied!' : 'Copy address' }}
          </button>
        </div>
        <div class="provider-note">
          <span class="provider-indicator"></span>
          <div>
            <strong>Hosting: not connected</strong>
            <p class="m-0 mt-1 text-sm text-secondary">
              No server has been provisioned, and no provider credentials or status are configured.
            </p>
          </div>
        </div>
      </section>
      <section class="content-panel">
        <h3 class="m-0 text-lg font-semibold">Server hosting vs. cloud gaming</h3>
        <div class="definition-grid">
          <div>
            <span class="definition-tag">MULTIPLAYER HOSTING</span>
            <p class="text-sm text-secondary">
              Runs a server world so multiple players can join. Requires a server host or your own
              machine.
            </p>
          </div>
          <div>
            <span class="definition-tag">CLOUD GAMING</span>
            <p class="text-sm text-secondary">
              Runs the graphical Minecraft client on a remote machine and streams video/input.
              Requires a separate cloud PC provider.
            </p>
          </div>
        </div>
      </section>
    </template>

    <template v-else>
      <section class="content-panel">
        <div class="panel-heading">
          <div>
            <p class="eyebrow m-0 mb-2">WORLD BACKUP WORKSPACE</p>
            <h2 class="m-0 text-2xl font-bold">Saves & backups</h2>
          </div>
          <span class="status-pill muted">Local backups · cloud sync off</span>
        </div>
        <p class="text-secondary mt-3">
          Back up a world by copying its selected folder to a destination you choose, or restore a
          CloudCraft backup into a new folder. Close Minecraft first so the world is not changing
          during the copy. Large worlds may take a while. The launcher will not overwrite an
          existing world folder.
        </p>
        <div class="backup-actions">
          <button
            class="primary-action"
            type="button"
            :disabled="backupBusy"
            @click="createWorldBackup"
          >
            {{ backupBusy ? 'Working…' : 'Back up a world folder' }}
          </button>
          <button
            class="secondary-action"
            type="button"
            :disabled="backupBusy"
            @click="restoreWorldBackup"
          >
            {{ backupBusy ? 'Working…' : 'Restore a backup' }}
          </button>
        </div>
        <p v-if="backupMessage" class="success-message break-all" role="status">
          {{ backupMessage }}
        </p>
        <p v-if="backupError" class="inline-error" role="alert">{{ backupError }}</p>
        <p class="mt-3 text-sm text-secondary">
          Safety: source and destination must be separate folders. Symbolic links are rejected, and
          restore stops rather than overwriting an existing directory. Keep your own additional
          copies of important worlds. No files are uploaded anywhere.
        </p>
        <div class="provider-note">
          <span class="provider-indicator"></span>
          <div>
            <strong>
              Cloud storage:
              {{
                cloudStorageStatus.state === 'unconfigured'
                  ? 'not configured'
                  : cloudStorageStatus.state
              }}
            </strong>
            <p class="m-0 mt-1 text-sm text-secondary">{{ cloudStorageStatus.message }}</p>
          </div>
        </div>
        <div class="divider-line"></div>
        <div class="panel-heading">
          <h3 class="m-0 text-lg font-semibold">Local backup history</h3>
          <span class="status-pill muted">{{ worldBackupHistory.length }} saved</span>
        </div>
        <p class="mt-2 text-sm text-secondary">
          History is stored on this device and can become stale if you move or delete a backup
          folder.
        </p>
        <div v-if="worldBackupHistory.length" class="reminder-list">
          <div
            v-for="entry in worldBackupHistory"
            :key="entry.createdAt + entry.path"
            class="reminder-row"
          >
            <div class="min-w-0 flex-1">
              <strong>{{ entry.name }}</strong>
              <p class="m-0 mt-1 text-xs text-secondary">
                {{ formatBackupTimestamp(entry.createdAt) }}
              </p>
              <p class="m-0 mt-1 text-xs text-secondary break-all">{{ entry.path }}</p>
            </div>
            <button
              class="secondary-action"
              type="button"
              :disabled="backupBusy"
              @click="restoreWorldBackup(entry)"
            >
              Restore
            </button>
          </div>
        </div>
        <div v-else class="empty-state">
          <strong>No backups recorded</strong>
          <span class="text-sm text-secondary"
            >Backups created here will appear with their timestamp and local path.</span
          >
        </div>
        <div class="divider-line"></div>
        <h3 class="m-0 text-lg font-semibold">Save reminders</h3>
        <p class="text-secondary mt-2">
          Cloud storage and cross-device sync are not configured. These optional reminders are
          stored locally and can be exported as JSON; the reminder file is not a world backup.
        </p>
        <form class="save-form" @submit.prevent="addSaveNote">
          <label for="save-name">World or backup label</label
          ><input
            id="save-name"
            v-model="saveName"
            placeholder="e.g. Survival world — before update"
            required
          />
          <label for="save-notes">Notes <span class="text-secondary">(optional)</span></label
          ><textarea
            id="save-notes"
            v-model="saveNotes"
            rows="3"
            placeholder="Version, backup location, or what to remember…"
          ></textarea>
          <div class="flex flex-wrap gap-2">
            <button class="primary-action" type="submit">Add reminder</button
            ><button
              class="secondary-action"
              type="button"
              :disabled="backupHistory.length === 0"
              @click="exportSaveManifest"
            >
              Export reminder list
            </button>
          </div>
        </form>
        <div v-if="backupHistory.length" class="reminder-list">
          <div
            v-for="(entry, index) in backupHistory"
            :key="entry.createdAt + entry.name"
            class="reminder-row"
          >
            <div>
              <strong>{{ entry.name }}</strong>
              <p v-if="entry.notes" class="m-0 mt-1 text-sm text-secondary">{{ entry.notes }}</p>
            </div>
            <button
              class="text-sm text-secondary hover:text-contrast"
              type="button"
              @click="backupHistory.splice(index, 1)"
            >
              Remove
            </button>
          </div>
        </div>
        <div v-else class="empty-state">
          <span class="feature-icon"><FolderOpenIcon /></span><strong>No reminders yet</strong
          ><span class="text-sm text-secondary"
            >Add a reminder to keep track of a manual backup.</span
          >
        </div>
      </section>
      <section class="notice-panel">
        <div class="notice-symbol">!</div>
        <div>
          <h3 class="m-0 text-base font-semibold">Local means local</h3>
          <p class="m-0 mt-1 text-sm text-secondary">
            World backup and restore operate on folders you select on this device. Cloud sync is not
            configured; only reminder metadata is exported as JSON.
          </p>
        </div>
      </section>
    </template>
  </main>
</template>

<style scoped>
.cloudcraft-page {
  min-height: 100%;
  color: var(--color-base);
}
.eyebrow {
  color: #b59cff;
  font-size: 0.7rem;
  font-weight: 800;
  letter-spacing: 0.18em;
}
.status-pill {
  display: inline-flex;
  align-items: center;
  gap: 0.5rem;
  border: 1px solid rgba(166, 139, 255, 0.28);
  border-radius: 999px;
  padding: 0.5rem 0.8rem;
  font-size: 0.75rem;
  color: #d6c9ff;
  background: rgba(128, 91, 230, 0.1);
  white-space: nowrap;
}
.status-pill.muted {
  color: var(--color-secondary);
  border-color: var(--color-button-bg);
  background: transparent;
}
.status-dot,
.provider-indicator {
  width: 0.45rem;
  height: 0.45rem;
  border-radius: 50%;
  background: #a78bfa;
  display: inline-block;
}
.section-tabs {
  display: flex;
  flex-wrap: wrap;
  gap: 0.4rem;
  border-bottom: 1px solid var(--color-button-bg);
}
.section-tabs button {
  border: 0;
  background: transparent;
  color: var(--color-secondary);
  padding: 0.8rem 1rem;
  border-bottom: 2px solid transparent;
  cursor: pointer;
}
.section-tabs button.selected {
  color: var(--color-contrast);
  border-bottom-color: #a78bfa;
}
.hero-panel {
  min-height: 265px;
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 2rem;
  padding: clamp(1.5rem, 4vw, 3rem);
  border: 1px solid rgba(167, 139, 250, 0.25);
  border-radius: 1.5rem;
  background: radial-gradient(ellipse at 85% 15%, rgba(125, 83, 220, 0.22), transparent 38%),
    linear-gradient(125deg, var(--color-bg-raised), rgba(80, 52, 135, 0.15));
  overflow: hidden;
}
.hero-copy {
  position: relative;
  z-index: 1;
}
.hero-orbit {
  position: relative;
  flex: 0 0 190px;
  height: 190px;
  display: grid;
  place-items: center;
}
.orbit-ring {
  position: absolute;
  border: 1px solid rgba(183, 156, 255, 0.4);
  border-radius: 50%;
  transform: rotate(-28deg);
}
.ring-one {
  width: 155px;
  height: 155px;
}
.ring-two {
  width: 112px;
  height: 178px;
  transform: rotate(45deg);
}
.orbit-core {
  display: grid;
  place-items: center;
  width: 78px;
  height: 78px;
  border-radius: 1.5rem;
  color: white;
  font-weight: 900;
  letter-spacing: -0.08em;
  background: linear-gradient(135deg, #a78bfa, #6d28d9);
  box-shadow: 0 12px 50px rgba(109, 40, 217, 0.35);
}
.orbit-star {
  position: absolute;
  color: #c4b5fd;
}
.star-one {
  top: 8px;
  right: 30px;
  font-size: 1.6rem;
}
.star-two {
  bottom: 16px;
  left: 12px;
  font-size: 1.4rem;
}
.provider-card {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: 0.75rem;
  min-width: 0;
  padding: 1.25rem;
  border: 1px solid var(--color-button-bg);
  border-radius: 1rem;
  background: var(--color-bg-raised);
}
.provider-card-heading {
  display: flex;
  align-items: center;
  gap: 0.8rem;
  width: 100%;
}
.provider-logo {
  display: grid;
  place-items: center;
  width: 2.75rem;
  height: 2.75rem;
  flex: 0 0 2.75rem;
  border-radius: 0.85rem;
  color: white;
  font-weight: 900;
  font-size: 1.25rem;
}
.kaggle-logo {
  background: linear-gradient(135deg, #20beff, #0876c9);
}
.terabox-logo {
  background: linear-gradient(135deg, #56a7ff, #2255ce);
}
.connection-state {
  display: inline-flex;
  align-items: center;
  gap: 0.4rem;
  flex: 0 0 auto;
  color: var(--color-secondary);
  font-size: 0.75rem;
  white-space: nowrap;
}
.connection-dot {
  width: 0.45rem;
  height: 0.45rem;
  border-radius: 50%;
  display: inline-block;
}
.connection-dot.disconnected {
  background: #f59e0b;
}
.provider-facts {
  margin: 0;
  padding-left: 1.15rem;
  color: var(--color-secondary);
  font-size: 0.8rem;
  line-height: 1.6;
}
.provider-text-link {
  color: #b59cff;
  font-size: 0.85rem;
  text-decoration: none;
}
.provider-text-link:hover {
  text-decoration: underline;
}
.primary-action,
.secondary-action {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  gap: 0.5rem;
  padding: 0.7rem 1rem;
  border-radius: 0.8rem;
  text-decoration: none;
  font-size: 0.875rem;
  font-weight: 700;
  cursor: pointer;
  transition:
    transform 0.15s,
    filter 0.15s;
}
.primary-action {
  border: 1px solid transparent;
  background: linear-gradient(120deg, #a78bfa, #7c3aed);
  color: white;
}
.secondary-action {
  border: 1px solid var(--color-button-bg);
  background: var(--color-bg);
  color: var(--color-contrast);
}
.primary-action:hover,
.secondary-action:hover {
  filter: brightness(1.08);
  transform: translateY(-1px);
}
.primary-action:disabled,
.secondary-action:disabled {
  opacity: 0.45;
  cursor: not-allowed;
  transform: none;
}
.feature-card {
  display: flex;
  flex-direction: column;
  gap: 0.65rem;
  min-height: 190px;
  padding: 1.25rem;
  border: 1px solid var(--color-button-bg);
  border-radius: 1.1rem;
  background: var(--color-bg-raised);
  text-decoration: none;
  color: inherit;
  cursor: pointer;
  transition:
    border-color 0.15s,
    transform 0.15s;
}
.feature-card:hover {
  border-color: rgba(167, 139, 250, 0.6);
  transform: translateY(-2px);
}
.feature-icon {
  display: grid;
  place-items: center;
  width: 2.4rem;
  height: 2.4rem;
  border-radius: 0.8rem;
  background: rgba(167, 139, 250, 0.13);
  color: #c4b5fd;
}
.feature-icon :deep(svg) {
  width: 1.15rem;
  height: 1.15rem;
}
.feature-title {
  font-weight: 750;
}
.feature-copy {
  color: var(--color-secondary);
  font-size: 0.85rem;
  line-height: 1.5;
}
.feature-link {
  color: #b59cff;
  font-size: 0.8rem;
  font-weight: 700;
  margin-top: auto;
}
.notice-panel,
.provider-note {
  display: flex;
  align-items: flex-start;
  gap: 0.9rem;
  border: 1px solid var(--color-button-bg);
  border-radius: 1rem;
  padding: 1rem 1.2rem;
  background: var(--color-bg-raised);
}
.notice-symbol {
  display: grid;
  place-items: center;
  flex: 0 0 1.6rem;
  height: 1.6rem;
  border-radius: 50%;
  background: rgba(167, 139, 250, 0.15);
  color: #c4b5fd;
  font-weight: 800;
}
.instance-grid {
  display: grid;
  grid-template-columns: repeat(1, minmax(0, 1fr));
  gap: 0.8rem;
  margin-top: 1.5rem;
}
.instance-card {
  display: flex;
  align-items: center;
  gap: 1rem;
  flex-wrap: wrap;
  padding: 1rem;
  border: 1px solid var(--color-button-bg);
  border-radius: 1rem;
  background: var(--color-bg);
}
.instance-icon {
  display: grid;
  place-items: center;
  flex: 0 0 2.8rem;
  width: 2.8rem;
  height: 2.8rem;
  border-radius: 0.85rem;
  color: white;
  background: linear-gradient(135deg, #a78bfa, #6d28d9);
  font-size: 1.2rem;
  font-weight: 800;
}
.memory-form {
  display: flex;
  flex-direction: column;
  gap: 0.7rem;
  max-width: 560px;
  margin-top: 1.5rem;
}
.memory-form label {
  font-size: 0.85rem;
  font-weight: 700;
}
.cloud-controls {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: 0.75rem;
  margin-top: 1.25rem;
}
.cloud-controls label {
  font-size: 0.85rem;
  font-weight: 700;
}
.cloud-controls select {
  width: 100%;
  max-width: 560px;
  box-sizing: border-box;
  border: 1px solid var(--color-button-bg);
  border-radius: 0.7rem;
  padding: 0.8rem 0.9rem;
  background: var(--color-bg);
  color: var(--color-contrast);
}
.memory-form input {
  width: 100%;
  box-sizing: border-box;
  border: 1px solid var(--color-button-bg);
  border-radius: 0.7rem;
  padding: 0.8rem 0.9rem;
  background: var(--color-bg);
  color: var(--color-contrast);
}
.memory-presets {
  display: flex;
  flex-wrap: wrap;
  gap: 0.5rem;
  margin: 0.25rem 0 0.5rem;
}
.inline-error {
  color: #fca5a5;
  background: rgba(185, 28, 28, 0.1);
  border: 1px solid rgba(248, 113, 113, 0.3);
  border-radius: 0.75rem;
  padding: 0.8rem 1rem;
  overflow-wrap: anywhere;
}
.success-message {
  color: #86efac;
  margin-top: 1rem;
}
.content-panel {
  padding: clamp(1.2rem, 3vw, 2rem);
  border: 1px solid var(--color-button-bg);
  border-radius: 1.25rem;
  background: var(--color-bg-raised);
}
.panel-heading {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 1rem;
  flex-wrap: wrap;
}
.backup-actions {
  display: flex;
  flex-wrap: wrap;
  gap: 0.65rem;
  margin-top: 1.25rem;
}
.divider-line {
  height: 1px;
  margin: 1.75rem 0;
  background: var(--color-button-bg);
}
.server-form,
.save-form {
  display: flex;
  flex-direction: column;
  gap: 0.65rem;
  max-width: 760px;
  margin-top: 1.5rem;
}
.server-form label,
.save-form label {
  font-size: 0.85rem;
  font-weight: 700;
}
.server-form input,
.save-form input,
.save-form textarea {
  width: 100%;
  box-sizing: border-box;
  border: 1px solid var(--color-button-bg);
  border-radius: 0.7rem;
  padding: 0.8rem 0.9rem;
  background: var(--color-bg);
  color: var(--color-contrast);
  outline: none;
}
.server-form input:focus,
.save-form input:focus,
.save-form textarea:focus {
  border-color: #a78bfa;
}
.saved-address {
  margin-top: 1.5rem;
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 1rem;
  flex-wrap: wrap;
  padding: 1rem;
  border-radius: 0.9rem;
  background: var(--color-bg);
  border: 1px solid var(--color-button-bg);
}
.small-label,
.definition-tag {
  color: #b59cff;
  font-size: 0.65rem;
  font-weight: 800;
  letter-spacing: 0.12em;
}
.provider-note {
  margin-top: 1.5rem;
}
.provider-indicator {
  background: #8b849a;
  margin-top: 0.35rem;
  flex-shrink: 0;
}
.definition-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 1rem;
  margin-top: 1.2rem;
}
.definition-grid > div {
  padding: 1rem;
  border-radius: 0.9rem;
  background: var(--color-bg);
}
.reminder-list {
  margin-top: 1.5rem;
  border-top: 1px solid var(--color-button-bg);
}
.reminder-row {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 1rem;
  padding: 1rem 0;
  border-bottom: 1px solid var(--color-button-bg);
}
.reminder-row button {
  border: 0;
  background: transparent;
  cursor: pointer;
}
.empty-state {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 0.75rem;
  padding: 2.5rem 1rem;
  text-align: center;
}
@media (max-width: 620px) {
  .hero-orbit {
    display: none;
  }
  .definition-grid {
    grid-template-columns: 1fr;
  }
  .panel-heading {
    align-items: stretch;
  }
}
</style>
