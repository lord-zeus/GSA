<script setup lang="ts">
import { onMounted, onUnmounted, ref, computed } from 'vue'
import { useHealthStore } from '@/stores/health'
import { useCatalogStore } from '@/stores/catalog'
import { api, type CatalogStats, type GlobalRuns, type RunStatusActive, type RunStatusEngine, type RunStatusRejections } from '@/api/client'

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

// — T11 · cross-project run board (the operator view) —
// Live "what's running now across ALL projects". Polls every 3s while at
// least one run is active; per-run cards carry progress, engine health
// (blacklisted engines marked), worker pressure, rejection counters, the
// last log lines, and cancel.
interface RunBoardFair {
  target?: number
  links_created?: number
  reached_target?: boolean
  cancelled?: boolean
  stop_reason?: string | null
  engines_blacklisted?: number
  engines?: RunStatusEngine[]
  rejections?: RunStatusRejections
}
interface RunBoard extends RunStatusActive {
  project_name?: string
  last_events?: string[]
  pct?: number
  elapsed?: string
  fair?: RunBoardFair          // T10c live fair-share block (auto runs)
  fair_cancelled?: boolean
}
const board = ref<GlobalRuns | null>(null)
const boardErr = ref(false)
const cancelBusy = ref<Record<string, boolean>>({})

function fmtElapsed(startedAt: string): string {
  const ms = Date.now() - new Date(startedAt).getTime()
  const s = Math.max(0, Math.floor(ms / 1000))
  if (s < 60) return `${s}s`
  const m = Math.floor(s / 60)
  if (m < 60) return `${m}m ${s % 60}s`
  return `${Math.floor(m / 60)}h ${m % 60}m`
}

async function refreshBoard() {
  try {
    const g = await api.globalRuns()
    board.value = g
    boardErr.value = false
    if (g.runs.length > 0) {
      // Enrich with project names + last log lines (bounded, best-effort).
      const names = await api.projectList().catch(() => [])
      const nameById: Record<string, string> = {}
      for (const p of names) nameById[p.id] = p.name
      await Promise.all(
        g.runs.map(async (r) => {
          const proj: RunBoard = r as RunBoard
          proj.project_name = nameById[r.project_id]
          proj.elapsed = fmtElapsed(r.started_at)
          proj.pct = r.links_target > 0
            ? Math.min(100, (100 * r.links_created) / r.links_target)
            : 0
          try {
            const ev = await api.projectEvents(r.project_id, 0, 40)
            const tail = (ev.events ?? [])
              .filter((e) => typeof e.message === 'string' && (e.message as string).length > 0)
              .slice(-4)
            proj.last_events = tail.map((e) => `${e.level ?? ''} ${e.message ?? ''}`.trim())
          } catch {
            proj.last_events = []
          }
        }),
      )
    }
  } catch {
    boardErr.value = true
  }
}

async function cancelRun(projectId: string) {
  cancelBusy.value[projectId] = true
  try {
    await api.projectRunCancel(projectId)
    await refreshBoard()
  } finally {
    cancelBusy.value[projectId] = false
  }
}

let boardTimer: number | undefined
async function tickBoard() {
  await refreshBoard()
  // Keep polling only while something is running.
  if (board.value && board.value.runs.length > 0) {
    boardTimer = window.setTimeout(tickBoard, 3000)
  } else {
    window.clearTimeout(boardTimer)
  }
}

// Expose for the template + a manual refresh handle.
const boardRuns = computed<RunBoard[]>(() => (board.value?.runs ?? []) as RunBoard[])

onMounted(() => {
  void tickBoard()
})
onUnmounted(() => {
  window.clearTimeout(boardTimer)
})
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

    <!-- — T11 · cross-project run board (the operator view) — -->
    <section style="margin:18px 0 6px">
      <div class="row" style="justify-content:space-between;margin-bottom:10px">
        <div>
          <h2 style="margin:0;font-size:15px;color:var(--muted);text-transform:uppercase;letter-spacing:.8px;font-weight:600">
            Live run board
            <span v-if="boardRuns.length" class="muted" style="text-transform:none;letter-spacing:0">
              · {{ boardRuns.length }}/{{ board?.capacity ?? '—' }} in flight
            </span>
            <span v-else class="muted" style="text-transform:none;letter-spacing:0">· idle</span>
          </h2>
          <p class="muted" style="margin:4px 0 0">
            Every background run across all projects, live. Per-engine health, worker pressure, rejections, and the last log lines.
          </p>
        </div>
        <button @click="refreshBoard()" class="ghost" style="white-space:nowrap">Refresh</button>
      </div>

      <div v-if="boardErr" class="card" style="color:var(--muted)">
        <div class="sub">Could not reach the runs endpoint ({{ boardErr }}). Is the API up?</div>
      </div>

      <div v-else-if="!boardRuns.length" class="card" style="padding:14px 16px">
        <div class="muted" style="display:flex;align-items:center;gap:10px">
          <span aria-hidden>🛰️</span>
          No runs in flight. Start one from a project (or run several at once — the board updates every 3&nbsp;s while anything is running).
        </div>
      </div>

      <div v-else class="grid" style="gap:12px">
        <div v-for="r in boardRuns" :key="r.run_id" class="card runboard" style="border-left:3px solid var(--accent)">
          <div class="row" style="justify-content:space-between;align-items:flex-start">
            <div>
              <div style="font-weight:600;font-size:15px">
                {{ r.project_name ?? r.project_id.slice(0, 8) }}
              </div>
              <div class="muted" style="font-size:12px">
                run {{ r.run_id.slice(0, 8) }} · started {{ r.elapsed ?? '' }} ago · status <b>{{ r.status }}</b>
              </div>
            </div>
            <button
              class="ghost"
              :disabled="cancelBusy[r.project_id]"
              @click="cancelRun(r.project_id)"
            >{{ cancelBusy[r.project_id] ? 'Cancelling…' : 'Cancel run' }}</button>
          </div>

          <div style="margin:12px 0 4px">
            <div class="row" style="justify-content:space-between;font-size:12px">
              <span class="muted">links</span>
              <span>
                <b>{{ (r.fair?.links_created ?? r.links_created ?? 0).toLocaleString() }}</b>
                <span class="muted">/ {{ (r.fair?.target ?? r.links_target ?? 0).toLocaleString() }}</span>
                <span v-if="r.fair?.reached_target ?? r.reached_target" class="muted"> · target reached</span>
              </span>
            </div>
            <div class="track" style="margin-top:6px">
              <div class="fill" :style="{ width: (r.pct ?? 0).toFixed(1) + '%' }"></div>
            </div>
          </div>

          <!-- T10c — fair-share pressure + engine health -->
          <div v-if="r.fair" class="muted" style="font-size:12px;margin-top:10px;display:flex;flex-wrap:wrap;gap:8px 16px">
            <span>blacklisted engines: <b>{{ r.fair.engines_blacklisted ?? 0 }}</b></span>
            <span v-if="r.fair.rejections">
              rejected — target {{ r.fair.rejections.target ?? 0 }}, cool {{ r.fair.rejections.cooling ?? 0 }}, cap {{ r.fair.rejections.in_flight_cap ?? 0 }}, quota {{ r.fair.rejections.quota ?? 0 }}, disabled {{ r.fair.rejections.disabled ?? 0 }}
            </span>
          </div>
          <div v-if="r.fair?.cancelled || r.fair?.stop_reason || r.stop_reason" style="font-size:12px;margin-top:8px;color:#d66">
            <b v-if="r.fair?.cancelled">cancelled by operator</b>
            <span v-if="r.fair?.stop_reason"> · {{ r.fair.stop_reason }}</span>
            <span v-else-if="r.stop_reason"> · {{ r.stop_reason }}</span>
          </div>

          <!-- per-engine breakdown (blacklisted ones marked) -->
          <div v-if="(r.fair?.engines ?? []).length" style="margin-top:10px;display:flex;flex-wrap:wrap;gap:6px">
            <span
              v-for="e in r.fair?.engines"
              :key="e.engine"
              class="chip"
              :style="e.disabled
                ? { opacity: .55, textDecoration: 'line-through' }
                : e.rate_hits
                  ? { borderStyle: 'dashed' }
                  : {}"
              :title="`links ${e.links} · admitted ${e.admitted ?? 0} · rate hits ${e.rate_hits ?? 0}${e.disabled ? ' · blacklisted' : ''}`"
            >
              {{ e.engine }} · {{ e.links }}
              <b v-if="e.disabled" style="opacity:.8">✕</b>
            </span>
          </div>

          <!-- last log lines -->
          <div v-if="r.last_events?.length" class="log" style="margin-top:10px">
            <div v-for="(ln, i) in r.last_events" :key="i" class="logline" :class="ln.indexOf('error') === 0 ? 'err' : ''">
              {{ ln }}
            </div>
          </div>
        </div>
      </div>
    </section>

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

<style scoped>
/* — T11 run board — */
.chip {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 3px 10px;
  border: 1px solid var(--border);
  border-radius: 999px;
  font-size: 12px;
  color: var(--muted);
  background: transparent;
}
button.ghost {
  background: transparent;
  border: 1px solid var(--border);
  color: var(--muted);
  border-radius: 8px;
  padding: 6px 12px;
  font-size: 13px;
  cursor: pointer;
}
button.ghost:hover {
  border-color: var(--accent, #4a6cf7);
  color: var(--fg, #101418);
}
.log {
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 11.5px;
  line-height: 1.7;
  padding: 8px 12px;
  border: 1px solid var(--border);
  border-radius: 8px;
  max-height: 150px;
  overflow-y: auto;
  white-space: pre-wrap;
  word-break: break-word;
  color: var(--muted);
}
.logline.err {
  color: #c2412f;
}
</style>
