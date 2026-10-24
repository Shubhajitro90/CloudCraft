<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'

import { deletionNotice, formatBytes } from '@/helpers/cloudcraftOperations'
import {
  createServerBackup,
  deleteServerResource,
  listServerBackups,
  restoreServerBackup,
  type ServerBackupSummary,
  verifyServerBackup,
} from '@/helpers/cloudcraftServer'
import { useServerOperation } from '@/helpers/useServerOperation'

import ServerDeleteConfirm from './ServerDeleteConfirm.vue'
import ServerOperationProgress from './ServerOperationProgress.vue'

const props = defineProps<{ serverDir: string; serverActive: boolean }>()

const operation = useServerOperation()
const backups = ref<ServerBackupSummary[]>([])
const activeId = ref<string | null>(null)
const pendingDelete = ref<ServerBackupSummary | null>(null)
const error = ref('')
const message = ref('')

const busy = computed(() => operation.running.value)
const showProgress = computed(
  () =>
    operation.running.value &&
    operation.snapshot.value !== null &&
    operation.snapshot.value.kind !== 'delete',
)
const deletingBackup = computed(
  () => operation.running.value && operation.snapshot.value?.kind === 'delete',
)

function reset() {
  error.value = ''
  message.value = ''
}

async function refresh() {
  if (!props.serverDir.trim()) {
    backups.value = []
    return
  }
  try {
    backups.value = await listServerBackups(props.serverDir.trim())
  } catch (cause) {
    backups.value = []
    error.value = cause instanceof Error ? cause.message : String(cause)
  }
}

async function create() {
  reset()
  const result = await operation.run('backup', () => createServerBackup(props.serverDir.trim()))
  if (result.ok) {
    message.value = `Backup ${result.value.id} created: ${result.value.fileCount} files copied and recorded with SHA-256 hashes.`
  } else if (result.outcome.kind === 'cancelled') {
    message.value = result.outcome.text
  } else {
    error.value = result.outcome.text
  }
  await refresh()
}

async function verify(backup: ServerBackupSummary) {
  reset()
  activeId.value = backup.id
  const result = await operation.run('verify', () =>
    verifyServerBackup(props.serverDir.trim(), backup.id),
  )
  activeId.value = null
  if (!result.ok) {
    if (result.outcome.kind === 'cancelled') {
      message.value =
        'Verification cancelled. The backup was not changed and no result was recorded.'
    } else {
      error.value = result.outcome.text
    }
    return
  }
  const report = result.value
  if (report.ok) message.value = `Backup ${backup.id} passed integrity verification.`
  else error.value = `Backup ${backup.id} failed: ${report.problems.slice(0, 3).join('; ')}`
}

async function restore(backup: ServerBackupSummary) {
  const confirmed = window.confirm(
    `Restore backup ${backup.id} into a NEW folder next to your server files?\n\n` +
      'Your current world will not be changed or deleted. To use the restored copy, set ' +
      'level-name in server.properties to the new folder name.',
  )
  if (!confirmed) return
  reset()
  activeId.value = backup.id
  const result = await operation.run('restore', () =>
    restoreServerBackup(props.serverDir.trim(), backup.id),
  )
  activeId.value = null
  if (result.ok) {
    message.value = `Restored to the new folder "${result.value}". The current world was not modified.`
  } else if (result.outcome.kind === 'cancelled') {
    message.value = `Restore cancelled. No partial world was created and the current world was not modified.`
  } else {
    error.value = result.outcome.text
  }
}

async function confirmDeleteBackup(typed: string) {
  const backup = pendingDelete.value
  if (!backup) return
  reset()
  const result = await operation.run('delete', () =>
    deleteServerResource(props.serverDir.trim(), { kind: 'backup', id: backup.id }, typed),
  )
  if (result.ok) {
    const notice = deletionNotice(result.value)
    if (notice.ok) message.value = notice.text
    else error.value = notice.text
    pendingDelete.value = null
  } else {
    error.value = result.outcome.text
  }
  await refresh()
}

onMounted(() => operation.attach())
watch(() => props.serverDir, refresh, { immediate: true })
watch(
  () => props.serverActive,
  (active, wasActive) => {
    if (wasActive && !active) refresh()
  },
)
</script>

<template>
  <section class="backups" aria-labelledby="server-backups-title">
    <div class="backups-header">
      <div>
        <h3 id="server-backups-title">World backups</h3>
        <p>
          Local snapshots with a SHA-256 manifest, stored in
          <code>cloudcraft-backups</code> inside the server folder. These are not cloud backups.
        </p>
      </div>
      <button type="button" :disabled="busy || serverActive || !serverDir.trim()" @click="create">
        {{ busy && operation.snapshot.value?.kind === 'backup' ? 'Backing up…' : 'Back up world' }}
      </button>
    </div>
    <p v-if="serverActive" class="backups-note" role="status">
      Stop the server before creating or restoring a backup so the world is consistent.
    </p>
    <ServerOperationProgress
      v-if="showProgress && operation.snapshot.value"
      :snapshot="operation.snapshot.value"
      :cancel-error="operation.cancelError.value"
      @cancel="operation.cancel"
    />
    <p v-if="message" class="backups-ok" role="status">{{ message }}</p>
    <p v-if="error" class="backups-error" role="alert">{{ error }}</p>
    <p v-if="!backups.length && !error" class="backups-empty">No backups yet.</p>
    <ul v-else class="backups-list">
      <li v-for="backup in backups" :key="backup.id">
        <div>
          <strong>{{ new Date(backup.createdAt).toLocaleString() }}</strong>
          <small>
            {{ backup.levelName }} · {{ backup.fileCount }} files ·
            {{ formatBytes(backup.totalBytes) }}
          </small>
        </div>
        <div class="backups-actions">
          <button type="button" :disabled="busy" @click="verify(backup)">
            {{ activeId === backup.id && busy ? 'Working…' : 'Verify' }}
          </button>
          <button type="button" :disabled="busy || serverActive" @click="restore(backup)">
            Restore to new folder
          </button>
          <button
            type="button"
            :disabled="busy || serverActive"
            :aria-label="`Delete backup from ${new Date(backup.createdAt).toLocaleString()}`"
            @click="pendingDelete = backup"
          >
            Delete…
          </button>
        </div>
      </li>
    </ul>
    <ServerDeleteConfirm
      v-if="pendingDelete"
      title="Delete this backup"
      :expected-name="pendingDelete.id"
      :removes="[
        `Backup ${pendingDelete.id} (${pendingDelete.fileCount} files, ${formatBytes(pendingDelete.totalBytes)})`,
      ]"
      :retains="['Your current world', 'All other backups']"
      warning="This cannot be undone."
      confirm-label="Delete backup"
      :busy="deletingBackup"
      @confirm="confirmDeleteBackup"
      @cancel="pendingDelete = null"
    />
  </section>
</template>

<style scoped>
.backups {
  display: flex;
  flex-direction: column;
  gap: 0.75rem;
  padding: 1rem;
  border: 1px solid var(--color-button-bg);
  border-radius: 1rem;
  background: var(--color-bg);
}
.backups-header,
.backups-list li {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 1rem;
  flex-wrap: wrap;
}
.backups h3 {
  margin: 0;
  font-size: 1rem;
}
.backups p,
.backups small {
  margin: 0.25rem 0 0;
  color: var(--color-secondary);
  font-size: 0.85rem;
}
.backups-list {
  display: flex;
  flex-direction: column;
  gap: 0.5rem;
  margin: 0;
  padding: 0;
  list-style: none;
}
.backups-list li {
  padding: 0.6rem 0.75rem;
  border: 1px solid var(--color-button-bg);
  border-radius: 0.75rem;
}
.backups-list small {
  display: block;
}
.backups-actions {
  display: flex;
  gap: 0.5rem;
}
.backups button {
  padding: 0.45rem 0.9rem;
  border: 1px solid var(--color-button-bg);
  border-radius: 0.6rem;
  background: var(--color-button-bg);
  color: var(--color-base);
  cursor: pointer;
}
.backups button:focus-visible {
  outline: 2px solid var(--color-brand);
  outline-offset: 2px;
}
.backups button:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}
.backups .backups-ok {
  color: var(--color-green);
}
.backups .backups-error {
  color: var(--color-red);
}
</style>
