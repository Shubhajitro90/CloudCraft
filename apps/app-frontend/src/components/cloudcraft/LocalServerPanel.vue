<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, reactive, ref } from 'vue'
import { open as openDialog } from '@tauri-apps/plugin-dialog'
import {
  configureLocalServer,
  forceStopLocalServer,
  getLocalServerLogs,
  getLocalServerStatus,
  loadLocalServer,
  restartLocalServer,
  startLocalServer,
  stopLocalServer,
  type DownloadedSoftware,
  type LocalServerConfig,
  type LocalServerStatus,
} from '@/helpers/cloudcraftServer'
import ServerBackupsPanel from './ServerBackupsPanel.vue'
import ServerDeletePanel from './ServerDeletePanel.vue'
import ServerSoftwarePanel from './ServerSoftwarePanel.vue'
import ServerWorldsPanel from './ServerWorldsPanel.vue'

const LAST_DIRECTORY_KEY = 'cloudcraft.local-server-directory'
const form = reactive<LocalServerConfig>({
  serverDir: '',
  serverJar: '',
  javaExecutable: 'java',
  name: 'CloudCraft temporary server',
  gameVersion: '',
  memoryMb: 2048,
  maxPlayers: 10,
  port: 25565,
  acceptEula: false,
})
const status = ref<LocalServerStatus>({
  state: 'stopped',
  message: 'Choose a server directory and a legitimate Minecraft server JAR.',
  reachable: false,
  canStart: false,
  canStop: false,
  canForceStop: false,
})
const logs = ref<string[]>([])
const busy = ref(false)
const refreshing = ref(false)
const error = ref('')
const message = ref('')
const copied = ref(false)

const active = computed(() => ['starting', 'running', 'stopping'].includes(status.value.state))
const canEdit = computed(() => !busy.value && !active.value)
const logText = computed(() => logs.value.join('\n') || 'No server output yet.')
const statusLabel = computed(() => {
  const labels = {
    stopped: 'Stopped',
    starting: 'Starting',
    running: 'Running',
    stopping: 'Stop pending',
    failed: 'Failed',
  }
  return labels[status.value.state]
})

function errorMessage(caught: unknown) {
  return caught instanceof Error ? caught.message : String(caught)
}

function validatedConfig(): LocalServerConfig {
  return {
    ...form,
    serverDir: form.serverDir.trim(),
    serverJar: form.serverJar.trim(),
    javaExecutable: form.javaExecutable.trim() || 'java',
    name: form.name.trim(),
    gameVersion: form.gameVersion.trim(),
    memoryMb: Number(form.memoryMb),
    maxPlayers: Number(form.maxPlayers),
    port: Number(form.port),
  }
}

function applyStatus(next: LocalServerStatus, syncForm = false) {
  status.value = next
  if (syncForm && next.config) Object.assign(form, next.config)
}

function rememberDirectory() {
  try {
    if (form.serverDir) localStorage.setItem(LAST_DIRECTORY_KEY, form.serverDir)
  } catch {
    // The backend configuration remains persistent even if web storage is unavailable.
  }
}

async function chooseServerDirectory() {
  const selection = await openDialog({
    directory: true,
    multiple: false,
    title: 'Choose a dedicated local server directory',
  })
  if (typeof selection === 'string') form.serverDir = selection
}

async function chooseServerJar() {
  const selection = await openDialog({
    directory: false,
    multiple: false,
    title: 'Choose a Minecraft server JAR inside the server directory',
    filters: [{ name: 'Minecraft server JAR', extensions: ['jar'] }],
  })
  if (typeof selection === 'string') form.serverJar = selection
}

async function saveConfiguration() {
  if (!canEdit.value) return
  busy.value = true
  error.value = ''
  message.value = ''
  try {
    const config = validatedConfig()
    applyStatus(await configureLocalServer(config), true)
    rememberDirectory()
    message.value = 'Local server configuration saved and validated.'
  } catch (caught) {
    error.value = errorMessage(caught)
  } finally {
    busy.value = false
  }
}

async function startServer() {
  if (busy.value || active.value) return
  busy.value = true
  error.value = ''
  message.value = ''
  try {
    const config = validatedConfig()
    await configureLocalServer(config)
    rememberDirectory()
    applyStatus(await startLocalServer(config.serverDir), true)
    await refreshLogs()
  } catch (caught) {
    error.value = errorMessage(caught)
    await refreshStatus(false)
  } finally {
    busy.value = false
  }
}

async function stopServer() {
  if (busy.value || !status.value.canStop) return
  busy.value = true
  error.value = ''
  message.value = ''
  try {
    applyStatus(await stopLocalServer())
    message.value =
      status.value.state === 'stopped'
        ? 'Server stopped after the graceful shutdown request.'
        : 'Graceful stop is still pending. Check logs before considering force-stop.'
  } catch (caught) {
    error.value = errorMessage(caught)
  } finally {
    busy.value = false
  }
}

async function restartServer() {
  if (busy.value || status.value.state !== 'running') return
  busy.value = true
  error.value = ''
  message.value = ''
  try {
    applyStatus(await restartLocalServer(form.serverDir.trim()), true)
    await refreshLogs()
    message.value = 'Server restarted.'
  } catch (caught) {
    error.value = errorMessage(caught)
    await refreshStatus(false)
  } finally {
    busy.value = false
  }
}

function onSoftwareDownloaded(result: DownloadedSoftware, version: string) {
  form.serverJar = result.path
  form.gameVersion = version
  message.value = `${result.fileName} verified. Review the configuration and save it to use this JAR.`
}

async function forceStop() {
  if (busy.value || !status.value.canForceStop) return
  const confirmed = window.confirm(
    'Force-stop the local server now? Unsaved world data may be lost. Try a graceful stop first whenever possible.',
  )
  if (!confirmed) return

  busy.value = true
  error.value = ''
  message.value = ''
  try {
    applyStatus(await forceStopLocalServer())
    message.value = 'Server process was force-stopped. Unsaved changes may have been lost.'
  } catch (caught) {
    error.value = errorMessage(caught)
  } finally {
    busy.value = false
  }
}

async function refreshLogs() {
  try {
    logs.value = await getLocalServerLogs()
  } catch (caught) {
    if (!error.value) error.value = errorMessage(caught)
  }
}

async function refreshStatus(includeError = true) {
  if (refreshing.value) return
  refreshing.value = true
  try {
    applyStatus(await getLocalServerStatus())
    await refreshLogs()
    if (includeError) error.value = ''
  } catch (caught) {
    if (includeError) error.value = errorMessage(caught)
  } finally {
    refreshing.value = false
  }
}

async function copyConnectionAddress() {
  if (!status.value.connectionAddress) return
  try {
    await navigator.clipboard.writeText(status.value.connectionAddress)
    copied.value = true
    window.setTimeout(() => (copied.value = false), 1800)
  } catch {
    copied.value = false
  }
}

async function restoreConfiguration() {
  let savedDirectory = ''
  try {
    savedDirectory = localStorage.getItem(LAST_DIRECTORY_KEY) || ''
  } catch {
    return
  }
  if (!savedDirectory) return

  try {
    applyStatus(await loadLocalServer(savedDirectory), true)
    await refreshLogs()
  } catch (caught) {
    error.value = errorMessage(caught)
  }
}

onMounted(async () => {
  await restoreConfiguration()
  await refreshStatus(false)
})
const pollTimer = window.setInterval(() => refreshStatus(false), 1500)
onBeforeUnmount(() => window.clearInterval(pollTimer))
</script>

<template>
  <section class="server-panel" aria-labelledby="local-server-heading">
    <div class="server-heading">
      <div>
        <p class="server-eyebrow">TEMPORARY MULTIPLAYER · LOCAL MODE</p>
        <h2 id="local-server-heading">Run a real server on this device</h2>
        <p>
          This is separate from cloud gaming. Java runs the selected Minecraft server JAR locally;
          no remote host is created and no public connection is advertised.
        </p>
      </div>
      <span class="server-status" :class="`is-${status.state}`">
        <span aria-hidden="true"></span>{{ statusLabel }}
      </span>
    </div>

    <div class="server-notice">
      <strong>Local-only network boundary</strong>
      <p>
        CloudCraft preserves unrelated <code>server.properties</code> entries but manages the port,
        player limit, MOTD, <code>online-mode=true</code>, and <code>server-ip=127.0.0.1</code>.
        Players on other devices cannot join unless you separately configure and secure networking.
      </p>
    </div>

    <form class="server-form" @submit.prevent="saveConfiguration">
      <fieldset :disabled="!canEdit">
        <div class="server-field server-field-wide">
          <label for="local-server-dir">Dedicated server directory</label>
          <div class="server-path-control">
            <input
              id="local-server-dir"
              v-model="form.serverDir"
              required
              autocomplete="off"
              placeholder="Choose an empty or existing local server folder"
            />
            <button type="button" class="server-secondary" @click="chooseServerDirectory">
              Browse
            </button>
          </div>
        </div>
        <div class="server-field server-field-wide">
          <label for="local-server-jar">Minecraft server JAR</label>
          <div class="server-path-control">
            <input
              id="local-server-jar"
              v-model="form.serverJar"
              required
              autocomplete="off"
              placeholder="The .jar must be directly inside the server directory"
            />
            <button type="button" class="server-secondary" @click="chooseServerJar">
              Choose JAR
            </button>
          </div>
          <small>
            Use the checksum-verified downloader below, or pick a JAR you obtained from the
            <a
              href="https://www.minecraft.net/en-us/download/server"
              target="_blank"
              rel="noopener"
            >
              official Minecraft site</a
            >.
          </small>
        </div>
        <div class="server-field">
          <label for="local-server-name">Server name / MOTD</label>
          <input id="local-server-name" v-model="form.name" maxlength="64" required />
        </div>
        <div class="server-field">
          <label for="local-server-version">Version label (optional)</label>
          <input
            id="local-server-version"
            v-model="form.gameVersion"
            maxlength="64"
            placeholder="For your reference; the JAR determines the version"
          />
        </div>
        <div class="server-field">
          <label for="local-server-java">Java executable</label>
          <input
            id="local-server-java"
            v-model="form.javaExecutable"
            required
            autocomplete="off"
            placeholder="java or a full path to java/java.exe"
          />
        </div>
        <div class="server-field">
          <label for="local-server-memory">Maximum memory (MB)</label>
          <input
            id="local-server-memory"
            v-model.number="form.memoryMb"
            type="number"
            min="512"
            max="1048576"
            step="256"
            required
          />
        </div>
        <div class="server-field">
          <label for="local-server-players">Allowed players</label>
          <input
            id="local-server-players"
            v-model.number="form.maxPlayers"
            type="number"
            min="1"
            max="1000"
            required
          />
        </div>
        <div class="server-field">
          <label for="local-server-port">Local port</label>
          <input
            id="local-server-port"
            v-model.number="form.port"
            type="number"
            min="1"
            max="65535"
            required
          />
        </div>
      </fieldset>
      <label class="server-eula">
        <input v-model="form.acceptEula" type="checkbox" :disabled="!canEdit" required />
        <span>
          I have read and accept the
          <a href="https://www.minecraft.net/en-us/eula" target="_blank" rel="noopener">
            Minecraft EULA</a
          >. Saving writes <code>eula=true</code> for this local server.
        </span>
      </label>
      <div class="server-actions">
        <button class="server-secondary" type="submit" :disabled="!canEdit">
          Save configuration
        </button>
        <button
          class="server-primary"
          type="button"
          :disabled="busy || active"
          @click="startServer"
        >
          {{ busy && !active ? 'Working…' : active ? 'Server active' : 'Start local server' }}
        </button>
        <button
          v-if="status.canStop"
          class="server-secondary"
          type="button"
          :disabled="busy"
          @click="stopServer"
        >
          {{ status.state === 'stopping' ? 'Retry graceful stop' : 'Stop gracefully' }}
        </button>
        <button
          v-if="status.state === 'running'"
          class="server-secondary"
          type="button"
          :disabled="busy"
          @click="restartServer"
        >
          Restart
        </button>
        <button
          v-if="status.canForceStop"
          class="server-danger"
          type="button"
          :disabled="busy"
          @click="forceStop"
        >
          Force stop…
        </button>
        <button
          class="server-secondary"
          type="button"
          :disabled="refreshing"
          @click="refreshStatus()"
        >
          Refresh
        </button>
      </div>
    </form>

    <p class="server-state-message" role="status">{{ status.message }}</p>
    <p v-if="message" class="server-success" role="status">{{ message }}</p>
    <p v-if="error" class="server-error" role="alert">{{ error }}</p>

    <div v-if="status.reachable && status.connectionAddress" class="server-connection">
      <div>
        <span>VERIFIED LOCAL CONNECTION</span>
        <strong>{{ status.connectionAddress }}</strong>
        <small>The port accepted a real TCP connection on this device.</small>
      </div>
      <button class="server-secondary" type="button" @click="copyConnectionAddress">
        {{ copied ? 'Copied!' : 'Copy address' }}
      </button>
    </div>

    <ServerSoftwarePanel
      :server-dir="form.serverDir"
      :disabled="!canEdit"
      @downloaded="onSoftwareDownloaded"
    />
    <ServerWorldsPanel :server-dir="form.serverDir" :server-active="active" />
    <ServerBackupsPanel :server-dir="form.serverDir" :server-active="active" />
    <ServerDeletePanel :server-dir="form.serverDir" :server-active="active" />

    <details class="server-logs" :open="active">
      <summary>
        Server logs <span>{{ logs.length }} lines retained</span>
      </summary>
      <pre tabindex="0">{{ logText }}</pre>
    </details>
    <p class="server-footnote">
      Stopping sends the Minecraft <code>stop</code> command and waits up to 15 seconds. A separate
      confirmation is required to kill an unresponsive process. Closing CloudCraft also attempts a
      graceful shutdown before terminating any remaining child process.
    </p>
  </section>
</template>

<style scoped>
.server-panel {
  display: flex;
  flex-direction: column;
  gap: 1.1rem;
  padding: clamp(1.2rem, 3vw, 2rem);
  border: 1px solid var(--color-button-bg);
  border-radius: 1.25rem;
  background: var(--color-bg-raised);
  color: var(--color-base);
}
.server-heading {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 1rem;
  flex-wrap: wrap;
}
.server-heading h2 {
  margin: 0;
  font-size: 1.5rem;
}
.server-heading p:not(.server-eyebrow) {
  max-width: 760px;
  margin: 0.6rem 0 0;
  color: var(--color-secondary);
}
.server-eyebrow {
  margin: 0 0 0.5rem;
  color: #b59cff;
  font-size: 0.68rem;
  font-weight: 800;
  letter-spacing: 0.14em;
}
.server-status {
  display: inline-flex;
  align-items: center;
  gap: 0.45rem;
  padding: 0.45rem 0.75rem;
  border: 1px solid var(--color-button-bg);
  border-radius: 999px;
  color: var(--color-secondary);
  font-size: 0.75rem;
  white-space: nowrap;
}
.server-status > span {
  width: 0.45rem;
  height: 0.45rem;
  border-radius: 50%;
  background: #8b849a;
}
.server-status.is-running > span {
  background: #4ade80;
}
.server-status.is-starting > span,
.server-status.is-stopping > span {
  background: #fbbf24;
}
.server-status.is-failed > span {
  background: #f87171;
}
.server-notice,
.server-connection {
  padding: 1rem;
  border: 1px solid var(--color-button-bg);
  border-radius: 0.9rem;
  background: var(--color-bg);
}
.server-notice p {
  margin: 0.35rem 0 0;
  color: var(--color-secondary);
  font-size: 0.85rem;
}
.server-form fieldset {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 1rem;
  min-width: 0;
  margin: 0;
  padding: 0;
  border: 0;
}
.server-field {
  display: flex;
  flex-direction: column;
  gap: 0.45rem;
  min-width: 0;
}
.server-field-wide {
  grid-column: 1 / -1;
}
.server-field label {
  font-size: 0.82rem;
  font-weight: 700;
}
.server-field small,
.server-connection small {
  color: var(--color-secondary);
  font-size: 0.75rem;
}
.server-field input {
  width: 100%;
  box-sizing: border-box;
  padding: 0.75rem 0.85rem;
  border: 1px solid var(--color-button-bg);
  border-radius: 0.7rem;
  outline: none;
  background: var(--color-bg);
  color: var(--color-contrast);
}
.server-field input:focus-visible,
.server-eula input:focus-visible,
button:focus-visible,
summary:focus-visible,
pre:focus-visible {
  outline: 2px solid #a78bfa;
  outline-offset: 2px;
}
.server-path-control {
  display: flex;
  gap: 0.5rem;
}
.server-path-control button {
  flex: 0 0 auto;
}
.server-eula {
  display: flex;
  align-items: flex-start;
  gap: 0.6rem;
  margin-top: 1rem;
  color: var(--color-secondary);
  font-size: 0.82rem;
}
.server-eula input {
  margin-top: 0.2rem;
}
.server-actions {
  display: flex;
  flex-wrap: wrap;
  gap: 0.6rem;
  margin-top: 1.2rem;
}
.server-primary,
.server-secondary,
.server-danger {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  padding: 0.65rem 0.9rem;
  border-radius: 0.7rem;
  font-size: 0.8rem;
  font-weight: 700;
  cursor: pointer;
}
.server-primary {
  border: 1px solid transparent;
  background: linear-gradient(120deg, #a78bfa, #7c3aed);
  color: white;
}
.server-secondary {
  border: 1px solid var(--color-button-bg);
  background: var(--color-bg);
  color: var(--color-contrast);
}
.server-danger {
  border: 1px solid rgba(248, 113, 113, 0.35);
  background: rgba(185, 28, 28, 0.12);
  color: #fca5a5;
}
.server-primary:disabled,
.server-secondary:disabled,
.server-danger:disabled {
  opacity: 0.45;
  cursor: not-allowed;
}
.server-state-message,
.server-success,
.server-footnote {
  margin: 0;
  color: var(--color-secondary);
  font-size: 0.82rem;
}
.server-success {
  color: #86efac;
}
.server-error {
  margin: 0;
  padding: 0.8rem 1rem;
  border: 1px solid rgba(248, 113, 113, 0.3);
  border-radius: 0.75rem;
  background: rgba(185, 28, 28, 0.1);
  color: #fca5a5;
  overflow-wrap: anywhere;
}
.server-connection {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 1rem;
  flex-wrap: wrap;
  border-color: rgba(74, 222, 128, 0.3);
}
.server-connection > div {
  display: flex;
  flex-direction: column;
  gap: 0.25rem;
}
.server-connection span {
  color: #86efac;
  font-size: 0.65rem;
  font-weight: 800;
  letter-spacing: 0.12em;
}
.server-logs {
  border: 1px solid var(--color-button-bg);
  border-radius: 0.8rem;
  background: var(--color-bg);
}
.server-logs summary {
  padding: 0.8rem 1rem;
  cursor: pointer;
  font-size: 0.82rem;
  font-weight: 700;
}
.server-logs summary span {
  margin-left: 0.5rem;
  color: var(--color-secondary);
  font-weight: 400;
}
.server-logs pre {
  max-height: 300px;
  margin: 0;
  padding: 1rem;
  overflow: auto;
  border-top: 1px solid var(--color-button-bg);
  color: var(--color-secondary);
  font-size: 0.75rem;
  white-space: pre-wrap;
  overflow-wrap: anywhere;
}
a {
  color: #b59cff;
}
code {
  font-size: 0.95em;
}
@media (max-width: 700px) {
  .server-form fieldset {
    grid-template-columns: 1fr;
  }
  .server-field-wide {
    grid-column: auto;
  }
  .server-path-control {
    flex-direction: column;
  }
}
</style>
