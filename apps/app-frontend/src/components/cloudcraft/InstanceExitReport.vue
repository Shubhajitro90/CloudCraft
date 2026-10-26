<script setup>
import { computed, ref } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { describeExit } from '@/helpers/cloudcraftDiagnostics'

const props = defineProps({
  exit: { type: Object, required: true },
  crash: { type: Object, default: null },
})
const emit = defineEmits(['repair', 'dismiss', 'logs'])

const summary = computed(() => describeExit(props.exit))
const showReport = ref(false)
const folderError = ref('')

async function openFolder(child) {
  folderError.value = ''
  try {
    await invoke('cloudcraft_instance_open_folder', { profilePath: props.exit.profile_path, child })
  } catch (err) {
    folderError.value = String(err?.message || err)
  }
}
</script>

<template>
  <div class="exit" :class="`exit-${summary.kind}`" role="status">
    <div class="exit-heading">
      <strong>{{ summary.title }}</strong>
      <button class="link" type="button" aria-label="Dismiss" @click="emit('dismiss')">
        Dismiss
      </button>
    </div>
    <p>{{ summary.detail }}</p>
    <template v-if="summary.kind === 'crashed'">
      <p v-if="crash" class="small">
        Crash report: <code>{{ crash.fileName }}</code> ({{
          crash.kind === 'jvm' ? 'Java runtime crash' : 'Minecraft crash report'
        }})
      </p>
      <p v-else class="small">No crash report was written; the game log may explain the failure.</p>
      <div class="actions">
        <button v-if="crash" class="button" type="button" @click="showReport = !showReport">
          {{ showReport ? 'Hide report' : 'View report' }}
        </button>
        <button
          class="button"
          type="button"
          @click="openFolder(crash?.kind === 'minecraft' ? 'crash-reports' : undefined)"
        >
          Open folder
        </button>
        <button class="button" type="button" @click="emit('logs')">Game logs</button>
        <button class="button" type="button" @click="emit('repair')">Repair instance</button>
      </div>
      <pre v-if="showReport && crash" class="report"
        >{{ crash.excerpt
        }}{{ crash.truncated ? '\n… (truncated; open the folder for the full file)' : '' }}</pre
      >
      <p v-if="folderError" class="small error">{{ folderError }}</p>
    </template>
  </div>
</template>

<style scoped>
.exit {
  display: flex;
  flex-direction: column;
  gap: 0.4rem;
  width: 100%;
  padding: 0.7rem 0.85rem;
  border-radius: 0.75rem;
  border: 1px solid var(--color-button-bg);
  font-size: 0.85rem;
}
.exit p {
  margin: 0;
}
.exit-crashed {
  border-color: #7f1d1d;
  background: rgba(127, 29, 29, 0.18);
}
.exit-clean {
  border-color: #14532d;
}
.exit-heading {
  display: flex;
  justify-content: space-between;
  align-items: center;
}
.small {
  font-size: 0.8rem;
  color: var(--color-secondary);
}
.error {
  color: #f87171;
}
.actions {
  display: flex;
  flex-wrap: wrap;
  gap: 0.4rem;
}
.button {
  padding: 0.35rem 0.75rem;
  border: 1px solid var(--color-button-bg);
  border-radius: 0.6rem;
  background: var(--color-raised-bg);
  color: var(--color-base);
  font-weight: 600;
  cursor: pointer;
}
.link {
  background: none;
  border: none;
  color: var(--color-secondary);
  cursor: pointer;
  font-size: 0.8rem;
}
.report {
  max-height: 16rem;
  overflow: auto;
  margin: 0;
  padding: 0.6rem;
  border-radius: 0.5rem;
  background: #0b0b10;
  color: #d4d4d8;
  font-size: 0.72rem;
  white-space: pre-wrap;
  word-break: break-word;
}
</style>
