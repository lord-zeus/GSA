# GSA SER v19.06 → ser-replica gap analysis & build plan
_Created 2026-10-18 from: live GSA New-Project dialog (Data / Options / Notes / External APIs tabs),
strings extracted from `Search_Engine_Ranker_FREE.exe` + `Search_Engine_Ranker.exe`, and the
v19.06 install dir `C:/Users/kingsly/Downloads/Compressed/toCrack/ser_setup/install/`._

Goal (user): **automatically create good backlinks with anchor text, using proxies, bypassing
captchas.** Everything below is judged against that.

---

## 1. What the original's New-Project dialog actually contains (reference spec)

### 1a. Left panel — "Where to Submit (11)"
Engine tree with 22 categories (checkbox per engine, per-category select):
`Article, Blog Comment, Directory, Document Sharing, Exploit, Forum, Guestbook, Image Comment,
Indexer, Microblog, Pingback, Referrer, RSS, Social Bookmark, Social Network, Trackback,
URL Shortener, Video, Video-Adult, Web 2.0, Wiki`.
Below: counters `Keyword related Target sites / Partly Keyword related Target sites /
No keyword related Target sites` (site classification by keyword relevance).

### 1b. Data tab (every field)
| Field | Options |
|---|---|
| **Keyword** (spintax, Edit = full keyword manager) | Import from file |
| **URL** (spintax) | Edit button |
| "Use a random URL from above on every submission (else in sequence)" | checked by default |
| "Use verified URLs of another project (**Multi-Tier/Linkwheel**)" | + *Edit Tier Filter Options* |
| "Use URL variations with **20 %**" | (http://domain.de/ → HTTP://www.Domain.de) |
| "Use the root/main URL with **30 %**" | (http://domain.de/link.html → http://domain.de) |
| **Keywords-for-search** field (spintax URL shown) | + *Tool* button |
| "Collect keywords from target sites" | checkbox |
| "Use collected keywords to find new target sites" | checkbox |
| "Put keyword in quotes when used in search queries" | checkbox |
| "Try searching with similar looking keywords" | checkbox |
| **Anchor Text (based on Main Keyword)** free field | *Import*; "ignored if you use an anchor text in the URL field" |
| Anchor mix (each: checkbox + %): | |
|  "Use keywords as anchor text" | main anchor (balance) |
|  Partial match anchor text | default **5 %** |
|  Secondary anchor text | default **10 %** |
|  Branding anchor text | default **70 %** |
|  LSI anchor text | default **3 %** |
|  Generic anchor text | default **5 %** + *Use Custom File / Edit* |
|  Domain as anchor text | default **20 %** + *Edit* |
|  **Summary line: "=> Total Percentage: X % (100% − main anchor usage)"** | enforced |
|  Use Citation | default **25 %** |
|  Anchor text variations | default **20 %** (e.g. "GSA becomes gsa or Gsa") |
|  "Try to use domain/generic anchor with main anchor" | checkbox (on in demo) |
| **Blog Comment** | big spintax block + *Import*; footer "Shortest: 14 chars (3 words), Longest: 694 chars (123 words)" — i.e. it **spins the template and reports the min/max of the concrete output** |

### 1c. Options tab (every field)
**[-] How to submit / verify**
- "Pause the project after **100** +/- **0** submissions for **30** minutes [per URL]"
- "Ask all services/user to fill captchas" ☑
- "Custom Retry **5** times" · "If a form field can't be filled → Choose Random"
- "Skip hard to solve captchas"
- "Enable Custom Mode" · "Custom Proxy Setup" + *Configure*
- *Configure Verification Options* (big button)
- "Automatically export verified URLs"
- "Send verified URLs to Index-Services" ☑ | "GSA SEO Indexer" ☑ | "Other Indexer(s)" ☑
- Send to Index/Ping delayed by **7200** minutes ☑
- "Use character spinning when possible" (For stop words only) + *Edit*
- "Randomly use typos/spell errors in descriptions (fight duplicate content)" **10 %**
- "Try to always place an URL with an anchor text in descriptions/comments"
- "Retry to submit to previously failed submitted sites **10** times" +"Ignore submit history"
- "Try to locate new URL on 'no engine match' (useful for some engines)" ☑
- "For TAGs use: (●) Anchor texts ○ Keywords"
- "Folder with data files to use" (custom .dat override dir)

**[-] How to get Target URLs**
- Search Engines to use: Name / Country / Description table — **3/2525 Checked** (per-country
  "4get - Brave XX" style entries), Add/Edit/Delete buttons
- "Always use keywords to find target sites" · "Add stop words to query with **20 %**" + *Edit*
- "Don't use quotes on search terms (less accurate, more results)"
- "Use URLs from global site lists if enabled" ☑ + checkboxes: **Identified ☑, Submitted ☑,
  Verified ☑, Failed ☑, Other ☑, Don't add, User Defined (Configure)**
- "Use URLs linking on same verified URL (supported by some engines only)" ☑
- "Analyse and post to competitor backlinks"
- banner: **-> Instant success using Link Lists <-**

**[-] Scheduled Posting**
- "Allow posting on same site again" + sub: "Post first article without links",
  "Link only on **1 %** of all posted articles"
- Time to wait on further registrations **10-50** minutes (random range!)
- Time to wait before first post **0-5** minutes
- Time to wait between two posts **10-1400** minutes
- Maximum posts per account **5** +/- **0** [per URL]
- Maximum accounts per site **5** +/- **0** [per URL]

**[-] Filter URLs**
- "Skip sites with more than **100** outgoing links on one page"
- "Skip sites with a PR below **1**" [x] "And with a PR above **6**" · "Skip also unknown PR" (use PR of Domain)
- **Type of Backlinks to create** (checkbox list): Anchor text ✓, Article ✓, Article-Wiki,
  Bookmark, Bookmark-Anchor text, Bookmark-URL, Comment-Anchor text, Comment-Anchor text - Domain,
  Comment-Contextual, Comment-Contextual/Anchor text, Comment-Contextual-URL, Comment-URL …
- "Try to skip creating Nofollow links" (opt.)
- "Avoid posting to subdomain sites" · "Avoid posting to sites with just IP and no domain"
- "Avoid posting to sites with duplicate IP" · "Avoid posting to sites with numbers only in domain"
- "At least **1** keywords must be present on *Visible text on site*"
- "Skip sites where the following words appear" — list (wikispam.de, sex, cunt, dick, boobs, boob)
  + Add/Edit/Delete/Import
- "Skip sites with the following words in URL/Domain" + **"Create Domain Filter"** button
- "Skip sites from the following countries" (country list w/ checkboxes) ·
  "Skip sites from the following languages" (language list w/ ISO codes)

### 1d. Notes tab
- Free-text notes ("Make yourself some notes about the project here")
- Auto line **"Created on: 2026-10-18 14:04"**
- "Custom Project Color" + swatch (magenta in demo)

### 1e. External APIs tab
- "Import verified URLs from external file" — File name … + **Interval **30** minutes**
  (a scheduled external URL feed — linkwheel input)
- (Implied by installed siblings: `auto_softsubmit_demo.exe`, `auto_websubmit_demo.exe`,
  `captcha_breaker.exe`, `content_generator.exe`, `keyword_research.exe` — GSA exposes these
  as companion services.)

### 1f. Bottom bar
status text *"You can use the spin syntax in almost all fields."* · **Test** (single submission
test) · **Tools** · OK / Cancel

---

## 2. Data assets in the v19.06 install dir (we have NOT imported any of these yet)

| File | Size | What it is | Gap value |
|---|---|---|---|
| `Engines/*.ini` | **959** (vs our 825) | v19.06 engine specs, 134 new engines, updated INIs | **catalog gap** |
| `Email_Engines/*.ini` | **93** (vs 217 in v18.20) | v19 mail engines (shrunken set) | catalog |
| `se.dat` | 1.8 MB | search-engine master data (the "How to get Target URLs" table — 2525 engines) | **target-site search gap** |
| `generic_fields.dat` | 75 KB | form-field defaults: `*Gender*=%random_option%`, `*mail*address*=%random_email%`, `%columnspinfile-address_data.dat-4%`, `CaptchaTV2=%leave%` … | **auto-form-fill gap** |
| `titlegen.dat` | 119 KB | `[Lists]` templates: `%noun% Awards: {3|4|5…} Reasons…` — keyword→title generator | **title gen gap** |
| `charspin.dat` | 3.5 KB | char → unicode-alts table (`|`→`|&#32;|&nbsp;…`, `!`→`&#65281;…`) | **char-spin gap** |
| `calc_replace.dat` | 12 KB | HTML-ent/word-number→digit table (multilingual words!) for **captcha math solving** | **captcha gap** |
| `text_captcha_extract.dat` | 55 KB | hundreds of `label-prefix\|capture\|suffix` patterns to locate text-captcha answers in HTML | **captcha gap** |
| `html_variations.dat` | 7 KB | `[Section]`-grouped text mutations ("max length=30") to defraud duplicate content | dup-content gap |
| `content_search.dat` | 50 KB | regex-style content extraction incl. `local_images_to_data_url`, `spinfolder_data_url` (random image→data-url) | content-gen gap |
| `user_agents.dat` | 13 KB | 13 KB of real UAs (incl. very fresh Chrome 147/148 builds) | fingerprinting gap |
| `stop_words.dat` | 25 KB | multilingual stop words (`Add stop words to query with 20 %`) | search gap |
| `ip-to-country.csv` | 20 MB | 16-bit-prefix → country table (offline geo) | geo gap (we use live ipify only) |
| `gfx.dat`, `xgrab.txt`, `calc_replace.dat` | – | misc graphics/fields | minor |

Binary strings recovered (FREE.exe, 294k strings) — the **complete token vocabulary** GSA
resolves into engine specs:

`%anchor_text% %banchor_text% %panchor_text% %lanchor_text% %keyword% %keyword_1-5% %title%
 %generate_title% %noun% %blog_comment% %blog_title% %Guestbook_Comment%
 %Guestbook_Comment_(german)% %Image_Comment% %article% %article_title% %article_summary%
 %first_paragraph-article% %first_sentence-article% %forum_nick% %forum_password%
 %forum_subject% %email% %emailuser% %email_password% %emailhost% %domain% %country% %lang%
 %leave% %leave1% %captcha% %captcha_low% %captcha_reverse% %capquest% %hcaptcha%
 %referrer% %random_option% %random_email% %random_text% …`

Plus: **Captcha solvers = 2Captcha API (incl. Cloudflare Turnstile + hCaptcha demos), Anti-Captcha,
"GSA Captcha Breaker" (local OCR, ships `captcha_breaker.exe` + registry
`HKLM\SOFTWARE\GSA\GSA CAPTCHA BREAKER`), `textcaptchasolver`** — and export schema:
`"Date","URL","Anchor text","Engine","Type","DoFollow Link","Indexable","Links (int)","Links (ext)","Project"`.

---

## 3. What WE have (verified in-tree today)

✅ Spin/spintax · DB pipeline · 1042-engine catalog (v18.20) · Tauri+Vue app · step executor +
HTTP + CDP drivers · job queue + worker pool · keywords · **F#12 email pipeline (POP3+WEB inbox)** ·
**F#13 proxies (list import, round-robin, down-skip, proxy testing)** · **F#14 geo/proxy-country
routing + egress-IP verify (live ipify)** · F#15 settings/profiles · F#16 stats · F#17 scheduled runs ·
F#18 notifications · SSE live bus.

Project model today = only: `name, description, use_proxies`. **No URLs, no anchor mix, no comment
text, no search config, no filters, no timing.**

---

## 4. THE GAP LIST — ordered by their effect on "good backlinks, proxies, captchas, automation"

### P0 — the submission core (nothing we build works like GSA without these)
1. **Anchor-text rotation** — none of: main/secondary/branding/partial/LSI/generic/domain/citation/
   variations, **no % weights, no "total % ≤ 100" check**. Our model has zero of it.
   → `anchor_mix` (JSONB on project) + `AnchorAllocator::pick(rng, keyword, domain, spin_ctx) ->
   (token_to_substitute, resolved_text)` + `%banchor_text%/%panchor_text%/%lanchor_text%` in the
   context resolver. This is the #1 missing feature — without it every link says the same anchor.
2. **URL options**: random-per-submission (sequence vs random), URL variations %,
   root/main-URL % ("use root/main URL with 30 %"). → 3 booleans+ints on project, applied in
   executor before the `%url%` substitution.
3. **Multi-Tier/Linkwheel**: submit project-A's verified URLs from project-B (+ tier filter, +
   "Import verified URLs from external file @ interval" in External APIs). → table
   `verified_urls` + runner hook.
4. **Engine category selection** (the 22 categories + per-engine checkboxes) — we currently select
   engines by `type` string ad-hoc; GSA exposes the exact category tree. Import the v19.06 catalog
   (959 engines) and map categories from each INI's type.
5. **Anchor type filter "Type of Backlinks to create"** (anchor-text vs URL vs contextual per
   project) + "Try to skip creating Nofollow links" + dofollow-aware export column.

### P1 — make the submissions look human
6. **Blog Comment / Guestbook / Image comment templates**: per-engine-type comment text (spintax)
   + "Shortest/Longest" spin preview. → `project.comments` map (per category).
7. **Title generator** (`titlegen.dat`, `%noun%` templates) for Article/Forum/Bookmark subjects
   + `generate_title` token.
8. **Generic fields** (`generic_fields.dat`): auto-fill `%leave%`, `%random_email%`,
   `%random_option%`, email/name columns — our executor currently leaves unknown fields.
9. **Character spinning** (`charspin.dat` char→alt map, e.g. space→`&nbsp;`/`&#8287;`) on
   description/comment text — the "Use character spinning" option.
10. **Typos/spell errors injection (10 %)** on descriptions.
11. **Duplicate-content defusing** (`html_variations.dat`): rotate punctuation/case/word order of
    repeated text.
12. **User-agent pool** (`user_agents.dat`, 800+ real UAs, rotate per session).
13. **Random delays**: "wait before first post 0-5 min", "between two posts 10-1400 min",
    "further registrations 10-50 min" — **range** (min-max) random + "max posts per account **5**
    ±2" caps. Our runner currently has fixed per-job pacing only (F#17 is calendar-level).

### P2 — captchas (the explicit user requirement)
14. **Captcha pipeline**: (a) HTML locate via `text_captcha_extract.dat` patterns; (b) solve math
    via `calc_replace.dat` (ent + word-numbers); (c) OCR/local solve = "Captcha Breaker";
    (d) **2Captcha API adapter** (incl. `hcaptcha`, `cf-turnstile`, `recaptcha` tokens — our engines
    have 76 `recaptcha` specs!) + `Anti-Captcha` adapter; (e) "Skip hard to solve captchas"
    (skip-flag fallback); (f) "Custom Retry N times".
    → new `crates/engine/src/captcha.rs` + `crates/engine/src/solvertoken.rs`; config in settings
    (F#15) already exists to hold the API key.
15. **Captcha field tokens** in resolver: `%captcha% %captcha_low% %captcha_reverse% %capquest%`.

### P3 — finding GOOD target sites (what makes backlinks "very good")
16. **Target-site search config**: which search engines (`se.dat` — 2525 countries/engines list),
    "Add stop words to query with 20 %", quotes toggle, "similar looking keywords",
    "collect keywords from target sites / use collected keywords to find new target sites".
17. **Site-quality filters** (Filter URLs tab): outgoing-links cap, PR under/over,
    subdomain/IP-domain/number-domain/num-only-domain skips, "at least N keywords present on
    visible text", **word blocklists** (title + URL/domain), `"*Create Domain Filter"` (builds a
    TLD/domain wildcard list), skip-by-country (offline `ip-to-country.csv`) and **skip-by-language**.
18. **Duplicate-IP avoidance** ("Avoid posting to sites with duplicate IP") — we have geo/egress
    but not a seen-IP dedupe table.
19. **Global site-list reuse** ("Use URLs from global site lists if enabled" +
    Identified/Submitted/Verified/Failed/Other status filters) + "URLs linking on same verified
    URL" + "Analyse and post to competitor backlinks".
20. **DoFollow/NoFollow awareness in results** ("Verified DoFollow URLs" pool for Tier-2).

### P4 — workflow/QoL
21. **Notes tab** (notes + created-on stamp) + **Custom Project Color** (UI).
22. **Test button** (single-engine dry-run) — we have the runner; expose `POST …/test`.
23. **Export**: GSA's CSV schema (Date/URL/Anchor/Engine/Type/Dofollow/Indexable/Links
    int&ext/Project) + "Automatically export verified URLs".
24. **Pause-after-N-submissions** (project pause auto-stop with a resume timestamp).
25. **TAGS config** ("For TAGs use: Anchor texts / Keywords") — engine metadata injection.
26. **Index-Services**: send verified URLs to GSA SEO Indexer / other pingers; delay 2h.
    → out of scope unless user asks, but schema slot now.
27. **v19.06 catalog rebuild** (959 vs 825) + **se.dat** target engines (2525).

---

## 5. Suggested build order (respects our "one feature at a time, tests green" rule)

- **F#19 Anchor mix** (gap 1) — biggest single value; model + allocator + resolver + UI.
- **F#20 URL options** (gap 2) — small, pairs with F#19 UI (same form section).
- **F#21 Captcha pipeline** (gap 14–15) — user requirement; 2Captcha + math-ent + text-captcha
  locate + skip-hard + retry.
- **F#22 Humanize** (gaps 6–13) — one feature: comments/titlegen/generic-fields/charspin/typos/dup-
  content/UA pool/delays. Big but all "text transform" — reuse spin ctx.
- **F#23 Site finding** (gaps 16–19) — search-query builder + filters + word lists + dup-IP +
  country/language skip (uses ip-to-country.csv offline path).
- **F#24 Linkwheel / Multi-Tier + External-APIs import** (gap 3, 19, 20).
- **F#25 v19 catalog + se.dat** (gap 4, 27) + dofollow export (23).
- **F#26 Polish** (21–22, 24–26) — notes/color/test/export/pause/tags/index — original F#19 scope.

## 6. Immediate "copy these files in" list (data we can import unchanged)

```text
charspin.dat, calc_replace.dat, text_captcha_extract.dat, titlegen.dat,
generic_fields.dat, html_variations.dat, user_agents.dat, stop_words.dat,
content_search.dat, ip-to-country.csv,
Engines/ (959), Email_Engines/ (93)
```
Store under `data/gsa/` at repo root, load via a `SpecSource`-style loader (mirrors F#12's
install-root approach, no rebuild of the data).
