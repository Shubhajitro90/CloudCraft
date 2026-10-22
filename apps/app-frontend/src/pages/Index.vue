<script setup>
import { ref, onUnmounted, computed } from 'vue'
import { useRoute } from 'vue-router'
import RowDisplay from '@/components/RowDisplay.vue'
import { list } from '@/helpers/profile.js'
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

const offline = ref(!navigator.onLine)
window.addEventListener('offline', () => {
  offline.value = true
})
window.addEventListener('online', () => {
  offline.value = false
})

const getInstances = async () => {
  const profiles = await list().catch(handleError)

  recentInstances.value = profiles
    .filter((x) => x.last_played)
    .sort((a, b) => {
      const dateA = dayjs(a.last_played)
      const dateB = dayjs(b.last_played)

      if (dateA.isSame(dateB)) {
        return a.name.localeCompare(b.name)
      }

      return dateB - dateA
    })

  const filters = []
  for (const instance of profiles) {
    if (instance.linked_data && instance.linked_data.project_id) {
      filters.push(`NOT"project_id"="${instance.linked_data.project_id}"`)
    }
  }
  filter.value = filters.join(' AND ')
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
    featuredModpacks.value = []
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
}
</style>