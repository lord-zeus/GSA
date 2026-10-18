<script setup lang="ts">
import { onMounted, ref, computed } from 'vue'
import { api, type Dashboard } from '@/api/client'

const WIN = [
  { label: '7d', days: 7 },
  { label: '30d', days: 30 },
  { label: '90d', days: 90 },
  { label: '1y', days: 365 },
]

const dash = ref<Dashboard | null>(null)
const win = ref(1) // 30d default (index into WIN)
const loading = ref(true)
const loadErr = ref('')
const loadedAt = ref<number | null>(null)

async function load() {
  loading.value = true
  loadErr.value = ''
  try {
    dash.value = await api.stats(WIN[win.value].days)
    loadedAt.value = Date.now()
  } catch (e: any) {
    loadErr.value = String(e?.message || e)
  } finally {
    loading.value = false
  }
}

onMounted(load)

const pct = (n: number | null | undefined) =>
  n == null || Number.isNaN(n) ? '—' : `${(n * 100).toFixed(n * 100 >= 10 ? 1 : 1)}%`

const s = computed(() => dash.value?.summary ?? null)
const rate = computed(() => s.value?.success_rate ?? 0)
const rateColor = computed(() => {
  const r = rate.value
  if (r >= 0.75) return 'var(--accent)'
  if (r >= 0.4) return '#f59e0b'
  return '#ef4444'
})

// timeline scaling helpers
const maxLinks = computed(() => Math.max(1, ...(dash.value?.days ?? []).map(d => d.links)))

const topEngines = computed(() => (dash.value?.engines ?? []).slice(0, 12))
const topProjects = computed(() => (dash.value?.projects ?? []).slice(0, 12))

const fmtDate = (d: string) => {
  // 'YYYY-MM-DD' → 'DD MMM' compact for ticks
  const parts = d.split('-')
  const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec']
  return `${Number(parts[2])} ${MONTHS[Number(parts[1]) - 1]}`
}
</script>

<template>
  <div>
    <div class="page-head">
      <div>
        <h1>Stats &amp; Analytics</h1>
        <p>
          GSA-style dashboard — success / fail / captcha ratios, link count over
          time, per-engine and per-project breakdowns.
        </p>
      </div>
      <div class="row">
        <div class="win">
          <button
            v-for="(w, i) in WIN"
            :key="w.days"
            :class="{ active: i === win }"
            @click="win = i; load()"
          >
            {{ w.label }}
          </button>
        </div>
        <button @click="load()" :disabled="loading">
          {{ loading ? 'loading…' : 'Refresh' }}
        </button>
      </div>
    </div>

    <div v-if="loadErr" class="banner err">
      <span>⚠</span> Stats failed: {{ loadErr }}
    </div>

    <template v-if="s">
      <!-- KPI cards -->
      <div class="grid kpis">
        <div class="card">
          <div class="label">Success rate</div>
          <div class="value" :style="{ color: rateColor }">{{ pct(rate) }}</div>
          <div class="sub">{{ s.successes.toLocaleString() }} / {{ s.results_total.toLocaleString() }} outcomes</div>
        </div>
        <div class="card accent">
          <div class="label">Created links</div>
          <div class="value">{{ s.unique_links.toLocaleString() }}</div>
          <div class="sub">de-duplicated by URL</div>
        </div>
        <div class="card">
          <div class="label">Failures</div>
          <div class="value">{{ (s.failures + s.errors).toLocaleString() }}</div>
          <div class="sub">{{ s.failures.toLocaleString() }} failed · {{ s.errors.toLocaleString() }} error</div>
        </div>
        <div class="card">
          <div class="label">Captcha walls</div>
          <div class="value">{{ s.captchas.toLocaleString() }}</div>
          <div class="sub">tracked separately from failures</div>
        </div>
      </div>

      <div class="grid kpis" style="margin-top:14px">
        <div class="card">
          <div class="label">Jobs total</div>
          <div class="value">{{ s.jobs_total.toLocaleString() }}</div>
          <div class="sub">
            {{ s.jobs_done.toLocaleString() }} done · {{ s.jobs_failed.toLocaleString() }} failed
          </div>
        </div>
        <div class="card">
          <div class="label">In flight</div>
          <div class="value">{{ s.jobs_inflight.toLocaleString() }}</div>
          <div class="sub">pending + running</div>
        </div>
        <div class="card">
          <div class="label">Rejected</div>
          <div class="value">{{ s.rejected.toLocaleString() }}</div>
          <div class="sub">engine declined the submission</div>
        </div>
        <div class="card">
          <div class="label">Info</div>
          <div class="value">{{ s.info.toLocaleString() }}</div>
          <div class="sub">notes / non-success events</div>
        </div>
      </div>

      <!-- link count over time -->
      <div class="chart-card" v-if="dash && dash.days.length">
        <div class="chart-head">
          <div>
            <div class="chart-title">Links over time</div>
            <div class="chart-sub">
              success outcomes vs failures per day · {{ WIN[win].label }} window
            </div>
          </div>
          <div class="legend">
            <span class="lg"><i class="sw ok"></i> links (success)</span>
            <span class="lg"><i class="sw other"></i> failures + other</span>
            <span class="lg"><i class="sw cap"></i> captchas</span>
          </div>
        </div>
        <div class="timeline">
          <div class="lane">
            <div class="lane-bars" role="img" :aria-label="`links per day, last ${WIN[win].label}`">
              <div
                v-for="pt in dash.days"
                :key="pt.date"
                class="lane-bar"
                :title="`${pt.date} — links ${pt.links}, captchas ${pt.captchas}, other ${pt.other}`"
              >
                <div class="lane-seg ok" :style="{ height: `${(pt.links / maxLinks) * 100}%` }"></div>
                <div class="lane-seg other" :style="{ height: `${(pt.other / maxLinks) * 100}%` }"></div>
                <div class="lane-seg cap" :style="{ height: `${(pt.captchas / maxLinks) * 100}%` }"></div>
              </div>
            </div>
          </div>
          <div class="lane-ticks">
            <span>{{ fmtDate(dash.days[0].date) }}</span>
            <span v-if="dash.days.length > 1">{{ fmtDate(dash.days[Math.floor(dash.days.length / 2)].date) }}</span>
            <span>{{ fmtDate(dash.days[dash.days.length - 1].date) }}</span>
          </div>
          <div class="chart-foot">
            <span>max day · {{ maxLinks.toLocaleString() }} links</span>
            <span>
              window total ·
              {{ dash.days.reduce((a, p) => a + p.links, 0).toLocaleString() }} links ·
              {{ dash.days.reduce((a, p) => a + p.captchas, 0).toLocaleString() }} captchas
            </span>
          </div>
        </div>
      </div>

      <!-- per-engine breakdown -->
      <div class="table-card" v-if="topEngines.length">
        <div class="table-title">
          <h2>Per-engine</h2>
          <span class="mut">top {{ topEngines.length }} by outcomes</span>
        </div>
        <table class="table">
          <thead>
            <tr>
              <th>Engine</th>
              <th class="num">Jobs</th>
              <th class="num">Success</th>
              <th class="num">Failures</th>
              <th class="num">Captchas</th>
              <th class="num">Links</th>
              <th class="num">Rate</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="e in topEngines" :key="e.engine_id">
              <td>{{ e.name }}</td>
              <td class="num">{{ e.jobs }}</td>
              <td class="num ok-text">{{ e.successes.toLocaleString() }}</td>
              <td class="num bad-text">{{ (e.failures).toLocaleString() }}</td>
              <td class="num cap-text">{{ e.captchas.toLocaleString() }}</td>
              <td class="num">{{ e.unique_links.toLocaleString() }}</td>
              <td class="num">
                <span class="rate" :style="{ color: e.success_rate >= 0.75 ? 'var(--accent)' : e.success_rate >= 0.4 ? '#f59e0b' : '#ef4444' }">
                  {{ pct(e.success_rate) }}
                </span>
              </td>
            </tr>
          </tbody>
        </table>
      </div>

      <!-- per-project breakdown -->
      <div class="table-card" v-if="topProjects.length">
        <div class="table-title">
          <h2>Per-project</h2>
          <span class="mut">top {{ topProjects.length }} by outcomes</span>
        </div>
        <table class="table">
          <thead>
            <tr>
              <th>Project</th>
              <th class="num">Jobs</th>
              <th class="num">Success</th>
              <th class="num">Failures</th>
              <th class="num">Captchas</th>
              <th class="num">Links</th>
              <th class="num">Rate</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="p in topProjects" :key="p.project_id">
              <td>{{ p.name }}</td>
              <td class="num">{{ p.jobs }}</td>
              <td class="num ok-text">{{ p.successes.toLocaleString() }}</td>
              <td class="num bad-text">{{ p.failures.toLocaleString() }}</td>
              <td class="num cap-text">{{ p.captchas.toLocaleString() }}</td>
              <td class="num">{{ p.unique_links.toLocaleString() }}</td>
              <td class="num">
                <span class="rate" :style="{ color: p.success_rate >= 0.75 ? 'var(--accent)' : p.success_rate >= 0.4 ? '#f59e0b' : '#ef4444' }">
                  {{ pct(p.success_rate) }}
                </span>
              </td>
            </tr>
          </tbody>
        </table>
      </div>

      <div class="mut-foot" v-if="loadedAt">
        loaded {{ new Date(loadedAt).toLocaleTimeString() }} · window {{ WIN[win].label }} ·
        source <code>GET /api/stats?days={{ WIN[win].days }}</code>
      </div>
    </template>

    <div v-else-if="!loading" class="empty">
      <div class="empty-icon">📊</div>
      <p>No stats to display.</p>
      <p class="mut">Run some jobs from <router-link to="/projects">Projects</router-link>, then come back.</p>
    </div>
  </div>
</template>

<style scoped>
.row { display: flex; gap: 10px; align-items: center; }
.win {
  display: inline-flex; padding: 3px; border-radius: 10px;
  background: rgba(255,255,255,.03); border: 1px solid var(--border);
}
.win button {
  border: none; background: transparent; padding: 6px 12px; font-size: 13px;
  color: var(--muted); border-radius: 8px; cursor: pointer; font-family: inherit;
}
.win button.active { color: var(--fg); background: rgba(16,185,129,.14); }

.kpis { grid-template-columns: repeat(auto-fit, minmax(210px, 1fr)); }
.chart-card {
  margin-top: 18px; background: var(--panel); border: 1px solid var(--border);
  border-radius: var(--radius); padding: 16px 18px;
}
.chart-head { display: flex; justify-content: space-between; align-items: baseline; margin-bottom: 14px; gap: 12px; flex-wrap: wrap; }
.chart-title { font-size: 15px; font-weight: 600; letter-spacing: .2px; }
.chart-sub { color: var(--muted); font-size: 12px; margin-top: 2px; }
.legend { display: flex; gap: 14px; font-size: 12px; color: var(--muted); flex-wrap: wrap; }
.legend .sw { display: inline-block; width: 10px; height: 10px; border-radius: 3px; margin-right: 6px; vertical-align: -1px; }
.legend .ok { background: linear-gradient(180deg, var(--accent), var(--accent-2)); }
.legend .other { background: #ef4444; }
.legend .cap { background: #f59e0b; }

.timeline {
  display: grid; grid-template-columns: 1fr; gap: 8px; padding: 6px 2px 2px;
}
.lane { display: grid; grid-template-columns: 1fr; align-items: end; }
.lane-bars {
  display: flex; align-items: flex-end; gap: 2px; height: 160px;
  overflow: hidden; padding-bottom: 2px; border-bottom: 1px solid rgba(133,149,171,.25);
}
.lane-bar {
  flex: 1 1 0; min-width: 3px; border-radius: 2px 2px 0 0; height: 100%;
  display: flex; flex-direction: column; justify-content: flex-end;
  background: rgba(255,255,255,.02); overflow: hidden;
}
.lane-seg { border-radius: 2px; }
.lane-seg.ok { background: linear-gradient(180deg, var(--accent), var(--accent-2)); }
.lane-seg.other { background: #ef4444; }
.lane-seg.cap { background: #f59e0b; }
.lane-ticks {
  display: grid; grid-template-columns: 1fr auto 1fr; padding-left: 10px; padding-right: 10px;
  color: var(--muted); font-size: 11px;
}
.lane-ticks span:first-child { text-align: left; }
.lane-ticks span:last-child { text-align: right; }
.chart-foot {
  display: flex; justify-content: space-between; color: var(--muted);
  font-size: 12px; margin-top: 4px; flex-wrap: wrap; gap: 6px;
}

.table-card {
  margin-top: 18px; background: var(--panel); border: 1px solid var(--border);
  border-radius: var(--radius); padding: 16px 18px;
}
.table-title { display: flex; align-items: baseline; justify-content: space-between; margin-bottom: 10px; }
.table-title h2 { font-size: 15px; font-weight: 600; letter-spacing: .2px; margin: 0; }
.table-title .mut { color: var(--muted); font-size: 12px; }

.num, .table td.num { text-align: right; font-variant-numeric: tabular-nums; }
.ok-text { color: var(--accent); }
.bad-text { color: #f87171; }
.cap-text { color: #f59e0b; }
.rate { font-weight: 600; font-variant-numeric: tabular-nums; }

.mut { color: var(--muted); }
.mut-foot { color: var(--muted); font-size: 11px; margin-top: 18px; }
.mut-foot code { background: rgba(255,255,255,.05); padding: 1px 6px; border-radius: 6px; font-size: 11px; }

.banner {
  display: flex; gap: 10px; align-items: center; padding: 10px 14px;
  border: 1px solid rgba(239,68,68,.35); border-radius: 10px; margin-bottom: 16px;
  background: rgba(239,68,68,.08); color: #fca5a5; font-size: 13px;
}
.empty { padding: 60px 0; text-align: center; }
.empty-icon { font-size: 44px; margin-bottom: 8px; opacity: .6; }
.empty p { color: var(--fg); margin: 4px 0; }
</style>
