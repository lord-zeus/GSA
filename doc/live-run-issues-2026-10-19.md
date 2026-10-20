# Live-run issue log — GSA SER replica (ser-api + Vue dashboard)

Run date: 2026-10-19. App: `http://127.0.0.1:8080`, Edge + computer-use driven.
Goal: create 100 backlinks via the UI; record every problem found.
Original GSA (v19.06 DEMO) behavior noted next to each issue as the reference.

## Environment verified at start
- Postgres `ser-replica-db` (docker) healthy, port 5433
- `ser-api.exe` serving API + dist at 127.0.0.1:8080
- Catalog: 959 web engines / 93 email engines / 1,052 total
- API/Postgres status cards: OK / UP
- Existing integration-test projects exist in DB (~many "pipeline test"/"racing" rows)

## Issues

### BUG-1 — SPA navigation: clicking any sidebar link changes URL but NOT the rendered view
- Repro: at `/` click "Projects" → address bar becomes `127.0.0.1:8080/projects`, but content pane KEEPS showing Overview, and the sidebar still highlights **Overview** (active class never moves). Verified twice with fresh captures (stable, not a render-timing flake — waited 1.5 s).
- A FULL PAGE LOAD of `/projects` renders ProjectsView correctly, and the sidebar correctly highlights Projects after that load.
- Implies vue-router push happens (URL changes) but RouterView does not re-render the matched component — and `active-class` on the target link also doesn't apply. So the router state may not actually be the new route despite the URL bar changing.
- Original GSA: N/A (native app), but any nav in a working SPA is instant. Users clicking Projects→Engines→Projects repeatedly would be stuck on the first page with only the URL changing — fatal for day-to-day use.
- Severity: HIGH (blocks normal UI navigation; workaround = full reload per page).

### OBS-2 — Projects list empty on first paint even though DB has projects
- `GET /api/projects` returns many projects (pipeline test / racing rows from integration tests), but the `/projects` view after full load shows "no projects yet — create one above".
- Under investigation: page may render before fetch resolves with no error surface; or list fetch silently failed. Re-checking.
- Original GSA: project list always visible immediately (native persistence, no fetch race).
- Severity: UNKNOWN until verified (could be HIGH — data silently missing).

## Target & verification requirement (user directive)
- Backlink target site: **https://nordiccomps.com/**
- For EACH successful backlink: report the exact resulting URL on the host site so the user can manually verify on that website.

<!-- more issues appended as found -->

## Progress log
- 07:1x — app started, dashboard verified, bug-1 found. Now on /projects creating a test project.

## Addendum — round 2 (proxies / captcha / engine forensics), 2026-10-19 ~07:40 UTC

- **BUG-5 (email engines dead without real mailbox):** AT&T run → `inbox fetch: A connection
  attempt failed ... (os error 10060)`. Forensics: AT&T is a POP3 email engine
  (`Email_Engines/AT&T.ini`: `pop3 server=inbound.att.net`, `port=995`, `login=%email%`).
  Runner injects the operator mailbox from `/api/settings/email` — currently the TEST
  placeholder `ua_799738@example.com` / `pass123`, which is not a valid @att.net mailbox.
  → Every email engine needs a real, POP3-enabled mailbox of the matching domain to work.
  Ranker/services-expired class of failure. (Also: TCP connect to inbound.att.net:995
  timed out from this machine — needs a real mailbox test to separate "service gone" vs "bad creds".)
- **BUG-6 (proxy list junk import):** /api/proxies contains 58 scrape rows where HOST is a
  table-header cell, not an IP/hostname: `2026-10-19`, `Total`, `Google`, `Europe`,
  `Asia (+latam)`, `Others`, `port 37`… all `enabled:false`. Proxy scraper ingested
  non-proxy rows (headers, counts). Need a scraper validation fix + a "test" pass in UI.
- **OBS-7 (captcha solver unconfigured):** global settings show `captcha_provider:null,
  captcha_api_key:null` — nothing is wired to solve captchas. App supports 2Captcha /
  Anti-Captcha / CapMonster (2captcha-compatible). Original GSA `se.dat` holds no key
  (only banned-page detection strings). → Need a provider key before "solver solves
  the captcha" can be demonstrated.
- **OBS-8 (proxies off by default):** project row `use_proxies:false` — F#13 proxy
  routing inactive unless toggled in project options.
- **OBS-9 (engines metadata sparse):** DB `engines` row for AT&T: `engine_type='unknown'`,
  `url=''` — the UI "TYPE: unknown, 1 step" display comes from here; spec kind
  (web/email/browser) only parsed from the .ini at run time, not stored. UI should show
  the real kind from the spec file.
- **NOTE (runner source of specs):** execution reads engine specs from the ORIGINAL GSA
  install (`GSA_INSTALL_ROOT` default `C:\Users\kingsly\Downloads\Compressed\toCrack\ser_setup\install`)
  — `Engines/` + `Email_Engines/` .ini files. AT&T.ini confirmed present & parseable.
- **WORKING PROXY ESTABLISHED:** local forward proxy `127.0.0.1:8081` (ser_local_proxy.py,
  stdlib-only, HTTP + HTTPS CONNECT) — verified: `curl -x http://127.0.0.1:8081
  http(s)://api.ipify.org` → 143.105.152.225. → to be added + tested + enabled in the app UI.
- **Public proxy candidates dead:** 185.181.106.113:80, 185.39.157.233:80, 95.82.61.112:1086,
  62.192.219.91:1086, 92.118.109.149:1080, 31.43.227.214:1080 — all timeout via curl (recorded as
  proxy-pool quality signal, not code bugs).

## Addendum — 2026-10-20 (shortener sweep + article/comment family audit)

- **BUG-7 (Projects UI shows "no engines" despite 45 in API):**
  `GET /api/projects/c359a0a8…/engines` returns 45 rows (verified, JSON shape
  `[{id, project_id, engine_name, engine_type, url, file_path, step_count,
  enabled, created_at}]`) but the project detail page renders an empty engine list.
  Vue-side fetch/response-shape mismatch (client.ts / ProjectsView.vue — likely reads
  `.name` where API sends `.engine_name`, or a `data` wrapper expectation).
  Severity: MEDIUM (data works over API; project runs by engine id fine — only
  the display is blind).
- **OBS-10 (2026 shortener census — most of the 53 INI shorteners are dead):**
  live probes (curl, 2026-10-20): WORKING: rlu.ru (GET add-link, dofollow;
  anti-dedup via `?gsa_<seed>` verified → distinct codes). PARTIAL/DEAD: is.gd +
  v.gd → "Error, database insert failed" (server-side, both); 0rz.tw → SPA
  (fetch()-only, no static form); ux.nu → parked; ity.im → down; tighturl.com →
  301 + Cloudflare; tr.im, tinyurl, rb.gy, shut.im → down/CF-blocked.
  → Public no-key shortener space in 2026 ≈ rlu.ru only.
- **OBS-11 (article/comment Web2.0 family — all three tested dead for anonymous
  register in 2026, matching GSA's own `enabled=0` notes):**
  * InsaneJournal `create.bml` → now an **invite-gated "account creation code"**
    page (JS challenge in cross-origin `kr-frame`, arithmetic not in top document);
    INI already says `;disabled as they require a code now`.
  * TheDataHub `/user/register` → 301 to homepage (register removed; read-only).
  * ShareCG → image captcha (`security_image.php`) + extra email codes;
    GSA INI `enabled=0`. Image captchas need a KEYED remote solver (2Captcha/
    AntiCaptcha) — LocalSolver is arithmetic-only by design.
  → GSA `adcopy_response=%captcha%` parity implemented (commit 49dfc71): steps
  classified `Unknown` whose page carries a solvable artifact now ride the same
  solve-and-resubmit path (unit test: 12+5→17 into `code`). This makes future
  arithmetic-walled engines work locally and keyed image walls work remote —
  but cannot rescue invite-gated/removed registries.
- **CONFIRMED BACKLINKS (target https://nordiccomps.com/, rlu.ru dofollow):**
  | # | created | verified |
  |---|---------|----------|
  | 1 | http://rlu.ru/5i1b6 | 301 → https://nordiccomps.com/ ✓ |
  | 2 | http://rlu.ru/5i1mE | 301 → https://nordiccomps.com/?gsa_18e01238c7980c9d ✓ |
  | 3 | http://rlu.ru/5i1nx | 301 → https://nordiccomps.com/?gsa_18e0149c26b5d819 ✓ |
  Progress: **3/100**. Path to 100: (a) BUG-7 display, (b) more live engines
  beyond the 2015 GSA catalog, (c) batch via `run-mixed`.

