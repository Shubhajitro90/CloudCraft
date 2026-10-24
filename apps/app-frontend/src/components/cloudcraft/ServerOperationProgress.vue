<script setup lang="ts">
import { computed } from 'vue'

import {
  cancelButtonLabel,
  type OperationSnapshot,
  progressFraction,
  progressLabel,
} from '@/helpers/cloudcraftOperations'

const props = defineProps<{ snapshot: OperationSnapshot; cancelError: string }>()
defineEmits<{ cancel: [] }>()

const fraction = computed(() => progressFraction(props.snapshot))
const percent = computed(() => (fraction.value === null ? null : Math.round(fraction.value * 100)))
</script>

<template>
  <div class="operation" role="status" aria-live="polite">
    <div class="operation-line">
      <strong>{{ progressLabel(snapshot) }}</strong>
      <button
        type="button"
        :disabled="!snapshot.cancellable || snapshot.cancelRequested"
        @click="$emit('cancel')"
      >
        {{ cancelButtonLabel(snapshot) }}
      </button>
    </div>
    <progress
      v-if="percent !== null"
      :value="percent"
      max="100"
      :aria-label="`${snapshot.phase}: ${percent}%`"
    />
    <progress v-else :aria-label="`${snapshot.phase}: working`" />
    <small v-if="percent !== null">{{ percent }}%</small>
    <small v-if="snapshot.current" class="operation-current">{{ snapshot.current }}</small>
    <small v-if="!snapshot.cancellable && !snapshot.cancelRequested">
      This step cannot be cancelled because stopping now could leave data in an unsafe state.
    </small>
    <p v-if="cancelError" class="operation-error" role="alert">{{ cancelError }}</p>
  </div>
</template>

<style scoped>
.operation {
  display: flex;
  flex-direction: column;
  gap: 0.35rem;
  padding: 0.75rem;
  border: 1px solid var(--color-button-bg);
  border-radius: 0.75rem;
}
.operation-line {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 0.75rem;
  flex-wrap: wrap;
}
.operation progress {
  width: 100%;
  height: 0.6rem;
  accent-color: var(--color-brand);
}
.operation small {
  color: var(--color-secondary);
  font-size: 0.8rem;
}
.operation-current {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.operation button {
  padding: 0.35rem 0.8rem;
  border: 1px solid var(--color-button-bg);
  border-radius: 0.6rem;
  background: var(--color-button-bg);
  color: var(--color-base);
  cursor: pointer;
}
.operation button:focus-visible {
  outline: 2px solid var(--color-brand);
  outline-offset: 2px;
}
.operation button:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}
.operation .operation-error {
  margin: 0;
  color: var(--color-red);
  font-size: 0.85rem;
}
</style>
