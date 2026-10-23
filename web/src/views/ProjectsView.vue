<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import {
  api,
  type EngineRow,
  type EmailSetting,
  type JobRow,
  type Keyword,
  type Project,
  type ProjectResult,
  type ResultStats,
  type RunEventRow,
  type RunSummary,
  type TestAllResult,
  type AutoPickResult,
  type RunStatus,
  type GlobalRuns,
} from '@/api/client'

// — projects (master list) —
const projects = ref<Project[]>([])
const projFilter = ref('')
const filteredProjects = computed(() => {
  const q = projFilter.value.trim().toLowerCase()
  if (!q) return projects.value
  return projects.value.filter((p) => p.name.toLowerCase().includes(q) || (p.description || '').toLowerCase().includes(q))
})
const name = ref('')
const desc = ref('')
const busy = ref(false)
const error = ref<string | null>(null)
const created = ref<Project | null>(null)
const selected = ref<string>('')

// — detail tabs —
const tab = ref<'engines' | 'keywords' | 'jobs' | 'results' | 'options'>('engines')

// — engines tab —
const engines = ref<EngineRow[]>([])
const enginesLoaded = ref(false)
const enginesBusy = ref(false)
const editEngine = ref<EngineRow | null>(null)
const engName = ref('')
const engType = ref('')
const engUrl = ref('')
const engSteps = ref(0)
const customName = ref('')
const customType = ref('')
const customUrl = ref('')

async function loadEngines() {
  if (!selected.value) return
  enginesBusy.value = true
  try {
    engines.value = await api.engineList(selected.value)
    enginesLoaded.value = true
    error.value = null
  } catch (e: any) {
    enginesLoaded.value = false
    error.value = e?.message ?? String(e)
  } finally {
    enginesBusy.value = false
  }
}

async function addCustomEngine(spec?: { name: string; url: string }) {
  if (!selected.value) return
  const n = (spec?.name ?? customName.value).trim()
  if (!n) return
  try {
    await api.engineAdd(selected.value, {
      engine_name: n,
      engine_type: (customType.value.trim() || 'web') as any,
      url: (spec?.url ?? customUrl.value).trim(),
      file_path: 'custom://' + n,
      step_count: 0,
    })
    customName.value = ''
    customType.value = ''
    customUrl.value = ''
    await loadEngines()
  } catch (e: any) {
    error.value = e?.message ?? String(e)
  }
}

async function toggleEngine(e: EngineRow) {
  if (!selected.value) return
  try {
    await api.engineToggle(selected.value, e.id, !e.enabled)
    await loadEngines()
  } catch (err: any) {
    error.value = err?.message ?? String(err)
  }
}

function startEditEngine(e: EngineRow) {
  editEngine.value = e
  engName.value = e.engine_name
  engType.value = e.engine_type
  engUrl.value = e.url
  engSteps.value = e.step_count
}

async function saveEngine() {
  if (!selected.value || !editEngine.value) return
  try {
    await api.engineUpdate(selected.value, editEngine.value.id, {
      engine_name: engName.value.trim() || editEngine.value.engine_name,
      engine_type: engType.value.trim() || undefined,
      url: engUrl.value.trim() || undefined,
      step_count: engSteps.value,
    })
    editEngine.value = null
    await loadEngines()
  } catch (e: any) {
    error.value = e?.message ?? String(e)
  }
}

async function deleteEngine(e: EngineRow) {
  if (!selected.value) return
  if (!confirm(`Delete engine “${e.engine_name}” from this project?`)) return
  try {
    await api.engineDelete(selected.value, e.id)
    if (editEngine.value?.id === e.id) editEngine.value = null
    await loadEngines()
  } catch (err: any) {
    error.value = err?.message ?? String(err)
  }
}

// — catalog (from import) for quick-add —
const catalog = ref<{ name: string; url: string; file_path: string; step_count: number; kind: string }[]>([])
const catSearch = ref('')
const catBusy = ref(false)
const catalogFiltered = computed(() =>
  catalog.value.filter((c) => !catSearch.value || c.name.toLowerCase().includes(catSearch.value.toLowerCase())),
)
async function loadCatalog() {
  catBusy.value = true
  try {
    const r = await api.catalogList({ limit: 100 })
    catalog.value = r.data.map((c) => ({
      name: c.name,
      url: c.url,
      file_path: c.file_path,
      step_count: c.step_count,
      kind: c.kind,
    }))
  } catch (e: any) {
    error.value = e?.message ?? String(e)
  } finally {
    catBusy.value = false
  }
}

async function addCatalogEngine(c: (typeof catalog.value)[0]) {
  if (!selected.value) return
  try {
    await api.engineAdd(selected.value, {
      engine_name: c.name,
      engine_type: c.kind as any,
      url: c.url,
      file_path: c.file_path,
      step_count: c.step_count,
    })
    await loadEngines()
  } catch (e: any) {
    error.value = e?.message ?? String(e)
  }
}

// — keywords tab —
const keywords = ref<Keyword[]>([])
const kwInput = ref('')
const bulkInput = ref('')
const kwBusy = ref(false)
const kwLoaded = ref(false)
const kwNotice = ref('')

async function loadKeywords() {
  if (!selected.value) return
  kwBusy.value = true
  try {
    keywords.value = await api.keywordList(selected.value)
    kwLoaded.value = true
    error.value = null
  } catch (e: any) {
    kwLoaded.value = false
    error.value = e?.message ?? String(e)
  } finally {
    kwBusy.value = false
  }
}

async function addKeyword() {
  const text = kwInput.value.trim()
  if (!text || !selected.value) return
  kwBusy.value = true
  try {
    await api.keywordAdd(selected.value, text)
    kwInput.value = ''
    await loadKeywords()
    kwNotice.value = 'keyword added'
  } catch (e: any) {
    error.value = e?.message ?? String(e)
  } finally {
    kwBusy.value = false
  }
}

async function addBulk() {
  const lines = bulkInput.value.split('\n').map((l) => l.trim()).filter(Boolean)
  if (!lines.length || !selected.value) return
  kwBusy.value = true
  try {
    const r = await api.keywordAddBulk(selected.value, lines)
    bulkInput.value = ''
    await loadKeywords()
    kwNotice.value = `bulk: stored ${r.stored} distinct`
  } catch (e: any) {
    error.value = e?.message ?? String(e)
  } finally {
    kwBusy.value = false
  }
}

async function deleteKeyword(k: Keyword) {
  if (!selected.value) return
  kwBusy.value = true
  try {
    await api.keywordDelete(selected.value, k.id)
    await loadKeywords()
  } catch (e: any) {
    error.value = e?.message ?? String(e)
  } finally {
    kwBusy.value = false
  }
}

// — jobs tab —
const jobs = ref<JobRow[]>([])
const jobsLoaded = ref(false)
const jobsBusy = ref(false)
const jobEngine = ref('')
const jobKeyword = ref('')
const jobResultsFor = ref<{ jobId: string; rows: any[] } | null>(null)
const pollTimer = ref<number | null>(null)

async function loadJobs() {
  if (!selected.value) return
  jobsBusy.value = true
  try {
    jobs.value = await api.jobList(selected.value)
    jobsLoaded.value = true
    error.value = null
  } catch (e: any) {
    jobsLoaded.value = false
    error.value = e?.message ?? String(e)
  } finally {
    jobsBusy.value = false
  }
}

async function createJob() {
  if (!selected.value) return
  jobsBusy.value = true
  try {
    await api.jobCreate(selected.value, jobEngine.value || undefined, jobKeyword.value || undefined)
    await loadJobs()
  } catch (e: any) {
    error.value = e?.message ?? String(e)
  } finally {
    jobsBusy.value = false
  }
}

async function cancelJob(j: JobRow) {
  if (!confirm(`Cancel job ${j.id.slice(0, 8)}…?`)) return
  try {
    await api.jobCancel(j.id)
    await loadJobs()
  } catch (e: any) {
    error.value = e?.message ?? String(e)
  }
}

async function openJobResults(j: JobRow) {
  try {
    const rows = (await api.jobResults(j.id)) as any[]
    jobResultsFor.value = { jobId: j.id, rows }
  } catch (e: any) {
    error.value = e?.message ?? String(e)
  }
}

// — F#12: in-process run-now + operator mailbox —
const running = ref(false)
const runMsg = ref('')
const runSummary = ref<RunSummary | null>(null)
const mailbox = ref<EmailSetting>({ address: '', password: '' })
const mailboxSaved = ref(false)
const mailboxSaving = ref(false)
const pendingCount = computed(() =>
  jobs.value.filter((j) => j.status === 'pending').length
)
const mailboxDirty = computed(() =>
  mailbox.value.address.trim() !== '' || mailbox.value.password !== ''
)

async function loadMailbox() {
  try {
    const e = await api.emailGet()
    mailbox.value = { address: e.address, password: e.password }
  } catch { /* keep empty on first run */ }
}

async function saveMailbox() {
  mailboxSaving.value = true
  mailboxSaved.value = false
  try {
    await api.emailSet(mailbox.value)
    mailboxSaved.value = true
    setTimeout(() => (mailboxSaved.value = false), 2000)
  } catch (e: any) {
    error.value = e?.message ?? String(e)
  } finally {
    mailboxSaving.value = false
  }
}

// — F#26: Polish (notes · color · pause-after-N) —
const polishNotes = ref('')
const polishColor = ref('')
const polishPauseAfter = ref(0)
const polishSaved = ref(false)
const polishSaving = ref(false)
const polishErr = ref<string | null>(null)
const isPaused = ref(false)
const resuming = ref(false)
const resumeMsg = ref('')

async function loadPolish(projectId: string) {
  polishSaved.value = false
  isPaused.value = false
  try {
    const p: any = await api.projectGet(projectId)
    polishNotes.value = p.notes || ''
    polishColor.value = p.color || ''
    polishPauseAfter.value = p.pause_after ?? 0
    isPaused.value = !!p.paused_at
    polishErr.value = null
  } catch (e: any) {
    polishErr.value = e?.message ?? String(e)
  }
}

async function savePolish() {
  if (!selected.value) return
  polishSaving.value = true
  polishSaved.value = false
  try {
    const p: any = await api.projectSetPolish(selected.value, {
      notes: polishNotes.value.trim(),
      color: polishColor.value || null,
      pause_after: polishPauseAfter.value || null,
    })
    isPaused.value = !!p.paused_at
    polishSaved.value = true
    polishErr.value = null
    setTimeout(() => (polishSaved.value = false), 2000)
  } catch (e: any) {
    polishErr.value = e?.message ?? String(e)
  } finally {
    polishSaving.value = false
  }
}

async function resumeRun() {
  if (!selected.value) return
  resuming.value = true
  resumeMsg.value = ''
  try {
    const p: any = await api.projectResume(selected.value)
    isPaused.value = !!p.paused_at
    resumeMsg.value = 'Cleared — the run can continue.'
    setTimeout(() => (resumeMsg.value = ''), 3000)
  } catch (e: any) {
    resumeMsg.value = e?.message ?? String(e)
  } finally {
    resuming.value = false
  }
}

// — F#26: Test engine (dry run) —
const testRunning = ref('')
const testResult = ref<{
  engineId: string
  success: boolean
  message: string | null
  url: string | null
  anchor: string | null
  attempts: number
  elapsed_ms: number
} | null>(null)

async function testEngine(id: string) {
  if (!selected.value) return
  testRunning.value = id
  testResult.value = null
  try {
    const r = await api.projectTestEngine(selected.value, id)
    const rep = r.report
    const successful = rep?.successful ?? 0
    const failed = rep?.failed ?? 0
    const errs: string[] = (rep?.errors ?? []).slice(0, 3)
    const ok = !r.error && successful > 0
    let message: string
    if (r.error) message = r.error
    else if (successful > 0) message = `Submitted OK — ${successful} success${failed ? `, ${failed} failed` : ''}${rep?.captcha_failed ? ` (${rep.captcha_failed} captcha failures)` : ''}`
    else message = `No submission succeeded — ${errs.join(' · ') || `${failed} failure(s), ${rep?.retried ?? 0} retried`}`
    testResult.value = {
      engineId: id,
      success: ok,
      message,
      url: rep?.submitted_url || null,
      anchor: null,
      attempts: (rep?.successful ?? 0) + (rep?.failed ?? 0) + (rep?.retried ?? 0),
      elapsed_ms: 0,
    }
    await loadPolish(selected.value)
  } catch (e: any) {
    testResult.value = { engineId: id, success: false, message: e?.message ?? String(e), url: null, anchor: null, attempts: 0, elapsed_ms: 0 }
  } finally {
    testRunning.value = ''
  }
}

// — F#13: per-project `use_proxies` gate —
const useProxies = ref(false)
const useProxiesLoaded = ref(false)
const useProxiesBusy = ref(false)

async function loadUseProxies(projectId: string) {
  useProxiesLoaded.value = false
  useProxies.value = false
  try {
    const p: any = await api.projectGet(projectId)
    useProxies.value = !!p?.use_proxies
  } catch { /* gate defaults to off if the API predates F#13 */ }
  useProxiesLoaded.value = true
}

async function saveUseProxies() {
  if (!selected.value) return
  useProxiesBusy.value = true
  try {
    await api.projectSetUseProxies(selected.value, useProxies.value)
  } catch (e: any) {
    error.value = e?.message ?? String(e)
  } finally {
    useProxiesBusy.value = false
  }
}

async function runNow() {
  if (!selected.value) return
  running.value = true
  runMsg.value = 'Launching a background drain of the queued jobs…'
  try {
    const a = await api.runProject(selected.value)
    runMsg.value =
      `✓ Started background run ${a.run_id.slice(0, 8)}… — draining queued jobs across ${a.engines ?? 0} engine(s). ` +
      `Watch it in the LIVE RUN panel at the top; the Jobs + Results tabs refresh live.`
    startRunPoll()
    await loadJobs()
  } catch (e: any) {
    runMsg.value = 'Run failed: ' + (e?.message || String(e))
  } finally {
    running.value = false
  }
}

const hasActiveJob = computed(() => jobs.value.some((j) => j.status === 'pending' || j.status === 'running'))

// — T6 · the auto UX: auto-build, live run log, test-all, captcha check —
const autoBusy = ref(false)
const autoMsg = ref('')
const autoFail = ref(false)
const autoLinks = ref(100)
const autoEngines = ref(0)
const testAllBusy = ref(false)
const testAllMsg = ref<{ kind: 'ok' | 'err'; text: string } | null>(null)
const captchaBusy = ref(false)
const captchaMsg = ref<{ kind: 'ok' | 'err'; text: string } | null>(null)
const liveLogOpen = ref(true)
const liveEvents = ref<RunEventRow[]>([])
const liveConnected = ref(false)
let liveES: EventSource | null = null
let liveLastId = 0

function levelClass(level: string): string {
  switch (level) {
    case 'success': return 'lv-ok'
    case 'error': return 'lv-err'
    case 'captcha': return 'lv-captcha'
    case 'auto': return 'lv-auto'
    case 'step': return 'lv-step'
    case 'engine': return 'lv-engine'
    default: return 'lv-info'
  }
}

function addEvent(ev: RunEventRow) {
  if (ev.id && ev.id <= liveLastId) return // already on screen (dup via catch-up)
  if (ev.id) liveLastId = ev.id
  liveEvents.value.push(ev)
  if (liveEvents.value.length > 500) liveEvents.value.splice(0, liveEvents.value.length - 500)
}

async function openLiveLog(projectId: string) {
  closeLiveLog()
  liveEvents.value = []
  liveLastId = 0
  // 1) catch up: the DB tail is the source of truth (covers events from
  //    before this tab opened, or missed during a disconnect)
  try {
    const t = await api.projectEvents(projectId, 0, 200)
    for (const ev of t.events) addEvent(ev)
  } catch { /* old API / transient — the live stream still works */ }
  // 2) live: SSE push for everything from now on
  liveES = new EventSource(api.projectEventsLiveUrl(projectId))
  liveES.addEventListener('run', (m: MessageEvent) => {
    try { addEvent(JSON.parse(m.data) as RunEventRow) } catch { /* ignore */ }
  })
  liveES.onopen = () => { liveConnected.value = true }
  liveES.onerror = () => {
    liveConnected.value = false
    // EventSource auto-reconnects; after each reconnect re-tail the DB
    // so any frames we missed are filled back in (both carry `id`).
    window.setTimeout(async () => {
      try {
        const t = await api.projectEvents(projectId, liveLastId, 200)
        for (const ev of t.events) addEvent(ev)
      } catch { /* ignore */ }
    }, 1500)
  }
}

function closeLiveLog() {
  if (liveES) { liveES.close(); liveES = null }
  liveConnected.value = false
}

async function runAuto() {
  if (!selected.value) return
  autoBusy.value = true
  autoFail.value = false
  autoMsg.value = 'System is ranking engines by health and launching a background run…'
  try {
    const a = await api.projectRunAuto(selected.value, {
      links: autoLinks.value,
      engines: autoEngines.value || undefined, // 0 = "all" → let the API fan out across every enabled engine
    })
    autoMsg.value =
      `✓ Launched background run ${a.run_id.slice(0, 8)}… — building ${a.links_target ?? autoLinks.value} link(s) across ${a.engines ?? autoEngines.value} engine(s), ${a.enqueued_jobs ?? 0} job(s) queued.` +
      ` It runs in the background — open other projects, come back anytime and watch the LIVE RUN panel above.` +
      (a.active_runs != null ? ` · ${a.active_runs}/${a.capacity} global slots busy` : '')
    startRunPoll()
  } catch (e: any) {
    autoFail.value = true
    autoMsg.value = 'Auto-build: ' + (e?.message || String(e))
  } finally {
    autoBusy.value = false
  }
}

async function testAllEngines() {
  if (!selected.value) return
  testAllBusy.value = true
  testAllMsg.value = { kind: 'ok', text: 'Testing engines (dry run, best health first)… can take a few minutes.' }
  try {
    const t = await api.projectTestAll(selected.value, 12)
    const winners: string[] = []
    const losers: string[] = []
    for (const r of t.results) {
      const name = r.engine || r.engine_id || 'engine'
      const ok = t && (r.report?.successful > 0 || r.ok === true)
      ;(ok ? winners : losers).push(name)
    }
    testAllMsg.value = {
      kind: ok_list(t, winners) ? 'ok' : 'err',
      text:
        `${t.ok}/${t.tested} engines created a link. ` +
        (winners.length ? `✓ ${winners.join(', ')} — ` : '') +
        (losers.length ? `✗ ${losers.length} failed (health demoted, tried last now).` : ''),
    }
    await loadEngines()
  } catch (e: any) {
    testAllMsg.value = { kind: 'err', text: 'Test-all failed: ' + (e?.message || String(e)) }
  } finally {
    testAllBusy.value = false
  }
}
// small helper: true if any engine succeeded (for the ok/err badge)
function ok_list(t: TestAllResult, winners: string[]): boolean {
  return t.ok > 0 && winners.length > 0
}

// — T10 · background multi-project runs: live progress · cancel · global view —
const liveRun = ref<RunStatus | null>(null)
const globalRuns = ref<GlobalRuns | null>(null)
const cancelling = ref(false)
let runPollTimer: ReturnType<typeof setInterval> | null = null
const active = computed(() => liveRun.value?.active ?? null)
const liveCreated = computed(() => active.value?.links_created ?? 0)
const liveTarget = computed(() => active.value?.links_target ?? 0)
const livePct = computed(() => (liveTarget.value > 0 ? Math.min(100, Math.round((liveCreated.value / liveTarget.value) * 100)) : 0))
function rejectReasons(): string[] {
  const r = active.value?.rejections
  if (!r) return []
  const out: string[] = []
  if (r.target) out.push(`${r.target} target-reached`)
  if (r.cooling) out.push(`${r.cooling} cooling-down`)
  if (r.in_flight_cap) out.push(`${r.in_flight_cap} in-flight cap`)
  if (r.quota) out.push(`${r.quota} fair-share quota`)
  return out
}
async function refreshRunStatus() {
  if (!selected.value) return
  try { liveRun.value = await api.projectRunStatus(selected.value) } catch { liveRun.value = null }
}
async function refreshGlobalRuns() {
  try { globalRuns.value = await api.globalRuns() } catch { globalRuns.value = null }
}
function startRunPoll() {
  stopRunPoll()
  refreshRunStatus()
  refreshGlobalRuns()
  runPollTimer = setInterval(() => { refreshRunStatus(); refreshGlobalRuns() }, 2500)
}
function stopRunPoll() {
  if (runPollTimer != null) { clearInterval(runPollTimer); runPollTimer = null }
}
async function cancelRun() {
  if (!selected.value) return
  cancelling.value = true
  try {
    const r = await api.projectRunCancel(selected.value)
    autoFail.value = false
    autoMsg.value =
      `⏹ Stopping… force-stop requested` +
      (r.pending_cancelled ? `, ${r.pending_cancelled} queued job(s) cancelled` : '') +
      ` — in-flight work drains, then it stops.`
    startRunPoll()
  } catch (e: any) {
    autoFail.value = true
    autoMsg.value = 'Cancel failed: ' + (e?.message || String(e))
  } finally {
    cancelling.value = false
  }
}

// — T7 · auto-pick: step 1 — the system scores the catalog, samples,
//    live-verifies, and builds this project's engine list for you.
const pickBusy = ref(false)
const pickErr = ref('')
const pickTest = ref(true) // live dry-run verification ON by default
const pickKeepUnverified = ref(true) // failed engines STAY in the pool (default)
const pickPerFamily = ref(1000) // 1000 = every posting engine in a family
const pickMaxTotal = ref(5000) // 5000 = the whole catalog
const pickResult = ref<AutoPickResult | null>(null)

async function autoPick() {
  if (!selected.value) return
  pickBusy.value = true
  pickErr.value = ''
  pickResult.value = null
  try {
    pickResult.value = await api.projectAutoPick(selected.value, {
      test: pickTest.value,
      keep_unverified: pickKeepUnverified.value,
      per_family: pickPerFamily.value,
      max_total: pickMaxTotal.value,
    })
    await loadEngines()
  } catch (e: any) {
    pickErr.value = 'Auto-pick failed: ' + (e?.message || String(e))
  } finally {
    pickBusy.value = false
  }
}

async function checkCaptcha() {
  captchaBusy.value = true
  captchaMsg.value = null
  try {
    const c = await api.captchaTest()
    captchaMsg.value = {
      kind: c.armed ? 'ok' : 'err',
      text:
        c.armed
          ? `✓ solver armed (${c.provider}) — ${c.detail}`
          : `✗ ${c.detail}` +
            (c.probes?.length ? ' · ' + c.probes.map((p) => `${p.target}: ${p.ok ? 'ok' : 'no'}`).join(' | ') : ''),
    }
  } catch (e: any) {
    captchaMsg.value = { kind: 'err', text: 'Captcha check failed: ' + (e?.message || String(e)) }
  } finally {
    captchaBusy.value = false
  }
}
watch(hasActiveJob, (on) => {
  if (on && pollTimer.value == null) {
    pollTimer.value = window.setInterval(() => {
      if (selected.value) loadJobs()
    }, 3000)
  } else if (!on && pollTimer.value != null) {
    window.clearInterval(pollTimer.value)
    pollTimer.value = null
  }
})

// — results tab —
const results = ref<ProjectResult[]>([])
const stats = ref<ResultStats | null>(null)
const resultsKind = ref<'' | 'success' | 'info' | 'error' | 'rejected'>('')
const resultsLoaded = ref(false)
const resultsBusy = ref(false)

async function loadResults() {
  if (!selected.value) return
  resultsBusy.value = true
  try {
    const [rs, st] = await Promise.all([
      api.projectResults(selected.value, resultsKind.value || undefined, 200),
      api.projectStats(selected.value),
    ])
    results.value = rs
    stats.value = st
    resultsLoaded.value = true
    error.value = null
  } catch (e: any) {
    resultsLoaded.value = false
    error.value = e?.message ?? String(e)
  } finally {
    resultsBusy.value = false
  }
}

// — master load —
async function load() {
  try {
    projects.value = await api.projectList()
    if (!selected.value && projects.value.length) {
      selected.value = projects.value[0].id
    }
    error.value = null
  } catch (e: any) {
    error.value = e?.message ?? String(e)
  } finally {
    await nextTick()
    if (selected.value) {
      loadEngines()
      loadKeywords()
      loadJobs()
      loadResults()
      loadCatalog()
      loadMailbox()
      loadUseProxies(selected.value)
      loadPolish(selected.value)
      openLiveLog(selected.value)
      startRunPoll()
    }
  }
}

async function selectProject(p: Project) {
  selected.value = p.id
  liveRun.value = null
  loadEngines()
  loadKeywords()
  loadJobs()
  loadResults()
  loadUseProxies(p.id)
  loadPolish(p.id)
  openLiveLog(p.id)
  startRunPoll()
}

async function createProject() {
  if (!name.value.trim()) return
  busy.value = true
  try {
    const p = await api.projectCreate({ name: name.value.trim(), description: desc.value.trim() || undefined })
    created.value = p
    name.value = ''
    desc.value = ''
    await load()
    selected.value = p.id
    await nextTick()
    loadEngines()
    loadKeywords()
    loadJobs()
    loadResults()
  } catch (e: any) {
    error.value = e?.message ?? String(e)
  } finally {
    busy.value = false
  }
}

// — project rename / delete —
const editProject = ref<Project | null>(null)
const editProjectName = ref('')
const editProjectDesc = ref('')
const editBusy = ref(false)

function startEditProject(p: Project) {
  editProject.value = p
  editProjectName.value = p.name
  editProjectDesc.value = p.description ?? ''
}

async function saveProjectEdit() {
  if (!editProject.value) return
  editBusy.value = true
  try {
    await api.projectUpdate(editProject.value.id, {
      name: editProjectName.value.trim() || editProject.value.name,
      description: editProjectDesc.value.trim(),
    })
    editProject.value = null
    await load()
  } catch (e: any) {
    error.value = e?.message ?? String(e)
  } finally {
    editBusy.value = false
  }
}

async function deleteProject(p: Project) {
  if (!confirm(`Delete project “${p.name}”? This removes its engines, keywords, and jobs.`)) return
  try {
    await api.projectDelete(p.id)
    if (selected.value === p.id) selected.value = ''
    if (editProject.value?.id === p.id) editProject.value = null
    await load()
  } catch (e: any) {
    error.value = e?.message ?? String(e)
  }
}

function onProjectChange() {
  kwNotice.value = ''
  editEngine.value = null
  jobResultsFor.value = null
  liveRun.value = null
  if (!selected.value) return
  loadEngines()
  loadKeywords()
  loadJobs()
  loadResults()
  loadUseProxies(selected.value)
  loadPolish(selected.value)
  if (tab.value === 'options') loadProjectOptions()
  startRunPoll()
}

// — options tab (F#19: GSA "Data" tab + "Anchor Text" tab) —
const optsBusy = ref(false)
const optsLoaded = ref(false)
const optsSaved = ref(false)
const optsErr = ref<string | null>(null)
// GSA "Data" tab — the backlink target URL (spintax allowed).
const optsUrl = ref('')
const optsUseRandomUrl = ref(false)
const optsUrlVariations = ref(0)
const optsRootUrl = ref(0)
// Multi-Tier linkwheel (upstream project ids whose verified URLs are used).
const optsLinkwheel = ref<string[]>([])
const optsAutoExport = ref(false)
// Anchor pool (GSA "Anchor Text (below the keyword)" tab) — PoolConfig JSON.
const optsSecondary = ref('')
const optsBranding = ref('')
const optsLsi = ref('')
const optsGeneric = ref('')
const optsDomain = ref(false)
const optsPctSecondary = ref(0)
const optsPctBranding = ref(0)
const optsPctLsi = ref(0)
const optsPctGeneric = ref(0)
const optsPctDomain = ref(0)
const optsVariationsPct = ref(20)
const optsDomainWithKeyword = ref(false)
// verified URL list (display + linkwheel source).
const optsVerified = ref<string[]>([])

function splitPool(s: string): string[] {
  return s.split(/\n|,|\|/).map((x) => x.trim()).filter(Boolean)
}

async function loadProjectOptions() {
  if (!selected.value) return
  optsSaved.value = false
  try {
    const p: any = await api.projectGet(selected.value)
    optsUrl.value = p.url || ''
    optsUseRandomUrl.value = !!p.use_random_url
    optsUrlVariations.value = p.url_variations ?? 0
    optsRootUrl.value = p.root_url ?? 0
    optsLinkwheel.value = p.linkwheel ?? []
    optsAutoExport.value = !!p.auto_export
    const cfg = p.anchor_mix ? JSON.parse(p.anchor_mix) : {}
    optsSecondary.value = (cfg.secondary || []).join('\n')
    optsBranding.value = (cfg.branding || []).join('\n')
    optsLsi.value = (cfg.lsi || []).join('\n')
    optsGeneric.value = (cfg.generic || []).join('\n')
    optsDomain.value = !!cfg.domain
    optsPctSecondary.value = cfg.percentages?.secondary ?? 0
    optsPctBranding.value = cfg.percentages?.branding ?? 0
    optsPctLsi.value = cfg.percentages?.lsi ?? 0
    optsPctGeneric.value = cfg.percentages?.generic ?? 0
    optsPctDomain.value = cfg.percentages?.domain ?? 0
    optsVariationsPct.value = cfg.variations_pct ?? 20
    optsDomainWithKeyword.value = !!cfg.domain_with_keyword
    optsVerified.value = await api.projectVerifiedUrls(selected.value, 500)
    optsLoaded.value = true
    optsErr.value = null
  } catch (e: any) {
    optsErr.value = e?.message ?? String(e)
  }
}

function buildPoolConfig(): string {
  return JSON.stringify({
    secondary: splitPool(optsSecondary.value),
    branding: splitPool(optsBranding.value),
    lsi: splitPool(optsLsi.value),
    generic: splitPool(optsGeneric.value),
    domain: optsDomain.value,
    percentages: {
      secondary: optsPctSecondary.value,
      branding: optsPctBranding.value,
      lsi: optsPctLsi.value,
      generic: optsPctGeneric.value,
      domain: optsPctDomain.value,
    },
    variations_pct: optsVariationsPct.value,
    domain_with_keyword: optsDomainWithKeyword.value,
  })
}

async function saveProjectOptions() {
  if (!selected.value) return
  optsBusy.value = true
  optsSaved.value = false
  try {
    if (optsUrl.value.trim()) await api.projectSetUrl(selected.value, optsUrl.value.trim())
    await api.projectSetAnchorOptions(selected.value, {
      anchor_mix: buildPoolConfig(),
      use_random_url: optsUseRandomUrl.value,
      url_variations: optsUrlVariations.value,
      root_url: optsRootUrl.value,
      linkwheel: optsLinkwheel.value,
      auto_export: optsAutoExport.value,
    })
    optsSaved.value = true
    optsErr.value = null
  } catch (e: any) {
    optsErr.value = e?.message ?? String(e)
  } finally {
    optsBusy.value = false
  }
}

watch(
  tab,
  (t) => { if (t === 'options' && selected.value) loadProjectOptions() },
)

onMounted(load)
onBeforeUnmount(() => {
  if (pollTimer.value != null) window.clearInterval(pollTimer.value)
  stopRunPoll()
  closeLiveLog()
})

function fmt(iso: string | null) {
  if (!iso) return '—'
  try { return new Date(iso).toLocaleString() } catch { return iso }
}
function cls(status: string) {
  return { pending: 'badge pending', running: 'badge running', done: 'badge done', failed: 'badge failed', rejected: 'badge rejected', cancelled: 'badge cancelled' }[status] || 'badge'
}
</script>

<template>
  <div>
    <div class="page-head">
      <div>
        <h1>Projects</h1>
        <p>
          Full CRUD on a project: engines · keywords · jobs (live status + cancel) · results (per-project
          success / info / error / rejected stats). Feature #11.
        </p>
      </div>
    </div>

    <div class="error" v-if="error">{{ error }}</div>

    <div class="card" style="margin-bottom:16px">
      <div class="row" style="gap:10px;align-items:center">
        <input v-model="name" placeholder="New project name" style="flex:1;min-width:200px" @keydown.enter="createProject" />
        <input v-model="desc" placeholder="description (optional)" style="flex:1;min-width:200px" />
        <button :disabled="busy || !name.trim()" @click="createProject">{{ busy ? 'Creating…' : 'Create project' }}</button>
      </div>
    </div>

    <div class="card" style="padding:0;overflow:hidden">
      <div style="padding:10px 14px;border-bottom:1px solid var(--border);display:flex;gap:10px;align-items:center">
        <input v-model="projFilter" placeholder="🔍 filter projects by name… (search 'Phase-1')" style="flex:1;max-width:420px" />
        <span class="muted" style="font-size:12px;white-space:nowrap">{{ filteredProjects.length }} / {{ projects.length }} shown</span>
      </div>
      <table class="table">
        <thead>
          <tr>
            <th>Name</th><th>Description</th><th class="spacer"></th><th>Created</th><th></th>
          </tr>
        </thead>
        <tbody>
          <tr v-if="!projects.length">
            <td colspan="5" class="muted" style="text-align:center;padding:26px">no projects yet — create one above</td>
          </tr>
          <tr
            v-for="p in filteredProjects"
            :key="p.id"
            style="cursor:pointer"
            :style="selected === p.id ? 'background:rgba(16,185,129,.08)' : ''"
            @click="selectProject(p)"
          >
            <td style="font-weight:600">
              {{ p.name }}
              <span v-if="(p as any).use_proxies" class="badge accent" style="margin-left:6px">⇌ proxies</span>
            </td>
            <td class="muted">{{ p.description || '—' }}</td>
            <td></td>
            <td class="mono muted">{{ fmt(p.created_at) }}</td>
            <td style="white-space:nowrap">
              <button class="muted" title="rename" @click.stop="startEditProject(p)">✎</button>
              <button class="muted" title="delete" @click.stop="deleteProject(p)">✕</button>
            </td>
          </tr>
        </tbody>
      </table>
    </div>

    <div v-if="editProject" class="card" style="margin-top:12px">
      <div class="row" style="gap:10px;align-items:center">
        <input v-model="editProjectName" style="flex:1;min-width:200px" placeholder="name" />
        <input v-model="editProjectDesc" style="flex:1;min-width:180px" placeholder="description" />
        <button :disabled="editBusy" @click="saveProjectEdit">{{ editBusy ? 'Saving…' : 'Save' }}</button>
        <button class="muted" @click="editProject = null">Cancel</button>
      </div>
    </div>

    <div v-if="selected" style="margin-top:18px">
      <div class="tabs" style="display:flex;gap:4px;align-items:center;border-bottom:1px solid #2a2f3a;padding-bottom:8px">
        <button
          v-for="t in (['engines', 'keywords', 'jobs', 'results', 'options'] as const)"
          :key="t"
          :style="tab === t ? 'padding:6px 14px;border-radius:8px;font-weight:600;background:rgba(16,185,129,.12);border:1px solid rgba(16,185,129,.4);cursor:pointer' : 'padding:6px 14px;border-radius:8px;background:transparent;border:1px solid #2a2f3a;color:#9aa4b2;cursor:pointer;text-transform:capitalize'"
          @click="tab = t"
        >
          {{ t }}
        </button>
        <span class="muted" style="margin-left:auto;align-self:center;white-space:nowrap;display:flex;gap:8px;align-items:center">
          Project
          <select v-model="selected" @change="onProjectChange" style="max-width:240px">
            <option v-for="p in projects" :key="p.id" :value="p.id">{{ p.name }}</option>
          </select>
        </span>
      </div>

      <!-- T7/T6 · THE FLOW — always on top: pick engines, build links, watch it live -->
              <!-- T7 · auto-pick — STEP 1: the system builds your engine list for you -->
        <div class="card" style="margin-top:12px;border:1.5px solid rgba(56,189,248,0.7);background:linear-gradient(180deg,rgba(56,189,248,0.08),rgba(56,189,248,0.02))">
          <div style="display:flex;align-items:center;justify-content:space-between;flex-wrap:wrap;gap:12px">
            <div>
              <strong>🎯 Step 1 · Auto-pick engines — the system does the selecting</strong>
              <p class="muted" style="margin:3px 0 0;max-width:64vw">
                Scores the whole engine catalog by family (article / comment /
                link), samples only the top of each, and — with live
                verification on — dry-runs them and keeps just the engines that
                actually create a link. Failed ones are disabled, not deleted,
                and each gets a reason you can audit. Then you press
                ⚡ Auto-build below.
              </p>
            </div>
            <div style="display:flex;gap:10px;align-items:center;flex-wrap:wrap">
              <label class="muted" style="display:flex;gap:6px;align-items:center;cursor:pointer"
                     title="live dry-run each sampled engine in parallel (12 at a time); verifies without removing — rejected engines stay in the pool">
                <input type="checkbox" v-model="pickTest" />
                live-verify (recommended)
              </label>
              <label class="muted" style="display:flex;gap:6px;align-items:center;cursor:pointer"
                     title="engines that FAIL the live test stay in the run pool (default — run as many as possible). Uncheck to keep only proven engines.">
                <input type="checkbox" v-model="pickKeepUnverified" />
                keep unverified in pool
              </label>
              <label class="muted" style="display:flex;gap:6px;align-items:center">
                per family
                <select v-model.number="pickPerFamily" style="width:88px">
                  <option :value="4">4</option>
                  <option :value="16">16</option>
                  <option :value="64">64</option>
                  <option :value="256">256</option>
                  <option :value="1000">all</option>
                </select>
              </label>
              <label class="muted" style="display:flex;gap:6px;align-items:center" title="cap on total engines sampled">
                max total
                <select v-model.number="pickMaxTotal" style="width:88px">
                  <option :value="24">24</option>
                  <option :value="128">128</option>
                  <option :value="512">512</option>
                  <option :value="2000">2000</option>
                  <option :value="5000">all</option>
                </select>
              </label>
              <button
                class="primary"
                style="padding:10px 20px;font-weight:700"
                :disabled="pickBusy"
                @click="autoPick"
                :title="pickTest ? 'score catalog, sample, live-verify, add proven engines' : 'score catalog, sample, add best engines (no live test)'"
              >
                {{ pickBusy ? (pickTest ? 'Picking & verifying live…' : 'Picking…') : '🎯 Auto-pick' }}
              </button>
            </div>
          </div>
          <div v-if="pickErr" style="margin-top:10px;color:#f87171;font-size:13px">{{ pickErr }}</div>
          <div v-if="pickResult" style="margin-top:12px">
            <!-- pipeline summary chips -->
            <div style="display:flex;gap:8px;flex-wrap:wrap;align-items:center;font-size:13px">
              <span class="muted" style="padding:4px 10px;border:1px solid var(--border);border-radius:999px">
                {{ pickResult.considered }} considered
              </span>
              <span class="muted">→</span>
              <span class="muted" style="padding:4px 10px;border:1px solid var(--border);border-radius:999px">
                {{ pickResult.sampled }} sampled
              </span>
              <span class="muted">→</span>
              <span style="padding:4px 10px;border-radius:999px;border:1px solid rgba(34,197,94,0.5);color:#86efac;font-weight:700">
                ✓ {{ pickResult.verified.length }} live-verified
              </span>
              <span v-if="pickResult.failed.length" style="padding:4px 10px;border-radius:999px;border:1px solid rgba(239,68,68,0.4);color:#fca5a5">
                ✗ {{ pickResult.failed.length }} rejected
              </span>
            </div>
            <!-- audited verdicts -->
            <div style="display:grid;grid-template-columns:1fr 1fr;gap:12px;margin-top:12px">
              <div style="border:1px solid rgba(34,197,94,0.35);border-radius:10px;overflow:hidden">
                <div style="padding:8px 12px;background:rgba(34,197,94,0.08);font-weight:700;font-size:13px">
                  ✓ Verified engines (added to this project)
                </div>
                <div v-if="!pickResult.verified.length" class="muted" style="padding:10px 12px;font-size:12px">
                  none yet — raise "per family" / "max total" and pick again
                </div>
                <div v-for="e in pickResult.verified" :key="'v'+e.engine"
                     style="display:flex;justify-content:space-between;gap:8px;padding:7px 12px;font-size:12.5px;border-top:1px solid var(--border)">
                  <span><span class="muted" style="font-size:11px;margin-right:6px;text-transform:uppercase">{{ e.family }}</span>{{ e.engine }}</span>
                  <span class="muted" style="overflow:hidden;text-overflow:ellipsis;white-space:nowrap;max-width:45%">{{ e.url || '' }}</span>
                </div>
              </div>
              <div style="border:1px solid rgba(239,68,68,0.3);border-radius:10px;overflow:hidden">
                <div style="padding:8px 12px;background:rgba(239,68,68,0.06);font-weight:700;font-size:13px">
                  ✗ Rejected (disabled — auditable, re-enableable)
                </div>
                <div v-if="!pickResult.failed.length" class="muted" style="padding:10px 12px;font-size:12px">
                  clean sweep — nothing failed
                </div>
                <div v-for="e in pickResult.failed" :key="'f'+e.engine"
                     style="display:flex;justify-content:space-between;gap:8px;padding:7px 12px;font-size:12.5px;border-top:1px solid var(--border)">
                  <span style="white-space:nowrap"><span class="muted" style="font-size:11px;margin-right:6px;text-transform:uppercase">{{ e.family }}</span>{{ e.engine }}</span>
                  <span class="muted" style="overflow:hidden;text-overflow:ellipsis;white-space:nowrap;max-width:60%" :title="e.reason || ''">{{ e.reason || '' }}</span>
                </div>
              </div>
            </div>
            <!-- next step -->
            <div style="margin-top:12px;padding:10px 14px;border-radius:10px;font-size:13.5px;font-weight:600"
                 :style="pickResult.verified.length
                        ? 'background:rgba(34,197,94,0.1);color:#86efac;border:1px solid rgba(34,197,94,0.4)'
                        : 'background:rgba(245,158,11,0.08);color:#fcd34d;border:1px solid rgba(245,158,11,0.4)'">
              <span v-if="pickResult.verified.length">
                ✓ Engine list ready — {{ pickResult.verified.length }} proven engine(s) added.
                Scroll down and press ⚡ Auto-build to create your links.
              </span>
              <span v-else>
                ⚠ Nothing verified this round — widen the sample (per family / max total) and Auto-pick again.
              </span>
            </div>
            <p class="muted" style="margin:8px 0 0;font-size:12px">{{ pickResult.note }}</p>
          </div>
        </div>
        <!-- T6 · the auto UX — THE way to build backlinks -->
        <div class="card" style="margin-top:12px;border:1.5px solid rgba(16,185,129,0.7);background:linear-gradient(180deg,rgba(16,185,129,0.07),rgba(16,185,129,0.02))">
          <div style="display:flex;align-items:center;justify-content:space-between;flex-wrap:wrap;gap:12px">
            <div>
              <strong>⚡ Auto-build backlinks — the system picks the engines</strong>
              <p class="muted" style="margin:3px 0 0;max-width:62vw">
                No engine selection: we rank this project's engines by
                real-world health (proven successes first, captcha walls
                last) and try them — first link created wins, then we stop.
                Watch it happen live in the log below.
              </p>
            </div>
            <div style="display:flex;gap:8px;align-items:center;flex-wrap:wrap">
              <label class="muted" style="display:flex;gap:6px;align-items:center">
                links
                <select v-model.number="autoLinks" style="width:88px">
                  <option :value="10">10</option>
                  <option :value="25">25</option>
                  <option :value="50">50</option>
                  <option :value="100">100</option>
                  <option :value="250">250</option>
                  <option :value="500">500</option>
                  <option :value="1000">1000</option>
                </select>
              </label>
              <label class="muted" style="display:flex;gap:6px;align-items:center">
                use engines:
                <select v-model.number="autoEngines" style="width:96px">
                  <option :value="0">all (every working one)</option>
                  <option :value="12">12</option>
                  <option :value="24">24</option>
                  <option :value="50">50</option>
                </select>
              </label>
              <button
                class="primary"
                style="padding:10px 20px;font-weight:700"
                :disabled="autoBusy"
                @click="runAuto"
                title="rank engines by health, try them until a link is created"
              >
                {{ autoBusy ? 'Building… (watch the log)' : '⚡ Auto-build' }}
              </button>
            </div>
          </div>
          <div v-if="autoMsg" style="margin-top:10px" class="muted" :style="autoFail ? 'color:#f87171' : 'color:#86efac'">
            {{ autoMsg }}
          </div>
        </div>

        <!-- T10 · LIVE RUN — the run's real-time progress, visible at any point, cancellable -->
        <div class="card" style="margin-top:12px;border:1.5px solid rgba(16,185,129,0.65);background:linear-gradient(180deg,rgba(16,185,129,0.10),rgba(16,185,129,0.02))">
          <div style="display:flex;align-items:center;gap:12px;flex-wrap:wrap">
            <span
              :style="active ? 'width:10px;height:10px;border-radius:50%;background:#22c55e;display:inline-block;box-shadow:0 0 9px #22c55e' : 'width:10px;height:10px;border-radius:50%;background:#475569;display:inline-block'"
              :title="active ? 'running in the background' : 'idle'"
            ></span>
            <div style="flex:1;min-width:240px">
              <strong>
                🛰 Live run — <span v-if="active" style="color:#86efac">{{ liveCreated }} / {{ liveTarget }}</span><span v-else style="color:#9aa4b2;font-weight:600">idle</span>
              </strong>
              <p class="muted" style="margin:2px 0 0">
                <span v-if="active">Runs in the background — switch projects, do other work, and come back anytime. Progress + per-engine detail below, refreshed every 2.5s.</span>
                <span v-else>No active run for this project. Press ⚡ Auto-build to launch one; it stays running even if you navigate away.</span>
              </p>
            </div>
            <div v-if="globalRuns" style="display:flex;gap:8px;align-items:center;flex-wrap:wrap">
              <span class="muted" style="display:inline-flex;align-items:center;padding:4px 10px;border:1px solid var(--border);border-radius:999px;font-size:12px">
                🖥 {{ globalRuns.active_runs }}/{{ globalRuns.capacity }} project slots busy
              </span>
              <span
                v-if="globalRuns.runs.some(r => r.project_id !== selected)"
                style="display:inline-flex;align-items:center;padding:4px 10px;border:1px solid rgba(56,189,248,0.5);border-radius:999px;font-size:12px;color:#7dd3fc"
                title="Other projects with in-flight runs right now"
              >
                ＋{{ globalRuns.runs.filter(r => r.project_id !== selected).length }} other project(s) running
              </span>
            </div>
            <button
              v-if="active"
              :disabled="cancelling"
              style="padding:9px 18px;font-weight:700;background:rgba(239,68,68,0.14);border:1.5px solid rgba(239,68,68,0.55);color:#fca5a5;border-radius:9px;cursor:pointer"
              @click="cancelRun"
              title="Soft stop: stops admitting new work and cancels queued jobs; in-flight work drains shortly"
            >
              {{ cancelling ? 'Stopping…' : '⏹ Cancel run' }}
            </button>
          </div>

          <div v-if="active" style="margin-top:12px;height:12px;border-radius:999px;background:rgba(255,255,255,0.08);overflow:hidden">
            <div style="height:100%;border-radius:999px;background:linear-gradient(90deg,#16a34a,#4ade80);transition:width .5s" :style="'width:' + livePct + '%'"></div>
          </div>
          <div v-if="active" style="margin-top:7px;display:flex;gap:15px;flex-wrap:wrap;font-size:12.5px;align-items:center">
            <span class="muted">target {{ liveTarget }}</span>
            <span style="color:#86efac;font-weight:700">{{ liveCreated }} created</span>
            <span class="muted">{{ active.in_flight ?? 0 }} in-flight</span>
            <span class="muted">{{ active.cooling_down ?? 0 }} cooling</span>
            <span v-if="rejectReasons().length" style="color:#fcd34d">rejected {{ rejectReasons().join(' · ') }}</span>
          </div>

          <div v-if="active && active.engines_json && active.engines_json.length" style="margin-top:10px;max-height:190px;overflow:auto;border:1px solid var(--border);border-radius:8px">
            <table class="table" style="margin:0">
              <thead>
                <tr><th>Engine</th><th>Links</th><th>Admitted</th><th>Rate hits</th><th>Cool</th></tr>
              </thead>
              <tbody>
                <tr v-for="e in active.engines_json" :key="'er' + e.engine">
                  <td style="white-space:nowrap">{{ e.engine }}</td>
                  <td style="font-weight:700;color:#86efac">{{ e.links }}</td>
                  <td class="muted">{{ e.admitted ?? 0 }}</td>
                  <td class="muted">{{ e.rate_hits ?? 0 }}</td>
                  <td class="muted">{{ e.cool_remaining_secs ? (e.cool_remaining_secs + 's') : '—' }}</td>
                </tr>
              </tbody>
            </table>
          </div>

          <div v-if="liveRun && liveRun.last_run" style="margin-top:10px;font-size:12.5px" class="muted">
            Previous run: {{ liveRun.last_run.status }} @ {{ fmt(liveRun.last_run.finished_at) }}
            {{ liveRun.last_run.summary.links_created != null ? ('— ' + liveRun.last_run.summary.links_created + ' link(s) created') : '' }}.
          </div>
        </div>

        <!-- T6 · live run log (SSE + DB catch-up) -->
        <div class="card" style="margin-top:12px;padding:0;overflow:hidden">
          <div style="display:flex;align-items:center;gap:10px;padding:10px 14px;border-bottom:1px solid var(--border)">
            <span
              :style="liveConnected ? 'width:9px;height:9px;border-radius:50%;background:#22c55e;display:inline-block' : 'width:9px;height:9px;border-radius:50%;background:#f59e0b;display:inline-block'"
              :title="liveConnected ? 'live' : 'reconnecting…'"
            ></span>
            <strong style="font-size:13px">Live run log</strong>
            <span class="muted" style="font-size:12px">
              {{ liveEvents.length }} event(s) · streams in real time
            </span>
            <span style="margin-left:auto;display:flex;gap:8px">
              <button
                class="muted"
                :disabled="captchaBusy"
                @click="checkCaptcha"
                title="verify the configured captcha solver account"
              >
                {{ captchaBusy ? 'Checking…' : '🛡 check captcha solver' }}
              </button>
              <button
                class="muted"
                :disabled="testAllBusy"
                @click="testAllEngines"
                title="dry-run every engine and report which create a link"
              >
                {{ testAllBusy ? 'Testing…' : '🧪 test all engines' }}
              </button>
              <button class="muted" @click="liveEvents = []">clear</button>
              <button class="muted" @click="liveLogOpen = !liveLogOpen">
                {{ liveLogOpen ? 'hide' : 'show' }}
              </button>
            </span>
          </div>
          <div
            v-if="liveLogOpen"
            style="height:190px;overflow:auto;padding:10px 14px;background:#0b0f14;font-family:ui-monospace,Consolas,monospace;font-size:12px;line-height:1.55"
          >
            <div v-if="!liveEvents.length" class="muted" style="padding:6px 0">
              idle — press ⚡ Auto-build and watch every step appear here as it happens
            </div>
            <div v-for="ev in liveEvents" :key="ev.id" style="white-space:pre-wrap">
              <span class="muted">{{ new Date(ev.ts).toLocaleTimeString() }}</span>
              <span :class="levelClass(ev.level)" style="font-weight:700">
                {{ ev.level.toUpperCase().padEnd(7) }}
              </span>
              <span v-if="ev.engine" style="color:#7dd3fc">[{{ ev.engine }}]</span>
              <span :style="ev.level === 'error' ? 'color:#fca5a5' : ev.level === 'success' ? 'color:#86efac' : ev.level === 'captcha' ? 'color:#fcd34d' : ''">
                {{ ev.message }}
              </span>
              <div style="height:2px"></div>
            </div>
          </div>
          <div v-if="testAllMsg" style="padding:8px 14px" class="muted" :style="testAllMsg.kind === 'err' ? 'color:#fca5a5' : ''">{{ testAllMsg.text }}</div>
          <div v-if="captchaMsg" style="padding:0 14px 8px" class="muted" :style="captchaMsg.kind === 'err' ? 'color:#fca5a5' : ''">{{ captchaMsg.text }}</div>
        </div>

      <!-- Engines -->
      <div v-if="tab === 'engines'">
        <div class="card" style="margin-top:12px">
          <div style="display:flex;gap:10px;align-items:center;flex-wrap:wrap">
            <input
              v-model="catSearch"
              placeholder="Filter the engine catalog…"
              style="flex:1;min-width:220px"
            />
            <div style="flex:2;min-width:260px;display:flex;gap:6px;align-items:center">
              <select v-if="catalogFiltered.length" style="flex:1">
                <option v-for="c in catalogFiltered.slice(0, 200)" :key="c.file_path" :value="c.file_path">
                  {{ c.name }} ({{ c.kind }}, {{ c.step_count }} steps)
                </option>
              </select>
              <button
                class="primary"
                :disabled="enginesBusy || !catalogFiltered.length"
                @click="addCatalogEngine(catalogFiltered[0])"
              >
                {{ 'Add highlighted' }}
              </button>
            </div>
          </div>
          <div style="max-height:180px;overflow:auto;border:1px solid #232834;border-radius:8px;margin-top:8px">
            <table class="table">
              <thead><tr><th></th><th>Name</th><th>Type</th><th class="num">Steps</th><th>URL</th></tr></thead>
              <tbody>
                <tr
                  v-for="c in catalogFiltered.slice(0, 100)"
                  :key="c.file_path"
                  style="cursor:pointer"
                  @click="addCatalogEngine(c)"
                  title="click to add to this project"
                >
                  <td>⌂</td>
                  <td style="font-weight:600">{{ c.name }}</td>
                  <td><span class="chip">{{ c.kind }}</span></td>
                  <td class="num">{{ c.step_count }}</td>
                  <td class="muted" style="max-width:280px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap">{{ c.url }}</td>
                </tr>
              </tbody>
            </table>
          </div>
          <div style="display:flex;gap:10px;align-items:center;margin-top:12px;flex-wrap:wrap">
            <input v-model="customName" placeholder="or add a custom engine name" style="flex:1;min-width:220px" @keydown.enter="addCustomEngine()" />
            <input v-model="customType" placeholder="type (web/email)" style="min-width:120px" @keydown.enter="addCustomEngine()" />
            <input v-model="customUrl" placeholder="url" style="flex:1;min-width:180px" @keydown.enter="addCustomEngine()" />
            <button :disabled="enginesBusy || !customName.trim()" @click="addCustomEngine()">Add custom</button>
          </div>
        </div>

        <div style="height:12px"></div>
        <div class="card" style="padding:0;overflow:hidden">
          <table class="table">
            <thead>
              <tr>
                <th>Name</th><th>Type</th><th>Steps</th><th>URL</th>
                <th class="num" title="jobs this engine has run (any run or test)">Used</th>
                <th class="num" title="jobs that created a real link">Won</th>
                <th class="num" title="jobs blocked by a captcha wall">Captcha</th>
                <th>Enabled</th><th></th>
              </tr>
            </thead>
            <tbody>
              <tr v-if="!enginesLoaded || (enginesLoaded && !engines.length)">
                <td colspan="8" class="muted" style="text-align:center;padding:22px">
                  no engines on this project yet — add from the catalog or custom above
                </td>
              </tr>
              <tr v-for="e in engines" :key="e.id">
                <td style="font-weight:600">{{ e.engine_name }}</td>
                <td><span class="chip">{{ e.engine_type }}</span></td>
                <td class="num">{{ e.step_count }}</td>
                <td style="max-width:280px;word-break:break-all">
                  <a v-if="e.url" :href="e.url" target="_blank" rel="noreferrer">{{ e.url }}</a>
                  <span v-else class="muted">—</span>
                </td>
                <td class="num muted">{{ e.attempts ?? 0 }}</td>
                <td class="num" :style="(e.successes ?? 0) > 0 ? 'color:#86efac;font-weight:700' : 'opacity:.6'">{{ e.successes ?? 0 }}</td>
                <td class="num" :style="(e.captcha_failures ?? 0) > 0 ? 'color:#fcd34d' : 'opacity:.6'">{{ e.captcha_failures ?? 0 }}</td>
                <td>
                  <input
                    type="checkbox"
                    :checked="e.enabled"
                    :disabled="enginesBusy"
                    @change="toggleEngine(e)"
                  />
                </td>
                <td style="white-space:nowrap">
                  <button
                    class="muted"
                    :disabled="!!testRunning"
                    title="dry-run this engine end-to-end (no results saved)"
                    @click="testEngine(e.id)"
                  >{{ testRunning === e.id ? 'testing…' : '▶ test' }}</button>
                  <button class="muted" title="edit" @click="startEditEngine(e)">✎</button>
                  <button class="muted" title="delete" @click="deleteEngine(e)">✕</button>
                </td>
              </tr>
            </tbody>
          </table>
        </div>

        <!-- F#26: test-engine result -->
        <div
          v-if="testResult"
          class="card"
          style="margin-top:10px"
          :style="testResult.success ? 'border:1.5px solid rgba(22,163,74,.5)' : 'border:1.5px solid rgba(239,68,68,.5)'"
        >
          <div style="display:flex;justify-content:space-between;align-items:center;flex-wrap:wrap;gap:8px">
            <div>
              <strong :style="testResult.success ? 'color:#22c55e' : 'color:#ef4444'">
                {{ testResult.success ? '✓ Test succeeded' : '✗ Test failed' }}
              </strong>
              <span class="muted" style="font-size:12px;margin-left:8px">
                {{ engines.find((e) => e.id === testResult!.engineId)?.engine_name || testResult.engineId }}
              </span>
            </div>
            <span class="muted mono" style="font-size:12px">{{ testResult.attempts }} check{{ testResult.attempts === 1 ? '' : 's' }}</span>
          </div>
          <div class="muted" style="margin-top:6px;font-size:13px">
            <span v-if="testResult.message">{{ testResult.message }}</span>
            <span v-else>no message</span>
          </div>
          <div class="row" style="gap:12px;margin-top:6px;font-size:12px;flex-wrap:wrap" v-if="testResult.url || testResult.anchor">
            <span v-if="testResult.anchor"><span class="muted">anchor:</span> {{ testResult.anchor }}</span>
            <a v-if="testResult.url" :href="testResult.url" target="_blank" rel="noreferrer" style="color:#60a5fa">{{ testResult.url }}</a>
          </div>
          <div style="margin-top:8px">
            <button class="muted" @click="testResult = null">close</button>
          </div>
        </div>

        <div v-if="editEngine" class="card" style="margin-top:10px">
          <div class="row" style="gap:10px;align-items:center;flex-wrap:wrap">
            <input v-model="engName" placeholder="name" style="flex:2;min-width:180px" />
            <input v-model="engType" placeholder="type" style="width:120px" />
            <input v-model.number="engSteps" type="number" min="0" placeholder="steps" style="width:100px" />
            <input v-model="engUrl" placeholder="url (optional)" style="flex:3;min-width:180px" />
            <button :disabled="enginesBusy" @click="saveEngine">{{ enginesBusy ? 'Saving…' : 'Save' }}</button>
            <button class="muted" @click="editEngine = null">Cancel</button>
          </div>
        </div>
      </div>

      <!-- Keywords -->
      <div v-if="tab === 'keywords'">
        <div class="card" style="margin-top:12px">
          <div class="row" style="gap:10px;align-items:center;margin-bottom:12px">
            <input v-model="kwInput" placeholder="e.g. SEO agency services" style="flex:1;min-width:200px" @keydown.enter="addKeyword" />
            <button :disabled="kwBusy || !kwInput.trim()" @click="addKeyword">Add</button>
          </div>
          <textarea
            v-model="bulkInput"
            rows="3"
            placeholder="bulk add — one keyword or one spintax keyword per line"
            style="width:100%;font-family:inherit;resize:vertical"
          ></textarea>
          <div class="row" style="justify-content:flex-end;margin-top:8px;gap:10px">
            <span v-if="kwNotice" class="muted" style="align-self:center">{{ kwNotice }}</span>
            <button :disabled="kwBusy || !bulkInput.trim()" @click="addBulk">Add all</button>
          </div>
        </div>
        <div style="height:12px"></div>
        <div class="card" style="padding:0;overflow:hidden">
          <table class="table">
            <thead>
              <tr><th>Keyword</th><th>Added</th><th></th></tr>
            </thead>
            <tbody>
              <tr v-if="!kwLoaded || (kwLoaded && !keywords.length)">
                <td colspan="3" class="muted" style="text-align:center;padding:22px">no keywords yet</td>
              </tr>
              <tr v-for="k in keywords" :key="k.id">
                <td style="font-weight:500">{{ k.text }}</td>
                <td class="mono muted">{{ fmt(k.added_at) }}</td>
                <td style="text-align:right">
                  <button title="delete" @click="deleteKeyword(k)" class="muted">✕</button>
                </td>
              </tr>
            </tbody>
          </table>
        </div>
      </div>

      <!-- Jobs -->
      <div v-if="tab === 'jobs'">
        <!-- F#13: proxy gate -->
        <div class="card" style="margin-top:12px;border:1.5px solid rgba(120,144,255,.45)">
          <div style="display:flex;align-items:center;justify-content:space-between;flex-wrap:wrap;gap:12px">
            <div>
              <strong>⇌ Route this project's jobs through the proxy pool (F#13)</strong>
              <p class="muted" style="margin:3px 0 0;max-width:62vw">
                When on, every engine visit (email verification too) goes through the enabled proxies on the
                <a href="/proxies">Proxies</a> page — round-robin, with one automatic re-roll to the next proxy
                when a failure looks like a dead proxy. Off (default) = direct connection.
              </p>
            </div>
            <div class="row" style="gap:8px;align-items:center">
              <label style="display:flex;gap:8px;align-items:center;font-size:13px;cursor:pointer">
                <input type="checkbox" v-model="useProxies" :disabled="useProxiesBusy || !useProxiesLoaded"
                       :style="{ width:'18px', height:'18px', accentColor:'var(--accent)' }"
                       @change="saveUseProxies" />
                {{ useProxies ? 'PROXIES ON' : 'proxies off' }}
              </label>
              <a class="muted" href="/proxies">manage proxies →</a>
            </div>
          </div>
        </div>

        <!-- F#26: Polish (notes · color · pause-after-N) -->
        <div class="card" style="margin-top:12px;border:1.5px solid rgba(168,85,247,.45)">
          <div style="display:flex;align-items:flex-start;justify-content:space-between;gap:20px;flex-wrap:wrap">
            <div style="flex:1;min-width:280px">
              <strong>🎨 Polish — notes, color tag, and run ceiling (F#26)</strong>
              <p class="muted" style="margin:3px 0 0;max-width:70vw">
                Sticky metadata for this project (like GSA's project notes + color dot) plus a
                <em>pause-after-N</em> ceiling: when a run succeeds this many submissions it stops
                automatically (the rest stays queued) so you can review, then <em>resume</em>.
              </p>

              <div style="display:flex;gap:16px;margin-top:12px;flex-wrap:wrap;align-items:flex-end">
                <label style="flex:1;min-width:240px;display:flex;flex-direction:column;gap:4px">
                  <span class="muted">Notes</span>
                  <textarea
                    v-model="polishNotes"
                    rows="2"
                    placeholder="internal note — this project targets …"
                    style="width:100%;font-family:inherit;resize:vertical"
                  ></textarea>
                </label>
                <label style="display:flex;flex-direction:column;gap:4px">
                  <span class="muted">Color tag</span>
                  <input
                    type="color"
                    v-model="polishColor"
                    :style="{ width:'52px', height:'34px', padding:'2px', background:'transparent', border:'1px solid #2a2f3a', borderRadius:'8px', cursor:'pointer' }"
                  />
                </label>
                <label style="display:flex;flex-direction:column;gap:4px;min-width:150px">
                  <span class="muted">Pause after N successes (0 = no limit)</span>
                  <input
                    type="number"
                    min="0"
                    v-model.number="polishPauseAfter"
                    placeholder="0"
                    style="width:150px"
                  />
                </label>
              </div>
            </div>

            <div style="display:flex;flex-direction:column;gap:8px;align-items:stretch">
              <div class="row" style="gap:10px;align-items:center">
                <button style="padding:8px 16px" :disabled="polishSaving" @click="savePolish">
                  {{ polishSaving ? 'Saving…' : (polishSaved ? '✓ Saved' : 'Save polish') }}
                </button>
                <button
                  class="muted"
                  :disabled="resuming || !isPaused"
                  @click="resumeRun"
                  :title="isPaused ? 'clear the pause ceiling and let the run continue' : 'set a pause-after-N ceiling first'"
                >
                  {{ resuming ? 'Clearing…' : '▶ Resume' }}
                </button>
              </div>
              <span v-if="isPaused" style="color:#f59e0b;font-weight:600;font-size:13px">
                ⏸ This project is PAUSED — run halted at the ceiling; resume to continue.
              </span>
              <span v-if="resumeMsg" class="muted" style="font-size:12px">{{ resumeMsg }}</span>
              <span v-if="polishErr" style="color:#ef4444;font-size:12px">{{ polishErr }}</span>
            </div>
          </div>
        </div>


        <div class="card" style="margin-top:12px;border:1.5px solid rgba(64,192,166,0.6)">
          <div style="display:flex;align-items:center;justify-content:space-between;flex-wrap:wrap;gap:12px">
            <div>
              <strong>▶ Run now — live email + web execution (F#12)</strong>
              <p class="muted" style="margin:3px 0 0;max-width:60vw">Drains this project's queued jobs in-process: web-form engines + WEB-inbox + POP3 email engines. The operator mailbox below is used for F#12 mail runs. Results stream to the Results tab.</p>
            </div>
            <button style="padding:9px 18px;font-weight:600" :disabled="!jobs.length || running" @click="runNow">{{ running ? 'Running…' : ('⚡ Run now (' + pendingCount + ' pending)' ) }}</button>
          </div>
          <hr style="border:0;border-top:1px solid var(--border);margin:14px 0" />
          <div class="row" style="gap:10px;align-items:flex-end;flex-wrap:wrap">
            <label style="flex:1;min-width:190px;display:flex;flex-direction:column;gap:4px">
              <span class="muted">Operator email (inbox to verify against)</span>
              <input v-model="mailbox.address" type="email" placeholder="you@example.com" />
            </label>
            <label style="flex:1;min-width:190px;display:flex;flex-direction:column;gap:4px">
              <span class="muted">Inbox / POP3 password</span>
              <input v-model="mailbox.password" type="password" placeholder="••••••••" />
            </label>
            <button style="padding:8px 16px" :disabled="!mailboxDirty || mailboxSaving" @click="saveMailbox">
              {{ mailboxSaving ? 'Saving…' : (mailboxSaved ? 'Saved ✓' : 'Save mailbox') }}
            </button>
          </div>
          <p class="muted" :class="runSummary ? 'ok' : ''" style="margin-top:12px">{{ runMsg }}</p>
        </div>
        <div style="height:12px"></div>
        <div class="card" style="margin-top:12px">
          <div class="row" style="gap:10px;align-items:center;flex-wrap:wrap">
            <label class="muted">engine</label>
            <select v-model="jobEngine" style="max-width:260px">
              <option value="">(any)</option>
              <option v-for="e in engines" :key="e.id" :value="e.id">{{ e.engine_name }}</option>
            </select>
            <label class="muted">keyword</label>
            <select v-model="jobKeyword" style="max-width:260px">
              <option value="">(any)</option>
              <option v-for="k in keywords" :key="k.id" :value="k.id">{{ k.text }}</option>
            </select>
            <button :disabled="jobsBusy" @click="createJob">{{ jobsBusy ? 'Enqueueing…' : 'Enqueue job' }}</button>
            <button class="muted" :disabled="jobsBusy" @click="loadJobs">↻ refresh</button>
          </div>
          <p class="muted" style="margin-top:6px">
            Jobs are claimed atomically by workers (F#8) and results land in the Results tab.
            Live status auto-refreshes every 3 s while any job is pending/running.
          </p>
        </div>
        <div style="height:12px"></div>
        <div class="card" style="padding:0;overflow:hidden">
          <table class="table">
            <thead>
              <tr>
                <th>Job</th><th>Status</th><th class="num">Attempts</th><th>Engine</th><th>Keyword</th>
                <th>Error</th><th>Created</th><th>Finished</th><th></th>
              </tr>
            </thead>
            <tbody>
              <tr v-if="!jobsLoaded || (jobsLoaded && !jobs.length)">
                <td colspan="9" class="muted" style="text-align:center;padding:22px">no jobs in this project</td>
              </tr>
              <tr v-for="j in jobs" :key="j.id">
                <td class="mono">{{ j.id.slice(0, 8) }}…</td>
                <td><span :class="cls(j.status)">{{ j.status }}</span></td>
                <td class="num">{{ j.attempts }}</td>
                <td class="muted">
                  <span v-if="j.engine_id">{{ engines.find((e) => e.id === j.engine_id)?.engine_name || j.engine_id.slice(0, 8) }}…</span>
                  <span v-else>—</span>
                </td>
                <td class="muted">
                  <span v-if="j.keyword_id">{{ keywords.find((k) => k.id === j.keyword_id)?.text || '—' }}</span>
                  <span v-else>—</span>
                </td>
                <td class="muted" style="max-width:240px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap">{{ j.last_error || '' }}</td>
                <td class="mono muted">{{ fmt(j.created_at) }}</td>
                <td class="mono muted">{{ fmt(j.finished_at) }}</td>
                <td style="white-space:nowrap">
                  <button
                    v-if="j.status === 'pending' || j.status === 'running'"
                    class="muted"
                    @click="cancelJob(j)"
                  >cancel</button>
                  <button class="muted" @click="openJobResults(j)">results</button>
                </td>
              </tr>
            </tbody>
          </table>
        </div>
        <div v-if="jobResultsFor" class="card" style="margin-top:10px">
          <div style="display:flex;justify-content:space-between;align-items:center;margin-bottom:8px">
            <div>
            <div style="font-weight:600">job {{ jobResultsFor.jobId.slice(0, 8) }}… results</div>
              <div class="muted" style="font-size:12px">{{ jobResultsFor.rows.length }} row(s)</div>
            </div>
            <button class="muted" @click="jobResultsFor = null">close</button>
          </div>
          <div style="max-height:240px;overflow:auto">
            <table class="table">
              <thead><tr><th>kind</th><th>url</th><th>anchor</th><th>message</th><th>at</th></tr></thead>
              <tbody>
                <tr v-for="(r, i) in jobResultsFor.rows" :key="i">
                  <td><span :class="'badge ' + r.kind">{{ r.kind }}</span></td>
                  <td>
                    <a v-if="r.url" :href="r.url" target="_blank" rel="noreferrer">{{ r.url }}</a>
                    <span v-else class="muted">—</span>
                  </td>
                  <td class="muted">{{ r.anchor || '—' }}</td>
                  <td class="muted">{{ r.message || '—' }}</td>
                  <td class="mono muted">{{ fmt(r.created_at) }}</td>
                </tr>
              </tbody>
            </table>
          </div>
        </div>
      </div>

      <!-- Results -->
      <div v-if="tab === 'results'">
        <div v-if="stats" style="display:grid;grid-template-columns:repeat(auto-fit,minmax(140px,1fr));gap:10px;margin-top:12px">
          <div class="card">
            <div style="font-size:28px;font-weight:700;color:#16a34a">{{ stats.success }}</div>
            <div class="muted" style="font-size:12px">success</div>
          </div>
          <div class="card">
            <div style="font-size:28px;font-weight:700;color:#3b82f6">{{ stats.info }}</div>
            <div class="muted" style="font-size:12px">info</div>
          </div>
          <div class="card">
            <div style="font-size:28px;font-weight:700;color:#ef4444">{{ stats.error }}</div>
            <div class="muted" style="font-size:12px">error</div>
          </div>
          <div class="card">
            <div style="font-size:28px;font-weight:700;color:#f59e0b">{{ stats.rejected }}</div>
            <div class="muted" style="font-size:12px">rejected</div>
          </div>
          <div class="card">
            <div style="font-size:28px;font-weight:700">{{ stats.unique_success_urls }}</div>
            <div class="muted" style="font-size:12px">unique success URLs</div>
          </div>
          <div class="card">
            <div style="font-size:28px;font-weight:700">{{ stats.total_jobs }}</div>
            <div class="muted" style="font-size:12px">jobs run</div>
          </div>
        </div>

        <div class="card" style="margin-top:12px">
          <div class="row" style="gap:10px;align-items:center">
            <select v-model="resultsKind" style="max-width:220px" @change="loadResults()">
              <option value="">all types</option>
              <option value="success">success</option>
              <option value="info">info</option>
              <option value="error">error</option>
              <option value="rejected">rejected</option>
            </select>
            <button class="muted" @click="loadResults()" :disabled="resultsBusy">↻ refresh</button>
            <span class="muted" v-if="resultsLoaded">showing first {{ results.length }} (max 200)</span>
          </div>
        </div>
        <div style="height:12px"></div>
        <div class="card" style="padding:0;overflow:hidden">
          <table class="table">
            <thead>
              <tr>
                <th>Kind</th><th>Engine</th><th>Keyword</th><th>URL</th><th>Anchor</th><th>Message</th><th>At</th>
              </tr>
            </thead>
            <tbody>
              <tr v-if="!resultsLoaded || (resultsLoaded && !results.length)">
                <td colspan="7" class="muted" style="text-align:center;padding:22px">no results captured yet</td>
              </tr>
              <tr v-for="r in results" :key="r.id">
                <td><span :class="'badge ' + r.kind">{{ r.kind }}</span></td>
                <td>{{ r.engine_name || '—' }}</td>
                <td class="muted">{{ r.keyword || '—' }}</td>
                <td style="max-width:280px;word-break:break-all">
                  <a v-if="r.url" :href="r.url" target="_blank" rel="noreferrer">{{ r.url }}</a>
                  <span v-else class="muted">—</span>
                </td>
                <td class="muted">{{ r.anchor || '—' }}</td>
                <td class="muted" style="max-width:260px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap">{{ r.message || '—' }}</td>
                <td class="mono muted">{{ fmt(r.created_at) }}</td>
              </tr>
            </tbody>
          </table>
        </div>
      </div>

      <!-- Options (F#19) — GSA "Data" tab + "Anchor Text" tab -->
      <div v-if="tab === 'options'" style="margin-top:12px">
        <div class="card" style="margin-bottom:12px">
          <div style="font-weight:700;font-size:15px">🎯 Backlink target</div>
          <div class="muted" style="font-size:12px;margin-top:2px">GSA "Data" tab → URL. Where every backlink in this project points. Spintax <code>[a|b]</code> allowed.</div>
          <input
            v-model="optsUrl"
            placeholder="https://example.com/[coffee|tea]-guide-[dubai|london]"
            style="flex:1;min-width:320px;margin-top:10px"
          />
          <div style="display:flex;gap:24px;margin-top:12px;flex-wrap:wrap">
            <label style="display:flex;gap:6px;align-items:center;font-size:13px">
              <input type="checkbox" v-model="optsUseRandomUrl" />
              Randomize the URL (use a variant)
            </label>
            <label style="display:flex;gap:6px;align-items:center;font-size:13px">
              Variations %
              <input type="number" min="0" max="100" v-model.number="optsUrlVariations" style="width:70px" />
            </label>
            <label style="display:flex;gap:6px;align-items:center;font-size:13px">
              Root URL % <span class="muted">(bare scheme://host/)</span>
              <input type="number" min="0" max="100" v-model.number="optsRootUrl" style="width:70px" />
            </label>
          </div>
          <div style="margin-top:14px">
            <div style="font-weight:600;font-size:13px">Multi-Tier / Linkwheel (upstream project URLs)</div>
            <div class="muted" style="font-size:12px">Projects whose <em>verified URLs</em> this one targets. Leave empty for direct submissions to the URL above.</div>
            <select v-model="optsLinkwheel" multiple size="3" style="margin-top:8px;min-width:320px">
              <option v-for="p in projects" :key="p.id" :value="p.id" :disabled="p.id === selected">
                {{ p.name }}
              </option>
            </select>
          </div>
          <label style="display:flex;gap:6px;align-items:center;font-size:13px;margin-top:14px">
            <input type="checkbox" v-model="optsAutoExport" />
            Auto-export verified URLs (GSA "Automatically export verified URLs")
          </label>
        </div>

        <div class="card" style="margin-bottom:12px">
          <div style="font-weight:700;font-size:15px">🔤 Anchor text mixing <span class="muted" style="font-weight:400;font-size:12px">(GSA "Anchor Text (below the keyword)" tab)</span></div>
          <div class="muted" style="font-size:12px;margin-top:2px">
            Main keyword = 100 % − sum of the other pools. One value per line. Weights are percentages of total draws.
          </div>

          <div style="display:grid;grid-template-columns:repeat(auto-fit,minmax(240px,1fr));gap:12px;margin-top:12px">
            <div>
              <label class="muted" style="font-size:12px">Secondary pool — <span class="mono">{{ optsPctSecondary }}%</span></label>
              <textarea v-model="optsSecondary" rows="3" placeholder="dubai coffee shop&#10;coffee roaster dubai" style="width:100%"></textarea>
              <input type="number" min="0" max="100" v-model.number="optsPctSecondary" style="width:100%;margin-top:6px" placeholder="weight %" />
            </div>
            <div>
              <label class="muted" style="font-size:12px">Branding pool — <span class="mono">{{ optsPctBranding }}%</span></label>
              <textarea v-model="optsBranding" rows="3" placeholder="DubaiCoffee&#10;Dubai Coffee Co." style="width:100%"></textarea>
              <input type="number" min="0" max="100" v-model.number="optsPctBranding" style="width:100%;margin-top:6px" placeholder="weight %" />
            </div>
            <div>
              <label class="muted" style="font-size:12px">LSI pool — <span class="mono">{{ optsPctLsi }}%</span></label>
              <textarea v-model="optsLsi" rows="3" placeholder="arabica beans&#10;single origin roastery" style="width:100%"></textarea>
              <input type="number" min="0" max="100" v-model.number="optsPctLsi" style="width:100%;margin-top:6px" placeholder="weight %" />
            </div>
            <div>
              <label class="muted" style="font-size:12px">Generic pool — <span class="mono">{{ optsPctGeneric }}%</span></label>
              <textarea v-model="optsGeneric" rows="3" placeholder="click here&#10;read more" style="width:100%"></textarea>
              <input type="number" min="0" max="100" v-model.number="optsPctGeneric" style="width:100%;margin-top:6px" placeholder="weight %" />
            </div>
            <div>
              <label style="font-size:12px;display:flex;align-items:center;gap:8px">
                <input type="checkbox" v-model="optsDomain" />
                Domain as anchor — <span class="mono">{{ optsPctDomain }}%</span>
              </label>
              <input type="number" min="0" max="100" v-model.number="optsPctDomain" :disabled="!optsDomain" style="width:100%;margin-top:6px" placeholder="weight %" />
            </div>
            <div>
              <label class="muted" style="font-size:12px">Anchor text variations % <span style="opacity:.7">(case mutation, e.g. gsa → Gsa)</span></label>
              <input type="number" min="0" max="100" v-model.number="optsVariationsPct" style="width:100%;margin-top:6px" placeholder="default 20" />
            </div>
            <div>
              <label style="display:flex;gap:6px;align-items:center;font-size:12px">
                <input type="checkbox" v-model="optsDomainWithKeyword" />
                Combine domain / generic with keyword
              </label>
              <span class="muted" style="font-size:11px">(piggy-back a pool anchor with the main keyword)</span>
            </div>
          </div>

          <div style="margin-top:14px;display:flex;gap:14px;align-items:center;flex-wrap:wrap">
            <button style="padding:9px 18px;font-weight:600" :disabled="optsBusy" @click="saveProjectOptions">
              {{ optsBusy ? 'Saving…' : (optsSaved ? '✓ Saved — click to re-save' : 'Save options') }}
            </button>
            <button class="muted" @click="loadProjectOptions()" :disabled="optsBusy">↻ reload</button>
            <span v-if="optsSaved" style="color:#16a34a;font-weight:600">Saved ✓</span>
            <span v-if="optsErr" style="color:#ef4444">{{ optsErr }}</span>
            <span v-else-if="optsLoaded" class="muted">Loaded ✓</span>
          </div>
        </div>

        <div class="card">
          <div style="font-weight:700;font-size:15px">✅ Verified URLs ({{ optsVerified.length }})</div>
          <div class="muted" style="font-size:12px;margin-top:2px">
            Successful backlinks built by this project so far — the pool the linkwheel consumes.
          </div>
          <div v-if="optsVerified.length" style="margin-top:10px;max-height:260px;overflow:auto;padding:8px;border:1px solid #2a2f3a;border-radius:8px">
            <div v-for="u in optsVerified" :key="u" style="font-size:12px;padding:3px 0;word-break:break-all;border-bottom:1px dashed #2a2f3a">
              <a :href="u" target="_blank" rel="noreferrer" style="color:#60a5fa">{{ u }}</a>
            </div>
          </div>
          <div v-else class="muted" style="margin-top:10px;font-size:13px">No successes yet — run a job to populate this list.</div>
        </div>
      </div>
    </div>
  </div>
</template>

