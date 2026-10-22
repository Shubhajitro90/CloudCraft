<script setup>
import { computed, ref, watch } from 'vue'
import { useBreadcrumbs } from '@/store/breadcrumbs'

const breadcrumbs = useBreadcrumbs()
breadcrumbs.setRootContext({ name: 'CloudCraft', link: '/cloudcraft' })

const activeSection = ref('overview')
const serverAddress = ref('')
const copied = ref(false)
const saveName = ref('')
const saveNotes = ref('')
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
watch(backupHistory, (entries) => {
  try {
    localStorage.setItem('cloudcraft.save-reminders', JSON.stringify(entries))
  } catch (error) {
    console.error('Could not save reminder list locally', error)
  }
}, { deep: true })

const sections = [
  { id: 'overview', label: 'Overview' },
  { id: 'servers', label: 'Servers' },
  { id: 'saves', label: 'Cloud saves' },
]
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
            One place to manage your play sessions, server shortcuts, and save planning.
          </p>
        </div>
        <span class="status-pill"><span class="status-dot"></span> Launcher connected</span>
      </div>
      <nav class="section-tabs" aria-label="CloudCraft sections">
        <button v-for="section in sections" :key="section.id" type="button"
          :class="{ selected: activeSection === section.id }" @click="activeSection = section.id">
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
            or keep a handy server address without pretending a remote game host is already connected.
          </p>
          <div class="flex flex-wrap gap-3">
            <RouterLink class="primary-action" to="/library">Open instances <span aria-hidden="true">↗</span></RouterLink>
            <RouterLink class="secondary-action" to="/browse/modpack">Explore modpacks <span aria-hidden="true">↗</span></RouterLink>
          </div>
        </div>
        <div class="hero-orbit" aria-hidden="true"><div class="orbit-ring ring-one"></div><div class="orbit-ring ring-two"></div><div class="orbit-core">CC</div><span class="orbit-star star-one">✦</span><span class="orbit-star star-two">✧</span></div>
      </section>

      <section class="grid grid-cols-1 md:grid-cols-3 gap-4">
        <button class="feature-card text-left" type="button" @click="activeSection = 'servers'">
          <span class="feature-icon">⌁</span><span class="feature-title">Server shortcuts</span>
          <span class="feature-copy">Save an address for a server you already run or use.</span>
          <span class="feature-link">Manage addresses <span aria-hidden="true">→</span></span>
        </button>
        <button class="feature-card text-left" type="button" @click="activeSection = 'saves'">
          <span class="feature-icon">◇</span><span class="feature-title">Cloud saves</span>
          <span class="feature-copy">Organize save notes and export a small manifest.</span>
          <span class="feature-link">Open save tools <span aria-hidden="true">→</span></span>
        </button>
        <RouterLink class="feature-card" to="/library">
          <span class="feature-icon">▦</span><span class="feature-title">Your instances</span>
          <span class="feature-copy">Use the existing launcher library and its real installed profiles.</span>
          <span class="feature-link">Open library <span aria-hidden="true">→</span></span>
        </RouterLink>
      </section>

      <section class="notice-panel">
        <div class="notice-symbol">i</div>
        <div><h3 class="m-0 text-base font-semibold">Transparent by design</h3>
          <p class="m-0 mt-1 text-sm text-secondary">Remote cloud gaming and hosted multiplayer servers need a real provider integration. Neither is presented as active until one is configured.</p>
        </div>
      </section>
    </template>

    <template v-else-if="activeSection === 'servers'">
      <section class="content-panel">
        <div class="panel-heading"><div><p class="eyebrow m-0 mb-2">MULTIPLAYER</p><h2 class="m-0 text-2xl font-bold">Server shortcuts</h2></div><span class="status-pill muted">No hosting provider connected</span></div>
        <p class="text-secondary mt-3">Keep a server address handy. This does not create or host a Minecraft server; it stores the address locally in this app's web storage.</p>
        <form class="server-form" @submit.prevent="rememberServer">
          <label for="server-address">Server address</label>
          <div class="flex flex-col sm:flex-row gap-2">
            <input id="server-address" v-model="serverAddress" autocomplete="off" placeholder="play.example.net or host:25565" />
            <button class="primary-action" type="submit" :disabled="!hasServerAddress">Save address</button>
          </div>
        </form>
        <div v-if="savedAddresses.length" class="reminder-list">
          <div v-for="address in savedAddresses" :key="address" class="reminder-row">
            <button class="text-left flex-1 min-w-0" type="button" @click="serverAddress = address">
              <strong class="break-all">{{ address }}</strong>
              <span class="block text-xs text-secondary mt-1">Saved on this device</span>
            </button>
            <button class="text-sm text-secondary hover:text-contrast" type="button" @click="savedAddresses = savedAddresses.filter(item => item !== address); localStorage.setItem('cloudcraft.server-addresses', JSON.stringify(savedAddresses))">Remove</button>
          </div>
        </div>
        <div v-if="hasServerAddress" class="saved-address">
          <div><span class="small-label">CURRENT ADDRESS</span><p class="m-0 mt-1 font-semibold break-all">{{ serverAddress.trim() }}</p></div>
          <button class="secondary-action" type="button" @click="copyAddress">{{ copied ? 'Copied!' : 'Copy address' }}</button>
        </div>
        <div class="provider-note"><span class="provider-indicator"></span><div><strong>Hosting: not connected</strong><p class="m-0 mt-1 text-sm text-secondary">No server has been provisioned, and no provider credentials or status are configured.</p></div></div>
      </section>
      <section class="content-panel">
        <h3 class="m-0 text-lg font-semibold">Server hosting vs. cloud gaming</h3>
        <div class="definition-grid">
          <div><span class="definition-tag">MULTIPLAYER HOSTING</span><p class="text-sm text-secondary">Runs a server world so multiple players can join. Requires a server host or your own machine.</p></div>
          <div><span class="definition-tag">CLOUD GAMING</span><p class="text-sm text-secondary">Runs the graphical Minecraft client on a remote machine and streams video/input. Requires a separate cloud PC provider.</p></div>
        </div>
      </section>
    </template>

    <template v-else>
      <section class="content-panel">
        <div class="panel-heading"><div><p class="eyebrow m-0 mb-2">BACKUP WORKSPACE</p><h2 class="m-0 text-2xl font-bold">Cloud saves</h2></div><span class="status-pill muted">Cloud sync not connected</span></div>
        <p class="text-secondary mt-3">Cloud storage is not configured in this build. You can keep a local list of save reminders and export that list as JSON. This is not a backup of your actual Minecraft world files.</p>
        <form class="save-form" @submit.prevent="addSaveNote">
          <label for="save-name">World or backup label</label><input id="save-name" v-model="saveName" placeholder="e.g. Survival world — before update" required />
          <label for="save-notes">Notes <span class="text-secondary">(optional)</span></label><textarea id="save-notes" v-model="saveNotes" rows="3" placeholder="Where the world is stored, version, or what to remember…"></textarea>
          <div class="flex flex-wrap gap-2"><button class="primary-action" type="submit">Add reminder</button><button class="secondary-action" type="button" :disabled="backupHistory.length === 0" @click="exportSaveManifest">Export reminder list</button></div>
        </form>
        <div v-if="backupHistory.length" class="reminder-list">
          <div v-for="(entry, index) in backupHistory" :key="entry.createdAt + entry.name" class="reminder-row">
            <div><strong>{{ entry.name }}</strong><p v-if="entry.notes" class="m-0 mt-1 text-sm text-secondary">{{ entry.notes }}</p></div>
            <button class="text-sm text-secondary hover:text-contrast" type="button" @click="backupHistory.splice(index, 1)">Remove</button>
          </div>
        </div>
        <div v-else class="empty-state"><span class="feature-icon">◇</span><strong>No reminders yet</strong><span class="text-sm text-secondary">Add a label to start a list before you back up a world manually.</span></div>
      </section>
      <section class="notice-panel"><div class="notice-symbol">!</div><div><h3 class="m-0 text-base font-semibold">Your world files stay yours</h3><p class="m-0 mt-1 text-sm text-secondary">This page does not read, upload, or synchronize worlds, credentials, or launcher data. A real cloud-save integration must be added and tested before sync can be enabled.</p></div></section>
    </template>
  </main>
</template>

<style scoped>
.cloudcraft-page { min-height: 100%; color: var(--color-base); }
.eyebrow { color: #b59cff; font-size: .7rem; font-weight: 800; letter-spacing: .18em; }
.status-pill { display: inline-flex; align-items: center; gap: .5rem; border: 1px solid rgba(166,139,255,.28); border-radius: 999px; padding: .5rem .8rem; font-size: .75rem; color: #d6c9ff; background: rgba(128,91,230,.1); white-space: nowrap; }
.status-pill.muted { color: var(--color-secondary); border-color: var(--color-button-bg); background: transparent; }
.status-dot,.provider-indicator { width: .45rem; height: .45rem; border-radius: 50%; background: #a78bfa; display: inline-block; }
.section-tabs { display: flex; flex-wrap: wrap; gap: .4rem; border-bottom: 1px solid var(--color-button-bg); }
.section-tabs button { border: 0; background: transparent; color: var(--color-secondary); padding: .8rem 1rem; border-bottom: 2px solid transparent; cursor: pointer; }
.section-tabs button.selected { color: var(--color-contrast); border-bottom-color: #a78bfa; }
.hero-panel { min-height: 265px; display: flex; align-items: center; justify-content: space-between; gap: 2rem; padding: clamp(1.5rem,4vw,3rem); border: 1px solid rgba(167,139,250,.25); border-radius: 1.5rem; background: radial-gradient(ellipse at 85% 15%, rgba(125,83,220,.22), transparent 38%), linear-gradient(125deg, var(--color-bg-raised), rgba(80,52,135,.15)); overflow: hidden; }
.hero-copy { position: relative; z-index: 1; }
.hero-orbit { position: relative; flex: 0 0 190px; height: 190px; display: grid; place-items: center; }
.orbit-ring { position: absolute; border: 1px solid rgba(183,156,255,.4); border-radius: 50%; transform: rotate(-28deg); }
.ring-one { width: 155px; height: 155px; }.ring-two { width: 112px; height: 178px; transform: rotate(45deg); }
.orbit-core { display: grid; place-items: center; width: 78px; height: 78px; border-radius: 1.5rem; color: white; font-weight: 900; letter-spacing: -.08em; background: linear-gradient(135deg,#a78bfa,#6d28d9); box-shadow: 0 12px 50px rgba(109,40,217,.35); }
.orbit-star { position: absolute; color: #c4b5fd; }.star-one { top: 8px; right: 30px; font-size: 1.6rem; }.star-two { bottom: 16px; left: 12px; font-size: 1.4rem; }
.primary-action,.secondary-action { display: inline-flex; align-items: center; justify-content: center; gap: .5rem; padding: .7rem 1rem; border-radius: .8rem; text-decoration: none; font-size: .875rem; font-weight: 700; cursor: pointer; transition: transform .15s,filter .15s; }
.primary-action { border: 1px solid transparent; background: linear-gradient(120deg,#a78bfa,#7c3aed); color: white; }.secondary-action { border: 1px solid var(--color-button-bg); background: var(--color-bg); color: var(--color-contrast); }
.primary-action:hover,.secondary-action:hover { filter: brightness(1.08); transform: translateY(-1px); }.primary-action:disabled,.secondary-action:disabled { opacity: .45; cursor: not-allowed; transform: none; }
.feature-card { display: flex; flex-direction: column; gap: .65rem; min-height: 190px; padding: 1.25rem; border: 1px solid var(--color-button-bg); border-radius: 1.1rem; background: var(--color-bg-raised); text-decoration: none; color: inherit; cursor: pointer; transition: border-color .15s,transform .15s; }
.feature-card:hover { border-color: rgba(167,139,250,.6); transform: translateY(-2px); }.feature-icon { display: grid; place-items: center; width: 2.4rem; height: 2.4rem; border-radius: .8rem; background: rgba(167,139,250,.13); color: #c4b5fd; font-size: 1.5rem; }.feature-title { font-weight: 750; }.feature-copy { color: var(--color-secondary); font-size: .85rem; line-height: 1.5; }.feature-link { color: #b59cff; font-size: .8rem; font-weight: 700; margin-top: auto; }
.notice-panel,.provider-note { display: flex; align-items: flex-start; gap: .9rem; border: 1px solid var(--color-button-bg); border-radius: 1rem; padding: 1rem 1.2rem; background: var(--color-bg-raised); }.notice-symbol { display: grid; place-items: center; flex: 0 0 1.6rem; height: 1.6rem; border-radius: 50%; background: rgba(167,139,250,.15); color: #c4b5fd; font-weight: 800; }
.content-panel { padding: clamp(1.2rem,3vw,2rem); border: 1px solid var(--color-button-bg); border-radius: 1.25rem; background: var(--color-bg-raised); }.panel-heading { display: flex; align-items: flex-start; justify-content: space-between; gap: 1rem; flex-wrap: wrap; }
.server-form,.save-form { display: flex; flex-direction: column; gap: .65rem; max-width: 760px; margin-top: 1.5rem; }.server-form label,.save-form label { font-size: .85rem; font-weight: 700; }.server-form input,.save-form input,.save-form textarea { width: 100%; box-sizing: border-box; border: 1px solid var(--color-button-bg); border-radius: .7rem; padding: .8rem .9rem; background: var(--color-bg); color: var(--color-contrast); outline: none; }.server-form input:focus,.save-form input:focus,.save-form textarea:focus { border-color: #a78bfa; }.saved-address { margin-top: 1.5rem; display: flex; align-items: center; justify-content: space-between; gap: 1rem; flex-wrap: wrap; padding: 1rem; border-radius: .9rem; background: var(--color-bg); border: 1px solid var(--color-button-bg); }.small-label,.definition-tag { color: #b59cff; font-size: .65rem; font-weight: 800; letter-spacing: .12em; }.provider-note { margin-top: 1.5rem; }.provider-indicator { background: #8b849a; margin-top: .35rem; flex-shrink: 0; }.definition-grid { display: grid; grid-template-columns: repeat(2,minmax(0,1fr)); gap: 1rem; margin-top: 1.2rem; }.definition-grid > div { padding: 1rem; border-radius: .9rem; background: var(--color-bg); }
.reminder-list { margin-top: 1.5rem; border-top: 1px solid var(--color-button-bg); }.reminder-row { display: flex; align-items: flex-start; justify-content: space-between; gap: 1rem; padding: 1rem 0; border-bottom: 1px solid var(--color-button-bg); }.reminder-row button { border: 0; background: transparent; cursor: pointer; }.empty-state { display: flex; flex-direction: column; align-items: center; gap: .75rem; padding: 2.5rem 1rem; text-align: center; }
@media (max-width: 620px) { .hero-orbit { display: none; }.definition-grid { grid-template-columns: 1fr; }.panel-heading { align-items: stretch; } }
</style>
