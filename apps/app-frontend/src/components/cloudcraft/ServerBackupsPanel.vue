<script setup lang="ts">
import { ref, watch } from 'vue'
import {
  createServerBackup,
  listServerBackups,
  restoreServerBackup,
  verifyServerBackup,
  type ServerBackupSummary,
} from '@/helpers/cloudcraftServer'

const props = defineProps<{ serverDir: string; serverActive: boolean }>()

const backups = ref<ServerBackupSummary[]>([])
const busyId = ref<string | null>(null)
const creating = ref(false)
const error = ref('')
const message = ref('')

const errorText = (cause: unknown) => (cause instanceof Error ? cause.message : String(cause))

function formatSize(bytes: number) {
  if (bytes < 1024 * 1024) return `${Math.max(1, Math.round(bytes / 1024))} KB`
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`
}

async function refresh() {
  error.value = ''
  if (!props.serverDir.trim()) {
    backups.value = []
    return
  }
  try {
    backups.value = await listServerBackups(props.serverDir.trim())
  } catch (cause) {
    backups.value = []
    error.value = errorText(cause)
  }
}

async function create() {
  creating.value = true
  error.value = ''
  message.value = ''
  try {
    const backup = await createServerBackup(props.serverDir.trim())
    message.value = `Backup ${backup.id} created with ${backup.fileCount} verified files.`
    await refresh()
  } catch (cause) {
    error.value = errorText(cause)
  } finally {
    creating.value = false
  }
}

async function verify(backup: ServerBackupSummary) {
  busyId.value = backup.id
  error.value = ''
  message.value = ''
  try {
    const report = await verifyServerBackup(props.serverDir.trim(), backup.id)
    if (report.ok) message.value = `Backup ${backup.id} passed integrity verification.`
    else error.value = `Backup ${backup.id} failed: ${report.problems.slice(0, 3).join('; ')}`
  } catch (cause) {
    error.value = errorText(cause)
  } finally {
    busyId.value = null
  }
}

async function restore(backup: ServerBackupSummary) {
  const confirmed = window.confirm(
    `Restore backup ${backup.id} into a NEW folder next to your server files?\n\n` +
      'Your current world will not be changed or deleted. To use the restored copy, set ' +
      'level-name in server.properties to the new folder name.',
  )
  if (!confirmed) return
  busyId.value = backup.id
  error.value = ''
  message.value = ''
  try {
    const folder = await restoreServerBackup(props.serverDir.trim(), backup.id)
    message.value = `Restored to the new folder "${folder}". The current world was not modified.`
  } catch (cause) {
    error.value = errorText(cause)
  } finally {
    busyId.value = null
  }
}

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
      <button
        type="button"
        :disabled="creating || serverActive || !serverDir.trim()"
        @click="create"
      >
        {{ creating ? 'Creating…' : 'Back up world' }}
      </button>
    </div>
    <p v-if="serverActive" class="backups-note" role="status">
      Stop the server before creating or restoring a backup so the world is consistent.
    </p>
    <p v-if="message" class="backups-ok" role="status">{{ message }}</p>
    <p v-if="error" class="backups-error" role="alert">{{ error }}</p>
    <p v-if="!backups.length && !error" class="backups-empty">No backups yet.</p>
    <ul v-else class="backups-list">
      <li v-for="backup in backups" :key="backup.id">
        <div>
          <strong>{{ new Date(backup.createdAt).toLocaleString() }}</strong>
          <small>
            {{ backup.levelName }} · {{ backup.fileCount }} files ·
            {{ formatSize(backup.totalBytes) }}
          </small>
        </div>
        <div class="backups-actions">
          <button type="button" :disabled="busyId === backup.id" @click="verify(backup)">
            Verify
          </button>
          <button
            type="button"
            :disabled="busyId === backup.id || serverActive"
            @click="restore(backup)"
          >
            Restore to new folder
          </button>
        </div>
      </li>
    </ul>
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
