<!--
  GSA SER v19.04 — engine-spec directive & token dictionary
  Reverse-engineered via REA (MCP v6.4.0) from Search_Engine_Ranker.exe (Delphi, UPX-variant packed)
  + the readable sibling build GSAna_test.exe + SerLib.dll (.NET 4.7.2 Selenium wrapper)
  + the 991 engine INI corpus + 94 email INI corpus + config.ini + change.log + charspin.dat.
  Grounding: 92/102 directives confirmed as exact literals in the readable build.
  Generated 2026-10-30 for the ser-replica project.
-->

# GSA SER → ser-replica directive dictionary

Method: `Search_Engine_Ranker.exe` is packed with a non-standard UPX variant (standard
`UPX!` tail magic patched out), so its Delphi string table is invisible to REA/Ghidra and to
raw `strings`. The identical directive vocabulary was recovered verbatim from the readable
sibling build `GSAna_test.exe` (92 of 102 directives as exact literals); the remaining 10 are
confirmed in GSA's own `Engines/*.ini` data, `SerLib.dll`, `charspin.dat`, and `change.log`.

## A. Engine-spec directives (exact INI key spelling)

### Submit / success classification
- `submit success`
- `submit failed`
- `submit failed retry`
- `submit success skip verify`

### Verification suite (runner re-verify policy)
- `verify by`
- `verify on unknown status`
- `verify submission`
- `verify interval`
- `verify timeout`
- `first verify`
- `verify url`
- `verify search detail url`
- `verify search for`

### Form / navigation
- `form url`
- `form name`
- `form id`
- `optional form`
- `form request with`
- `must be filled`
- `hint`
- `modify url`
- `modify step`
- `modify step condition`
- `auto modify`
- `alternative url`
- `alternative url` (alias)
- `fixed url`
- `just download`
- `just_download` (parsed alias)

### Unknown-variable / content substitution
- `set unknown variable`
- `variable must be used`
- `alternate data`
- `remove`
- `replace`
- `add fixed data`
- `add fixed data condition`
- `auto add anchor url`
- `auto add anchor url content`

### Find / extract (URL delimiters + created-link)
- `find url`
- `find link`
- `url_front`
- `url_back`
- `delurl_front`
- `front1`
- `front2`
- `front3`
- `back1`
- `extract keywords`
- `extract urls`
- `extract url`
- `extract from email`

### Success gates (content / URL must-contain)
- `url must have`
- `page must have`
- `page must have1`
- `dofollow`
- `creates own page`
- `uses pages`
- `multiple posts per account`

### Anchor
- `anchor text`
- `anchor_text`
- `upcase`
- `custom link format`
- `html to custom link format`
- `html to bbs`
- `html line break`
- `allow html`

### Search-driven (blog-search flow)
- `allow return`
- `add keyword to search`
- `search term`
- `keyword`
- `skip ext links on`
- `skip content on`
- `skip url content on`

### Anchor length clamps
- `min length`
- `max length`

### Captcha
- `captcha`
- `captcha failed`
- `recaptcha`
- `hcaptcha`
- `cfcaptcha`
- `cf-turnstile`
- `adcopy`
- `charspin`
- `generate_title`

### Email engines (SerLib / mail pipeline)
- `random_email`
- `pop3 server`
- `pop3 port`
- `pop3 login`
- `pop3 password`
- `pop3 ssl`
- `emails match`
- `message match`
- `separator`

### Engine metadata / routing
- `engine type`
- `custom mode`
- `default`
- `default checked`
- `static`
- `country`
- `location`
- `city`
- `gender`
- `bbs supports`
- `index services`
- `linkwheel`
- `update_hosts`
- `update hosts`
- `seconds to wait before submission`
- `posted domain check`

## B. %tokens% (spintax / context substitution, exact)

Core: `%url%` `%anchor_text%` `%keyword%` `%search%` `%page%` `%content%` `%appname%`
`%link%` `%date%` `%title%` `%engine%` `%size%` `%random_option%` `%random_title%`
`%targethost%` `%random_email%` `%tag%` `%random_image%` `%noun%` `%year%` `%random_url%`
`%random_anchor_text%` `%generate_title%` `%domain%` `%login%` `%password%`
`%forum_password%` `%email%` `%emailhost%` `%emailuser%` `%email_password%` `%random%`

URL decomposition: `%url_domain%` `%url_path%` `%url_parameters%` `%url_host%`

Anchor-family (social / profile / blog / link variants + random forms):
`%sanchor_text%` `%panchor_text%` `%banchor_text%` `%lanchor_text%`
`%random_sanchor_text%` `%random_panchor_text%` `%random_banchor_text%` `%random_lanchor_text%`

Captcha: `%recaptcha%` `%hcaptcha%` `%recaptchav2%` `%recaptcha_challenge%`

Article / content: `%article%` `%article_title%` `%article_summary%` `%blog_title%`
`%ab_body%` `%source_url%` `%random_articlemix2%`

Target decomposition: `%targeturl%` `%formurl%` `%targetsubdomain%` `%targetdomain%`
`%original_targeturl%` `%original_targethost%` `%domain_as_name%` `%subdomain_as_name%`

Debug / exception: `%exceptMsg%` `%errorDetails%`

Spintax file/folder (GSA external spinner) family:
`%spinfile-%` `%spinfolder-%` `%spinfolder2-%` `%spinfolderdelete-%` `%columnspinfile-%`
`%spinfilename-%` `%spinfile-names.dat%` `%spinfile-generic_anchor_text.dat%`
`%spinfolder_data_url-%`

## C. SerLib.dll (.NET) — behavioral internals (why the replica needs a CDP/Selenium lane)

- .NET Framework 4.7.2, Selenium WebDriver wrapper ("GSAserlists web 2.0 engines").
- Browser: ChromeDriver / FirefoxDriver, `--headless=new`, proxy `--proxy-server={0}://{1}:{2}`
  (socks4/socks5 supported), blocked popups, `ignore-certificate-errors`.
- Captcha solve: `solve_recaptcha` (+ `_version`), `solve_hcaptcha`, `solve_cfcaptcha`,
  `solve_image_captcha`; insert variants `insert_recaptcha` / `insert_hcaptcha` / `insert_cfcaptcha`.
  Response fields: `g-recaptcha-response`, `cf-turnstile-response`.
- Element actions: `click_link_by_linktext`, `click_link_by_url`, `click_element`,
  `mouse_action`, `type_into_field` (+ `_selector` / `_tagname`), `execute_javascript`,
  `wait_for_css_selector`, `wait_for_text`.
- Retries: "Page appeared blank after waiting. Retrying (Attempt {0} of {1})",
  "Error: Page load timed out after multiple attempts".

## D. Replica status (ser-replica) — covered vs. gap

Already implemented and test-gated (this session + prior tiers):
- Submit classification: `submit success/failed/(retry)`, `captcha failed`, `just download`.
- Find/extract: `find url`, `find link`, `url_front/url_back`, `front1/front2/back`, `delurl`.
- Success gates: `url must have`, `page must have` (+ numbered) — demote false success.
- Anchor: `anchor text`, `upcase`, `capitalize words`, `min length`, `max length`.
- Search flow: `use blog search`, `search term`, `add keyword to search`, `allow return`,
  `skip content on`, `skip ext links on`, `extract keywords`.
- Pacing: `seconds to wait before submission` (+ condition).
- Context tokens: `%url%` `%anchor_text%` `%keyword%` `%article%` `%article_title%`
  `%blog_title%` `%domain%` `%login%` `%password%` `%email%` `%random_email%` etc.

Top remaining gaps (ranked by engine coverage — recommended next tiers):
1. Verification suite: `verify by / interval / timeout / search detail url / search for`,
   `first verify`, `verify on unknown status`, `submit success skip verify`.
2. URL-decomposition tokens: `%url_domain%` `%url_path%` `%url_parameters%` `%url_host%`.
3. Spintax file/folder family (`spinfile-`, `spinfolder-`) — external spinner source.
4. Anchor-family tokens (`%sanchor_text%` `%panchor_text%` `%banchor_text%` `%lanchor_text%`).
5. `remove` / `replace` / `alternate data` / `add fixed data` content transforms.
6. `multiple posts per account`, `uses pages` (multi-post / crawl-count).
7. `posted domain check` (reject created link on other domain).
8. `variable must be used` / `must be filled` plan-validation → API 400.
9. `html to bbs`, `html line break`, `allow html`, `custom link format` (content pipeline).
10. `dofollow`, `creates own page`, `default checked` (metadata flags).
