# Modulator client (`tb-eqam`): log and design record

Working log of the package that builds the sans-io client and supervisor for the EQAM-420B
web forms (SPEC §10). Started and finished 2026-11-26, unattended. **No request was sent to
the modulator.** Everything was built and tested offline against the pages saved in the
survey (`work/device/web/`, 2026-11-24/25, firmware 1.1.13) and against strings in the CGI
binaries pulled over FTP (`work/device/ftp/mirror/fs/home/httpd/cgi-bin/`).

## State

Built, on branch `worktree-agent-a415de7a7e3b9374a`. Not yet done: the HTTP adapter and the
`[device]` TOML mapping in the server binary (both are thin; see "For the server"). No live
check has been made; the list of read-only GETs for the lead is under "Live checks".

## The crate

| Module | What |
|---|---|
| `types` | Values that can only hold what the device accepts: `Slot` 1..=16, `RfModule` 1..=4, `StreamName` (1–10 ASCII alphanumerics), `StreamUrl` (`UDP://`/`RTP://a.b.c.d:port`, no 0.0.0.0, no port 0), `Channel` 2..=158 with the device's "next channel up" table, `FirstChannel` 2..=155, `QamMode`, `QamMap`, `OutputLevel` 28..=41, `SessionId` and `Credentials` (redacted `Debug`), `Uptime` |
| `refusal` | `screen(RequestDraft) -> Result<Request, Refusal>`: the only constructor of `Request` |
| `request` | `RequestDraft` (anything at all), `ReadPage`, `WriteAuthority`, `Response`, `Transport` (the adapter seam) |
| `html` | A small Latin-1 tokenizer and tree builder for these pages; forms and "successful controls" as a browser computes them |
| `pages` | Typed parsers: `status`, `statusedit`, `qamconfig`, `streaminfo`, `diagnostic`, `debug`, `eventlog`; `classify` (Logged Out, Modulator Error, login form, Welcome, too many users); the page header |
| `plan` | Desired state: `RfPlan` (four `ModulePlan`s and the level; no channel twice), `StreamTable` (16 `SlotConfig`s; no URL twice), `DesiredState` |
| `write` | `StreamTableWrite` and `RfPlanWrite`, step machines (next request ← previous response) |
| `session` | `Session` (login exchange, Logged Out, backoff, idle timeout, yielding to another Admin) and `Pacer` (one at a time, ≥ 1 s apart) |
| `client` | `Client`: jobs (`Read`, `WriteStreams`, `WriteRfPlan`), session, pacing, one retry for reads |
| `supervisor` | `Supervisor`: poll schedule, drift, report/apply, write limits, same-drift halt, reboot detection, event-log alarms, defect-D bookkeeping, metrics |
| `controller` | `Controller`: supervisor and client wired together; the adapter loop calls `next(now)` and `on_response(now, result)` |

Dependencies: `thiserror` only. Time is a monotonic `Duration` passed in.

## How the refusal list is enforced by types

- `Request` is defined in `refusal.rs` with private fields and no constructor other than
  `screen`; it is not `Clone` or `Default`. The `Transport` trait takes `&Request`. Two
  `compile_fail` doctests show that neither a `Request` nor a `WriteAuthority` can be made
  outside the crate (`cargo test -p tb-eqam --doc`; nextest does not run doctests).
- `screen` is an allow-list (table in the module docs). Checked in order: anything naming
  "admin" after percent-decoding and lower-casing (any method); a query parameter starting
  with `btn` (also one smuggled into the path or percent-encoded); `logout`; `config.cgi` or
  the export file; then only exact CGI names; POSTs to `mode.cgi` (rule `mode_write`), to
  every other existing page (`forbidden_write`), to `statusedit.cgi`/`qamconfig.cgi` without
  write authority (`write_outside_protocol`); anything else (`not_allow_listed`); then the
  exact query/body shape and values (`malformed`).
- `WriteAuthority` has a crate-private constructor and names its form (and module). Only the
  two write machines create one. A `qamconfig.cgi` post needs authority for the same module
  as its `slotNumber`, and the only holder, `RfPlanWrite`, always runs modules 1–4 in order.
  The machines themselves are crate-private (third `compile_fail` doctest): from outside, a
  write is a whole `Job` run by `Client`, so a single-module save cannot be expressed through
  the public API. Limit: an adapter that fabricates responses can make the client skip steps;
  no sans-io design can stop a lying transport.
- Session ids must be in the device's exact form (digits, no leading zeros or spaces).
- An exhaustive test feeds `screen` every combination of 19 page names × 5 path prefixes × 2
  methods × all 512 subsets of 9 parameters (as query or as body): exactly the 16 allow-listed
  reads and login pages are accepted, nothing else.
- A `statusedit.cgi` post is refused unless all 16 `streamGroupAddress<n>`/`streamName<n>`
  pairs are present (each a valid URL and name, or both empty), plus only
  `proIdiomEnable<n>=on`, `session_id` and `btnSave=Save`.
- Tests: `refusal::tests` (every refused class, by every method and spelling, and the
  allow-list itself).

## What each parser was tested against

| Parser | Saved page (golden test) | Synthetic fixture (and its source) |
|---|---|---|
| `classify` | `login.cgi` (session id replaced by 123456789), `login_post.html` | Logged Out, Modulator Error and "Maximum number of users" pages: verbatim from the strings in `status.cgi`/`login.cgi`. Login page with the admin-present `confirm()` and "Login failed": the saved page edited with strings from `login.cgi` (built in the session tests) |
| `PageHeader` | `status.cgi` and `status.cgi?boxNumber=2` (ESN, 80.8/81.2 °F, uptime, MPTS) | — |
| `StatusPage` | `status.cgi` (16 rows unassigned, channels 2–5, 10–13, 6/95/96/97, 140–143, all On, 4 × Locked, 40 dBmV) | `synthetic/status_assigned.html`: rows 1–2 and modules 2–3 rewritten from the `status.cgi` format strings (UDP/RTP URL, name, IP cell `#00FF00`/`#FF0000`, `%3.1fAvg/%3.1fMax`, `- / Off`, `ERROR`, `Not Installed`) |
| `StatusEditPage` | `statusedit.cgi` (16 empty rows, 82 controls, exact field set) | Edited copies: an enabled source field and an extra/missing field make the field-set check fail |
| `QamConfigPage` | `qamconfig.cgi?slotNumber=1..4` (channels, On/Off, CW, level and whether it is editable, 256B, STD, 5.360500 Mbaud, 128-1, 12%, Lock, the 2..155 option list); our channel table equals the page's `cable_channel_table_after` | Edited copies: module 2 with the level enabled; title and selected slot disagreeing |
| `StreamInfoPage` | `streaminfo.cgi` (no streams) | `synthetic/streaminfo_streams.html`: cells from the `streaminfo.cgi` format strings (`Stream %d - UDP://…`, `%d PMT %d`, `%d MPEG-2 Video`…) and the manual's example; the nesting is a guess |
| `DiagnosticPage` | `diagnostic.cgi` (header only) | One row built from the `diagnostic.cgi` format strings; column order beyond the first five cells is a guess (raw cell texts are kept) |
| `DebugPage` | `debug.cgi` (all counters, FPGA2B clocks 5–8 = 2 and highlighted, bitrates `0bps`, registers 0..33) | — |
| `EventLogPage` | `errorlog.cgi` (the three boot lines, colors, destination 10.42.0.53:514) | Alarm catalog from the event strings in the firmware (`mux_sync`, `admin.cgi`, `dhcpd`, `set_eth*_ip`, `set_route`) |

The write machines and the supervisor were tested against `tests/common` (`FakeDevice`), which
serves the saved pages with its state substituted, applies posts as the firmware does (a row
missing from a `statusedit.cgi` POST is zeroed), checks the session id, and logs requests.

## Decisions where the notes were ambiguous

1. **Session id on POSTs.** SPEC §10.1 says `?session_id=` on every request. The vendor page
   posts to `statusedit.cgi`/`qamconfig.cgi` with no query and the id in the hidden body field
   (filled from the cookie by the page's script); the login form does the same. POSTs here copy
   the browser exactly: id in the body, no query string. GETs carry `?session_id=` only (plus
   `slotNumber`). Not verified live; see open questions.
2. **"Post every field back"** is read as "post what a browser posts": enabled named controls,
   checked boxes, each select's selected option, and the clicked submit button, in document
   order. So `streamSourceAddress*` (disabled, IGMPv2), `outputChannelB..D` (disabled, derived
   from A on the server), and `outputLevel` on modules 2–4 (disabled) are not sent. A form
   whose disabled set differs (e.g. IGMPv3 enabling the source fields) fails the field-set
   check and the write is aborted.
3. `statusedit.cgi` gets `btnSave=Save` (the CGI reads `btnSave`); `qamconfig.cgi` gets
   `qam_saved=1` (what `validate_qam()` sets) and no button (`btnQAMSave` is `type=button`).
   `chkCW1` is never sent (CW off). These button fields are in POST bodies only; any `btn*`
   in a query is refused.
4. **All 16 slots are managed.** A slot absent from the configuration is desired empty (URL
   and name both empty), so in `apply` mode an unexpected stream in an unused slot is
   cleared. An assigned slot needs both a URL and a name. SPEC §10.1 says names are never
   empty; that rule (defect B turns an empty name into `0`) is kept for assigned slots. Unused
   rows are posted empty because that is what all 16 rows hold today and what the vendor page
   posts; a placeholder name in an unused row would be a change nobody asked for. If the lead
   prefers placeholders, `SlotConfig::name_text` (its `Empty` arm) is the one place to change.
5. If the form already holds the intended rows, `StreamTableWrite` posts nothing
   (`AlreadyInPlace`). `RfPlanWrite` always posts all four modules, even when nothing
   differs (SPEC §10.3: that also repairs defect A's stale bytes).
6. **Intermediate duplicates.** Saving modules 1→4 one at a time passes through mixed
   states. If any of them would put two carriers on one channel (e.g. swapping two modules'
   channels) the write is aborted before the first post. The page's own `check_duplicates()`
   refuses duplicates; what the firmware does with one is unknown. If the status page shows
   `-` for any current channel, the states cannot be checked and the write is aborted
   (`UnknownCurrentChannels`); every channel is shown on this unit today.
7. MPTS is checked twice: on `status.cgi` before the sequence, and on each module page
   before its post. `mode.cgi` is neither read nor written.
8. **Drift confirmation** (not in the SPEC): a drift must be seen on two consecutive polls
   before it is applied, so one odd page cannot cause a write.
9. **One reconcile** = every form that needs a write at that moment (RF plan first, then the
   stream table), counted once against "one per 10 min, three per 24 h" (rolling window).
10. **Same drift.** A drift signature is the exact set of (field, expected, observed). Each
    verified write counts one apply for the signature it fixed. When a signature that has been
    applied three times is seen again, writes stop (sticky, `HaltReason::SameDriftReturned`)
    and an alarm is raised. Counts do not decay; `resume_writes()` (operator) clears them.
    Note: with Configuration Mode still Automatic, defect B rewrites the stream table at every
    boot, so three reboots with re-applies will halt writes. That is the intended signal.
11. **Pro:Idiom boxes.** They are not managed, but a checked box rendered disabled would not
    be submitted and the CGI would clear it, so such a form aborts the write (`WouldChange`);
    the read-back also compares every row's box with its state before the post.
12. **Write failures.** Aborted before any post for a transient reason (logged out, transport,
    Modulator Error, module communication error): alarm only; the next reconcile is still rate
    limited. Anything else (unexpected field set, refused request, value not offered, not MPTS,
    intermediate duplicate, unknown channels, a box that would change, any failure after a
    post, read-back mismatch, a module communication error during read-back): writes stop
    (sticky) and, if anything was posted, a full comparison is scheduled.
13. **Reboot.** Expected uptime = last uptime + elapsed host time; an observed uptime more
    than 60 s below it is a reboot (also catches a reboot during an outage). Then: alarm,
    every page due now, the event-log memory cleared (the log is in RAM), and the defect-D
    table-reset time set to the boot time.
14. **Event log.** Lines are remembered by (time, message, severity). Alarm lines: "Difference
    detected in Config File" (defect B), "Difference detected in modulator settings" (defect
    A's retune), `CRITICAL ERROR` / EEPROM errors (corruption guard), "Default IP Used" /
    "Invalid Ethernet" (network defaults). The first poll after the process starts reports
    lines already there; on this unit that is one alarm for the boot-time reconcile line.
15. **Session.** Log in again without asking once idle for 14 of the 15 min. "Logged Out"
    while in use: log in again at once, back off (5 s doubling to 5 min) if it happens within
    5 s of a login. If our live session was lost and the login page then warns that an
    Administrator is logged in, wait 15 min before taking over (`YieldingToOtherAdmin`); the
    API pause (`Supervisor::pause`) is the explicit way. "Login failed": wait 15 min. The id
    in the Welcome page's refresh URL wins over the one offered (the two saved captures differ
    because they come from different logins).
16. **Retries.** One retry after a timeout for reads (also the read steps inside a write) and
    one after a Modulator Error page; a read that hits "Logged Out" is repeated once after
    logging in. POSTs (including login) are never retried.
17. **HTML.** A ~400-line tokenizer and tree builder instead of `scraper`/`html5ever`: the
    pages are `printf` templates whose grammar is visible in the binaries, and the general
    parsers bring a large dependency tree. Rules: Latin-1, raw-text `<script>`, implied ends
    for `td`/`tr`/`option`, unknown end tags ignored.
18. **`device_overflow_total{carrier}`** maps slot n to `FPGA2A Stream n` (1–8) and `FPGA2B
    Stream n−8` (9–16) of debug.cgi's "Buffer Overflow Errors". An inference from device
    §11.3; check once streams run.
19. **Defect D.** "Table reset" = a detected boot, or `note_table_reset(now)` from the server
    after it restarts `mux_sync`. `device_seconds_since_table_reset` = min(uptime, time since
    the noted reset). The scheduled reboot itself (telnet `epcs -c`) is not in this crate;
    `admin.cgi` (the web reboot) is refused.

## Review

An independent read-only review of `refusal`, `request`, `write`, `types`, `client` and
`session` found no request that `screen` accepts and should refuse. Its other findings were
fixed: the write machines were public (a caller could feed them made-up pages and keep a
screened single-module post), so they are now crate-private; unknown current channels were
skipped in the intermediate-duplicate check (now an abort); a checked-but-disabled Pro:Idiom
box would have been cleared silently (now an abort, and compared on read-back); a module
communication error during read-back was ignored (now a mismatch); non-canonical session ids
were passed through (now refused); a latent path could drop a write without a report (now
reports `Abandoned`). The empty-name question is decision 4.

## For the server

- Adapter: loop on `Controller::next(now)`; `Send(req)` → `GET`/`POST` `base_url +
  req.target()` with `req.body()` and `req.content_type()`, 30 s timeout, no redirects, no
  cookies needed, pass `Ok(Response{status, body})` or `Err(TransportFailure)` to
  `on_response`. `WaitUntil(t)` → sleep. Forward `Action`s to alarms, metrics and the
  `device_log` table (`WriteLogged` carries before/after lines and the post responses).
- `[device]` mapping: `reconcile` → `SupervisorConfig::mode`; `status_interval`;
  `rf_module` × 4 → `RfPlan::new` (a level key is needed: the sketch has none); each carrier's
  `destination` + framing + a name → `StreamTable` (the name must satisfy `StreamName`);
  `defect_d` limit → `defect_d_limit`. Credentials from the credentials file →
  `Credentials::new`.
- `/api/device/pause` → `supervisor_mut().pause(until)`; `/api/device/reconcile` →
  `compare_now(now)`; metrics → `supervisor().metrics().samples(client().refused())`.

## Live checks for the lead (read-only)

In this order, one request at a time, ≥ 1 s apart, through the existing `tools/eqam_get.sh`
or a browser. Save each body under a name the opt-in test understands and run
`TB_EQAM_CAPTURES=<dir> cargo nextest run -p tb-eqam --run-ignored only captures`.

1. `GET /cgi-bin/login.cgi` → save as `login.html`. Check: a session id in the hidden field.
   If someone is logged in as Admin in a browser at the time, check the `confirm()` text
   "An Administrator is currently logged into the system" appears (the yield rule relies on
   it).
2. The login itself is a POST (`txtUserName`, `txtPassword`, `session_id`, `btnSubmit=Submit`),
   as `tools/eqam_login.sh` does; it ends any other Admin session. Save the Welcome page as
   `login_post.html` and check its refresh URL carries the same id that was posted.
3. `GET /cgi-bin/status.cgi?session_id=<id>` → `status.html`.
4. `GET /cgi-bin/statusedit.cgi?session_id=<id>` → `statusedit.html` (the field-set check
   runs in the test of the saved page; compare the control list).
5. `GET /cgi-bin/qamconfig.cgi?session_id=<id>&slotNumber=1` … `=4` → `qamconfig_1.html` …
   `qamconfig_4.html`.
6. `GET /cgi-bin/streaminfo.cgi?session_id=<id>` → `streaminfo.html`.
7. `GET /cgi-bin/diagnostic.cgi?session_id=<id>` → `diagnostic.html`.
8. `GET /cgi-bin/debug.cgi?session_id=<id>` → `debug.html`.
9. `GET /cgi-bin/errorlog.cgi?session_id=<id>` → `errorlog.html`.
10. `GET /cgi-bin/status.cgi?session_id=1` (a wrong id) → `logged_out.html`: confirms the
    synthetic Logged Out page.

Later, once a stream is configured (by hand) and flowing, repeat 3, 6, 7 and 8: they settle
the IP-cell colors and bitrate format on `status.cgi`, the real layout of `streaminfo.cgi`,
the diagnostics column order, and the debug-counter-to-slot mapping.

## Open questions

1. **Does a POST authenticate by the body's `session_id`?** (Decision 1.) Only a write can
   tell. The first supervised write should be watched (lead present), and its
   `post_responses` (title of what each POST returned) recorded.
2. **What do `statusedit.cgi` and `qamconfig.cgi` answer to a POST?** Not seen. The machines
   accept any 200 page that is not "Logged Out" / "Modulator Error" and rely on the read-back.
3. **Guest account.** The manual allows four concurrent read-only Guest sessions that do not
   end the Admin's. Polling as Guest would leave the Admin session to people. Unknown whether
   Guest can read every page here; the crate takes any user name, so it is a configuration
   choice once checked.
4. **Name auto-detection.** The manual says the Name is auto-detected "if assigned by the
   source". If the box rewrites names from our streams' tables, name drift would recur and
   trip the same-drift halt. Watch the Name column once streams run.
5. **Relock after a QAM save.** If a module reads "Unlock" right after its save, the read-back
   still passes (lock is not compared) but a `ModuleNotLocked` alarm may fire once.
6. **Configuration Mode.** Defect B's mitigation (Manual mode) lives on `admin.cgi`, which the
   crate refuses, so the supervisor cannot confirm it (SPEC §18). Until it is set by hand, the
   event-log alarm after each boot and the same-drift halt are the evidence.
7. Status-page module cell values other than `Locked` / `Not Installed` / `ERROR` are not in
   the strings; anything else is reported verbatim as an error state.

## Live check, 2026-11-26 03:10 local (lead)

Steps 1–10 above were run once, through `tools/eqam_login.sh` and `tools/eqam_get.sh` (one
login POST, then bare GETs 1.5 s apart); captures kept outside the repository. Every page was
recognized and parsed by the opt-in `captures` test: login form (no Admin present), Welcome,
status (MPTS, four modules Locked, 85.8 °F, uptime 706,706 s = 8 d 4 h, i.e. no reboot since
the survey of 2026-11-24), statusedit (same field set as the saved page; all 16 rows still
empty), qamconfig 1–4 (Lock), streaminfo (no streams), diagnostic, debug, errorlog (the same
three boot lines, including "Difference detected in Config File"), and the Logged Out page for a
wrong id (the synthetic fixture's shape confirmed). Nothing was written. The GigE input port
10.42.0.183 does not answer from bagel or nixark, so no stream can reach the box yet.
