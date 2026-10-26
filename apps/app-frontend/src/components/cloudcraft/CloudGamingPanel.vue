<script setup>
import { computed, onBeforeUnmount, onMounted, reactive, ref, watch } from 'vue'
import {
  cloudHost,
  cloudSessionProvider,
  cloudSessionStateLabel,
  formatFingerprint,
  latencyQuality,
} from '@/helpers/cloudSession'
import { validateCloudHostForm } from '@/helpers/cloudcraftDiagnostics'

const emit = defineEmits(['status'])

const view = ref(null)
const status = ref({
  state: 'unavailable',
  message: 'Checking cloud gaming status…',
  canStart: false,
  canStop: false,
  canCancel: false,
  canReconnect: false,
  cleanupRequired: false,
})
const busy = ref('')
const error = ref('')
const notice = ref('')
const editing = ref(false)
const password = ref('')
const latency = ref(null)
const logs = ref([])
const showLogs = ref(false)
const probedFingerprint = ref('')
const trustConfirmed = ref(false)

const form = reactive({
  host: '',
  apiPort: '47990',
  username: '',
  launchCommand: '',
  workingDir: '',
  moonlightPath: '',
  resolution: '',
  fps: '',
})

const config = computed(() => view.value?.config || null)
const formErrors = computed(() => validateCloudHostForm(form))
const identityChanged = computed(
  () =>
    !config.value ||
    config.value.host !== form.host.trim() ||
    String(config.value.apiPort) !== form.apiPort.trim(),
)
const fingerprintForSave = computed(() =>
  identityChanged.value ? probedFingerprint.value : config.value?.certSha256 || '',
)
const canSave = computed(
  () =>
    !formErrors.value.length &&
    !!fingerprintForSave.value &&
    (!identityChanged.value || trustConfirmed.value) &&
    !busy.value,
)
const sessionActive = computed(
  () =>
    ['starting', 'ready', 'stopping'].includes(status.value.state) || status.value.cleanupRequired,
)
const quality = computed(() => latencyQuality(latency.value))

function fillForm(source) {
  form.host = source?.host || ''
  form.apiPort = String(source?.apiPort || 47990)
  form.username = source?.username || ''
  form.launchCommand = source?.launchCommand || ''
  form.workingDir = source?.workingDir || ''
  form.moonlightPath = source?.moonlightPath || view.value?.moonlightCandidates?.[0] || ''
  form.resolution = source?.resolution || ''
  form.fps = source?.fps ? String(source.fps) : ''
  probedFingerprint.value = ''
  trustConfirmed.value = false
}

function message(err) {
  return String(err?.message || err)
}

async function refresh() {
  try {
    const [hostView, snapshot] = await Promise.all([
      cloudHost.get(),
      cloudSessionProvider.getStatus(),
    ])
    view.value = hostView
    status.value = snapshot
    emit('status', snapshot)
    if (!hostView.config && !editing.value) {
      editing.value = true
      fillForm(null)
    }
  } catch (err) {
    error.value = message(err)
  }
}

async function run(label, action) {
  if (busy.value) return
  busy.value = label
  error.value = ''
  notice.value = ''
  try {
    await action()
  } catch (err) {
    error.value = message(err)
  } finally {
    busy.value = ''
    await refresh()
  }
}

function startEditing() {
  editing.value = true
  fillForm(config.value)
}

function probe() {
  return run('probe', async () => {
    probedFingerprint.value = ''
    trustConfirmed.value = false
    probedFingerprint.value = await cloudHost.probe(form.host.trim(), Number(form.apiPort))
  })
}

function save() {
  if (!canSave.value) return
  return run('save', async () => {
    view.value = await cloudHost.save({
      host: form.host.trim(),
      apiPort: Number(form.apiPort),
      username: form.username.trim(),
      certSha256: fingerprintForSave.value,
      launchCommand: form.launchCommand.trim(),
      workingDir: form.workingDir.trim() || null,
      moonlightPath: form.moonlightPath.trim(),
      resolution: form.resolution.trim() || null,
      fps: form.fps.trim() ? Number(form.fps) : null,
    })
    editing.value = false
    notice.value = 'Remote host saved. Unlock it with the Sunshine password to start a session.'
  })
}

function removeHost() {
  return run('remove', async () => {
    view.value = await cloudHost.remove()
    fillForm(null)
    editing.value = true
    notice.value = 'Remote host removed from CloudCraft. Sunshine on the host was not changed.'
  })
}

function unlock() {
  if (!password.value) return
  const value = password.value
  password.value = ''
  return run('unlock', async () => {
    view.value = await cloudHost.unlock(value)
    notice.value = 'Host unlocked for this launcher session. The password is not saved.'
  })
}

function lock() {
  return run('lock', async () => {
    view.value = await cloudHost.lock()
  })
}

let startPoll = null
function start() {
  return run('start', async () => {
    startPoll = setInterval(async () => {
      status.value = await cloudSessionProvider.getStatus().catch(() => status.value)
    }, 1000)
    try {
      status.value = await cloudSessionProvider.startSession()
    } finally {
      clearInterval(startPoll)
      startPoll = null
    }
  })
}

async function cancelStart() {
  try {
    status.value = await cloudSessionProvider.cancelStart()
  } catch (err) {
    error.value = message(err)
  }
}

function stop() {
  return run('stop', async () => {
    status.value = await cloudSessionProvider.stopSession()
    latency.value = null
  })
}

function reconnect() {
  return run('reconnect', async () => {
    status.value = await cloudSessionProvider.reconnect()
  })
}

async function loadLogs() {
  logs.value = await cloudHost.logs().catch(() => logs.value)
}

let healthTimer = null
async function healthTick() {
  if (busy.value) return
  try {
    status.value = await cloudSessionProvider.getStatus()
    emit('status', status.value)
    if (status.value.state === 'ready' && view.value?.unlocked) {
      latency.value = await cloudHost.ping().catch(() => null)
    }
    if (showLogs.value) await loadLogs()
  } catch {
    // Next tick retries; the last known status stays visible.
  }
}

watch(showLogs, (open) => {
  if (open) loadLogs()
})

onMounted(async () => {
  await refresh()
  healthTimer = setInterval(healthTick, 5000)
})

onBeforeUnmount(() => {
  clearInterval(healthTimer)
  if (startPoll) clearInterval(startPoll)
})
</script>

<template>
  <section class="cloud-panel" aria-labelledby="cloud-gaming-heading">
    <div class="cloud-heading">
      <div>
        <p class="eyebrow">REMOTE COMPUTER · SUNSHINE + MOONLIGHT</p>
        <h3 id="cloud-gaming-heading">Cloud gaming</h3>
      </div>
      <span class="state-pill" :class="`state-${status.state}`" role="status">
        {{ cloudSessionStateLabel(status.state) }}
      </span>
    </div>

    <p class="muted">
      Minecraft runs on a computer you control (a home gaming PC or any machine with a GPU). That
      computer renders the game; Moonlight on this device receives the video and audio and sends
      your keyboard, mouse and controller input back over an encrypted connection.
    </p>

    <p class="status-message" aria-live="polite">{{ status.message }}</p>
    <p v-if="status.state === 'starting'" class="muted small">
      Waiting until Moonlight is running and the remote computer reports the game session as
      started. This can take up to 90 seconds while Minecraft launches on the host.
    </p>
    <p v-if="status.cleanupRequired" class="error" role="alert">
      The remote computer may still be running Minecraft for this session. Stop it to close the
      remote app.
    </p>

    <div v-if="status.state === 'ready'" class="metrics" aria-label="Connection quality">
      <div>
        <span class="metric-label">Host API round trip</span>
        <strong :class="`quality-${quality}`">
          {{ latency === null ? 'Measuring…' : `${latency} ms` }}
        </strong>
      </div>
      <div>
        <span class="metric-label">Stream window</span>
        <strong>{{ view?.streaming ? 'Open' : 'Closed' }}</strong>
      </div>
      <p class="muted small">
        Press Ctrl+Alt+Shift+S in the stream window for Moonlight&apos;s frame and network
        statistics. Ctrl+Alt+Shift+Q leaves the stream.
      </p>
    </div>

    <div class="actions">
      <button
        v-if="status.canStart"
        class="button primary"
        type="button"
        :disabled="!!busy"
        @click="start"
      >
        {{ busy === 'start' ? 'Starting…' : 'Start cloud session' }}
      </button>
      <button
        v-if="status.state === 'starting'"
        class="button"
        type="button"
        :disabled="!status.canCancel"
        @click="cancelStart"
      >
        Cancel startup
      </button>
      <button
        v-if="status.canReconnect"
        class="button primary"
        type="button"
        :disabled="!!busy"
        @click="reconnect"
      >
        {{ busy === 'reconnect' ? 'Reconnecting…' : 'Reconnect' }}
      </button>
      <button v-if="status.canStop" class="button" type="button" :disabled="!!busy" @click="stop">
        {{ busy === 'stop' ? 'Stopping…' : 'Stop and close remote game' }}
      </button>
      <button class="button" type="button" @click="showLogs = !showLogs">
        {{ showLogs ? 'Hide stream log' : 'Show stream log' }}
      </button>
    </div>

    <p v-if="error" class="error" role="alert">{{ error }}</p>
    <p v-else-if="notice" class="success" role="status">{{ notice }}</p>

    <pre v-if="showLogs" class="log" aria-label="Moonlight stream log">{{
      logs.length ? logs.join('\n') : 'No stream output yet.'
    }}</pre>

    <div v-if="config && !editing" class="host-summary">
      <div class="host-row">
        <span>
          <strong>{{ config.host }}</strong>
          <small>Sunshine API port {{ config.apiPort }} · user {{ config.username }}</small>
        </span>
        <span class="lock-state" :class="{ unlocked: view?.unlocked }">
          {{ view?.unlocked ? 'Unlocked' : 'Locked' }}
        </span>
      </div>
      <p class="muted small">
        Remote command: <code>{{ config.launchCommand }}</code>
      </p>
      <p v-if="!view?.moonlightFound" class="error">
        Moonlight was not found at <code>{{ config.moonlightPath }}</code
        >.
      </p>
      <form v-if="!view?.unlocked" class="unlock" @submit.prevent="unlock">
        <label for="sunshine-password">Sunshine password</label>
        <input
          id="sunshine-password"
          v-model="password"
          type="password"
          autocomplete="off"
          spellcheck="false"
          :disabled="!!busy"
        />
        <button class="button primary" type="submit" :disabled="!password || !!busy">
          {{ busy === 'unlock' ? 'Checking…' : 'Unlock' }}
        </button>
      </form>
      <div class="actions">
        <button v-if="view?.unlocked" class="button" type="button" :disabled="!!busy" @click="lock">
          Lock
        </button>
        <button
          class="button"
          type="button"
          :disabled="sessionActive || !!busy"
          @click="startEditing"
        >
          Edit host
        </button>
        <button
          class="button danger"
          type="button"
          :disabled="sessionActive || !!busy"
          @click="removeHost"
        >
          Remove host
        </button>
      </div>
    </div>

    <form v-if="editing" class="host-form" @submit.prevent="save">
      <h4>Remote computer</h4>
      <div class="field-grid">
        <label>
          Hostname or IP
          <input v-model="form.host" placeholder="gaming-pc.tail1234.ts.net" spellcheck="false" />
        </label>
        <label>
          Sunshine API port
          <input v-model="form.apiPort" inputmode="numeric" />
        </label>
        <label>
          Sunshine username
          <input v-model="form.username" autocomplete="off" spellcheck="false" />
        </label>
        <label class="wide">
          Command that starts Minecraft on the remote computer
          <input
            v-model="form.launchCommand"
            placeholder="C:\Program Files\CloudCraft\CloudCraft.exe"
            spellcheck="false"
          />
        </label>
        <label class="wide">
          Remote working directory (optional)
          <input v-model="form.workingDir" spellcheck="false" />
        </label>
        <label class="wide">
          Moonlight program on this device
          <input v-model="form.moonlightPath" list="moonlight-candidates" spellcheck="false" />
          <datalist id="moonlight-candidates">
            <option v-for="path in view?.moonlightCandidates || []" :key="path" :value="path" />
          </datalist>
        </label>
        <label>
          Resolution (optional)
          <input v-model="form.resolution" placeholder="1920x1080" />
        </label>
        <label>
          Frame rate (optional)
          <input v-model="form.fps" inputmode="numeric" placeholder="60" />
        </label>
      </div>

      <div v-if="identityChanged" class="trust">
        <p class="muted small">
          Sunshine uses a self-signed certificate. CloudCraft pins its fingerprint so credentials
          are only ever sent to this exact host. Compare the value below with the host&apos;s
          certificate before trusting it.
        </p>
        <button class="button" type="button" :disabled="!form.host.trim() || !!busy" @click="probe">
          {{ busy === 'probe' ? 'Connecting…' : 'Check certificate' }}
        </button>
        <template v-if="probedFingerprint">
          <code class="fingerprint">{{ formatFingerprint(probedFingerprint) }}</code>
          <label class="checkbox">
            <input v-model="trustConfirmed" type="checkbox" />
            I compared this SHA-256 fingerprint with the host and trust it.
          </label>
        </template>
      </div>

      <ul v-if="formErrors.length" class="form-errors">
        <li v-for="item in formErrors" :key="item">{{ item }}</li>
      </ul>

      <div class="actions">
        <button class="button primary" type="submit" :disabled="!canSave">
          {{ busy === 'save' ? 'Saving…' : 'Save host' }}
        </button>
        <button v-if="config" class="button" type="button" @click="editing = false">Cancel</button>
      </div>

      <details class="setup-guide">
        <summary>One-time setup on the remote computer</summary>
        <ol>
          <li>
            Install <strong>Sunshine</strong> (free, open source) on the remote computer and create
            its web UI username and password.
          </li>
          <li>
            Install your Minecraft launcher there and sign in with your own Microsoft account. The
            remote command above should start it, for example the launcher executable or a launch
            shortcut for one instance.
          </li>
          <li>
            Make the computer reachable from this device: same network, or a private overlay such as
            Tailscale or ZeroTier. Do not expose the Sunshine web UI port to the public internet.
          </li>
          <li>
            Install <strong>Moonlight</strong> on this device, add the host, and pair it once by
            entering Moonlight&apos;s PIN in the Sunshine web UI.
          </li>
        </ol>
      </details>
    </form>
  </section>
</template>

<style scoped>
.cloud-panel {
  display: flex;
  flex-direction: column;
  gap: 0.85rem;
  padding: 1.25rem;
  border: 1px solid var(--color-button-bg);
  border-radius: 1rem;
  background: var(--color-bg);
}
.cloud-heading {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 1rem;
  flex-wrap: wrap;
}
.cloud-heading h3 {
  margin: 0;
  font-size: 1.2rem;
}
.eyebrow {
  margin: 0 0 0.3rem;
  color: #b59cff;
  font-size: 0.65rem;
  font-weight: 800;
  letter-spacing: 0.14em;
}
.muted {
  margin: 0;
  color: var(--color-secondary);
}
.small {
  font-size: 0.8rem;
}
.status-message {
  margin: 0;
  font-weight: 600;
}
.state-pill {
  padding: 0.3rem 0.75rem;
  border-radius: 999px;
  border: 1px solid var(--color-button-bg);
  font-size: 0.8rem;
  font-weight: 700;
}
.state-ready {
  border-color: #22c55e;
  color: #4ade80;
}
.state-starting,
.state-stopping {
  border-color: #eab308;
  color: #facc15;
}
.state-error {
  border-color: #ef4444;
  color: #f87171;
}
.metrics {
  display: flex;
  flex-wrap: wrap;
  gap: 1rem 2rem;
  align-items: flex-end;
  padding: 0.8rem 1rem;
  border-radius: 0.75rem;
  background: var(--color-button-bg);
}
.metrics div {
  display: flex;
  flex-direction: column;
  gap: 0.15rem;
}
.metric-label {
  color: var(--color-secondary);
  font-size: 0.75rem;
}
.quality-good {
  color: #4ade80;
}
.quality-fair {
  color: #facc15;
}
.quality-poor {
  color: #f87171;
}
.actions {
  display: flex;
  flex-wrap: wrap;
  gap: 0.5rem;
}
.button {
  padding: 0.5rem 1rem;
  border: 1px solid var(--color-button-bg);
  border-radius: 0.65rem;
  background: var(--color-raised-bg);
  color: var(--color-base);
  font-weight: 600;
  cursor: pointer;
}
.button.primary {
  border-color: #8b5cf6;
  background: #7c3aed;
  color: #fff;
}
.button.danger {
  border-color: #7f1d1d;
  color: #fca5a5;
}
.button:disabled {
  opacity: 0.55;
  cursor: not-allowed;
}
.error {
  margin: 0;
  color: #f87171;
}
.success {
  margin: 0;
  color: #4ade80;
}
.log {
  max-height: 14rem;
  overflow: auto;
  margin: 0;
  padding: 0.75rem;
  border-radius: 0.65rem;
  background: #0b0b10;
  color: #d4d4d8;
  font-size: 0.75rem;
  white-space: pre-wrap;
  word-break: break-word;
}
.host-summary,
.host-form {
  display: flex;
  flex-direction: column;
  gap: 0.75rem;
  padding-top: 0.85rem;
  border-top: 1px solid var(--color-button-bg);
}
.host-form h4 {
  margin: 0;
}
.host-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 1rem;
}
.host-row small {
  display: block;
  color: var(--color-secondary);
}
.lock-state {
  font-size: 0.8rem;
  font-weight: 700;
  color: #facc15;
}
.lock-state.unlocked {
  color: #4ade80;
}
.unlock {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 0.5rem;
}
.field-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(14rem, 1fr));
  gap: 0.75rem;
}
.field-grid label,
.unlock label {
  display: flex;
  flex-direction: column;
  gap: 0.3rem;
  font-size: 0.85rem;
  font-weight: 600;
}
.field-grid .wide {
  grid-column: 1 / -1;
}
input:not([type='checkbox']) {
  padding: 0.5rem 0.7rem;
  border: 1px solid var(--color-button-bg);
  border-radius: 0.6rem;
  background: var(--color-raised-bg);
  color: var(--color-base);
}
.trust {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: 0.5rem;
}
.fingerprint {
  padding: 0.5rem 0.7rem;
  border-radius: 0.5rem;
  background: var(--color-button-bg);
  font-size: 0.75rem;
  word-break: break-all;
}
.checkbox {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  font-size: 0.85rem;
}
.form-errors {
  margin: 0;
  padding-left: 1.1rem;
  color: #fca5a5;
  font-size: 0.85rem;
}
.setup-guide summary {
  cursor: pointer;
  font-weight: 600;
}
.setup-guide ol {
  margin: 0.5rem 0 0;
  padding-left: 1.2rem;
  color: var(--color-secondary);
  font-size: 0.85rem;
  display: flex;
  flex-direction: column;
  gap: 0.35rem;
}
code {
  font-size: 0.8rem;
}
</style>
