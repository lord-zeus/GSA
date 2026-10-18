<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { api } from '@/api/client'
import { useNotificationStore } from '@/stores/notifications'
import { NotificationItem, NotifySettings } from '@/api/client'

const notes = useNotificationStore()

// — filters —
const filter = ref<'all' | 'unread'>('all')
const sevFilter = ref<'all' | 'good' | 'warn' | 'bad' | 'info'>('all')

const visible = computed(() =>
  notes.items.filter((n) => {
    if (filter.value === 'unread' && n.is_read) return false
    if (sevFilter.value !== 'all' && n.severity !== sevFilter.value) return false
    return true
  }),
)

const unreadCount = computed(() => notes.items.filter((i) => !i.is_read).length)

function fmtDT(iso: string): string {
  const d = new Date(iso)
  const p = (n: number) => String(n).padStart(2, '0')
  const months = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec']
  return `${p(d.getUTCHours())}:${p(d.getUTCMinutes())}:${p(d.getUTCSeconds())} · ${p(d.getUTCDate())}-${months[d.getUTCMonth()]} ${d.getUTCFullYear()}`
}

const typeLabel: Record<string, string> = {
  runCompleted: 'run completed',
  runCompletedWithFailures: 'run completed w/ failures',
  runFailed: 'run failed',
  captchaBlocked: 'captcha blocked',
  scheduleFired: 'schedule fired',
  scheduleFailed: 'schedule failed',
  proxyDown: 'proxy down',
  settingsChanged: 'settings changed',
  systemError: 'system error',
  systemInfo: 'system info',
}

async function markRead(n: NotificationItem) {
  if (n.is_read) return
  await notes.markRead(n.id)
}

async function readAll() {
  await notes.markAllRead()
}

// — notify settings (email alerts) —
const settings = ref<NotifySettings>({
  email_enabled: false,
  recipient: '',
  smtp_host: '',
  smtp_port: 587,
  smtp_mode: 'starttls',
  smtp_user: '',
  smtp_password: '',
  sender_name: '',
  perType: {},
})
const saveMsg = ref<{ ok: boolean; text: string } | null>(null)
const saveBusy = ref(false)
const testBusy = ref(false)
const testMsg = ref<{ ok: boolean; text: string } | null>(null)

const EMAIL_TYPES = [
  'runCompleted',
  'runCompletedWithFailures',
  'runFailed',
  'captchaBlocked',
  'scheduleFired',
  'scheduleFailed',
  'proxyDown',
  'settingsChanged',
  'systemError',
  'systemInfo',
] as const

async function loadSettings() {
  const s = await api.notifySettingsGet()
  settings.value = { ...s }
}

// engine defaults (mirror notify.rs `default_email`): everything emails by
// default except the two cosmetic types.
const TYPE_DEFAULT_EMAIL: Record<string, boolean> = {
  runCompleted: true,
  runCompletedWithFailures: true,
  runFailed: true,
  captchaBlocked: true,
  scheduleFired: true,
  scheduleFailed: true,
  proxyDown: true,
  settingsChanged: false,
  systemError: true,
  systemInfo: false,
}
function typeEmailOn(t: string): boolean {
  return settings.value.perType[t] ?? TYPE_DEFAULT_EMAIL[t] ?? true
}
function setTypeEmail(t: string, on: boolean) {
  settings.value.perType = { ...settings.value.perType, [t]: on }
}

async function save() {
  saveMsg.value = null
  saveBusy.value = true
  try {
    const res = await api.notifySettingsSave(settings.value)
    saveMsg.value = {
      ok: true,
      text: res.email_enabled
        ? `saved — email alerts ON to ${settings.value.recipient}`
        : 'saved — email alerts OFF (bell + toasts only)',
    }
  } catch (e: any) {
    saveMsg.value = { ok: false, text: e?.message || String(e) }
  } finally {
    saveBusy.value = false
  }
}

async function testEmail() {
  testMsg.value = null
  testBusy.value = true
  try {
    // save first so we test the exact config on screen
    await api.notifySettingsSave(settings.value)
    const r = await api.notifySettingsTest()
    if (r.status === 'ok') {
      testMsg.value = { ok: true, text: `delivered via ${r.mode ?? settings.value.smtp_mode} (${r.message_bytes ?? 0} bytes) — check your inbox (or the app logs if mode is log)` }
    } else {
      testMsg.value = { ok: false, text: r.error || 'send failed' }
    }
  } catch (e: any) {
    testMsg.value = { ok: false, text: e?.message || String(e) }
  } finally {
    testBusy.value = false
  }
}

onMounted(() => {
  notes.load()
  loadSettings().catch(() => {})
})
</script>

<template>
  <div>
    <div class="page-head">
      <div>
        <h1>Notifications</h1>
        <p>
          Every good or bad thing the system does lands here — runs that finished (with or without
          failures), CAPTCHA walls, proxy dropouts, scheduled fires, SMTP failures. The
          <span class="mono">🔔</span> bell in the sidebar shows unread count; new alerts pop as
          toasts in real time over <span class="mono">/api/notifications/live</span> (SSE).
        </p>
      </div>
      <div class="grid" style="grid-template-columns:repeat(2,minmax(96px,1fr));gap:8px;min-width:220px">
        <div class="card accent" style="padding:10px 12px">
          <div class="label">Unread</div>
          <div class="value" style="font-size:20px">{{ unreadCount }}</div>
        </div>
        <div class="card" style="padding:10px 12px">
          <div class="label">Live channel</div>
          <div style="display:flex;align-items:center;gap:6px;margin-top:4px">
            <span class="dot" :class="notes.live ? 'ok' : 'bad'"></span>
            {{ notes.live ? 'connected' : 'reconnecting…' }}
          </div>
        </div>
      </div>
    </div>

    <!-- alert channels: email -->
    <div class="card" style="margin-bottom:16px">
      <div class="row" style="margin-bottom:10px">
        <span style="font-weight:600">Alert channels</span>
        <span class="muted" style="font-size:12px">
          in-app always on · optional email via your SMTP relay
        </span>
        <span class="spacer"></span>
        <span class="badge" :class="settings.email_enabled ? 'accent' : ''">
          email: {{ settings.email_enabled ? 'ON' : 'OFF' }}
        </span>
      </div>
      <div class="row" style="margin-bottom:10px">
        <label style="display:flex;align-items:center;gap:6px;font-size:13px">
          <input type="checkbox" v-model="settings.email_enabled" /> enable email alerts
        </label>
        <label style="font-size:13px">Recipient</label>
        <input v-model="settings.recipient" type="email" placeholder="operator@example.com" style="flex:1;min-width:220px" class="mono" />
      </div>
      <div class="row" style="margin-bottom:10px">
        <label style="font-size:13px">SMTP host</label>
        <input v-model="settings.smtp_host" placeholder="smtp.example.com" style="flex:1;min-width:180px" class="mono" />
        <label style="font-size:13px">Port</label>
        <input v-model.number="settings.smtp_port" type="number" style="width:90px" class="mono" />
        <label style="font-size:13px">Mode</label>
        <select v-model="settings.smtp_mode" style="width:120px">
          <option value="starttls">starttls (587)</option>
          <option value="direct-tls">direct-tls (465)</option>
          <option value="log">log (dry-run)</option>
        </select>
        <label style="font-size:13px">Sender name</label>
        <input v-model="settings.sender_name" placeholder="SER Replica" style="width:140px" />
      </div>
      <div class="row" style="margin-bottom:12px">
        <label style="font-size:13px">SMTP user</label>
        <input v-model="settings.smtp_user" placeholder="app@example.com (opt)" style="flex:1;min-width:180px" class="mono" />
        <label style="font-size:13px">SMTP pass</label>
        <input v-model="settings.smtp_password" type="password" placeholder="app password (opt)" style="flex:1;min-width:180px" class="mono" />
      </div>
      <div style="margin-bottom:12px">
        <div class="row" style="margin-bottom:6px">
          <span style="font-size:13px;font-weight:600">Email by alert type</span>
          <span class="muted" style="font-size:12px">unselected types use the engine default</span>
        </div>
        <div class="row" style="flex-wrap:wrap;gap:8px">
          <label v-for="t in EMAIL_TYPES" :key="t"
                 style="display:flex;align-items:center;gap:6px;font-size:12px;border:1px solid var(--border,#333);border-radius:6px;padding:4px 8px;cursor:pointer"
                 :style="{ borderColor: typeEmailOn(t) && settings.email_enabled ? 'rgba(64,192,166,.5)' : '' }">
            <input type="checkbox" :checked="settings.email_enabled && typeEmailOn(t)" @change="setTypeEmail(t, !typeEmailOn(t))" />
            {{ typeLabel[t] ?? t }}
          </label>
        </div>
      </div>
      <div class="row">
        <button class="primary" :disabled="saveBusy" @click="save">{{ saveBusy ? 'Saving…' : 'Save channel settings' }}</button>
        <button :disabled="testBusy || !settings.email_enabled" @click="testEmail">
          {{ testBusy ? 'Testing…' : 'Send test email' }}
        </button>
        <span v-if="saveMsg" style="font-size:12px" :style="{ color: saveMsg.ok ? 'var(--ok)' : 'var(--danger)' }">{{ saveMsg.text }}</span>
        <span v-if="testMsg" style="font-size:12px" :style="{ color: testMsg.ok ? 'var(--ok)' : 'var(--danger)' }">{{ testMsg.text }}</span>
      </div>
    </div>

    <!-- the feed -->
    <div class="card" style="padding:0;overflow:hidden">
      <div class="row" style="padding:10px 12px;border-bottom:1px solid var(--border,#2a2a2e)">
        <span style="font-size:13px;font-weight:600">{{ visible.length }} / {{ notes.items.length }} alerts</span>
        <span class="spacer"></span>
        <select v-model="filter" style="width:110px">
          <option value="all">all</option>
          <option value="unread">unread only</option>
        </select>
        <select v-model="sevFilter" style="width:120px">
          <option value="all">any severity</option>
          <option value="good">good</option>
          <option value="warn">warning</option>
          <option value="bad">bad</option>
          <option value="info">info</option>
        </select>
        <button :disabled="!unreadCount" @click="readAll">mark all read</button>
      </div>
      <table class="table">
        <thead>
          <tr>
            <th style="width:28px"></th>
            <th>Alert</th>
            <th>Type</th>
            <th>When (UTC)</th>
            <th></th>
          </tr>
        </thead>
        <tbody>
          <tr v-if="!visible.length">
            <td colspan="5" class="muted" style="text-align:center;padding:30px">
              {{ notes.items.length ? 'nothing matches this filter' : 'no alerts yet — runs, schedules and system events will appear here' }}
            </td>
          </tr>
          <tr v-for="n in visible" :key="n.id" :style="{ background: n.is_read ? '' : 'rgba(64,192,166,.05)' }">
            <td>
              <span class="sev" :class="n.severity">
                {{ n.severity === 'good' ? '✓' : n.severity === 'warn' ? '!' : n.severity === 'bad' ? '✕' : '·' }}
              </span>
            </td>
            <td>
              <div style="font-weight:600;font-size:13px">
                <span v-if="!n.is_read" style="display:inline-block;width:7px;height:7px;border-radius:4px;background:var(--accent,#40c0a6);margin-right:8px"></span>
                {{ n.title }}
              </div>
              <div style="font-size:12px;opacity:.75;margin-top:2px;line-height:1.45">{{ n.message }}</div>
            </td>
            <td style="font-size:12px">
              <span class="badge">{{ typeLabel[n.type] ?? n.type }}</span>
            </td>
            <td class="mono" style="font-size:12px;white-space:nowrap">{{ fmtDT(n.created_at) }}</td>
            <td>
              <button :disabled="n.is_read" @click="markRead(n)">{{ n.is_read ? 'read' : 'mark read' }}</button>
            </td>
          </tr>
        </tbody>
      </table>
    </div>
  </div>
</template>

<style scoped>
.sev {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 20px;
  height: 20px;
  border-radius: 10px;
  font-weight: 700;
  font-size: 12px;
}
.sev.good { color: #3fb950; background: rgba(63, 185, 80, .14); }
.sev.warn { color: #d9a514; background: rgba(217, 165, 20, .14); }
.sev.bad { color: #e25959; background: rgba(226, 89, 89, .14); }
.sev.info { color: #4a8fe2; background: rgba(74, 143, 226, .14); }
</style>
