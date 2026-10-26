<script setup>
import { computed, onMounted, reactive, ref } from 'vue'
import { edit as editProfile } from '@/helpers/profile'
import { get_max_memory } from '@/helpers/jre'
import { buildInstanceEdit, formatJvmArgs } from '@/helpers/cloudcraftDiagnostics'

const props = defineProps({
  instance: { type: Object, required: true },
  running: { type: Boolean, default: false },
})
const emit = defineEmits(['saved', 'close'])

const form = reactive({
  name: props.instance.name || '',
  memoryOverride: !!props.instance.memory?.maximum,
  maxMemoryMb: props.instance.memory?.maximum ? String(props.instance.memory.maximum) : '',
  javaPath: props.instance.java_path || '',
  jvmArgs: formatJvmArgs(props.instance.extra_launch_args),
})
const systemMaxMb = ref(null)
const saving = ref(false)
const error = ref('')
const result = computed(() => buildInstanceEdit(form, systemMaxMb.value))

onMounted(async () => {
  try {
    const kib = await get_max_memory()
    systemMaxMb.value = kib ? Math.floor(Number(kib) / 1024) : null
  } catch {
    systemMaxMb.value = null
  }
})

async function save() {
  if (!result.value.edit || saving.value || props.running) return
  saving.value = true
  error.value = ''
  try {
    await editProfile(props.instance.path, result.value.edit)
    emit('saved', result.value.edit)
  } catch (err) {
    error.value = `Could not save: ${String(err?.message || err)}`
  } finally {
    saving.value = false
  }
}
</script>

<template>
  <form
    class="editor"
    :aria-label="`Settings for ${instance.name || instance.path}`"
    @submit.prevent="save"
  >
    <label>
      Name
      <input v-model="form.name" maxlength="64" />
    </label>
    <label class="checkbox">
      <input v-model="form.memoryOverride" type="checkbox" />
      Use a custom memory limit for this instance
    </label>
    <label v-if="form.memoryOverride">
      Maximum memory (MB){{ systemMaxMb ? ` · this computer has ${systemMaxMb} MB` : '' }}
      <input v-model="form.maxMemoryMb" inputmode="numeric" placeholder="4096" />
    </label>
    <label>
      Java executable (leave empty to use the launcher&apos;s choice)
      <input
        v-model="form.javaPath"
        placeholder="/usr/lib/jvm/java-21/bin/java"
        spellcheck="false"
      />
    </label>
    <label>
      Extra JVM arguments
      <input v-model="form.jvmArgs" placeholder="-XX:+UseG1GC" spellcheck="false" />
    </label>
    <ul v-if="result.errors.length" class="errors">
      <li v-for="item in result.errors" :key="item">{{ item }}</li>
    </ul>
    <p v-if="running" class="errors">Stop the game before changing its settings.</p>
    <p v-if="error" class="errors" role="alert">{{ error }}</p>
    <div class="actions">
      <button class="button primary" type="submit" :disabled="!result.edit || saving || running">
        {{ saving ? 'Saving…' : 'Save settings' }}
      </button>
      <button class="button" type="button" @click="emit('close')">Cancel</button>
    </div>
  </form>
</template>

<style scoped>
.editor {
  display: flex;
  flex-direction: column;
  gap: 0.6rem;
  width: 100%;
  padding-top: 0.75rem;
  border-top: 1px solid var(--color-button-bg);
}
label {
  display: flex;
  flex-direction: column;
  gap: 0.3rem;
  font-size: 0.85rem;
  font-weight: 600;
}
.checkbox {
  flex-direction: row;
  align-items: center;
  gap: 0.5rem;
}
input:not([type='checkbox']) {
  padding: 0.45rem 0.65rem;
  border: 1px solid var(--color-button-bg);
  border-radius: 0.6rem;
  background: var(--color-raised-bg);
  color: var(--color-base);
}
.errors {
  margin: 0;
  padding-left: 1rem;
  color: #fca5a5;
  font-size: 0.8rem;
}
.actions {
  display: flex;
  gap: 0.5rem;
}
.button {
  padding: 0.45rem 0.9rem;
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
.button:disabled {
  opacity: 0.55;
  cursor: not-allowed;
}
</style>
