<script setup>
import { computed, onMounted, reactive, ref, watch } from 'vue'
import { create as createProfile, install as installProfile, remove } from '@/helpers/profile'
import { get_by_profile_path } from '@/helpers/process'
import { get_game_versions, get_loader_versions } from '@/helpers/metadata'
import { describeError } from '@/helpers/cloudcraftLauncher'
import {
  LOADERS,
  compatibleGameVersions,
  createAndInstall,
  loaderVersionChoices,
  validateNewInstance,
} from '@/helpers/cloudcraftInstances'

const props = defineProps({
  instances: { type: Array, default: () => [] },
})
const emit = defineEmits(['created'])

const form = reactive({ name: '', loader: 'vanilla', gameVersion: '', loaderVersion: '' })
const gameManifest = ref(null)
const loaderManifests = ref({})
const metadataLoading = ref(true)
const metadataError = ref('')
const stage = ref('')
const submitError = ref('')
const submitMessage = ref('')
const touched = ref(false)

const busy = computed(() => stage.value !== '')
const loaderMetadataKey = computed(
  () => LOADERS.find((item) => item.id === form.loader)?.metadataKey ?? null,
)
const activeLoaderManifest = computed(() =>
  loaderMetadataKey.value ? (loaderManifests.value[loaderMetadataKey.value] ?? null) : null,
)
const gameVersions = computed(() =>
  compatibleGameVersions(form.loader, gameManifest.value, activeLoaderManifest.value),
)
const loaderVersions = computed(() =>
  loaderVersionChoices(form.loader, form.gameVersion, activeLoaderManifest.value),
)
const issues = computed(() =>
  validateNewInstance(form, props.instances, gameVersions.value, loaderVersions.value),
)

async function loadMetadata() {
  metadataLoading.value = true
  metadataError.value = ''
  try {
    gameManifest.value = await get_game_versions()
    const entries = await Promise.all(
      LOADERS.filter((item) => item.metadataKey).map(async (item) => [
        item.metadataKey,
        await get_loader_versions(item.metadataKey).catch(() => null),
      ]),
    )
    loaderManifests.value = Object.fromEntries(entries)
  } catch (error) {
    metadataError.value = `Could not load Minecraft versions: ${describeError(error)}`
  } finally {
    metadataLoading.value = false
  }
}

watch([gameVersions, () => form.loader], () => {
  if (!gameVersions.value.includes(form.gameVersion)) form.gameVersion = gameVersions.value[0] ?? ''
})
watch([loaderVersions, () => form.loader], () => {
  form.loaderVersion = form.loader === 'vanilla' ? '' : (loaderVersions.value[0] ?? '')
})

async function submit() {
  touched.value = true
  if (busy.value || issues.value.length > 0) return
  submitError.value = ''
  submitMessage.value = ''
  const outcome = await createAndInstall(
    { ...form },
    {
      create: createProfile,
      install: installProfile,
      remove,
      processesFor: get_by_profile_path,
    },
    (next) => (stage.value = next),
  )
  stage.value = ''
  if (outcome.kind === 'ready') {
    submitMessage.value = `${form.name.trim()} is installed and ready to play.`
    form.name = ''
    touched.value = false
  } else if (outcome.kind === 'needs_repair') {
    submitError.value = outcome.message
  } else {
    submitError.value = outcome.message
  }
  emit('created')
}

onMounted(loadMetadata)
</script>

<template>
  <form class="create-form" novalidate aria-labelledby="create-heading" @submit.prevent="submit">
    <div>
      <p class="eyebrow">NEW INSTANCE</p>
      <h3 id="create-heading">Create and install</h3>
    </div>
    <p v-if="metadataLoading" class="muted">Loading Minecraft versions…</p>
    <template v-else-if="metadataError">
      <p class="error" role="alert">{{ metadataError }}</p>
      <button class="form-button" type="button" @click="loadMetadata">Retry</button>
    </template>
    <template v-else>
      <label for="new-instance-name">Name</label>
      <input
        id="new-instance-name"
        v-model="form.name"
        type="text"
        maxlength="64"
        autocomplete="off"
        :disabled="busy"
      />
      <div class="row">
        <div>
          <label for="new-instance-loader">Mod loader</label>
          <select id="new-instance-loader" v-model="form.loader" :disabled="busy">
            <option v-for="item in LOADERS" :key="item.id" :value="item.id">
              {{ item.label }}
            </option>
          </select>
        </div>
        <div>
          <label for="new-instance-version">Minecraft version</label>
          <select id="new-instance-version" v-model="form.gameVersion" :disabled="busy">
            <option v-for="version in gameVersions" :key="version" :value="version">
              {{ version }}
            </option>
          </select>
        </div>
        <div v-if="form.loader !== 'vanilla'">
          <label for="new-instance-loader-version">Loader version</label>
          <select
            id="new-instance-loader-version"
            v-model="form.loaderVersion"
            :disabled="busy || loaderVersions.length === 0"
          >
            <option v-for="version in loaderVersions" :key="version" :value="version">
              {{ version }}
            </option>
          </select>
        </div>
      </div>
      <ul v-if="touched && issues.length" class="error" role="alert">
        <li v-for="issue in issues" :key="issue">{{ issue }}</li>
      </ul>
      <p v-if="stage" class="muted" role="status">
        {{
          stage === 'creating'
            ? 'Creating the instance…'
            : 'Downloading Minecraft, libraries and assets. This can take several minutes.'
        }}
      </p>
      <p v-if="submitError" class="error" role="alert">{{ submitError }}</p>
      <p v-if="submitMessage" class="success" role="status">{{ submitMessage }}</p>
      <button class="form-button primary" type="submit" :disabled="busy">
        {{ busy ? 'Working…' : 'Create and install' }}
      </button>
    </template>
  </form>
</template>

<style scoped>
.create-form {
  display: flex;
  flex-direction: column;
  gap: 0.6rem;
  padding: 1.1rem 1.25rem;
  border: 1px solid var(--color-button-bg);
  border-radius: 1rem;
  background: var(--color-bg);
}
.create-form h3 {
  margin: 0;
  font-size: 1.1rem;
}
.eyebrow {
  margin: 0 0 0.3rem;
  color: #b59cff;
  font-size: 0.65rem;
  font-weight: 800;
  letter-spacing: 0.14em;
}
label {
  font-size: 0.8rem;
  font-weight: 600;
  color: var(--color-secondary);
}
input[type='text'],
select {
  width: 100%;
  padding: 0.55rem 0.7rem;
  border: 1px solid var(--color-button-bg);
  border-radius: 0.65rem;
  background: var(--color-bg-raised);
  color: var(--color-base);
}
.row {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(10rem, 1fr));
  gap: 0.75rem;
}
.form-button {
  align-self: flex-start;
  padding: 0.55rem 1.1rem;
  border: 1px solid var(--color-button-bg);
  border-radius: 0.65rem;
  background: transparent;
  color: var(--color-base);
  font-weight: 700;
  cursor: pointer;
}
.form-button.primary {
  border-color: transparent;
  background: #8b5cf6;
  color: #fff;
}
.form-button:disabled {
  opacity: 0.55;
  cursor: not-allowed;
}
.muted {
  margin: 0;
  color: var(--color-secondary);
  font-size: 0.875rem;
}
.error {
  margin: 0;
  padding-left: 0;
  color: #f87171;
  font-size: 0.875rem;
  list-style: none;
}
.success {
  margin: 0;
  color: #4ade80;
  font-size: 0.875rem;
}
</style>
