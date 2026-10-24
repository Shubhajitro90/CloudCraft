<script setup>
import { computed, onMounted, ref } from 'vue'
import { get_default_user, login, remove_user, set_default_user, users } from '@/helpers/auth'
import { describeError } from '@/helpers/cloudcraftLauncher'
import { summarizeAccounts } from '@/helpers/cloudcraftInstances'

const emit = defineEmits(['change'])

const accounts = ref([])
const defaultId = ref(null)
const loading = ref(true)
const busy = ref(false)
const error = ref('')
const message = ref('')

const summary = computed(() => summarizeAccounts(accounts.value, defaultId.value))

async function refresh() {
  loading.value = true
  error.value = ''
  try {
    const [list, selected] = await Promise.all([users(), get_default_user()])
    accounts.value = Array.isArray(list) ? list : []
    defaultId.value = selected ?? null
  } catch (cause) {
    error.value = `Could not load accounts: ${describeError(cause)}`
  } finally {
    loading.value = false
  }
}

async function signIn() {
  if (busy.value) return
  busy.value = true
  error.value = ''
  message.value = 'Complete the Microsoft sign-in in the window that opened…'
  try {
    const account = await login()
    if (account?.id) {
      await set_default_user(account.id)
      message.value = `Signed in as ${account.username ?? 'your Minecraft account'}.`
    } else {
      message.value = 'Sign-in was closed before it finished.'
    }
    await refresh()
    emit('change')
  } catch (cause) {
    message.value = ''
    error.value = `Sign-in failed: ${describeError(cause)}`
  } finally {
    busy.value = false
  }
}

async function select(account) {
  if (busy.value) return
  busy.value = true
  error.value = ''
  try {
    await set_default_user(account.id)
    await refresh()
    emit('change')
  } catch (cause) {
    error.value = `Could not switch account: ${describeError(cause)}`
  } finally {
    busy.value = false
  }
}

async function signOut(account) {
  if (busy.value) return
  busy.value = true
  error.value = ''
  try {
    await remove_user(account.id)
    const remaining = (await users()) ?? []
    if (account.id === defaultId.value && remaining.length > 0) {
      await set_default_user(remaining[0].id)
    }
    message.value = `${account.username ?? 'The account'} was signed out.`
    await refresh()
    emit('change')
  } catch (cause) {
    error.value = `Could not sign out: ${describeError(cause)}`
  } finally {
    busy.value = false
  }
}

onMounted(refresh)
</script>

<template>
  <section class="account-panel" aria-labelledby="account-heading">
    <div class="account-heading">
      <div>
        <p class="eyebrow">MINECRAFT ACCOUNT</p>
        <h3 id="account-heading">Microsoft sign-in</h3>
      </div>
      <button class="account-button primary" type="button" :disabled="busy" @click="signIn">
        {{ busy ? 'Waiting…' : summary.needsSignIn ? 'Sign in with Microsoft' : 'Add account' }}
      </button>
    </div>
    <p v-if="loading" class="muted">Loading accounts…</p>
    <p v-else-if="summary.needsSignIn" class="muted">
      No account is signed in. Playing requires a Minecraft account that owns the game; sign in
      through Microsoft to continue.
    </p>
    <ul v-else class="account-list">
      <li v-if="summary.selected" class="account-row selected">
        <span>
          <strong>{{ summary.selected.username }}</strong>
          <small>Active account</small>
        </span>
        <button
          class="account-button"
          type="button"
          :disabled="busy"
          @click="signOut(summary.selected)"
        >
          Sign out
        </button>
      </li>
      <li v-for="account in summary.others" :key="account.id" class="account-row">
        <span>
          <strong>{{ account.username }}</strong>
        </span>
        <span class="row-actions">
          <button class="account-button" type="button" :disabled="busy" @click="select(account)">
            Use
          </button>
          <button class="account-button" type="button" :disabled="busy" @click="signOut(account)">
            Sign out
          </button>
        </span>
      </li>
    </ul>
    <p v-if="error" class="error" role="alert">{{ error }}</p>
    <p v-else-if="message" class="muted" role="status">{{ message }}</p>
  </section>
</template>

<style scoped>
.account-panel {
  display: flex;
  flex-direction: column;
  gap: 0.8rem;
  padding: 1.1rem 1.25rem;
  border: 1px solid var(--color-button-bg);
  border-radius: 1rem;
  background: var(--color-bg);
}
.account-heading {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 1rem;
  flex-wrap: wrap;
}
.account-heading h3 {
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
.account-list {
  display: flex;
  flex-direction: column;
  gap: 0.5rem;
  margin: 0;
  padding: 0;
  list-style: none;
}
.account-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 0.75rem;
  padding: 0.6rem 0.8rem;
  border: 1px solid var(--color-button-bg);
  border-radius: 0.75rem;
}
.account-row.selected {
  border-color: #8b5cf6;
}
.account-row small {
  display: block;
  color: var(--color-secondary);
}
.row-actions {
  display: flex;
  gap: 0.5rem;
}
.account-button {
  padding: 0.45rem 0.9rem;
  border: 1px solid var(--color-button-bg);
  border-radius: 0.65rem;
  background: transparent;
  color: var(--color-base);
  font-weight: 600;
  cursor: pointer;
}
.account-button.primary {
  border-color: transparent;
  background: #8b5cf6;
  color: #fff;
}
.account-button:disabled {
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
  color: #f87171;
  font-size: 0.875rem;
}
</style>
