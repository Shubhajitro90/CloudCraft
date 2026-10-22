<script setup>
import { computed, onMounted, ref, watch } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { open as openDialog } from '@tauri-apps/plugin-dialog'
import { RouterLink, useRouter } from 'vue-router'
import { useBreadcrumbs } from '@/store/breadcrumbs'
import { list, run } from '@/helpers/profile'
import { get as getSettings, set as setSettings } from '@/helpers/settings'

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

const sections = [
  { id: 'overview', label: 'Overview' },
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
  if (
    (min !== null && (!Number.isFinite(min) || min < 256)) ||
    !Number.isFinite(max) ||
    max < 512 ||
    (min !== null && min > max)
  ) {
    settingsError.value =
      'Enter valid memory values in MB. Minimum must be at least 256 MB and cannot exceed maximum.'
    return
  }
  settingsLoading.value = true
  settingsError.value = ''
  settingsMessage.value = ''
  try {
    const updated = {
      ...launcherSettings.value,
      memory: { ...launcherSettings.value.memory, min, max },
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

async function createWorldBackup() {
  backupBusy.value = true
  backupError.value = ''
  backupMessage.value = ''
  try {
    const sourcePath = await chooseDirectory('Choose the Minecraft world folder to back up')
    if (!sourcePath) return
    const backupRoot = await chooseDirectory('Choose a destination folder for the world backup')
    if (!backupRoot) return
    const destination = String(
      await invoke('cloudcraft_backup_world', { sourcePath, backupRoot }),
    )
    backupMessage.value = `World backup created: ${destination}`
  } catch (error) {
    backupError.value = String(error?.message || error)
  } finally {
    backupBusy.value = false
  }
}

async function restoreWorldBackup() {
  backupBusy.value = true
  backupError.value = ''
  backupMessage.value = ''
  try {
    const backupPath = await chooseDirectory('Choose a CloudCraft world backup folder')
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
          <span class="feature-icon">▶</span><span class="feature-title">Play your instances</span>
          <span class="feature-copy"
            >{{ instances.length }} profile{{ instances.length === 1 ? '' : 's' }} found in your
            existing launcher library.</span
          >
          <span class="feature-link">Open profiles <span aria-hidden="true">→</span></span>
        </button>
        <button class="feature-card text-left" type="button" @click="activeSection = 'mods'">
          <span class="feature-icon">✳</span><span class="feature-title">Mods & modpacks</span>
          <span class="feature-copy"
            >Browse real Modrinth content and add it through the existing launcher flow.</span
          >
          <span class="feature-link">Explore content <span aria-hidden="true">→</span></span>
        </button>
        <button class="feature-card text-left" type="button" @click="activeSection = 'servers'">
          <span class="feature-icon">⌁</span><span class="feature-title">Server shortcuts</span>
          <span class="feature-copy">Save an address for a server you already run or use.</span>
          <span class="feature-link">Manage addresses <span aria-hidden="true">→</span></span>
        </button>
        <button class="feature-card text-left" type="button" @click="activeSection = 'performance'">
          <span class="feature-icon">↗</span><span class="feature-title">Performance</span>
          <span class="feature-copy"
            >Adjust the launcher's actual Minecraft memory allocation.</span
          >
          <span class="feature-link">Tune memory <span aria-hidden="true">→</span></span>
        </button>
        <button class="feature-card text-left" type="button" @click="activeSection = 'saves'">
          <span class="feature-icon">◇</span><span class="feature-title">Save workspace</span>
          <span class="feature-copy">Keep backup reminders and export your reminder list.</span>
          <span class="feature-link">Open save tools <span aria-hidden="true">→</span></span>
        </button>
        <RouterLink class="feature-card" to="/library">
          <span class="feature-icon">▦</span><span class="feature-title">Your instances</span>
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
              @click="loadInstances"
              :disabled="instancesLoading"
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
          <span class="feature-icon">▦</span><strong>No instances found</strong
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
            ><span class="feature-icon">✳</span><span class="feature-title">Browse mods</span
            ><span class="feature-copy"
              >Find supported mods and install them to an instance using the existing flow.</span
            ><span class="feature-link">Browse mods ↗</span></RouterLink
          >
          <RouterLink class="feature-card" to="/browse/modpack"
            ><span class="feature-icon">▦</span><span class="feature-title">Explore modpacks</span
            ><span class="feature-copy"
              >Discover complete modpacks from the real content catalogue.</span
            ><span class="feature-link">Browse modpacks ↗</span></RouterLink
          >
          <RouterLink class="feature-card" to="/browse/resourcepack"
            ><span class="feature-icon">◈</span><span class="feature-title">Resource packs</span
            ><span class="feature-copy"
              >Find visual customizations through the existing browser.</span
            ><span class="feature-link">Browse packs ↗</span></RouterLink
          >
          <RouterLink class="feature-card" to="/browse/shader"
            ><span class="feature-icon">✧</span><span class="feature-title">Shaders</span
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
            <button class="secondary-action" type="button" @click="memoryMax = '4096'">4 GB</button
            ><button class="secondary-action" type="button" @click="memoryMax = '6144'">6 GB</button
            ><button class="secondary-action" type="button" @click="memoryMax = '8192'">
              8 GB
            </button>
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
              @click="
                savedAddresses = savedAddresses.filter((item) => item !== address)
                localStorage.setItem('cloudcraft.server-addresses', JSON.stringify(savedAddresses))
              "
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
          <span class="feature-icon">◇</span><strong>No reminders yet</strong
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
  font-size: 1.5rem;
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