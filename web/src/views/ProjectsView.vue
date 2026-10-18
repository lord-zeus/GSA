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
  type RunSummary,
} from '@/api/client'

// — projects (master list) —
const projects = ref<Project[]>([])
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
  runMsg.value = 'Running queued jobs in-process (web + email engines)… this can take a while'
  runSummary.value = null
  try {
    const s = await api.runProject(selected.value)
    runSummary.value = s
    runMsg.value = `Done — claimed ${s.claimed}, completed ${s.done}, failed ${s.failed}, panics ${s.panics}.`
    await loadJobs()
    await loadResults()
  } catch (e: any) {
    runMsg.value = 'Run failed: ' + (e?.message || String(e))
  } finally {
    running.value = false
  }
}

const hasActiveJob = computed(() => jobs.value.some((j) => j.status === 'pending' || j.status === 'running'))
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
    }
  }
}

async function selectProject(p: Project) {
  selected.value = p.id
  loadEngines()
  loadKeywords()
  loadJobs()
  loadResults()
  loadUseProxies(p.id)
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
  if (!selected.value) return
  loadEngines()
  loadKeywords()
  loadJobs()
  loadResults()
  if (tab.value === 'options') loadProjectOptions()
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
            v-for="p in projects"
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
                <th>Name</th><th>Type</th><th>Steps</th><th>URL</th><th>Enabled</th><th></th>
              </tr>
            </thead>
            <tbody>
              <tr v-if="!enginesLoaded || (enginesLoaded && !engines.length)">
                <td colspan="6" class="muted" style="text-align:center;padding:22px">
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
                <td>
                  <input
                    type="checkbox"
                    :checked="e.enabled"
                    :disabled="enginesBusy"
                    @change="toggleEngine(e)"
                  />
                </td>
                <td style="white-space:nowrap">
                  <button class="muted" title="edit" @click="startEditEngine(e)">✎</button>
                  <button class="muted" title="delete" @click="deleteEngine(e)">✕</button>
                </td>
              </tr>
            </tbody>
          </table>
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
        <div style="height:12px"></div>
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

