<script setup lang="ts">
import { computed, ref } from 'vue'
import { open as openDialog, save as saveDialog } from '@tauri-apps/plugin-dialog'

import { formatBytes } from '@/helpers/cloudcraftOperations'
import {
  activateServerWorld,
  exportServerWorld,
  importServerWorld,
} from '@/helpers/cloudcraftServer'
import { useServerOperation } from '@/helpers/useServerOperation'

import ServerOperationProgress from './ServerOperationProgress.vue'

const props = defineProps<{ serverDir: string; serverActive: boolean }>()

const operation = useServerOperation()
const importSource = ref('')
const importName = ref('')
const activateFolder = ref('')
const error = ref('')
const message = ref('')
const activating = ref(false)

const busy = computed(() => operation.running.value || activating.value)
const blocked = computed(() => busy.value || props.serverActive || !props.serverDir.trim())
const showProgress = computed(
  () =>
    operation.running.value &&
    operation.snapshot.value !== null &&
    ['import', 'export'].includes(operation.snapshot.value.kind),
)

function reset() {
  error.value = ''
  message.value = ''
}

function describe(cause: unknown) {
  return cause instanceof Error ? cause.message : String(cause)
}

async function chooseImportZip() {
  const selection = await openDialog({
    directory: false,
    multiple: false,
    title: 'Choose a world .zip archive',
    filters: [{ name: 'World archive', extensions: ['zip'] }],
  })
  if (typeof selection === 'string') importSource.value = selection
}

async function chooseImportFolder() {
  const selection = await openDialog({
    directory: true,
    multiple: false,
    title: 'Choose a world folder',
  })
  if (typeof selection === 'string') importSource.value = selection
}

async function exportWorld() {
  reset()
  const destination = await saveDialog({
    title: 'Export the active world as a .zip',
    defaultPath: 'world-export.zip',
    filters: [{ name: 'World archive', extensions: ['zip'] }],
  })
  if (typeof destination !== 'string') return
  const result = await operation.run('export', () =>
    exportServerWorld(props.serverDir.trim(), destination),
  )
  if (result.ok) {
    message.value = `Exported "${result.value.levelName}": ${result.value.files} files (${formatBytes(result.value.bytes)}) into an archive of ${formatBytes(result.value.archiveBytes)}, verified after writing.`
  } else if (result.outcome.kind === 'cancelled') {
    message.value = 'Export cancelled. No partial archive was kept.'
  } else {
    error.value = result.outcome.text
  }
}

async function importWorld() {
  reset()
  const name = importName.value.trim()
  const result = await operation.run('import', () =>
    importServerWorld(props.serverDir.trim(), importSource.value.trim(), name),
  )
  if (result.ok) {
    activateFolder.value = result.value.folder
    message.value = `Imported into "${result.value.folder}": ${result.value.files} files (${formatBytes(result.value.bytes)}). The active world was not changed.`
    if (result.value.skippedEntries > 0) {
      message.value += ` ${result.value.skippedEntries} unneeded archive entries were skipped.`
    }
    importSource.value = ''
  } else if (result.outcome.kind === 'cancelled') {
    message.value = 'Import cancelled. No partial world was created.'
  } else {
    error.value = result.outcome.text
  }
}

async function activateWorld() {
  const folder = activateFolder.value.trim()
  if (!folder) return
  reset()
  activating.value = true
  try {
    const previous = await activateServerWorld(props.serverDir.trim(), folder)
    message.value = `"${folder}" is now the active world (previously "${previous}"). Switch back by activating "${previous}".`
  } catch (cause) {
    error.value = describe(cause)
  } finally {
    activating.value = false
  }
}
</script>

<template>
  <section class="worlds" aria-labelledby="server-worlds-title">
    <div>
      <h3 id="server-worlds-title">World import, export and activation</h3>
      <p>
        Archives are validated before extraction: path traversal, absolute paths, symlinks and
        oversized or corrupt entries are rejected, and nothing replaces an existing world.
      </p>
    </div>
    <p v-if="serverActive" class="worlds-note" role="status">
      Stop the server before importing, exporting or switching worlds.
    </p>

    <div class="worlds-row">
      <button type="button" :disabled="blocked" @click="exportWorld">
        Export active world (.zip)
      </button>
    </div>

    <form class="worlds-form" @submit.prevent="importWorld">
      <div class="worlds-path">
        <input
          v-model="importSource"
          :disabled="blocked"
          required
          autocomplete="off"
          aria-label="World archive or folder to import"
          placeholder="World .zip or folder to import"
        />
        <button type="button" :disabled="blocked" @click="chooseImportZip">Zip…</button>
        <button type="button" :disabled="blocked" @click="chooseImportFolder">Folder…</button>
      </div>
      <div class="worlds-path">
        <input
          v-model="importName"
          :disabled="blocked"
          required
          maxlength="64"
          autocomplete="off"
          aria-label="Name for the imported world folder"
          placeholder="New world folder name"
        />
        <button type="submit" :disabled="blocked || !importSource.trim() || !importName.trim()">
          Import as new world
        </button>
      </div>
    </form>

    <form class="worlds-path" @submit.prevent="activateWorld">
      <input
        v-model="activateFolder"
        :disabled="blocked"
        required
        autocomplete="off"
        aria-label="World folder to activate"
        placeholder="World folder to make active (sets level-name)"
      />
      <button type="submit" :disabled="blocked || !activateFolder.trim()">
        {{ activating ? 'Switching…' : 'Make active' }}
      </button>
    </form>

    <ServerOperationProgress
      v-if="showProgress && operation.snapshot.value"
      :snapshot="operation.snapshot.value"
      :cancel-error="operation.cancelError.value"
      @cancel="operation.cancel"
    />
    <p v-if="message" class="worlds-ok" role="status">{{ message }}</p>
    <p v-if="error" class="worlds-error" role="alert">{{ error }}</p>
  </section>
</template>

<style scoped>
.worlds {
  display: flex;
  flex-direction: column;
  gap: 0.8rem;
  padding: 1rem;
  border: 1px solid var(--color-button-bg);
  border-radius: 0.9rem;
  background: var(--color-bg);
}
.worlds h3 {
  margin: 0;
  font-size: 1rem;
}
.worlds p {
  margin: 0.3rem 0 0;
  color: var(--color-secondary);
  font-size: 0.82rem;
}
.worlds-form {
  display: flex;
  flex-direction: column;
  gap: 0.6rem;
}
.worlds-path,
.worlds-row {
  display: flex;
  flex-wrap: wrap;
  gap: 0.5rem;
}
.worlds-path input {
  flex: 1 1 14rem;
  min-width: 0;
  padding: 0.65rem 0.8rem;
  border: 1px solid var(--color-button-bg);
  border-radius: 0.7rem;
  background: var(--color-bg-raised);
  color: var(--color-contrast);
}
.worlds button {
  padding: 0.65rem 0.9rem;
  border: 1px solid var(--color-button-bg);
  border-radius: 0.7rem;
  background: var(--color-bg-raised);
  color: var(--color-contrast);
  font-size: 0.8rem;
  font-weight: 700;
  cursor: pointer;
}
.worlds button:disabled,
.worlds input:disabled {
  opacity: 0.45;
  cursor: not-allowed;
}
.worlds :focus-visible {
  outline: 2px solid #a78bfa;
  outline-offset: 2px;
}
.worlds .worlds-ok {
  color: #86efac;
}
.worlds .worlds-error {
  padding: 0.7rem 0.9rem;
  border: 1px solid rgba(248, 113, 113, 0.3);
  border-radius: 0.7rem;
  background: rgba(185, 28, 28, 0.1);
  color: #fca5a5;
  overflow-wrap: anywhere;
}
</style>
