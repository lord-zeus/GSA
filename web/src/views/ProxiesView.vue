<script setup lang="ts">
import { ref, onMounted, computed } from 'vue'
import { api, ProxyRow, ProxyTestResult } from '@/api/client'

// — list state —
const proxies = ref<ProxyRow[]>([])
const listError = ref('')
const busy = ref(false)
const tested = ref<Record<string, { t: ProxyTestResult; at: number }>>({})

// — add form —
const f = ref({ host: '', port: 8080, kind: 'http', login: '', password: '', showAdd: false })
const addError = ref('')

async function load() {
  listError.value = ''
  try {
    proxies.value = await api.proxyList()
  } catch (e: any) {
    listError.value = e.message || String(e)
  }
}

async function add() {
  addError.value = ''
  if (!f.value.host) { addError.value = 'host is required'; return }
  busy.value = true
  try {
    await api.proxyAdd({
      host: f.value.host.trim(),
      port: f.value.port,
      kind: f.value.kind,
      login: f.value.login,
      password: f.value.password,
    })
    f.value = { host: '', port: 8080, kind: 'http', login: '', password: '', showAdd: false }
    await load()
  } catch (e: any) {
    addError.value = e.message || String(e)
  } finally {
    busy.value = false
  }
}

async function del(p: ProxyRow) {
  if (!confirm(`Remove ${p.host}:${p.port}?`)) return
  await api.proxyDelete(p.id).catch((e: any) => alert(e.message || String(e)))
  tested.value = { ...tested.value }
  const rest = { ...tested.value }
  delete rest[p.id]
  await load()
}

async function toggle(p: ProxyRow) {
  const cur = { ...tested.value }
  delete cur[p.id]
  await api.proxySetEnabled(p.id, !p.enabled)
  tested.value = cur
  await load()
}

async function doTest(p: ProxyRow) {
  const cur = { ...tested.value }
  tested.value = { ...cur, [p.id]: { t: { proxy_id: p.id, ok: false, detail: 'testing…', latency_ms: -1 }, at: Date.now() } }
  try {
    const t = await api.proxyTest(p.id)
    tested.value = { ...tested.value, [p.id]: { t, at: Date.now() } }
  } catch (e: any) {
    tested.value = { ...tested.value, [p.id]: { t: { proxy_id: p.id, ok: false, detail: e.message || String(e), latency_ms: -1 }, at: Date.now() } }
  }
}

const stats = computed(() => {
  const n = proxies.value.length
  const on = proxies.value.filter((p) => p.enabled).length
  const ok = proxies.value.filter((p) => tested.value[p.id]?.t.ok).length
  return { n, on, ok }
})

onMounted(load)
</script>

<template>
  <div>
    <div class="page-head">
      <div>
        <h1>Proxies</h1>
        <p>
          Round-robin proxy pool (GSA <span class="mono">proxy list</span>) — add HTTP/SOCKS5 servers,
          test connectivity, and let projects route jobs through the enabled set.
        </p>
      </div>
      <div class="grid" style="grid-template-columns:repeat(3,minmax(72px,1fr));gap:8px;min-width:240px">
        <div class="card accent" style="padding:10px 12px">
          <div class="label">Total</div>
          <div class="value" style="font-size:20px">{{ stats.n }}</div>
        </div>
        <div class="card accent" style="padding:10px 12px">
          <div class="label">Enabled</div>
          <div class="value" style="font-size:20px">{{ stats.on }}</div>
        </div>
        <div class="card accent" style="padding:10px 12px">
          <div class="label">Verified up</div>
          <div class="value" style="font-size:20px">{{ stats.ok }}</div>
        </div>
      </div>
    </div>

    <!-- add form -->
    <div class="card" :style="{ marginBottom: '16px', borderColor: f.showAdd ? 'rgba(64,192,166,.5)' : '' }">
      <div class="row" style="margin-bottom: 10px">
        <span style="font-weight:600">Add proxy</span>
        <span class="spacer"></span>
        <button @click="f.showAdd = !f.showAdd">{{ f.showAdd ? 'Hide' : 'New' }}</button>
      </div>
      <div v-if="f.showAdd">
        <div class="row">
          <input v-model="f.host" placeholder="host (e.g. 185.0.12.4)" style="flex:2;min-width:180px" />
          <input v-model.number="f.port" type="number" title="port" style="width:110px" />
          <select v-model="f.kind" style="width:110px">
            <option value="http">http</option>
            <option value="socks5">socks5</option>
          </select>
          <input v-model="f.login" placeholder="login (opt)" style="flex:1;min-width:120px" />
          <input v-model="f.password" placeholder="pass (opt)" type="password" style="flex:1;min-width:120px" />
        </div>
        <div class="mt" style="margin-top:10px;display:flex;align-items:center;gap:10px">
          <button class="primary" :disabled="busy" @click="add">{{ busy ? 'Adding…' : 'Add' }}</button>
          <span v-if="addError" class="error">{{ addError }}</span>
          <span v-else class="muted mono" style="font-size:12px">auth only used when the proxy demands it (407 / SOCKS-userpass)</span>
        </div>
      </div>
    </div>

    <!-- list -->
    <div class="card" style="padding:0;overflow:hidden">
      <table class="table">
        <thead>
          <tr>
            <th>Host</th>
            <th>Port</th>
            <th>Type</th>
            <th>Auth</th>
            <th>Status</th>
            <th>Last test</th>
            <th class="spacer"></th>
            <th></th>
          </tr>
        </thead>
        <tbody>
          <tr v-if="!proxies.length">
            <td colspan="8" class="muted" style="text-align:center;padding:30px">
              no proxies yet — add one above to route jobs through a proxy pool
            </td>
          </tr>
          <tr v-for="p in proxies" :key="p.id">
            <td style="font-weight:600">{{ p.host }}</td>
            <td class="mono">{{ p.port }}</td>
            <td><span class="badge" :class="p.proxy_kind === 'socks5' ? 'accent' : ''">{{ p.proxy_kind }}</span></td>
            <td class="mono muted" style="font-size:11px">
              {{ (p.login ? p.login : '—') }}
            </td>
            <td>
              <span class="dot" :class="p.enabled ? 'ok' : 'bad'" style="margin-right:6px"></span>
              {{ p.enabled ? 'enabled' : 'disabled' }}
            </td>
            <td class="mono" style="font-size:12px">
              <template v-if="tested[p.id]">
                <span :style="{ color: tested[p.id].t.ok ? 'var(--ok)' : 'var(--danger)' }">
                  {{ tested[p.id].t.ok ? '✓' : '✗' }}
                </span>
                <span class="muted"> {{ tested[p.id].t.latency_ms >= 0 ? tested[p.id].t.latency_ms + ' ms' : '' }}</span>
                <div class="muted" style="font-size:11px" :title="tested[p.id].t.detail">{{ tested[p.id].t.detail }}</div>
              </template>
              <span v-else class="muted">—</span>
            </td>
            <td></td>
            <td>
              <div class="row" style="gap:6px;justify-content:flex-end">
                <button title="toggle enabled/disabled" @click="toggle(p)">
                  {{ p.enabled ? 'disable' : 'enable' }}
                </button>
                <button title="run a real socket probe (GSA proxy test)" @click="doTest(p)">test</button>
                <button title="remove" @click="del(p)">✕</button>
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
          <strong style="color:var(--fg)">How routing works</strong> — when a project has
          <span class="mono">use_proxies</span> enabled (set on the <a href="/projects">Projects</a>
          page), each job's engine (the visit step, and the F#12 mailbox verification) is sent
          through one of the <em>enabled</em> proxies above. If the request fails for a
          transport-level reason (connection refused / reset / timeout) the runner marks that proxy
          "down" for the current batch and re-rolls to the next one — the application-level failure
          (4xx/5xx, parse miss, form error) stays attributed to the engine, not the proxy. This
          mirrors GSA's <span class="mono">proxy rotate</span> + <span class="mono">proxy test</span>
          behaviour.
        </div>
      </div>
    </div>
  </div>
</template>
