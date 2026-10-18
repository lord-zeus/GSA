<script setup lang="ts">
import { ref, onMounted, computed } from 'vue'
import { api, Project, ScheduleRow, ScheduleCreateInput } from '@/api/client'

// — list state —
const schedules = ref<ScheduleRow[]>([])
const projects = ref<Project[]>([])
const listError = ref('')
const busy = ref(false)
const runBusy = ref<Record<string, boolean>>({})
const runResult = ref<Record<string, { at: number; text: string; ok: boolean }>>({})

// — new schedule form —
const DAY_LABELS: Record<number, string> = { 1: 'Mo', 2: 'Tu', 3: 'We', 4: 'Th', 5: 'Fr', 6: 'Sa', 7: 'Su' }
interface Form {
  showAdd: boolean
  project_id: string
  name: string
  mode: 'once' | 'recurring'
  start_at: string          // "YYYY-MM-DD HH:MM:SS" naive UTC
  stop_at: string           // "" = none
  repeat_every_minutes: number | null
  weekdays: number[]
  window_start: string      // "HH:MM" or "" 
  window_end: string
  max_runs: number | null
  enabled: boolean
}
function blankForm(): Form {
  const d = new Date()
  // default: 1 min from now, +1s to be safely "due"
  d.setSeconds(d.getSeconds() + 30)
  return {
    showAdd: false,
    project_id: projects.value[0]?.id ?? '',
    name: '',
    mode: 'recurring',
    start_at: toNaiveUTC(d),
    stop_at: '',
    repeat_every_minutes: 360,
    weekdays: [],
    window_start: '',
    window_end: '',
    max_runs: null,
    enabled: true,
  }
}
const f = ref<Form>(blankForm())
const addError = ref('')

// format a JS Date as naive UTC "YYYY-MM-DD HH:MM:SS"
function toNaiveUTC(d: Date): string {
  const p = (n: number) => String(n).padStart(2, '0')
  return `${d.getUTCFullYear()}-${p(d.getUTCMonth() + 1)}-${p(d.getUTCDate())} ${p(d.getUTCHours())}:${p(d.getUTCMinutes())}:${p(d.getUTCSeconds())}`
}

async function load() {
  listError.value = ''
  try {
    const [list, proj] = await Promise.all([api.scheduleList(), api.projectList()])
    schedules.value = list
    projects.value = proj
    if (f.value.project_id === '' && proj[0]) f.value.project_id = proj[0].id
  } catch (e: any) {
    listError.value = e.message || String(e)
  }
}

function fmtDT(iso: string): string {
  const d = new Date(iso)
  const p = (n: number) => String(n).padStart(2, '0')
  return `${p(d.getUTCHours())}:${p(d.getUTCMinutes())} · ${p(d.getUTCDate())}-${['Jan','Feb','Mar','Apr','May','Jun','Jul','Aug','Sep','Oct','Nov','Dec'][d.getUTCMonth()]}`
}

const projectName = (id: string) => projects.value.find((p) => p.id === id)?.name ?? id.slice(0, 8) + '…'

function toggleDay(d: number) {
  const i = f.value.weekdays.indexOf(d)
  if (i >= 0) f.value.weekdays.splice(i, 1)
  else { f.value.weekdays.push(d); f.value.weekdays.sort((a, b) => a - b) }
}

const weekdayStr = (wd: number[] | null) => {
  if (!wd || wd.length === 0) return 'any day'
  if (wd.length === 7) return 'every day'
  return wd.map((d) => DAY_LABELS[d] ?? String(d)).join(', ')
}

function cadenceMin(m: number | null): string {
  if (m == null) return '—'
  if (m < 60) return m + ' min'
  if (m < 1440) return (m / 60) + ' h'
  let days = Math.floor(m / 1440)
  let rem = m % 1440
  return (days + ' d' + (rem ? ' ' + rem / 60 + ' h' : ''))
}

async function create() {
  addError.value = ''
  if (!f.value.project_id) { addError.value = 'select a project'; return }
  if (!f.value.name.trim()) { addError.value = 'name is required'; return }
  if (!f.value.start_at) { addError.value = 'start time is required'; return }
  if (f.value.mode === 'recurring' && (!f.value.repeat_every_minutes || f.value.repeat_every_minutes < 1)) {
    addError.value = 'recurring schedules need a cadence ≥ 1 minute'
    return
  }
  busy.value = true
  const input: ScheduleCreateInput = {
    name: f.value.name.trim(),
    mode: f.value.mode,
    start_at: f.value.start_at,
    stop_at: f.value.stop_at || null,
    repeat_every_minutes: f.value.mode === 'recurring' ? f.value.repeat_every_minutes : null,
    repeat_weekdays: f.value.weekdays.length ? f.value.weekdays : null,
    window_start: f.value.window_start || null,
    window_end: f.value.window_end || null,
    max_runs: f.value.max_runs,
    enabled: f.value.enabled,
  }
  try {
    await api.scheduleCreate(f.value.project_id, input)
    f.value = blankForm()
    f.value.showAdd = false
    await load()
  } catch (e: any) {
    addError.value = e.message || String(e)
  } finally {
    busy.value = false
  }
}

async function remove(s: ScheduleRow) {
  if (!confirm(`Delete schedule "${s.name}"?`)) return
  await api.scheduleDelete(s.id).catch((e: any) => alert(e.message || String(e)))
  await load()
}

async function toggle(s: ScheduleRow) {
  busy.value = true
  try {
    await api.scheduleSetEnabled(s.id, !s.enabled)
    await load()
  } finally {
    busy.value = false
  }
}

async function runNow(s: ScheduleRow) {
  if (!confirm(`Fire "${s.name}" now? (ignores next_run_at deadline via run-now)`)) return
  runBusy.value = { ...runBusy.value, [s.id]: true }
  try {
    const res = await api.scheduleRunNow(s.id)
    const ok = res.status !== 'skipped'
    runResult.value = { ...runResult.value, [s.id]: { at: Date.now(), ok, text: ok ? `done: ${(res as any).claimed ?? 0} claimed, ${(res as any).done ?? 0} done` : (res as any).reason || 'skipped' } }
    await load()
  } catch (e: any) {
    runResult.value = { ...runResult.value, [s.id]: { at: Date.now(), ok: false, text: e.message || String(e) } }
  } finally {
    const b = { ...runBusy.value }; delete b[s.id]; runBusy.value = b
  }
}

const stats = computed(() => {
  const n = schedules.value.length
  const on = schedules.value.filter((s) => s.enabled).length
  const now = Date.now()
  const due = schedules.value.filter((s) => s.enabled && s.next_run_at && s.next_run_at <= new Date().toISOString()).length
  const consumed = schedules.value.filter((s) => s.next_run_at == null || (s.max_runs != null && s.run_count >= s.max_runs)).length
  return { n, on, due, consumed, _now: now }
})

onMounted(load)
</script>

<template>
  <div>
    <div class="page-head">
      <div>
        <h1>Schedules</h1>
        <p>
          Scheduled runs (GSA <span class="mono">scheduler</span>) — fire a project on a once or recurring
          calendar: start/stop, cadence, weekday filters, UTC active window, and a total-run cap. The
          background loop claims each due slot atomically (restart-safe, no double-fire).
        </p>
      </div>
      <div class="grid" style="grid-template-columns:repeat(4,minmax(72px,1fr));gap:8px;min-width:320px">
        <div class="card accent" style="padding:10px 12px"><div class="label">Total</div><div class="value" style="font-size:20px">{{ stats.n }}</div></div>
        <div class="card accent" style="padding:10px 12px"><div class="label">Enabled</div><div class="value" style="font-size:20px">{{ stats.on }}</div></div>
        <div class="card accent" style="padding:10px 12px"><div class="label">Due now</div><div class="value" style="font-size:20px">{{ stats.due }}</div></div>
        <div class="card accent" style="padding:10px 12px"><div class="label">Consumed</div><div class="value" style="font-size:20px">{{ stats.consumed }}</div></div>
      </div>
    </div>

    <!-- add form -->
    <div class="card" :style="{ marginBottom: '16px', borderColor: f.showAdd ? 'rgba(64,192,166,.5)' : '' }">
      <div class="row" style="margin-bottom: 10px">
        <span style="font-weight:600">New schedule</span>
        <span class="spacer"></span>
        <button @click="f.showAdd = !f.showAdd">{{ f.showAdd ? 'Hide' : 'New' }}</button>
      </div>
      <div v-if="f.showAdd">
        <div class="row" style="margin-bottom:10px">
          <label style="font-size:13px">Project</label>
          <select v-model="f.project_id" style="flex:1;min-width:200px">
            <option v-for="p in projects" :key="p.id" :value="p.id">{{ p.name }}</option>
          </select>
          <input v-model="f.name" placeholder="schedule name" style="flex:1;min-width:180px" />
        </div>
        <div class="row" style="margin-bottom:10px">
          <label style="font-size:13px">Mode</label>
          <select v-model="f.mode" style="width:130px">
            <option value="recurring">recurring</option>
            <option value="once">once</option>
          </select>
          <label style="font-size:13px">Start</label>
          <input v-model="f.start_at" placeholder="YYYY-MM-DD HH:MM:SS (UTC)" style="flex:1;min-width:220px" class="mono" />
          <label style="font-size:13px">Stop</label>
          <input v-model="f.stop_at" placeholder="none (opt, stop_at)" style="flex:1;min-width:200px" class="mono" />
        </div>
        <div v-if="f.mode === 'recurring'" class="row" style="margin-bottom:10px">
          <label style="font-size:13px">Every</label>
          <input v-model.number="f.repeat_every_minutes" type="number" min="1" title="minutes" style="width:110px" />
          <span style="font-size:13px">minutes</span>
          <span class="spacer"></span>
          <span style="font-size:15px">
            <button v-for="d in [1,2,3,4,5,6,7]" :key="d"
                    :style="{ marginLeft: '3px', borderColor: f.weekdays.includes(d) ? 'rgba(64,192,166,.6)' : '', background: f.weekdays.includes(d) ? 'rgba(64,192,166,.15)' : '' }"
                    @click="toggleDay(d)">{{ DAY_LABELS[d] }}</button>
          </span>
        </div>
        <div class="row" style="margin-bottom:10px">
          <label style="font-size:13px">Window</label>
          <input v-model="f.window_start" placeholder="from HH:MM (UTC, opt)" style="width:150px" class="mono" title="UTC active window start — a wrap (start>end) means overnight" />
          <input v-model="f.window_end" placeholder="to HH:MM (opt)" style="width:150px" class="mono" title="UTC active window end" />
          <span class="spacer"></span>
          <label style="font-size:13px">Max runs</label>
          <input v-model.number="f.max_runs" type="number" min="1" placeholder="∞ (opt)" style="width:110px" title="total fire cap — after this the schedule retires" />
          <label style="font-size:13px;display:flex;align-items:center;gap:6px;margin-left:12px">
            <input type="checkbox" v-model="f.enabled" /> enabled on create
          </label>
        </div>
        <div style="margin-top:10px;display:flex;align-items:center;gap:10px">
          <button class="primary" :disabled="busy" @click="create">{{ busy ? 'Adding…' : 'Add schedule' }}</button>
          <span v-if="addError" class="error">{{ addError }}</span>
          <span v-else class="muted mono" style="font-size:12px">fires at the first eligible slot ≥ creation time</span>
        </div>
      </div>
    </div>

    <!-- list -->
    <div class="card" style="padding:0;overflow:hidden">
      <table class="table">
        <thead>
          <tr>
            <th>Name</th>
            <th>Project</th>
            <th>Mode</th>
            <th>Cadence</th>
            <th>Window</th>
            <th>Next run</th>
            <th>Runs</th>
            <th>Status</th>
            <th></th>
          </tr>
        </thead>
        <tbody>
          <tr v-if="!schedules.length">
            <td colspan="9" class="muted" style="text-align:center;padding:30px">no schedules yet — add one above to fire a project on a calendar</td>
          </tr>
          <tr v-for="s in schedules" :key="s.id">
            <td style="font-weight:600">{{ s.name }}
              <div v-if="runResult[s.id]" style="font-size:11px;font-weight:400" :style="{ color: runResult[s.id]!.ok ? 'var(--ok)' : 'var(--danger)' }">{{ runResult[s.id]!.text }}</div>
            </td>
            <td style="font-size:12px" class="muted">{{ projectName(s.project_id) }}</td>
            <td><span class="badge" :class="s.mode === 'recurring' ? 'accent' : ''">{{ s.mode }}</span></td>
            <td class="mono" style="font-size:12px">
              <template v-if="s.mode === 'recurring'">
                every {{ cadenceMin(s.repeat_every_minutes) }}<br /><span class="muted">{{ weekdayStr(s.repeat_weekdays) }}</span>
              </template>
              <template v-else><span class="muted">fire once</span></template>
            </td>
            <td class="mono" style="font-size:12px">
              <template v-if="s.window_start || s.window_end">{{ s.window_start || '00:00' }}–{{ s.window_end || '23:59' }} UTC</template>
              <span v-else class="muted">all day</span>
            </td>
            <td class="mono" style="font-size:12px">
              <span v-if="s.next_run_at">{{ fmtDT(s.next_run_at) }}</span>
              <span v-else :style="{ color: s.max_runs != null && s.run_count >= s.max_runs ? 'var(--ok)' : 'var(--muted)' }">
                {{ s.max_runs != null && s.run_count >= s.max_runs ? 'cap reached ✓' : 'consumed' }}
              </span>
            </td>
            <td class="mono" style="font-size:12px">
              {{ s.run_count }}<span v-if="s.max_runs != null">/{{ s.max_runs }}</span>
            </td>
            <td>
              <span class="dot" :class="s.enabled ? 'ok' : 'bad'" style="margin-right:6px"></span>
              {{ s.enabled ? 'enabled' : 'disabled' }}
            </td>
            <td>
              <div class="row" style="gap:6px;justify-content:flex-end">
                <button :disabled="!!runBusy[s.id]" @click="runNow(s)">{{ runBusy[s.id] ? 'running…' : 'run now' }}</button>
                <button :disabled="busy" @click="toggle(s)">{{ s.enabled ? 'disable' : 'enable' }}</button>
                <button title="delete" @click="remove(s)">✕</button>
              </div>
            </td>
          </tr>
        </tbody>
      </table>
    </div>

    <div v-if="listError" class="error">{{ listError }}</div>

    <div class="card" style="margin-top:16px">
      <div class="row" style="align-items:flex-start">
        <span style="color:var(--accent);font-size:18px;line-height:1">ℹ</span>
        <div style="font-size:13px;color:var(--muted);line-height:1.6">
          <strong style="color:var(--fg)">How it fires</strong> — a background loop wakes every 30 s
          (<span class="mono">SEER_SCHEDULER_TICK_MS</span>, disable with <span class="mono">SEER_SCHEDULER_DISABLE=1</span>).
          Each tick lists enabled schedules whose <span class="mono">next_run_at</span> is ≤ now, claims the slot
          atomically (row-level update, so two ticks never both fire), advances
          <span class="mono">next_run_at</span> to the next eligible slot per the pure-core calendar, and runs the
          project through the same in-process worker pool as a manual
          <a href="/projects">Run</a>. A once-schedule retires after one fire; a recurring one retires when it hits
          <span class="mono">max_runs</span> (or its <span class="mono">stop_at</span>).
        </div>
      </div>
    </div>
  </div>
</template>
