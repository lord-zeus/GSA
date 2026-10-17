<script setup lang="ts">
import { onMounted, ref, computed } from 'vue'
import { useHealthStore } from '@/stores/health'
import { useCatalogStore } from '@/stores/catalog'
import { api, type CatalogStats } from '@/api/client'

const health = useHealthStore()
const catalog = useCatalogStore()
const stats = ref<CatalogStats | null>(null)
const importMs = ref<number | null>(null)
const loading = ref(true)

onMounted(async () => {
  try {
    const t0 = performance.now()
    const [, st] = await Promise.all([
      catalog.load({ limit: 1, offset: 0 }).catch(() => null),
      api.catalogStats().catch(() => null),
    ])
    stats.value = st
    importMs.value = performance.now() - t0
  } finally {
    loading.value = false
  }
})

const topTypes = computed(() => (stats.value?.types ?? []).slice(0, 8))
const maxType = computed(() => (topTypes.value.length ? topTypes.value[0].n : 1))
</script>

<template>
  <div>
    <div class="page-head">
      <div>
        <h1>Overview</h1>
        <p>Live status of the API, Postgres pool, and the real GSA SER engine catalog.</p>
      </div>
      <div class="row">
        <button @click="health.refresh()">Refresh health</button>
      </div>
    </div>

    <div class="grid">
      <div class="card" :class="health.status === 'ok' ? 'accent' : ''">
        <div class="label">API</div>
        <div class="value" :class="health.status === 'ok' ? '' : 'muted'">{{ health.status.toUpperCase() }}</div>
        <div class="sub">healthz · {{ health.checkedAt ? new Date(health.checkedAt).toLocaleTimeString() : 'never' }}</div>
      </div>
      <div class="card" :class="health.db === 'up' ? 'accent' : ''">
        <div class="label">Postgres</div>
        <div class="value">{{ health.db.toUpperCase() }}</div>
        <div class="sub">sqlx pool → ser-replica-db (docker)</div>
      </div>
      <div class="card accent">
        <div class="label">Web engines</div>
        <div class="value">{{ (stats?.web ?? 0).toLocaleString() }}</div>
        <div class="sub">GSA Engines/*.ini — real specs</div>
      </div>
      <div class="card accent">
        <div class="label">Email engines</div>
        <div class="value">{{ (stats?.email ?? 0).toLocaleString() }}</div>
        <div class="sub">GSA Email_Engines/*.ini</div>
      </div>
    </div>

    <div class="grid" style="margin-top:14px">
      <div class="card">
        <div class="label">Total catalog</div>
        <div class="value">{{ (stats?.total ?? catalog.total).toLocaleString() }}</div>
        <div class="sub">imported idempotently — re-run any time</div>
      </div>
      <div class="card">
        <div class="label">First-page latency</div>
        <div class="value">{{ importMs !== null ? importMs.toFixed(0) + " ms" : "—" }}</div>
        <div class="sub">catalog.list(1) + stats — dev box</div>
      </div>
    </div>

    <h2 v-if="topTypes.length" style="margin:26px 0 10px;font-size:15px;color:var(--muted);text-transform:uppercase;letter-spacing:.8px;font-weight:600">
      Engine types (top)
    </h2>
    <div class="card" v-if="topTypes.length">
      <div class="bars">
        <div class="bar" v-for="t in topTypes" :key="t.engine_type">
          <div class="mono">{{ t.engine_type }}</div>
          <div class="track"><div class="fill" :style="{ width: (100 * t.n / maxType).toFixed(1) + '%' }" /></div>
          <div class="muted" style="text-align:right">{{ t.n }}</div>
        </div>
      </div>
    </div>
  </div>
</template>
