<script setup lang="ts">
import { computed, ref, watch } from 'vue'

import { formatBytes } from '@/helpers/cloudcraftOperations'
import {
  downloadServerSoftware,
  listServerSoftwareVersions,
  type DownloadedSoftware,
  type ServerSoftwareKind,
} from '@/helpers/cloudcraftServer'
import { useServerOperation } from '@/helpers/useServerOperation'

import ServerOperationProgress from './ServerOperationProgress.vue'

const props = defineProps<{ serverDir: string; disabled: boolean }>()
const emit = defineEmits<{ downloaded: [result: DownloadedSoftware, version: string] }>()

const operation = useServerOperation()
const kind = ref<ServerSoftwareKind>('vanilla')
const versions = ref<string[]>([])
const version = ref('')
const loadingVersions = ref(false)
const error = ref('')
const message = ref('')

const busy = computed(() => operation.running.value)
const downloading = computed(() => busy.value && operation.snapshot.value?.kind === 'download')

async function loadVersions() {
  loadingVersions.value = true
  error.value = ''
  versions.value = []
  version.value = ''
  try {
    versions.value = await listServerSoftwareVersions(kind.value)
    version.value = versions.value[0] ?? ''
  } catch (cause) {
    error.value = cause instanceof Error ? cause.message : String(cause)
  } finally {
    loadingVersions.value = false
  }
}

async function download() {
  if (!props.serverDir.trim() || !version.value) return
  error.value = ''
  message.value = ''
  const chosenVersion = version.value
  const result = await operation.run('download', () =>
    downloadServerSoftware(props.serverDir.trim(), kind.value, chosenVersion),
  )
  if (result.ok) {
    const algorithm = result.value.algorithm.toUpperCase()
    message.value = result.value.reused
      ? `${result.value.fileName} was already present and matched the official ${algorithm} checksum.`
      : `${result.value.fileName} downloaded (${formatBytes(result.value.bytes)}) and verified against the official ${algorithm} checksum.`
    emit('downloaded', result.value, chosenVersion)
  } else if (result.outcome.kind === 'cancelled') {
    message.value = 'Download cancelled. No partial file was kept.'
  } else {
    error.value = result.outcome.text
  }
}

watch(kind, loadVersions, { immediate: true })
</script>

<template>
  <section class="software" aria-labelledby="server-software-title">
    <div>
      <h3 id="server-software-title">Server software</h3>
      <p>
        Download the official Vanilla (Mojang, SHA-1) or Paper (PaperMC, SHA-256) server. Every file
        is checked against the publisher&apos;s checksum before it appears in your server folder.
      </p>
    </div>
    <div class="software-controls">
      <label>
        <span>Software</span>
        <select v-model="kind" :disabled="busy || disabled">
          <option value="vanilla">Vanilla</option>
          <option value="paper">Paper</option>
        </select>
      </label>
      <label>
        <span>Version</span>
        <select
          v-model="version"
          :disabled="busy || disabled || loadingVersions || !versions.length"
        >
          <option v-for="entry in versions" :key="entry" :value="entry">{{ entry }}</option>
        </select>
      </label>
      <button
        type="button"
        :disabled="busy || disabled || !serverDir.trim() || !version"
        @click="download"
      >
        {{ downloading ? 'Downloading…' : 'Download and verify' }}
      </button>
      <button type="button" :disabled="loadingVersions" @click="loadVersions">
        {{ loadingVersions ? 'Loading…' : 'Reload versions' }}
      </button>
    </div>
    <p v-if="!serverDir.trim()" class="software-note">Choose a server directory first.</p>
    <ServerOperationProgress
      v-if="downloading && operation.snapshot.value"
      :snapshot="operation.snapshot.value"
      :cancel-error="operation.cancelError.value"
      @cancel="operation.cancel"
    />
    <p v-if="message" class="software-ok" role="status">{{ message }}</p>
    <p v-if="error" class="software-error" role="alert">{{ error }}</p>
  </section>
</template>

<style scoped>
.software {
  display: flex;
  flex-direction: column;
  gap: 0.8rem;
  padding: 1rem;
  border: 1px solid var(--color-button-bg);
  border-radius: 0.9rem;
  background: var(--color-bg);
}
.software h3 {
  margin: 0;
  font-size: 1rem;
}
.software p {
  margin: 0.3rem 0 0;
  color: var(--color-secondary);
  font-size: 0.82rem;
}
.software-controls {
  display: flex;
  flex-wrap: wrap;
  align-items: flex-end;
  gap: 0.6rem;
}
.software-controls label {
  display: flex;
  flex-direction: column;
  gap: 0.3rem;
  font-size: 0.78rem;
  font-weight: 700;
}
.software-controls select {
  min-width: 9rem;
  padding: 0.6rem 0.7rem;
  border: 1px solid var(--color-button-bg);
  border-radius: 0.6rem;
  background: var(--color-bg-raised);
  color: var(--color-contrast);
}
.software-controls button {
  padding: 0.65rem 0.9rem;
  border: 1px solid var(--color-button-bg);
  border-radius: 0.7rem;
  background: var(--color-bg-raised);
  color: var(--color-contrast);
  font-size: 0.8rem;
  font-weight: 700;
  cursor: pointer;
}
.software-controls button:disabled,
.software-controls select:disabled {
  opacity: 0.45;
  cursor: not-allowed;
}
.software-controls :focus-visible {
  outline: 2px solid #a78bfa;
  outline-offset: 2px;
}
.software .software-ok {
  color: #86efac;
}
.software .software-error {
  padding: 0.7rem 0.9rem;
  border: 1px solid rgba(248, 113, 113, 0.3);
  border-radius: 0.7rem;
  background: rgba(185, 28, 28, 0.1);
  color: #fca5a5;
  overflow-wrap: anywhere;
}
</style>
