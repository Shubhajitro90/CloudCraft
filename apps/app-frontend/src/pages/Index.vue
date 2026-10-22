<script setup>
import { ref, onUnmounted, computed } from 'vue'
import { useRoute } from 'vue-router'
import RowDisplay from '@/components/RowDisplay.vue'
import { list, run } from '@/helpers/profile.js'
import { profile_listener } from '@/helpers/events'
import { useBreadcrumbs } from '@/store/breadcrumbs'
import { handleError } from '@/store/notifications.js'
import dayjs from 'dayjs'
import { get_search_results } from '@/helpers/cache.js'

const featuredModpacks = ref({})
const featuredMods = ref({})
const filter = ref('')

const route = useRoute()
const breadcrumbs = useBreadcrumbs()

breadcrumbs.setRootContext({ name: 'Home', link: route.path })

const recentInstances = ref([])
const launchInProgress = ref('')
const launchError = ref('')
const launchMessage = ref('')
const selectedInstance = computed(() => recentInstances.value[0] || null)

const offline = ref(!navigator.onLine)
window.addEventListener('offline', () => {
  offline.value = true
})
window.addEventListener('online', () => {
  offline.value = false
})

const getInstances = async () => {
  const result = await list().catch(handleError)
  const profiles = Array.isArray(result) ? result : Object.values(result || {})

  recentInstances.value = profiles
    .filter((x) => x?.last_played)
    .sort((a, b) => {
      const dateA = dayjs(a.last_played)
      const dateB = dayjs(b.last_played)

      if (dateA.isSame(dateB)) {
        return (a.name || '').localeCompare(b.name || '')
      }

      return dateB - dateA
    })

  const filters = []
  for (const instance of profiles) {
    if (instance?.linked_data?.project_id) {
      filters.push(`NOT"project_id"="${instance.linked_data.project_id}"`)
    }
  }
  filter.value = filters.join(' AND ')
}

async function launchSelectedInstance() {
  const instance = selectedInstance.value
  if (
    !instance?.path ||
    instance.install_stage !== 'installed' ||
    launchInProgress.value
  ) {
    return
  }

  launchInProgress.value = instance.path
  launchError.value = ''
  launchMessage.value = ''

  try {
    await run(instance.path)
    launchMessage.value = 'Launch request sent to the existing launcher.'
  } catch (error) {
    launchError.value = String(error?.message || error)
  } finally {
    launchInProgress.value = ''
  }
}

const getFeaturedModpacks = async () => {
  const response = await get_search_results(
    `?facets=[["project_type:modpack"]]&limit=10&index=follows&filters=${filter.value}`,
  )

  if (response) {
    featuredModpacks.value = response.result.hits
  } else {
    featuredModpacks.value = []
  }
}
const getFeaturedMods = async () => {
  const response = await get_search_results('?facets=[["project_type:mod"]]&limit=10&index=follows')

  if (response) {
    featuredMods.value = response.result.hits
  } else {
    featuredMods.value = []
  }
}

await getInstances()

await Promise.all([getFeaturedModpacks(), getFeaturedMods()])

const unlistenProfile = await profile_listener(async (e) => {
  await getInstances()

  if (e.event === 'added' || e.event === 'created' || e.event === 'removed') {
    await Promise.all([getFeaturedModpacks(), getFeaturedMods()])
  }
})

// computed sums of recentInstances, featuredModpacks, featuredMods, treating them as arrays if they are not
const total = computed(() => {
  return (
    (recentInstances.value?.length ?? 0) +
    (featuredModpacks.value?.length ?? 0) +
    (featuredMods.value?.length ?? 0)
  )
})

onUnmounted(() => {
  unlistenProfile()
})
</script>

<template>
  <div class="p-6 flex flex-col gap-5">
    <section class="cloudcraft-welcome">
      <div>
        <p class="cloudcraft-eyebrow">CLOUDCRAFT LAUNCHER</p>
        <h1 class="m-0 text-3xl font-bold">
          {{ recentInstances.length ? 'Welcome back.' : 'Your next world awaits.' }}
        </h1>
        <p class="mt-2 mb-4 text-secondary">
          Your instances, modpacks, and multiplayer shortcuts — together in one place.
        </p>
        <RouterLink class="cloudcraft-link" to="/cloudcraft"
          >Open CloudCraft hub <span aria-hidden="true">↗</span></RouterLink
        >
      </div>
      <div class="cloudcraft-mark" aria-hidden="true">CC</div>
    </section>
    <section v-if="selectedInstance" class="selected-play-panel">
      <div class="min-w-0">
        <p class="cloudcraft-eyebrow">YOUR MOST RECENT INSTANCE</p>
        <h2 class="m-0 text-xl font-bold truncate">
          {{ selectedInstance.name || 'Minecraft instance' }}
        </h2>
        <p class="mt-2 mb-0 text-sm text-secondary">
          {{ selectedInstance.loader || 'Vanilla' }} · Minecraft
          {{ selectedInstance.game_version || 'version unknown' }}
        </p>
        <p class="mt-2 mb-0 text-xs text-secondary">
          {{
            selectedInstance.install_stage === 'installed'
              ? 'Installed locally · ready to launch'
              : 'This instance needs installation or repair before it can launch'
          }}
        </p>
        <p v-if="launchError" class="launch-feedback error" role="alert">{{ launchError }}</p>
        <p v-if="launchMessage" class="launch-feedback" role="status">{{ launchMessage }}</p>
      </div>
      <div class="play-actions">
        <button
          v-if="selectedInstance.install_stage === 'installed'"
          class="cloudcraft-link"
          type="button"
          :disabled="!!launchInProgress"
          @click="launchSelectedInstance"
        >
          {{ launchInProgress ? 'Launching…' : 'Play now' }}
          <span aria-hidden="true">▶</span>
        </button>
        <RouterLink v-else class="cloudcraft-link" to="/library">Finish setup</RouterLink>
        <RouterLink class="cloudcraft-secondary-link" to="/library">Manage instances</RouterLink>
      </div>
    </section>
    <section v-else class="selected-play-panel empty-play-panel">
      <div>
        <p class="cloudcraft-eyebrow">LET’S GET STARTED</p>
        <h2 class="m-0 text-xl font-bold">No recently played instance yet</h2>
        <p class="mt-2 mb-0 text-sm text-secondary">
          Create or install a Minecraft instance in your existing launcher library, then launch it
          from here.
        </p>
      </div>
      <RouterLink class="cloudcraft-link" to="/library">Open instance library</RouterLink>
    </section>
    <h2 class="m-0 text-xl font-semibold">Continue your adventure</h2>
    <RowDisplay
      v-if="total > 0"
      :instances="[
        {
          label: 'Recently played',
          route: '/library',
          instances: recentInstances,
          instance: true,
          downloaded: true,
          compact: true,
        },
        {
          label: 'Discover a modpack',
          route: '/browse/modpack',
          instances: featuredModpacks,
          downloaded: false,
        },
        {
          label: 'Discover mods',
          route: '/browse/mod',
          instances: featuredMods,
          downloaded: false,
        },
      ]"
      :can-paginate="true"
    />
  </div>
</template>

<style scoped>
.cloudcraft-welcome {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 1.5rem;
  padding: clamp(1.4rem, 3.5vw, 2.5rem);
  border: 1px solid rgba(167, 139, 250, 0.28);
  border-radius: 1.25rem;
  background: radial-gradient(ellipse at 90% 15%, rgba(124, 58, 237, 0.22), transparent 42%),
    var(--color-bg-raised);
}
.cloudcraft-eyebrow {
  margin: 0 0 0.6rem;
  color: #b59cff;
  font-size: 0.7rem;
  font-weight: 800;
  letter-spacing: 0.18em;
}
.cloudcraft-link {
  display: inline-flex;
  gap: 0.5rem;
  align-items: center;
  border-radius: 0.75rem;
  padding: 0.7rem 1rem;
  color: white;
  background: linear-gradient(120deg, #a78bfa, #7c3aed);
  font-size: 0.875rem;
  font-weight: 700;
  text-decoration: none;
}
.selected-play-panel {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 1.5rem;
  padding: 1.25rem 1.5rem;
  border: 1px solid var(--color-button-bg);
  border-radius: 1.1rem;
  background: var(--color-bg-raised);
}
.empty-play-panel {
  border-color: rgba(167, 139, 250, 0.25);
}
.play-actions {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 0.75rem;
}
.cloudcraft-link:disabled {
  opacity: 0.55;
  cursor: not-allowed;
}
.cloudcraft-secondary-link {
  color: var(--color-contrast);
  font-size: 0.875rem;
  font-weight: 600;
  text-decoration: none;
}
.launch-feedback {
  margin: 0.65rem 0 0;
  color: #86efac;
  font-size: 0.85rem;
  overflow-wrap: anywhere;
}
.launch-feedback.error {
  color: #fca5a5;
}
.cloudcraft-mark {
  display: grid;
  place-items: center;
  flex: 0 0 5rem;
  width: 5rem;
  height: 5rem;
  border-radius: 1.5rem;
  color: white;
  background: linear-gradient(135deg, #a78bfa, #6d28d9);
  box-shadow: 0 12px 45px rgba(109, 40, 217, 0.3);
  font-weight: 900;
  font-size: 1.5rem;
  letter-spacing: -0.08em;
}
@media (max-width: 620px) {
  .cloudcraft-mark {
    display: none;
  }
  .selected-play-panel {
    align-items: stretch;
    flex-direction: column;
  }
  .play-actions {
    align-items: stretch;
  }
}
</style>