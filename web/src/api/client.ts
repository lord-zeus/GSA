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

  // — F#12 email-runner glue: run now + operator mailbox + email engine field —
  runProject: (projectId: string) =>
    req<RunSummary>(`/api/projects/${projectId}/run`, { method: 'POST' }),
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
}
