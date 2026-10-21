// Minimal typed fetch wrapper for the Rust REST API.
// Dev: Vite proxies /api and /healthz to :8080. Prod: same-origin (served by ser-api).

export interface Health { status: string; db: string }
export interface CatalogEngine {
  id: number
  kind: string
  name: string
  url: string
  step_count: number
  category: 'web' | 'email'
  file_path: string
}
export interface CatalogList {
  data: CatalogEngine[]
  total: number
}
export interface TypeCount { engine_type: string; n: number }
export interface Project {
  id: string
  name: string
  description: string | null
  use_proxies?: boolean
  // F#19 — the GSA "Data" tab + "Anchor Text" tab (backlink target + rotation).
  /** The backlink target URL (GSA Data-tab "URL"; spintax allowed). */
  url?: string
  /** Weighted anchor text pool (JSON string of PoolConfig). */
  anchor_mix?: string | null
  /** URL randomization / variation options (GSA "Data" tab "URL options"). */
  use_random_url?: boolean
  url_variations?: number
  root_url?: number
  /** Multi-Tier linkwheel: upstream project ids whose verified URLs are used. */
  linkwheel?: string[]
  /** GSA "Automatically export verified URLs" (F#24-lite). */
  auto_export?: boolean
  created_at: string
}
export interface EngineRow {
  id: string
  project_id: string
  engine_name: string
  engine_type: string
  url: string
  file_path: string
  step_count: number
  enabled: boolean
  created_at: string
  // — T6 · per-engine health (the auto-run router ranks by this) —
  attempts?: number
  successes?: number
  captcha_failures?: number
  last_attempt_at?: string | null
  last_success_at?: string | null
  last_error?: string | null
}
export interface JobRow {
  id: string
  project_id: string
  engine_id: string | null
  keyword_id: string | null
  status: 'pending' | 'running' | 'done' | 'failed' | 'rejected' | 'cancelled'
  attempts: number
  last_error: string | null
  started_at: string | null
  finished_at: string | null
  created_at: string
}
export interface ProjectResult {
  id: string
  job_id: string
  kind: 'success' | 'info' | 'error' | 'rejected'
  url: string | null
  anchor: string | null
  message: string | null
  engine_name: string | null
  keyword: string | null
  created_at: string
}
export interface ResultStats {
  success: number
  info: number
  error: number
  rejected: number
  total: number
  total_jobs: number
  unique_success_urls: number
}
export interface Keyword {
  id: string
  project_id: string
  text: string
  added_at: string
}
export interface KeywordBulkResult {
  stored: number
  keywords: Keyword[]
}
export interface ImportSummary {
  category: string
  directory?: string
  scanned: number
  imported: number
  failed: number
  duration_ms?: number
}
export interface ImportResult {
  status: string
  imported: number
  failed: number
  categories: ImportSummary[]
  source: string
}
export interface CatalogStats {
  total: number
  web: number
  email: number
  types: TypeCount[]
}
export interface EmailSetting {
  address: string
  password: string
}
export interface ProxyRow {
  id: string
  host: string
  port: number
  proxy_kind: 'http' | 'socks5'
  login: string
  password: string
  enabled: boolean
  created_at: string
  // F#14 — geo/IP columns
  country?: string | null
  ip?: string | null
  ip_public?: boolean | null
  geo_verified_at?: string | null
}
export interface ProxyTestResult {
  proxy_id: string
  ok: boolean
  detail: string
  latency_ms: number
}
export interface ProxyGeoVerifyResult {
  ip: string
  ip_public: boolean
  speed_ms: number | null
  country: string | null
}
export interface RunSummary {
  claimed: number
  done: number
  failed: number
  panics: number
}
// — T6 · live run feed (one line of the project's live log) —
export type RunEventLevel = 'info' | 'engine' | 'step' | 'captcha' | 'success' | 'error' | 'auto'
export interface RunEventRow {
  id: number
  project_id: string
  job_id?: string | null
  engine?: string | null
  level: RunEventLevel
  message: string
  ts: string
}
export interface TestAllResult {
  tested: number
  ok: number
  results: Array<Record<string, any>>
}
export interface CaptchaProbe {
  target: string
  ok: boolean
  detail: string
}
export interface CaptchaTestResult {
  provider: string
  armed: boolean
  detail: string
  probes: CaptchaProbe[]
}

// ── F#17 · scheduled runs (cron) ──
// Mirrors `Schedule` in crates/db/src/model.rs. Datetimes serialize as
// ISO-8601 (chrono's serde default); times as "HH:MM:SS".
export interface ScheduleRow {
  id: string
  project_id: string
  name: string
  mode: 'once' | 'recurring'
  start_at: string
  stop_at: string | null
  repeat_every_minutes: number | null
  repeat_weekdays: number[] | null   // ISO 1=Mon…7=Sun
  window_start: string | null        // "HH:MM:SS"
  window_end: string | null
  max_runs: number | null
  run_count: number
  enabled: boolean
  next_run_at: string | null         // ISO datetime, null = never fired / consumed
  created_at: string
  updated_at: string
}
export interface ScheduleCreateInput {
  name: string
  mode: 'once' | 'recurring'
  start_at: string                   // "YYYY-MM-DD HH:MM:SS" or ISO-8601 (naive UTC)
  stop_at?: string | null
  repeat_every_minutes?: number | null
  repeat_weekdays?: number[] | null
  window_start?: string | null       // "HH:MM" or "HH:MM:SS" (UTC)
  window_end?: string | null
  max_runs?: number | null
  enabled?: boolean
}
export interface ScheduleCreated {
  schedule: ScheduleRow
  next_run_at: string | null
}

// ── F#16 · stats & analytics (GSA dashboard metrics) ──
export interface StatsSummary {
  jobs_total: number
  jobs_done: number
  jobs_failed: number
  jobs_inflight: number
  jobs_cancelled: number
  results_total: number
  successes: number
  failures: number
  rejected: number
  errors: number
  info: number
  captchas: number
  unique_links: number
  success_rate: number
}
export interface TimelinePoint {
  date: string   // YYYY-MM-DD
  links: number
  captchas: number
  other: number
}
export interface EngineStat {
  engine_id: string
  name: string
  jobs: number
  successes: number
  failures: number
  captchas: number
  unique_links: number
  success_rate: number
}
export interface ProjectStat {
  project_id: string
  name: string
  jobs: number
  successes: number
  failures: number
  captchas: number
  unique_links: number
  success_rate: number
}
export interface Dashboard {
  summary: StatsSummary
  days: TimelinePoint[]
  engines: EngineStat[]
  projects: ProjectStat[]
}

async function req<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await fetch(path, {
    headers: { 'Content-Type': 'application/json', ...(init?.headers || {}) },
    ...init,
  })
  const text = await res.text()
  let body: any = null
  try { body = text ? JSON.parse(text) : null } catch { body = text }
  if (!res.ok) {
    throw new Error(
      (body && body.error && (body.error as string)) || `HTTP ${res.status}`,
    )
  }
  return body as T
}

// — F#18 notifications & alert channels —
export type NotificationSeverity = 'info' | 'good' | 'warn' | 'bad'
export interface NotificationItem {
  id: string
  type: string                     // camelCase engine type: runCompleted, captchaBlocked, …
  severity: NotificationSeverity
  project_id: string | null
  job_id: string | null
  title: string
  message: string
  payload: Record<string, unknown> | null
  is_read: boolean
  created_at: string               // ISO-8601 UTC
}
export interface NotifySettings {
  email_enabled: boolean
  recipient: string
  smtp_host: string
  smtp_port: number
  smtp_mode: string                // 'starttls' | 'direct-tls' | 'log'
  smtp_user: string
  smtp_password: string
  sender_name: string
  perType: Record<string, boolean>
}
export interface NotifyTestResult {
  status: 'ok' | 'error'
  delivered?: boolean
  message_bytes?: number
  mode?: string
  rendered?: Record<string, string>
  error?: string
}
export const api = {
  health: () => req<Health>('/healthz'),

  // — F#16 · stats & analytics dashboard (one round-trip: summary + timeline + per-engine + per-project) —
  stats: (days?: number) =>
    req<Dashboard>(`/api/stats${days != null ? `?days=${days}` : ''}`),

  catalogList: async (q?: { kind?: string; search?: string; offset?: number; limit?: number }): Promise<CatalogList> => {
    const p = new URLSearchParams()
    if (q?.kind) p.set('kind', q.kind)
    if (q?.search) p.set('q', q.search)
    p.set('limit', String(q?.limit ?? 50))
    p.set('offset', String(q?.offset ?? 0))
    // The Rust API returns a bare array; we add total from /stats for the UI.
    const [rows, stats] = await Promise.all([
      req<CatalogEngine[]>(`/api/catengine?${p}`),
      req<CatalogStats>('/api/catengine/stats').catch(() => null),
    ])
    return { data: rows, total: stats?.total ?? rows.length }
  },
  catalogImport: (directory?: string) =>
    req<ImportResult>('/api/catengine/import', {
      method: 'POST',
      body: JSON.stringify({ directory: directory ?? null }),
    }),
  catalogStats: () => req<CatalogStats>('/api/catengine/stats'),

  projectList: () => req<Project[]>('/api/projects'),
  projectGet: (id: string) => req<any>(`/api/projects/${id}`),
  projectCreate: (p: { name: string; description?: string }) =>
    req<Project>('/api/projects', { method: 'POST', body: JSON.stringify(p) }),

  keywordList: (projectId: string) => req<Keyword[]>(`/api/projects/${projectId}/keywords`),
  keywordAdd: (projectId: string, text: string) =>
    req<Keyword>(`/api/projects/${projectId}/keywords`, { method: 'POST', body: JSON.stringify({ text }) }),
  keywordAddBulk: (projectId: string, keywords: string[]) =>
    req<KeywordBulkResult>(`/api/projects/${projectId}/keywords/bulk`, { method: 'POST', body: JSON.stringify({ keywords }) }),
  keywordDelete: (projectId: string, keywordId: string) =>
    req<void>(`/api/projects/${projectId}/keywords/${keywordId}`, { method: 'DELETE' }),

  // — F#11 Projects UI polish: full CRUD —
  projectUpdate: (id: string, p: { name: string; description?: string }) =>
    req<Project>(`/api/projects/${id}`, { method: 'PUT', body: JSON.stringify(p) }),
  projectDelete: (id: string) =>
    req<void>(`/api/projects/${id}`, { method: 'DELETE' }),
  // — F#19: per-project URL + anchor-rotation + linkwheel + verified export —
  /** Set the backlink target URL (GSA Data-tab "URL"). */
  projectSetUrl: (id: string, url: string) =>
    req<Project>(`/api/projects/${id}/url`, { method: 'PUT', body: JSON.stringify({ url }) }),
  /** Save the "Anchor Text (below the keyword)" tab. Omitted fields unchanged. */
  projectSetAnchorOptions: (
    id: string,
    o: {
      anchor_mix?: string
      use_random_url?: boolean
      url_variations?: number
      root_url?: number
      linkwheel?: string[]
      auto_export?: boolean
    },
  ) =>
    req<Project>(`/api/projects/${id}/anchor-options`, { method: 'PUT', body: JSON.stringify(o) }),
  /** List the project's verified (successful) submission URLs. */
  projectVerifiedUrls: (id: string, limit = 500) =>
    req<string[]>(`/api/projects/${id}/verified?limit=${limit}`),
  engineList: (projectId: string) => req<EngineRow[]>(`/api/projects/${projectId}/engines`),
  engineAdd: (projectId: string, e: { engine_name: string; engine_type?: string; url?: string; file_path?: string; step_count?: number }) =>
    req<EngineRow>(`/api/projects/${projectId}/engines`, { method: 'POST', body: JSON.stringify(e) }),
  engineUpdate: (projectId: string, engineId: string, e: { engine_name: string; engine_type?: string; url?: string; step_count?: number }) =>
    req<EngineRow>(`/api/projects/${projectId}/engines/${engineId}`, { method: 'PUT', body: JSON.stringify(e) }),
  engineDelete: (projectId: string, engineId: string) =>
    req<void>(`/api/projects/${projectId}/engines/${engineId}`, { method: 'DELETE' }),
  engineToggle: (projectId: string, engineId: string, enabled: boolean) =>
    req<EngineRow>(`/api/projects/${projectId}/engines/${engineId}/enabled`, { method: 'PUT', body: JSON.stringify({ enabled }) }),
  jobList: (projectId: string, status?: string) =>
    req<JobRow[]>(`/api/projects/${projectId}/jobs` + (status ? `?status=${status}` : '')),
  jobCreate: (projectId: string, engineId?: string, keywordId?: string) =>
    req<JobRow>(`/api/projects/${projectId}/jobs`, { method: 'POST', body: JSON.stringify({ engine_id: engineId ?? null, keyword_id: keywordId ?? null }) }),
  jobCancel: (jobId: string) =>
    req<JobRow>(`/api/jobs/${jobId}/cancel`, { method: 'POST' }),
  jobResults: (jobId: string) =>
    req<unknown[]>(`/api/jobs/${jobId}/results`),
  projectResults: (projectId: string, kind?: string, limit?: number) => {
    const p = new URLSearchParams()
    if (kind) p.set('kind', kind)
    if (limit) p.set('limit', String(limit))
    const q = p.toString()
    return req<ProjectResult[]>(`/api/projects/${projectId}/results` + (q ? `?${q}` : ''))
  },
  projectStats: (projectId: string) => req<ResultStats>(`/api/projects/${projectId}/results/stats`),

  // — F#13 proxy support (GSA "proxy list") —
  proxyList: (country?: string) =>
    req<ProxyRow[]>(`/api/proxies${country ? `?country=${encodeURIComponent(country)}` : ''}`),
  proxyAdd: (p: { host: string; port: number; kind?: string; login?: string; password?: string; country?: string }) =>
    req<ProxyRow>('/api/proxies', { method: 'POST', body: JSON.stringify(p) }),
  proxyDelete: (id: string) => req<void>(`/api/proxies/${id}`, { method: 'DELETE' }),
  proxySetEnabled: (id: string, enabled: boolean) =>
    req<ProxyRow>(`/api/proxies/${id}/enabled`, { method: 'PUT', body: JSON.stringify({ enabled }) }),
  proxyTest: (id: string) =>
    req<ProxyTestResult>(`/api/proxies/${id}/test`, { method: 'POST' }),
  // — F#14 geo / IP handling —
  proxySetCountry: (id: string, country: string) =>
    req<ProxyRow>(`/api/proxies/${id}/country`, { method: 'PUT', body: JSON.stringify({ country }) }),
  proxyVerifyGeo: (id: string, opts?: { egress_url?: string; timeout_secs?: number }) =>
    req<ProxyGeoVerifyResult>(`/api/proxies/${id}/verify-geo`, {
      method: 'POST',
      body: JSON.stringify(opts ?? {}),
    }),
  proxyVerifyAll: (timeoutSecs?: number) =>
    req<{ verified: number; failed: number; results: Array<Record<string, unknown>> }>(
      '/api/proxies/verify-all',
      { method: 'POST', body: JSON.stringify(timeoutSecs != null ? { timeout_secs: timeoutSecs } : {}) },
    ),
  projectSetUseProxies: (projectId: string, useProxies: boolean) =>
    req<Project>(`/api/projects/${projectId}/proxies`, { method: 'PUT', body: JSON.stringify({ use_proxies: useProxies }) }),

  // — F#26: Polish (notes · color · pause-after-N · test engine) —
  /** Save polish fields: project notes, color tag, and pause-after-N ceiling. */
  projectSetPolish: (
    id: string,
    o: { notes?: string | null; color?: string | null; pause_after?: number | null },
  ) =>
    req<Project>(`/api/projects/${id}/polish`, { method: 'PATCH', body: JSON.stringify(o) }),
  /** Clear the pause ceiling so a paused run can resume. */
  projectResume: (id: string) =>
    req<Project>(`/api/projects/${id}/resume`, { method: 'POST' }),
  /** Dry-run one engine end-to-end (no results persisted) and report the outcome.
   *  Matches `POST /api/projects/{id}/test-engine` (F#26): the server drives
   *  the engine once and returns the JobReport — nothing is written to the
   *  results table. */
  projectTestEngine: (id: string, engineId: string) =>
    req<{
      project_id: string
      engine_id: string
      job_id: string
      report: {
        successful: number
        failed: number
        retried: number
        captcha_failed: number
        errors: string[]
        submitted_url: string | null
      } | null
      error: string | null
    }>(`/api/projects/${id}/test-engine`, { method: 'POST', body: JSON.stringify({ engine_id: engineId }) }),

  // — F#17 · scheduled runs (cron) —
  // Row shape (mirrors `Schedule` in crates/db/src/model.rs).
  scheduleList: (projectId?: string) =>
    req<ScheduleRow[]>(projectId ? `/api/projects/${projectId}/schedules` : '/api/schedules'),
  scheduleGet: (id: string) => req<ScheduleRow>(`/api/schedules/${id}`),
  scheduleCreate: (projectId: string, s: ScheduleCreateInput) =>
    req<ScheduleCreated>(`/api/projects/${projectId}/schedules`, {
      method: 'POST',
      body: JSON.stringify(s),
    }),
  scheduleDelete: (id: string) => req<void>(`/api/schedules/${id}`, { method: 'DELETE' }),
  scheduleSetEnabled: (id: string, enabled: boolean) =>
    req<ScheduleRow>(`/api/schedules/${id}/enabled`, { method: 'PUT', body: JSON.stringify({ enabled }) }),
  scheduleRunNow: (id: string) => req<RunSummary & { status?: string; reason?: string }>(`/api/schedules/${id}/run-now`, { method: 'POST' }),

  // — F#12 email-runner glue: run now + operator mailbox + email engine field —
  runProject: (projectId: string) =>
    req<RunSummary>(`/api/projects/${projectId}/run`, { method: 'POST' }),

  // — T6 · the auto UX —
  /** Auto-run: the SYSTEM ranks the project's engines by health and
   *  tries them until `links` backlinks are created (first success
   *  soft-stops the rest). No engine picking in the UI. */
  projectRunAuto: (id: string, o?: { engines?: number; links?: number }) =>
    req<RunSummary & { auto_run: boolean; engines_tried_order: string[]; links_target: number; enqueued_jobs: number }>(
      `/api/projects/${id}/run-auto`,
      { method: 'POST', body: JSON.stringify(o ?? {}) },
    ),
  /** Tail of the project's live run log (the DB — source of truth). */
  projectEvents: (id: string, after = 0, limit = 200) =>
    req<{ events: RunEventRow[] }>(`/api/projects/${id}/events?after=${after}&limit=${limit}`),
  /** SSE URL for the live run log. */
  projectEventsLiveUrl: (id: string) => `/api/projects/${id}/events/live`,
  /** Dry-run every project engine (best health first) and report which
   *  ones actually create a link. */
  projectTestAll: (id: string, limit = 12) =>
    req<TestAllResult>(`/api/projects/${id}/test-all`, { method: 'POST', body: JSON.stringify({ limit }) }),
  /** Verify the configured captcha solver account (balance / key check). */
  captchaTest: () =>
    req<CaptchaTestResult>('/api/captcha/test', { method: 'POST' }),
  emailGet: () => req<EmailSetting>('/api/settings/email'),
  emailSet: (s: { address: string; password: string }) =>
    req<{ status: string }>('/api/settings/email', {
      method: 'PUT',
      body: JSON.stringify(s),
    }),

  // — F#15 settings & profiles —
  // Global settings (app-wide, the "Global Settings" panel of GSA).
  settingsGetGlobal: () => req<Record<string, unknown>>('/api/settings/global'),
  settingsSetGlobal: (s: Record<string, unknown>) =>
    req<{ status: string }>('/api/settings/global', { method: 'PUT', body: JSON.stringify(s) }),
  // Per-engine overrides (the three-level cascade: global → engine-override → effective).
  settingsListOverrides: () => req<Array<{ engine_name: string; fields: Record<string, string> }>>('/api/settings/engine/overrides'),
  settingsSetOverride: (engineName: string, field: string, value: string | null) =>
    req<{ status: string }>('/api/settings/engine/overrides', {
      method: 'POST',
      body: JSON.stringify({ engine_name: engineName, field, value }),
    }),
  settingsDeleteOverrides: (engineName: string) =>
    req<void>(`/api/settings/engine/overrides?engine_name=${encodeURIComponent(engineName)}`, { method: 'DELETE' }),
  // Effective (merged + clamped) settings for one engine.
  settingsGetEffective: (engineName: string) =>
    req<Record<string, unknown>>(`/api/settings/engine/${encodeURIComponent(engineName)}/effective`),
  // Import / export bundle (GSA_SER_Settings format as JSON).


  settingsExport: () => req<Record<string, unknown>>('/api/settings/export'),
  settingsImport: (bundle: Record<string, unknown>) =>
    req<{ status: string; proxies_upserted: number }>('/api/settings/import', {
      method: 'POST',
      body: JSON.stringify({ bundle }),
    }),

  // — F#18 notifications & alert channels —
  notificationsList: (q?: { limit?: number; offset?: number; unread?: boolean }) => {
    const p = new URLSearchParams()
    if (q?.limit != null) p.set('limit', String(q.limit))
    if (q?.offset != null) p.set('offset', String(q.offset))
    if (q?.unread != null) p.set('unread', String(q.unread))
    const s = p.toString()
    return req<NotificationItem[]>(`/api/notifications${s ? '?' + s : ''}`)
  },
  notificationsUnread: () => req<{ unread: number }>('/api/notifications/unread'),
  notificationMarkRead: (id: string) =>
    req<{ status: string }>(`/api/notifications/${id}/read`, { method: 'PUT' }),
  notificationsReadAll: () =>
    req<{ status: string; marked: number }>('/api/notifications/read-all', { method: 'PUT' }),
  notifySettingsGet: () => req<NotifySettings>('/api/settings/notify'),
  notifySettingsSave: (s: NotifySettings) =>
    req<{ status: string; email_enabled: boolean }>('/api/settings/notify', {
      method: 'PUT',
      body: JSON.stringify(s),
    }),
  notifySettingsTest: () =>
    req<NotifyTestResult>('/api/settings/notify', { method: 'POST' }),
}
