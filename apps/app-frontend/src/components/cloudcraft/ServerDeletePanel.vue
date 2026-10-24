<script setup lang="ts">
import { computed, ref, watch } from 'vue'

import { deletionNotice, formatBytes } from '@/helpers/cloudcraftOperations'
import {
  deleteServerResource,
  getServerDeletionPlan,
  type ServerDeletionPlan,
} from '@/helpers/cloudcraftServer'
import { useServerOperation } from '@/helpers/useServerOperation'

import ServerDeleteConfirm from './ServerDeleteConfirm.vue'
import ServerOperationProgress from './ServerOperationProgress.vue'

const props = defineProps<{ serverDir: string; serverActive: boolean }>()

const operation = useServerOperation()
const plan = ref<ServerDeletionPlan | null>(null)
const planNote = ref('')
const pending = ref<'configuration' | 'world' | null>(null)
const notice = ref<{ ok: boolean; text: string } | null>(null)

const deleting = computed(
  () => operation.running.value && operation.snapshot.value?.kind === 'delete',
)
const busy = computed(() => operation.running.value)
const backupSummary = computed(() => {
  const backups = plan.value?.backups ?? []
  if (!backups.length) return 'No backups exist for this server'
  return `${backups.length} backup${backups.length === 1 ? '' : 's'} (${formatBytes(plan.value?.backupBytes ?? 0)})`
})

async function loadPlan() {
  if (!props.serverDir.trim()) {
    plan.value = null
    planNote.value = ''
    return
  }
  try {
    plan.value = await getServerDeletionPlan(props.serverDir.trim())
    planNote.value = ''
  } catch (cause) {
    plan.value = null
    pending.value = null
    planNote.value = cause instanceof Error ? cause.message : String(cause)
  }
}

const configurationRetains = computed(() => [
  ...(plan.value?.configurationRetains ?? []),
  backupSummary.value,
])

const worldRemoves = computed(() => [
  `World folder "${plan.value?.levelName}" with ${plan.value?.worldFiles ?? 0} files (${formatBytes(plan.value?.worldBytes ?? 0)})`,
])
const worldRetains = computed(() => [
  backupSummary.value,
  'CloudCraft configuration, server JAR and server.properties',
])
const worldWarning = computed(() =>
  plan.value?.backups.length
    ? 'This cannot be undone. Your backups are kept and can be restored into a new folder.'
    : 'This cannot be undone and no backup exists. Create and verify a backup first if you may want this world again.',
)

async function confirmDelete(typed: string) {
  const target = pending.value
  if (!target) return
  notice.value = null
  const result = await operation.run('delete', () =>
    deleteServerResource(props.serverDir.trim(), { kind: target }, typed),
  )
  if (result.ok) {
    notice.value = deletionNotice(result.value)
    pending.value = null
  } else {
    notice.value = { ok: false, text: result.outcome.text }
  }
  await loadPlan()
}

watch(() => props.serverDir, loadPlan, { immediate: true })
watch(
  () => props.serverActive,
  () => {
    pending.value = null
    void loadPlan()
  },
)
watch(operation.revision, loadPlan)
</script>

<template>
  <section class="delete-panel" aria-labelledby="server-delete-title">
    <div>
      <h3 id="server-delete-title">Remove server data</h3>
      <p>
        Stopping a server only ends its process. Removing data deletes files from disk and is a
        separate, confirmed action. CloudCraft only ever deletes the items listed below and never
        the server folder itself.
      </p>
    </div>

    <p v-if="planNote" class="delete-note" role="status">{{ planNote }}</p>
    <template v-else-if="plan">
      <p v-if="plan.blockedReason" class="delete-note" role="status">{{ plan.blockedReason }}</p>
      <div class="delete-actions">
        <button
          type="button"
          :disabled="!!plan.blockedReason || busy"
          @click="pending = 'configuration'"
        >
          Remove server configuration…
        </button>
        <button
          type="button"
          :disabled="!!plan.blockedReason || busy || !plan.worldExists"
          @click="pending = 'world'"
        >
          Delete world…
        </button>
      </div>
      <p v-if="!plan.worldExists && plan.worldBlocker" class="delete-note">
        World deletion is unavailable: {{ plan.worldBlocker }}
      </p>
      <p v-else-if="plan.worldExists" class="delete-hint">
        Configuration removal keeps your world and backups. World deletion keeps your configuration
        and backups. Individual backups are deleted from the backup list above.
      </p>

      <ServerDeleteConfirm
        v-if="pending === 'configuration'"
        title="Remove the CloudCraft configuration"
        :expected-name="plan.serverName"
        :removes="plan.configurationRemoves"
        :retains="configurationRetains"
        confirm-label="Remove configuration"
        :busy="deleting"
        @confirm="confirmDelete"
        @cancel="pending = null"
      />
      <ServerDeleteConfirm
        v-else-if="pending === 'world' && plan.levelName"
        title="Delete the world folder"
        :expected-name="plan.levelName"
        :removes="worldRemoves"
        :retains="worldRetains"
        :warning="worldWarning"
        confirm-label="Delete world permanently"
        :busy="deleting"
        @confirm="confirmDelete"
        @cancel="pending = null"
      />
    </template>

    <ServerOperationProgress
      v-if="deleting && operation.snapshot.value"
      :snapshot="operation.snapshot.value"
      :cancel-error="operation.cancelError.value"
      @cancel="operation.cancel"
    />
    <p
      v-if="notice"
      :class="notice.ok ? 'delete-ok' : 'delete-error'"
      :role="notice.ok ? 'status' : 'alert'"
    >
      {{ notice.text }}
    </p>
  </section>
</template>

<style scoped>
.delete-panel {
  display: flex;
  flex-direction: column;
  gap: 0.75rem;
  padding: 1rem;
  border: 1px solid var(--color-button-bg);
  border-radius: 1rem;
  background: var(--color-bg);
}
.delete-panel h3 {
  margin: 0;
  font-size: 1rem;
}
.delete-panel p {
  margin: 0.25rem 0 0;
  color: var(--color-secondary);
  font-size: 0.85rem;
}
.delete-actions {
  display: flex;
  gap: 0.5rem;
  flex-wrap: wrap;
}
.delete-panel button {
  padding: 0.45rem 0.9rem;
  border: 1px solid var(--color-button-bg);
  border-radius: 0.6rem;
  background: var(--color-button-bg);
  color: var(--color-base);
  cursor: pointer;
}
.delete-panel button:focus-visible {
  outline: 2px solid var(--color-brand);
  outline-offset: 2px;
}
.delete-panel button:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}
.delete-panel .delete-ok {
  color: var(--color-green);
}
.delete-panel .delete-error {
  color: var(--color-red);
}
</style>
