<script setup lang="ts">
import { ref, onMounted, computed } from 'vue'
import { api } from '@/api/client'

// — F#15: shape of the stored Settings (all fields optional) —
interface GlobalSettings {
  threads?: number
  user_agent?: string | null
  pause_after_submissions?: number
  pause_minutes?: number
  max_submit_seconds?: number
  pop3_wait_between_logins?: number
  proxy_check_seconds?: number
  verify_link_max_seconds?: number
  verify_links?: boolean
  pop3_delete_messages?: boolean
  pop3_delete_days?: number
  // — F#21: captcha solver —
  captcha_provider?: string
  captcha_api_key?: string | null
  captcha_timeout_secs?: number
  captcha_poll_secs?: number
}
interface Override {
  engine_name: string
  fields: Record<string, string>
}

const fields = [
  { key: 'threads', label: 'Worker threads (1..=64)', hint: 'GSA "Number of threads to use" — pool fanout for every project run.' },
  { key: 'user_agent', label: 'User-Agent string', hint: 'Fixed UA; empty = rotate a built-in random pool.' },
  { key: 'pause_after_submissions', label: 'Pause project after N submissions', hint: 'GSA "pause project submissions"; 0/empty = never.' },
  { key: 'pause_minutes', label: 'Pause length (minutes, 1..=1440)', hint: 'How long to pause when the above trips.' },
  { key: 'max_submit_seconds', label: 'Max seconds per submission (1..=3600)', hint: 'Per-job timeout enforced by the runner.' },
  { key: 'pop3_wait_between_logins', label: 'POP3 wait between logins (s, 1..=3600)', hint: 'GSA "pop3 wait between logins".' },
  { key: 'proxy_check_seconds', label: 'Proxy check timeout (s, 1..=3600)', hint: 'How long a proxy test may run before being marked down.' },
  { key: 'verify_link_max_seconds', label: 'Link-verify timeout (s, 1..=3600)', hint: 'Max time the link re-verification may run.' },
  { key: 'verify_links', label: 'Verify links by URL', hint: 'Re-verify every submitted link after the visit step.' },
  { key: 'pop3_delete_messages', label: 'Delete processed POP3 messages', hint: 'GSA "pop3 del message".' },
  { key: 'pop3_delete_days', label: 'Days to keep POP3 messages (0..=1440)', hint: 'GSA "pop3 del message days".' },
  // — F#21: captcha solver (GSA parity: local arithmetic / 2Captcha / Anti-Captcha) —
  { key: 'captcha_provider', label: 'Captcha solver', hint: 'local = free offline arithmetic math-captchas. 2captcha / anticaptcha / capmonster = paid image captchas (needs API key).' },
  { key: 'captcha_api_key', label: 'Captcha API key', hint: 'Key for 2Captcha / CAPMonster / Anti-Captcha. Ignored when provider = local.' },
  { key: 'captcha_timeout_secs', label: 'Captcha solve timeout (s, 5..=600)', hint: 'Max time to wait for a provider answer before the engine is marked captcha-blocked.' },
  { key: 'captcha_poll_secs', label: 'Captcha poll interval (s, 1..=60)', hint: 'How often to poll a provider for its answer.' },
] as const

// — state —
const form = ref<GlobalSettings>({})
const loadError = ref('')
const saveMsg = ref('')
const busySave = ref(false)

const overrides = ref<Override[]>([])
const ovMsg = ref('')
const busyOv = ref(false)

const bMsg = ref('')

// Fix #4 — dropdown must be usable on a fresh install (before any override
// exists).  Load a slice of the actual catalogue so a first-time user can
// pick *any* real engine and cast a first override.  Override engines are
// always merged on top (so an engine with an override is never missing).
const catalogEngines = ref<string[]>([])

async function loadCataloguedEngines() {
  try {
    const res = await api.catalogList({ limit: 300 })
    catalogEngines.value = Array.from(
      new Set((res?.data || []).map((e: any) => e.name).filter(Boolean)),
    )
  } catch {
    // offline / catalog unavailable — the dropdown still works for override
    // engines already present
  }
}

const engineNames = computed(() => {
  const set = new Set<string>(['(pick an engine)'])
  for (const o of overrides.value) set.add(o.engine_name)
  for (const n of catalogEngines.value) set.add(n)
  return Array.from(set).sort()
})
const selEngine = ref('(pick an engine)')

const effective = ref<GlobalSettings | null>(null)
const effBusy = ref(false)

// — handlers —
async function loadGlobal() {
  loadError.value = ''
  try {
    form.value = (await api.settingsGetGlobal()) as GlobalSettings
  } catch (e: any) {
    loadError.value = e.message || String(e)
  }
}

async function saveGlobal() {
  busySave.value = true
  saveMsg.value = ''
  try {
    // Empty strings / nulls → drop (API treats null as "use default").
    const body: Record<string, unknown> = {}
    for (const [k, v] of Object.entries(form.value)) {
      if (v === '' || v == null) continue
      body[k] = v
    }
    await api.settingsSetGlobal(body)
    saveMsg.value = 'saved ✓'
    await loadGlobal()
  } catch (e: any) {
    saveMsg.value = e.message || String(e)
  } finally {
    busySave.value = false
  }
}

async function loadOverrides() {
  busyOv.value = true
  ovMsg.value = ''
  try {
    overrides.value = await api.settingsListOverrides()
  } catch (e: any) {
    ovMsg.value = e.message || String(e)
  } finally {
    busyOv.value = false
  }
}

async function setOverride(field: string, value: string) {
  if (selEngine.value === '(pick an engine)') return
  busyOv.value = true
  try {
    await api.settingsSetOverride(selEngine.value, field, value === '' ? null : value)
    // NOTE: loadOverrides() clears ovMsg internally — set the success ack
    // AFTER it, so it is not wiped (Fix #3).
    await loadOverrides()
    // Fix #2 — refresh the three-tier Effective preview so it reflects the
    // newly-cast override (was: panel kept showing the global value).
    await loadEffective()
    ovMsg.value = value === '' ? `cleared ${field} for ${selEngine.value}` : `set ${field} for ${selEngine.value}`
  } catch (e: any) {
    ovMsg.value = e.message || String(e)
  } finally {
    busyOv.value = false
  }
}

async function clearEngine() {
  if (selEngine.value === '(pick an engine)') return
  if (!confirm(`Remove ALL overrides for "${selEngine.value}"?`)) return
  try {
    await api.settingsDeleteOverrides(selEngine.value)
    // Fix #3 — set the ack AFTER loadOverrides() (which clears ovMsg).
    await loadOverrides()
    // Fix #2 — refresh effective preview after removal.
    await loadEffective()
    ovMsg.value = `cleared all for ${selEngine.value}`
  } catch (e: any) {
    ovMsg.value = e.message || String(e)
  }
}

async function loadEffective() {
  if (selEngine.value === '(pick an engine)') { effective.value = null; return }
  effBusy.value = true
  try { effective.value = (await api.settingsGetEffective(selEngine.value)) as GlobalSettings }
  finally { effBusy.value = false }
}

async function doExport() {
  bMsg.value = 'exporting…'
  try {
    const raw = await api.settingsExport()
    const text = JSON.stringify(raw, null, 2)
    const blob = new Blob([text], { type: 'application/json' })
    const url = URL.createObjectURL(blob)
    const a = document.createElement('a')
    a.href = url
    a.download = `GSA_SER_Settings_${new Date().toISOString().replace(/[:.]/g, '-')}.json`
    a.click()
    URL.revokeObjectURL(url)
    bMsg.value = 'download started ✓'
  } catch (e: any) {
    bMsg.value = e.message || String(e)
  }
}

const fileInput = ref<HTMLInputElement | null>(null)
async function doImport(ev: Event) {
  const inp = ev.target as HTMLInputElement
  const file = inp.files?.[0]
  if (!file) return
  bMsg.value = 'importing…'
  try {
    const text = await file.text()
    const parsed = JSON.parse(text)
    const r = await api.settingsImport(parsed)
    bMsg.value = `imported ✓ (proxies upserted: ${r.proxies_upserted})`
    await Promise.all([loadGlobal(), loadOverrides()])
  } catch (e: any) {
    bMsg.value = e.message || String(e)
  } finally {
    if (fileInput.value) fileInput.value.value = ''
  }
}

onMounted(() => {
  void loadGlobal()
  void loadOverrides()
  // Fix #4: pre-populate the override dropdown with real catalogue engines so
  // a first-time user can cast a first override (no override required first).
  void loadCataloguedEngines()
})
</script>

<template>
  <div>
    <div class="page-head">
      <div>
        <h1>Settings</h1>
        <p>
          Global Settings (app-wide), per-engine overrides (the three-tier cascade), and
          import/export of the GSA_SER_Settings bundle. <em>Threads</em> and
          <em>max-submit-seconds</em> drive the runner's pool for every project run.
        </p>
      </div>
    </div>

    <!-- Global Settings panel -->
    <div class="card">
      <div class="row" style="align-items:center;padding-bottom:8px">
        <span style="font-weight:600">Global Settings</span>
        <span class="muted" style="font-size:12px">applies to every project, every engine</span>
        <span class="spacer"></span>
        <span v-if="saveMsg" :class="saveMsg.includes('✓') ? 'ok-msg' : 'error'" style="font-size:12px">{{ saveMsg }}</span>
        <button class="primary" :disabled="busySave" @click="saveGlobal">
          {{ busySave ? 'Saving…' : 'Save global' }}
        </button>
      </div>

      <div style="display:grid;grid-template-columns:repeat(2,minmax(220px,1fr));gap:10px">
        <div v-for="f in fields" :key="f.key" class="field">
          <label>{{ f.label }}</label>
          <input
            v-if="typeof (form as any)[f.key] === 'boolean' || f.key === 'verify_links' || f.key === 'pop3_delete_messages'"
            v-model="(form as any)[f.key]"
            type="checkbox"
            style="width:auto"
            :title="f.hint"
          />
          <input
            v-else-if="f.key === 'user_agent'"
            v-model="(form as any)[f.key]"
            class="mono"
            style="font-size:12px"
            placeholder="empty = auto-rotate"
            :title="f.hint"
          />
          <select
            v-else-if="f.key === 'captcha_provider'"
            v-model="(form as any)[f.key]"
            :title="f.hint"
          >
            <option value="local">local (arithmetic, free)</option>
            <option value="2captcha">2Captcha</option>
            <option value="anticaptcha">Anti-Captcha</option>
            <option value="capmonster">CAPMonster</option>
          </select>
          <input
            v-else-if="f.key === 'captcha_api_key'"
            v-model="(form as any)[f.key]"
            type="text"
            class="mono"
            style="font-size:12px"
            autocomplete="off"
            placeholder="provider API key"
            :title="f.hint"
          />
          <input
            v-else
            v-model.number="(form as any)[f.key]"
            type="number"
            placeholder="unset"
            :title="f.hint"
          />
          <span class="muted" style="font-size:11px;line-height:1.45">{{ f.hint }}</span>
        </div>
      </div>
      <div v-if="loadError" class="error" style="margin-top:10px">{{ loadError }}</div>
    </div>

    <!-- Engine overrides (three-tier cascade) -->
    <div class="card" style="margin-top:16px">
      <div class="row" style="align-items:center;padding-bottom:10px">
        <span style="font-weight:600">Engine overrides</span>
        <span class="muted" style="font-size:12px">sparse per-engine fields — empty = inherit global</span>
        <span class="spacer"></span>
        <select v-model="selEngine" style="min-width:200px" @change="loadEffective">
          <option v-for="n in engineNames" :key="n" :value="n">{{ n }}</option>
        </select>
        <button :disabled="selEngine === '(pick an engine)'" @click="clearEngine">clear all for this</button>
        <span v-if="ovMsg" class="ok-msg" style="font-size:12px">{{ ovMsg }}</span>
      </div>
      <div v-if="selEngine === '(pick an engine)'" class="muted" style="padding:8px 0;font-size:13px">
        Pick an engine above to edit its overrides. Any field left empty/blank inherits the
        global value (three-tier cascade: global → engine-override → effective).
      </div>
      <div v-else>
        <div style="display:grid;grid-template-columns:repeat(2,minmax(220px,1fr));gap:8px">
          <div v-for="f in fields" :key="f.key" class="field">
            <label style="font-size:12px">{{ f.label }}</label>
            <input
              :value="overrides.find(o => o.engine_name === selEngine)?.fields[f.key] ?? ''"
              placeholder="inherit global"
              class="mono"
              style="font-size:12px"
              @change="(e: Event) => setOverride(f.key, (e.target as HTMLInputElement).value)"
            />
          </div>
        </div>

        <!-- Effective settings preview -->
        <div style="margin-top:14px;padding-top:10px;border-top:1px solid var(--border)">
          <div class="row" style="align-items:center">
            <span style="font-weight:600;font-size:13px">Effective settings for
              <span class="mono">{{ selEngine }}</span></span>
            <span class="muted" style="font-size:12px">(global + overrides, clamped)</span>
          </div>
          <div v-if="!effective && !effBusy" class="muted" style="font-size:13px;margin-top:6px">
            — inherits every global setting —
          </div>
          <div v-else-if="effective" class="mono" style="margin-top:8px;font-size:12px;line-height:1.7">
            <span v-for="(entry, i) in Object.entries(effective)" :key="String(i)" style="display:inline-block;margin-right:14px;white-space:nowrap">
              <span class="muted">{{ entry[0] }}=</span><span style="color:var(--fg)">{{ entry[1] === null ? '(default)' : entry[1] }}</span>
            </span>
          </div>
        </div>
      </div>
    </div>

    <!-- Import / Export bundle -->
    <div class="card" style="margin-top:16px">
      <div class="row" style="align-items:center">
        <span style="font-weight:600">Import / Export</span>
        <span class="muted" style="font-size:12px">
          GSA_SER_Settings bundle (global + overrides + proxy snapshot, JSON)
        </span>
        <span class="spacer"></span>
        <button class="primary" @click="doExport">Export → JSON</button>
        <label class="btn-file" style="display:inline-flex;align-items:center;gap:6px;cursor:pointer">
          <span style="padding:6px 14px;border:1px solid var(--border);border-radius:8px;font-size:13px">Import from file…</span>
          <input ref="fileInput" type="file" accept="application/json,.json" hidden @change="doImport" />
        </label>
        <span v-if="bMsg" :class="bMsg.includes('✓') ? 'ok-msg' : 'error'" style="font-size:12px">{{ bMsg }}</span>
      </div>
    </div>
  </div>
</template>
