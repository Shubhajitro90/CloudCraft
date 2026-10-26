<script setup>
import { computed } from 'vue'
import {
  backendStatusLabel,
  CLOUD_BACKENDS,
  formatDuration,
  formatMoment,
  mostUrgentWarning,
  resetScheduleLabel,
  sourceLabel,
  usagePercent,
  usageTone,
} from '@/helpers/cloudQuota'

const props = defineProps({
  quota: { type: Object, default: null },
})

const policy = computed(() => props.quota?.policy || null)
const sessionPercent = computed(() =>
  props.quota?.sessionActive && policy.value
    ? usagePercent(props.quota.sessionElapsedSecs || 0, policy.value.maxSessionSecs)
    : 0,
)
const weeklyPercent = computed(() =>
  policy.value ? usagePercent(props.quota.weeklyUsedSecs, policy.value.weeklyAllowanceSecs) : 0,
)
const sessionTone = computed(() => usageTone(props.quota?.sessionRemainingSecs ?? null))
const weeklyTone = computed(() =>
  props.quota?.weeklyRemainingSecs === 0
    ? 'critical'
    : usageTone(props.quota?.weeklyRemainingSecs ?? null),
)
const banner = computed(() => mostUrgentWarning(props.quota?.warnings || []))
</script>

<template>
  <section v-if="quota && policy" class="quota" aria-labelledby="quota-heading">
    <div class="quota-head">
      <h4 id="quota-heading">Usage limits</h4>
      <span class="source" :class="`source-${quota.source}`" :title="quota.sourceNote">
        {{ sourceLabel(quota.source) }}
      </span>
    </div>

    <p v-if="quota.notice" class="notice" role="alert">{{ quota.notice }}</p>
    <p
      v-if="banner"
      class="banner"
      :class="`level-${banner.level}`"
      :role="banner.level === 'critical' ? 'alert' : 'status'"
    >
      {{ banner.message }}
    </p>

    <div class="meters">
      <div class="meter">
        <div class="meter-label">
          <span>Current session</span>
          <strong v-if="quota.sessionActive">
            {{ formatDuration(quota.sessionElapsedSecs) }} of
            {{ formatDuration(policy.maxSessionSecs) }}
          </strong>
          <strong v-else>No session · limit {{ formatDuration(policy.maxSessionSecs) }}</strong>
        </div>
        <div
          class="bar"
          role="progressbar"
          aria-label="Continuous session time used"
          :aria-valuenow="sessionPercent"
          aria-valuemin="0"
          aria-valuemax="100"
        >
          <span :class="`tone-${sessionTone}`" :style="{ width: `${sessionPercent}%` }" />
        </div>
        <small v-if="quota.sessionActive">
          {{ formatDuration(quota.sessionRemainingSecs) }} left · ends
          {{ formatMoment(quota.sessionDeadline) }}
          <template v-if="quota.bindingLimit === 'weekly'"> (weekly allowance)</template>
        </small>
      </div>

      <div class="meter">
        <div class="meter-label">
          <span>This week</span>
          <strong>
            {{ formatDuration(quota.weeklyUsedSecs) }} of
            {{ formatDuration(policy.weeklyAllowanceSecs) }}
          </strong>
        </div>
        <div
          class="bar"
          role="progressbar"
          aria-label="Weekly allowance used"
          :aria-valuenow="weeklyPercent"
          aria-valuemin="0"
          aria-valuemax="100"
        >
          <span :class="`tone-${weeklyTone}`" :style="{ width: `${weeklyPercent}%` }" />
        </div>
        <small>
          {{ formatDuration(quota.weeklyRemainingSecs) }} left · resets
          {{ formatMoment(quota.nextResetAt) }} ({{ resetScheduleLabel(policy) }})
        </small>
      </div>
    </div>

    <p v-if="!quota.canLaunch && !quota.sessionActive" class="blocked" role="status">
      {{ quota.blockReason }}
    </p>
    <p v-if="quota.lastSessionEnd" class="muted small">
      Last session ended {{ formatMoment(quota.lastSessionEnd.endedAt) }}, counted
      {{ formatDuration(quota.lastSessionEnd.countedSecs) }}. {{ quota.lastSessionEnd.message }}
    </p>
    <p class="muted small">
      {{ quota.sourceNote }} These limits are CloudCraft&apos;s configured policy, not a provider
      guarantee.
    </p>

    <details class="backends">
      <summary>Compute backends</summary>
      <ul>
        <li v-for="backend in CLOUD_BACKENDS" :key="backend.id">
          <div class="backend-head">
            <strong>{{ backend.name }}</strong>
            <span class="backend-status" :class="`status-${backend.status}`">
              {{ backendStatusLabel(backend.status) }}
            </span>
          </div>
          <p class="muted small">{{ backend.summary }}</p>
          <p class="muted small">
            Checked {{ backend.checkedOn }}:
            <template v-for="(source, index) in backend.sources" :key="source.url">
              <a :href="source.url" target="_blank" rel="noopener noreferrer">{{ source.label }}</a
              ><template v-if="index < backend.sources.length - 1">, </template>
            </template>
          </p>
        </li>
      </ul>
    </details>
  </section>
</template>

<style scoped>
.quota {
  display: flex;
  flex-direction: column;
  gap: 0.7rem;
  padding: 0.9rem 1rem;
  border-radius: 0.75rem;
  background: var(--color-raised-bg);
  border: 1px solid var(--color-button-bg);
}
.quota-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 0.75rem;
}
.quota-head h4 {
  margin: 0;
}
.source {
  padding: 0.15rem 0.6rem;
  border-radius: 999px;
  font-size: 0.72rem;
  font-weight: 700;
  border: 1px solid #eab308;
  color: #facc15;
}
.source-provider {
  border-color: #22c55e;
  color: #4ade80;
}
.meters {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(15rem, 1fr));
  gap: 0.9rem;
}
.meter {
  display: flex;
  flex-direction: column;
  gap: 0.35rem;
}
.meter-label {
  display: flex;
  justify-content: space-between;
  gap: 0.5rem;
  font-size: 0.85rem;
}
.meter small {
  color: var(--color-secondary);
  font-size: 0.75rem;
}
.bar {
  height: 0.45rem;
  border-radius: 999px;
  background: var(--color-button-bg);
  overflow: hidden;
}
.bar span {
  display: block;
  height: 100%;
  border-radius: inherit;
  transition: width 0.3s ease;
}
.tone-ok {
  background: #8b5cf6;
}
.tone-low {
  background: #eab308;
}
.tone-critical {
  background: #ef4444;
}
.banner,
.notice,
.blocked {
  margin: 0;
  padding: 0.55rem 0.75rem;
  border-radius: 0.6rem;
  font-size: 0.85rem;
  font-weight: 600;
}
.level-info {
  background: rgba(139, 92, 246, 0.15);
  color: #c4b5fd;
}
.level-warning,
.notice {
  background: rgba(234, 179, 8, 0.15);
  color: #facc15;
}
.level-critical,
.blocked {
  background: rgba(239, 68, 68, 0.15);
  color: #fca5a5;
}
.muted {
  margin: 0;
  color: var(--color-secondary);
}
.small {
  font-size: 0.78rem;
}
.backends summary {
  cursor: pointer;
  font-weight: 600;
  font-size: 0.85rem;
}
.backends ul {
  list-style: none;
  margin: 0.5rem 0 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 0.6rem;
}
.backend-head {
  display: flex;
  justify-content: space-between;
  gap: 0.5rem;
  font-size: 0.85rem;
}
.backend-status {
  font-size: 0.72rem;
  font-weight: 700;
}
.status-available {
  color: #4ade80;
}
.status-policyBlocked,
.status-notIntegrated {
  color: #f87171;
}
a {
  color: #c4b5fd;
}
</style>
