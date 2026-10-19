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
