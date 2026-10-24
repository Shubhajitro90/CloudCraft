<script setup lang="ts">
import { computed, onMounted, ref, useId } from 'vue'

import { confirmationMatches } from '@/helpers/cloudcraftOperations'

const props = defineProps<{
  title: string
  expectedName: string
  removes: string[]
  retains: string[]
  warning?: string
  confirmLabel: string
  busy: boolean
}>()
const emit = defineEmits<{ confirm: [typed: string]; cancel: [] }>()

const typed = ref('')
const input = ref<HTMLInputElement | null>(null)
const titleId = useId()
const inputId = useId()
const matches = computed(() => confirmationMatches(props.expectedName, typed.value))

onMounted(() => input.value?.focus())

function submit() {
  if (matches.value && !props.busy) emit('confirm', typed.value)
}
</script>

<template>
  <form class="confirm" role="alertdialog" :aria-labelledby="titleId" @submit.prevent="submit">
    <h4 :id="titleId">{{ title }}</h4>
    <p v-if="warning" class="confirm-warning">{{ warning }}</p>
    <div class="confirm-columns">
      <div>
        <strong>Will be deleted</strong>
        <ul>
          <li v-for="item in removes" :key="item">{{ item }}</li>
        </ul>
      </div>
      <div>
        <strong>Will be kept</strong>
        <ul>
          <li v-for="item in retains" :key="item">{{ item }}</li>
        </ul>
      </div>
    </div>
    <label :for="inputId">
      Type <code>{{ expectedName }}</code> to confirm
    </label>
    <input
      :id="inputId"
      ref="input"
      v-model="typed"
      type="text"
      autocomplete="off"
      spellcheck="false"
      :disabled="busy"
    />
    <div class="confirm-actions">
      <button type="button" :disabled="busy" @click="emit('cancel')">Keep everything</button>
      <button type="submit" class="confirm-danger" :disabled="!matches || busy">
        {{ busy ? 'Deleting…' : confirmLabel }}
      </button>
    </div>
  </form>
</template>

<style scoped>
.confirm {
  display: flex;
  flex-direction: column;
  gap: 0.6rem;
  padding: 0.9rem;
  border: 1px solid var(--color-red);
  border-radius: 0.75rem;
}
.confirm h4,
.confirm p {
  margin: 0;
}
.confirm-warning {
  color: var(--color-red);
  font-size: 0.85rem;
}
.confirm-columns {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(14rem, 1fr));
  gap: 0.75rem;
  font-size: 0.85rem;
}
.confirm ul {
  margin: 0.25rem 0 0;
  padding-left: 1.1rem;
  color: var(--color-secondary);
}
.confirm input {
  padding: 0.45rem 0.6rem;
  border: 1px solid var(--color-button-bg);
  border-radius: 0.5rem;
  background: var(--color-bg);
  color: var(--color-base);
}
.confirm-actions {
  display: flex;
  gap: 0.5rem;
  justify-content: flex-end;
}
.confirm button {
  padding: 0.45rem 0.9rem;
  border: 1px solid var(--color-button-bg);
  border-radius: 0.6rem;
  background: var(--color-button-bg);
  color: var(--color-base);
  cursor: pointer;
}
.confirm button.confirm-danger {
  border-color: var(--color-red);
  background: var(--color-red);
  color: #fff;
}
.confirm button:focus-visible,
.confirm input:focus-visible {
  outline: 2px solid var(--color-brand);
  outline-offset: 2px;
}
.confirm button:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}
</style>
