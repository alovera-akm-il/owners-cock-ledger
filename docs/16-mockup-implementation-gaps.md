# Mockup vs. Implementation Gap Audit

A page-by-page comparison of every file in `mockups/` against its real
counterpart in `templates/`, done on request after a few gaps (a
static instead of live countdown, a missing nav link, an unwired toy
photo field) were spotted by inspection rather than by any systematic
pass. This document is that systematic pass, plus an explanation of
why the ad-hoc spotting missed things a page-by-page comparison
catches immediately.

**Methodology note, stated up front rather than left implicit:** pages
marked "Deep audit" below were compared field-by-field — every input,
button, and conditional state in the mockup checked against the real
template and, where relevant, against what the API actually accepts.
Pages marked "Structural pass" were compared by element-ID diff and
section-header scan only — real gaps at that level are reported with
the same confidence as the deep-audit pages, but a *matching* ID/field
count on a structural-pass page is weaker evidence than it looks: it
confirms the two pages have the same-shaped skeleton, not that every
field inside that skeleton behaves identically. Treat "no gap found"
on a structural-pass page as "nothing obvious," not "verified clean."

**No deference on divergences.** A follow-up research pass converted
every remaining "Structural pass, needs a closer look" row to a Deep
audit. The operating rule for that pass, stated explicitly after it
was pointed out mid-audit: a mockup feature with no real counterpart
is a gap to close, not something to wave away as an acceptable
simplification, and a place where the real app took a *different*
approach than its mockup (not just a smaller one) gets surfaced for
the user to decide, not resolved unilaterally by asserting "the real
version is fine." The Review-flow and Points/Redemptions rows in §4
below are the two cases that came out of that pass; both were
resolved by direct question rather than by this document's own
judgment, and both resolved toward closing the gap.

## Why the first pass (timer/menu/toy page) didn't catch the rest

That pass was scoped to *page and navigation structure* — comparing
nav-bar link sets, breakpoints, and route existence, because those
were the specific symptoms reported ("differences in timer, menu and
toy page"). It answered "does this page exist, and does the nav
around it match" for a handful of pages. It did not ask "does every
field inside this form have a real, wired counterpart" for any page,
which is a different and finer-grained question — that's precisely
where the toy photo/compatible-device gaps and the device-description
gap were hiding, and it's why they surfaced only once the question
changed from "is the toy page structurally right" to "does every
field in the toy form actually do something." The most severe finding
in this document (the missing 2FA login-challenge UI, §2) was invisible
to *both* passes and to the entire automated test suite, for a third,
distinct reason: the test suite verifies the API contract via direct
HTTP calls (`TestClient`), never through the browser page that's
supposed to consume that contract, and no Playwright walkthrough this
session ever logged in as a 2FA-enabled account — every browser
verification used a fresh, 2FA-off test account for speed. A gap in
"does the UI correctly handle every shape of API response" doesn't
show up in either an API test or a UI structure diff; it only shows up
by actually driving the page through that specific state.

---

## 1. Summary punch list, by severity

| # | Finding | Page(s) | Status |
|---|---|---|---|
| 1 | Login page has no 2FA challenge step at all — a 2FA-enabled user cannot log in through the web UI | `login.html` | **Fixed** |
| 2 | Toy photo upload: DB column + API field exist, no upload route, no UI anywhere | Toy catalog (both roles) | **Fixed** |
| 3 | Safety alerts list shows no submissive name/attribution — a Keyholder with 2+ submissives can't tell whose alert is whose without a separate lookup | `safety_alerts.html` | **Fixed** |
| 4 | Structured hard/soft limits + free-text limits live on a separate page instead of the profile — user-requested consolidation | Profile, Limits | **Fixed** |
| 5 | No per-submissive single-item proof review view (mockup's `proof-review.html` shape) — only the cross-submissive queue exists | Review flow | **Fixed** — additive to the existing queue, per user confirmation |
| 6 | No keyholder-wide pending-redemption-requests view — only visible per-submissive | Keyholder dashboard | **Fixed**, per user confirmation |
| 7 | Submit-proof page: no live countdown to code expiry, no "request new code" if it expires mid-fill, no in-browser record capture (file upload only) | `submit_proof.html` | **Live countdown + request-new-code fixed.** In-browser record capture still deferred — see note below. |
| 8 | Confinement timer was static text, not live | Dashboard, submissive detail | **Fixed** |
| 9 | `submissive_detail.html` nav missing "Limits" link | Submissive detail | **Fixed** |
| 10 | No mobile nav on any page (19 pages) | All | **Fixed** |
| 11 | Toy `compatible_device_id` and `acquired_at`: API/DB support them, no UI anywhere (mockup never designed for them either) | Toy catalog | `compatible_device_id` fixed; `acquired_at` was not actually wired up despite being marked Fixed here earlier — see item 23 |
| 12 | Device `description` field: API accepts it, no UI input (mockup never had one either) | Submissive detail (devices) | **Fixed** |
| 13 | No Audit Log UI (backend writes the log; nothing reads it back) | `audit_log.html` (`/keyholder/audit-log`) | **Fixed** — see note below the table |
| 14 | No submissive History page | `submissive_history.html` (`/submissive/history`) | **Fixed** — see note below the table |
| 15 | No keyholder-wide cross-submissive Toy catalog view | — (page doesn't exist) | Deferred (scoped out earlier) |
| 16 | A submissive cannot see their Keyholder's stated boundaries anywhere (mockup's `submissive-profile.html` "Your Keyholder's boundaries" read-only panel has no real counterpart) | `submissive-profile.html` | **Fixed** |
| 17 | Mockup's `keyholder-profile.html` has a "Your submissive's boundaries" read-only mirror; real has no equivalent on the Keyholder's own profile page | `keyholder-profile.html` | Not a gap — see note below |
| 18 | Keyholder dashboard is missing an entire feature the mockup designed: a cross-roster "Needs your attention" priority feed, four stat cards, and an enriched roster table (lock status/last verification/pending/open items) — the real page was a bare two-column table with a literal "not built yet" placeholder comment still in the code | `keyholder-dashboard.html` | **Fixed** |
| 19 | Layout pattern: the mockup consistently uses a two-column grid on `submissive-dashboard.html`/`submissive-detail.html` (status/config left, activity/actions right); the real templates had identical content but stacked every panel full-width in one column | `submissive-dashboard.html`, `submissive-detail.html` | **Fixed** for these two pages — see note below on scope |
| 20 | No email or display-name change anywhere in the real app — neither in the UI nor the API/domain layer — though the mockup's `keyholder-profile.html` designs a password-confirmed email-change flow and an editable display-name field | `keyholder-profile.html`, `submissive-profile.html` | Email change confirmed out of scope by the user. Display name: **Fixed.** |
| 21 | Proof review has no inline consequence-attachment: the mockup lets a Keyholder attach a punishment (from catalog or created on the spot, optionally saved to catalog) as part of marking something Failed, in one action; the real flow requires a separate manual "Assign something" step afterward | `proof-review.html` | **Fixed** — also fixed along the way: the Review Queue never attributed cards to a submissive at all (`ReviewQueueItem` now carries `submissive_id`/`submissive_display_name`) |
| 22 | Redemptions page shows only pending requests; the mockup's `keyholder-points-and-redemptions.html` also shows "Recently decided" (a decided-request history) | `keyholder-points-and-redemptions.html` | **Fixed** |
| 23 | Item 11's "Fixed" status was wrong: only `compatible_device_id` was ever wired into the toy form; `acquired_at` has no UI in either role's toy catalog page despite the API/DB supporting it (confirmed via direct code search — zero matches for `acquired` in either template) | Toy catalog (both roles) | **Fixed** — PATCH previously couldn't touch `acquired_at` at all either (create-only); that's fixed too |

Items 13–15 were already surfaced and explicitly deferred by the
user's own scoping decision in this session ("bugs + timer + mobile
menu only... leaves Audit Log, History, and cross-submissive Toys as
separate future work"). They're listed here again only so this
document is a complete inventory, not because they're new. All three
were later picked up and fixed in a later session — see below for
item 13's scope note, and `docs/17-duplication-ledger.md`'s intro for
item 15's shape (folded into the existing per-submissive Toy catalog
rather than a separate global page).

**Item 13's real scope, found before building it**: an audit of every
`audit::record(...)` call site (there are 15, not the "everywhere"
the mockup's example rows imply) found the write side is nowhere near
comprehensive — timer pause/resume/adjust, proof-review outcomes,
task completion, points/redemptions, and API token creation write
*no* audit row today. Confirmed with the user before building rather
than silently deciding: ship the page against the real, narrower
13 action types that actually exist (`audit_log.html`,
`domain::audit::list_for_keyholder`), not expand write-side coverage
first. The page is real and correctly scoped to what's actually
logged; it's just a sparser table than the mockup's examples suggest,
by design. Also fixed along the way: `assignments::run_deadline_sweep_tick`'s
`"assignment.auto_failed"` row never set `link_id` even though the
assignment's link was resolved a few lines later in the same
function — reordered so it does, since an audit row a Keyholder can't
filter by submissive defeats half the point of the page.

**Item 14** turned out to need almost no new backend at all — the
three data sources the mockup's tabs need already existed and already
returned everything required: `GET /submissive/proof-submissions`
(Verifications), `GET /submissive/assignments` (Tasks, Rewards &
Punishments), each already unfiltered by status. The one real gap was
timer-adjustment history: `confinement::list_adjustments` only ever
covered a single session, so a submissive who'd been unlocked and
relocked couldn't see adjustments from an earlier session. Added
`confinement::list_adjustments_for_submissive` (joins across every
session) and `GET /submissive/timer-adjustments` for it. Two mockup
details were deliberately dropped rather than built at real cost: a
"missed" verification-window row (there's no `proof_submissions` row
at all when nothing was ever submitted — would need cross-referencing
expired, unconsumed `verification_codes`, a separate feature) and the
"↳ if missed: X" forward-looking hint on an in-progress task (needs
resolving `on_failure_template_id` to a catalog title, not just
another assignment already in the list). The "↳ escalated to/from" chain
info that *is* shown is resolved client-side from the same fetched
assignments list — no chain-walking endpoint needed for that part.

Items 5 and 6 were resolved by direct consultation rather than
unilateral judgment, per explicit instruction after the first research
pass ("if there is a change in how UI worked, consult with user"):
both are cases where the real app took a different shape than its
mockup, and the resolution in both cases was "close the gap by adding
the missing piece" rather than "the divergence is fine as-is." See
the Review Queue and Points-and-Redemptions rows in §4 for the full
reasoning each was weighed against.

Item 17 is deliberately marked "not a gap" rather than deferred or
consulted: the mockup's "Your submissive's boundaries" panel assumes a
1:1 Keyholder↔submissive relationship (one profile page, one submissive
to mirror), but the real app is 1:many — a Keyholder's own profile page
has no single submissive to show. This isn't the real app taking a
narrower or different approach that needs a user decision; it's the
mockup's simplification not surviving contact with an architecture the
real app already generalizes beyond. The information itself is already
reachable via the submissive's own detail page, same posture as
`checkin-live.html`'s and `submit-checkin.html`'s "mockup is an
illustrative simplification" findings elsewhere in this document.

---

## 2. Critical: no 2FA login-challenge UI (fixed)

**`templates/login.html`** posts credentials to `POST /api/v1/auth/login`
and does:

```js
.done(function (res) {
  window.location = res.role === 'keyholder' ? '/dashboard' : '/submissive';
})
```

When the account has 2FA enabled, that endpoint does **not** return
`{role: ...}` — it returns `{requires_2fa: true, challenge_token: "..."}`
(`src/api/auth.rs`, `LoginOutcome::RequiresTwoFactor`), and completing
login requires a second call to `POST /api/v1/auth/2fa/verify` with
that token plus a 6-digit code. `login.html` has no code path for this
response shape at all: `res.role === 'keyholder'` is `false` (there is
no `role` field), so the page silently redirects to `/submissive`
without ever completing login — the request never actually
authenticates the session.

**mockups/login.html**, by contrast, has a full multi-step flow:
`login-step-password` → `login-step-forgot`/`login-step-forgot-sent`
(a self-service forgot-password flow the real backend also doesn't
support — that part of the mockup is correctly *not* built, since
`10-operations.md` §5 deliberately keeps password reset admin-only)
and, relevantly, a `tfa-code` input + `tfa-verify-btn` step that
appears after password submission when the server signals 2FA is
required.

**Net effect (as found):** every other 2FA surface in the app is real
and tested — setup, confirm, recovery codes, disable, all wired to
`account_settings.html` and covered by
`two_factor_setup_confirm_and_login_challenge_round_trip` — except the
one moment a 2FA-enabled user actually needs it: logging back in. This
was the highest-priority item in this document.

**Fixed:** `login.html` now branches on `res.requires_2fa`, shows a
code-entry step, and posts it to `/api/v1/auth/2fa/verify` before
redirecting — the same two-call flow the API always expected, just
completed by the page instead of abandoned after the first call.

---

## 3. Profile page: structured limits/kinks consolidation (design decision, fixed, later superseded)

**Historical note, added after this section went stale:** everything
below this point describes a mechanism that no longer exists. It was
accurate when written, but a later "Limits & boundaries" redesign
(`docs/17-duplication-ledger.md`'s intro list, commit `11976e8`)
replaced it entirely — no rating buttons, no category grouping, no
notes textarea, and no submissive-only guard remain in
`account_settings.html` today. Kept below anyway because the "why"
(consolidating two pages into one, removing `/submissive/limits`)
is still real project history, not because it's still an accurate
description of the current UI. **For the current mechanism, see
`docs/17-duplication-ledger.md`'s intro and §2–§3.**

**Current state (as of this section, since superseded):** one system
now exists for "boundaries," on one page (`account_settings.html`).
Previously there were two separate systems on two separate pages,
described below for context.

- **Free-text hard/soft limits** — a plain textarea pair on
  `account_settings.html` (`#profile-hard-limits`,
  `#profile-soft-limits`), already wired to `GET/PATCH /api/v1/profile`.
  This part already lives on the profile page today.
- **Structured, rated limits catalog** — a Keyholder-curated list of
  specific items (by category: Impact, Bondage & Restraint, House
  Rules, etc.), each rated `hard`/`soft`/`okay` by the submissive with
  an optional note. This is `templates/submissive_limits.html`
  (`/submissive/limits`) for rating and `templates/limits_catalog.html`
  (`/keyholder/limit-items`) for catalog management — two separate
  pages, reachable via their own nav links.

**Decision (this session):** the structured rating UI moved onto the
profile page instead of living at `/submissive/limits`, so a submissive
sets both their free-text limits and their per-item ratings ("kinks")
in one place. Implemented as:

- A "Limits & kinks" section added to `account_settings.html` (submissive
  view only, `{% if !is_keyholder %}`), reusing the rating-card component
  from the old `submissive_limits.html` almost verbatim (category
  grouping, hard/soft/okay buttons, notes textarea, clear-rating) —
  a relocation, not a rebuild. No "Manage catalog →" link was added: that
  page (`/keyholder/limit-items`) is Keyholder-only and redirects a
  submissive away, so linking to it from the submissive's own profile
  would just be a dead end.
- `/submissive/limits` removed as a route (`SubmissiveLimitsTemplate` and
  its handler deleted from `src/web/mod.rs`), its template file deleted,
  and its "My Limits" nav link/mobile-menu entry removed from the 7 pages
  that referenced it (the page itself no longer exists, so 6 remain to
  clean up plus itself).
- The Keyholder-side catalog *management* page (`/keyholder/limit-items`,
  `limits_catalog.html`) is untouched — different audience and action
  (curating the item list, not rating it).

**What changed in the later redesign** (full detail in
`docs/17-duplication-ledger.md`): the "Limits & kinks" rating-button
section above was itself removed and replaced by three symmetric
autocomplete-enabled free-text fields (Hard/Soft/Okay), and — unlike
the submissive-only section described above — the replacement renders
for **both roles**, each against its own endpoint
(`/api/v1/submissive/limit-items` vs. the newer
`/api/v1/keyholder/limit-ratings`). The `/submissive/limits` removal
and the "no Manage catalog link" reasoning above both still hold; the
rating-card UI they were removed alongside does not.

---

## 4. Page-by-page comparison

Grouped by area. "Real" gives the template file and route; "Mockup"
the corresponding file; "Audit" the depth per the methodology note
above.

### 4.1 Auth & onboarding

| Mockup | Real | Audit | Findings |
|---|---|---|---|
| `login.html` | `login.html` (`/login`) | Deep | **Critical gap** — see §2. Forgot-password flow in the mockup is correctly unbuilt (admin-only reset is the real design, `10-operations.md` §5). |
| `reset-password.html` | *(none — CLI-only)* | Deep | Intentional: password reset is `admin reset-password` on the box, never a web page. Mockup exists to visualize a flow this app deliberately doesn't offer self-service. |
| *(none)* | `redeem_invite.html` (`/invites/redeem`) | — | No corresponding mockup file exists under this exact name, but the flow is depicted inline as part of other mockups' invite-copy text. Real page matches the documented flow (`03-api-design.md` §1). |

### 4.2 Keyholder pages

| Mockup | Real | Audit | Findings |
|---|---|---|---|
| `keyholder-dashboard.html` | `dashboard.html` (`/dashboard`) | Structural | Mockup's top nav includes **Toys** and **Audit Log** as global items; real has neither (toys is per-submissive only, no cross-submissive route exists — item 15, deferred; audit log has no UI at all — item 13, deferred). Mockup's roster table and invite-modal shapes match the real implementation. |
| `keyholder-toy-catalog.html` | `toy_catalog.html` (`/keyholder/submissives/{id}/toys`) | Deep | Mockup is a **global, cross-submissive** catalog with a submissive-selector dropdown; real is scoped to one submissive, reached only by drilling in — a different IA, and the real API has no cross-submissive toy-listing route to support the mockup's shape (item 15, deferred). Photo upload (item 2) and `compatible_device_id` (item 11) — both **fixed**. |
| `keyholder-profile.html` | `account_settings.html` (`is_keyholder` branch) | Deep | Mockup's Personal info/Safety/Boundaries sections are split three ways; real merges them into one "Personal profile" section — functionally equivalent fields (bio, contact info, hard/soft limits, timezone), just fewer panels. 2FA, sessions, password change all real and matching. API tokens section is real-only (mockup predates it; correct, since tokens are a later addition per `01-data-model.md` §9). No "Your submissive's boundaries" read-only mirror section — see item 17: not a gap, the mockup's 1:1 assumption doesn't generalize to the real app's 1:many roster, and the information is already visible on the submissive's own detail page. |
| `keyholder-limits-catalog.html` | `limits_catalog.html` (`/keyholder/limit-items`) | Deep | Confirmed no gap: the mockup's closing note ("Sensation, Chastity & Denial, Fluids, Psychological, Medical, and Exhibitionism ship the same way — a modest starter list per category") is real — `migrations/0029_limits.sql` seeds exactly those categories with starter items. |
| `keyholder-safety-alerts.html` | `safety_alerts.html` (`/keyholder/safety-alerts`) | Deep | **Fixed (item 3):** the mockup labels every alert row with the submissive's name ("Riley," "Sam," "Jordan"); the real list previously had no name or attribution anywhere (`AlertResponse` only exposed a raw `submissive_id`). `alert_response()` (`src/api/safety.rs`) now resolves and includes `submissive_display_name`, and `safety_alerts.html`'s `renderAlert()` displays it. |
| `keyholder-recurring-tasks.html` | `recurring_tasks.html` (`/keyholder/submissives/{id}/recurring-tasks`) | Deep | Built this session directly against this mockup; matches. |
| `keyholder-submissive-statistics.html` | `keyholder_submissive_statistics.html` | Deep | Built this session directly against this mockup; matches. |
| `keyholder-points-and-redemptions.html` | *(folded into `submissive_detail.html`'s Points panel, per-submissive)* + `redemption_requests.html` (`/keyholder/redemption-requests`) | Deep | **UI-approach divergence, resolved by consultation and fixed (item 6).** Mockup is a cross-submissive "Pending redemption requests" table; real required opening each submissive individually to see theirs. Decision: keep the folded-in, per-submissive panel as-is (it's complete and working), and add the one missing piece — a keyholder-wide pending-redemptions view (`redemption_requests.html`), the same aggregation pattern the Review Queue already uses across submissives. Not a full return to the mockup's standalone-page shape. |
| `keyholder-audit-log.html` | *(none)* | — | Item 13, deferred. Backend (`domain::audit`) writes rows on every state-changing action already; nothing reads them back. |
| `catalog.html` | `catalog.html` (`/keyholder/catalog`) | Structural | Field counts match (4/4) for the task/reward/punishment template form. |
| `checkin-templates.html` | `checkin_templates.html` (`/keyholder/checkin-templates`) | Structural | Field counts match; real has an extra `field-key` input the mockup doesn't show explicitly. |
| `play-session-templates.html` | `play_session_templates.html` (`/keyholder/play-session-templates`) | Structural | Real has *more* fields than the mockup (6 vs 3) — real exceeds the mockup here, not a gap. |
| `proof-review.html` | `proof_review.html` (`/keyholder/review`) + `submissive_review.html` (`/keyholder/submissives/{id}/review`) | Deep | **UI-approach divergence, resolved by consultation and fixed (item 5).** The mockup is a single-submission review view reached from one submissive's own page (`← Riley` back-link); the real page is a cross-submissive "Review Queue" aggregating every submissive's pending proof. Decision: keep the queue *and* add the per-submissive single-item view the mockup shows (`submissive_review.html`) — additive, not a replacement. |

### 4.3 Submissive pages

| Mockup | Real | Audit | Findings |
|---|---|---|---|
| `submissive-dashboard.html` | `submissive_dashboard.html` (`/submissive`) | Deep | Live countdown — **fixed**. Nav: mockup lists "Rewards & Points" and "History" as their own pages; real folds points into the dashboard inline and has no History page at all (item 14, deferred). |
| `submissive-detail.html` | *(this is the Keyholder's page, see 4.2 — mockup name is misleading)* | Deep | Covered under `submissive_detail.html` in 4.2's row-equivalent; also: live countdown fixed, "Limits" nav-link bug fixed this session. |
| `submissive-toy-catalog.html` | `submissive_toys.html` (`/submissive/toys`) | Deep | Same photo-upload and `compatible_device_id` fixes as the Keyholder-side toy catalog (item 2, item 11) — these were backend/schema-level gaps shared by both roles' toy UI, not role-specific, so one fix covered both. |
| `submissive-profile.html` | `account_settings.html` (submissive branch) | Deep | See §3 for the limits/kinks consolidation (fixed). Also fixed (item 16): a read-only "Your Keyholder's boundaries" panel now shows the linked Keyholder's bio/hard limits/soft limits, via the new `GET /api/v1/submissive/keyholder-profile` endpoint (`src/api/profiles.rs`). |
| `submissive-limits.html` | *(folded into `account_settings.html`'s "Limits & kinks" section, per §3)* | Deep | Functionally solid match to the mockup (category grouping, rating buttons, notes) — now reached via the profile page instead of its own route. |
| `submissive-play-sessions.html` | `submissive_play_sessions.html` (`/submissive/play-sessions`) | Structural | Not deep-audited. |
| `submissive-play-session-detail.html` | `play_session_detail.html` (shared route, role-aware) | Structural | Real is substantially richer than the mockup (judgement flow, punishment/reward custom forms, schedule panel, toys panel) — real exceeds mockup, not a gap. |
| `submissive-rewards.html` | *(folded into `submissive_dashboard.html`'s Points panel)* | Structural | Functionally present (redeemable list, redeem button, pending-redemption state) — just inline on the dashboard instead of a dedicated page. Not a gap, a placement choice consistent with the points-and-redemptions consolidation on the Keyholder side too. |
| `submissive-history.html` | *(none)* | — | Item 14, deferred. Nothing in the real app currently explains "why did my time change" the way this mockup page promised (linked from the mockup dashboard's countdown hero). |
| `submit-proof.html` | `submit_proof.html` (`/submissive/submit-proof`) + `assignment_proof.html` (task-specific) | Deep | See item 7 — live code-expiry countdown and in-page "request new code" **fixed**; in-browser record capture (upload-only currently) still deferred, on the basis that it's a materially larger feature (getUserMedia capture UI, a record/stop/preview flow, converting the captured blob into the same multipart upload the file-picker path already uses) rather than a small addition alongside the other two, and the punch list itself flagged it as likely separate follow-up work rather than bundled with the countdown/request-code fixes. Voice as a proof `kind` is correctly *absent* from `submit_proof.html`'s selector (verification-code proof is photo/video only per `04-verification-workflow.md`; voice is real and correctly present on the task-specific `assignment_proof.html` instead, driven by `media_options`). |
| `submit-checkin.html` | `submit_checkin.html` (shared route) | Deep | Confirmed no gap. The mockup hardcodes one example template's fields (skin status, cage comfort, incidents, sleep quality, device, duration) as an illustration; the real page has a `#template-select` picker plus the same generic field-type renderer (`select`/`scale`/`number`/`boolean`/`text`) that `checkin_live.html` uses, so every field shape the mockup shows is reachable through a real template's configuration, not hardcoded. |
| `checkin-live.html` | `checkin_live.html` (shared route, role-aware) | Deep | Confirmed no gap. The mockup states outright, in its own body text, that it "shows both people's screens side by side and simulates the update happening... in the real app, each person only sees their own page" — the two-pane layout and the `sim-*`/`#log` demo-control panel are explicitly disclosed mockup-only scaffolding, not a spec for a two-pane feature. The real single-pane page implements the disclosed real concept faithfully: color banner with the same three-state switcher, the same generic custom-field renderer, and real-time sync via Server-Sent Events (`EventSource` on `/api/v1/play-sessions/{id}/checkin-stream`) rather than the mockup's simulated `flash()` call — a working implementation of what the mockup only pretended to do live. |
| `submissive-statistics.html` | `submissive_statistics.html` | Deep | Built this session directly against this mockup; matches. |

### 4.4 Design-only, not a page gap

- `mockups/index.html` — a gallery/directory of the mockups themselves, not a feature page.

---

## 5. What changes with role (Keyholder vs. Submissive)

Several real templates are a single file that branches on role
(`is_keyholder`) rather than two separate files, which is *not* itself
a gap — it's a legitimate implementation choice the mockups don't
always mirror 1:1 (the mockups are two separate static files per
role even where the real page merged them). Documenting the branch
points explicitly, since "what does role change" was asked directly:

| Page | Keyholder sees | Submissive sees |
|---|---|---|
| `account_settings.html` | Contact info field; API tokens section (create/list/revoke, scoped); full keyholder nav | Safeword + emergency contact fields instead of contact info; "Your link" end-request section instead of API tokens; submissive nav |
| `play_session_detail.html` | Judgement controls (reward/punishment forms with custom title/description/hours), cancel-session action | Read-only judgement outcome once set, no cancel action, own check-in submission form only |
| `submit_checkin.html` / `checkin_live.html` | Reached via `/keyholder/...` paths, viewing a linked submissive's check-in | Reached via `/submissive/...` paths, filling their own |
| Nav bar (all shared/role-branching pages) | Full keyholder link set (Submissives, Catalog, Review queue, Check-ins, Play Sessions, Limits, Safety Alerts) | Submissive link set (My Status, Submit proof, My Toys, Play Sessions, My Limits, Statistics) |
| Toy catalog | Can retire a toy outright; sees/actions removal requests | Can request removal, cannot retire directly |
| Confinement/oversight controls (`submissive_detail.html`) | Full pause/resume/adjust/end controls, oversight pause | Read-only status view on their own dashboard |

There is no in-app **role-change** mechanism (an account is created as
one role via invite redemption or `admin create-keyholder` and stays
that role) — "what changes with role" in this app means "what a
Keyholder sees vs. what a submissive sees on the same conceptual
page," not a transition a single account goes through, and the table
above is that comparison for every page where the two roles' content
meaningfully diverges.

---

## 6. Visual/layout audit against the hosted mockup site (items 18–19)

A follow-up pass comparing the app directly against the mockups as
rendered (screenshots, both static-mockup and a real instance seeded
with realistic data — see `scripts/seed_demo_data.py` below), rather
than the field-by-field diff §4 used. This caught two things the
field-level audit missed entirely, because both are about *how much
is on the page and how it's arranged*, not whether any individual
field exists:

- **Item 18 (dashboard):** confirmed by reading the actual template —
  `dashboard.html` still had the literal placeholder comment "Lock
  status, verification, and open tasks will show up here once those
  parts of the app are built" in production. Every one of those parts
  has since been built elsewhere in the app (confinement status,
  verification codes, assignments); the dashboard itself was just
  never wired up to show any of it. This means the §4 row for
  `keyholder-dashboard.html` (originally a "Structural" pass that only
  diffed nav links) was wrong — it never actually looked at the body
  content. Fixed: a "Needs your attention" feed aggregating
  unacknowledged safety alerts, unreviewed auto-applied punishment
  extensions, tasks due within 2 hours, and pending proof review
  across the whole roster (sorted by severity then recency); four
  stat cards (active submissives, pending review, open tasks, missed
  verifications in the last 7 days); and roster table columns for
  lock status, last verification, pending count, and open-item count.
  New domain function: `confinement::list_unreviewed_adjustments_for_links`
  (no cross-roster equivalent existed before this).

- **Item 19 (two-column layout):** the mockup's `submissive-dashboard.html`
  and `submissive-detail.html` both split primary status from secondary
  lists into a side-by-side two-column grid; the real templates had
  every field the mockup does (confirmed in §4) but stacked all of it
  full-width in one column, which reads as much sparser even with
  identical content. Fixed for these two pages by wrapping the
  existing panels in a `grid lg:grid-cols-2` layout — deliberately
  **without reordering any panel's content or renaming its underlying
  API calls**, to keep the change low-risk. On `submissive_detail.html`
  specifically, the two-column split groups panels by their existing
  document order (left: Confinement/Oversight/Devices/Assign/Open-tasks;
  right: Verification/Profile/Rated-limits/Points/Link-settings) rather
  than mirroring the mockup's exact left/right *semantic* grouping
  (status+config vs. activity+actions) — reordering to match exactly
  was judged higher-risk than the visual-density win was worth. Also
  added a "Welcome back, {name}" / "Kept by your Keyholder for N days"
  header to `submissive_dashboard.html`, matching the mockup's framing.
  The same layout pattern likely applies to other pages not touched in
  this pass (`catalog.html`, `checkin_templates.html`, etc. were
  spot-checked and found closer to the mockup already — see §4 — but
  a full page-by-page layout audit beyond these two was out of scope
  here).

- **Branding — confirmed not a gap.** Every mockup file (all 30) reads
  "The Ledger" with a plain amber-square logo; the real app is
  "Owner's Cock Ledger" with the actual logo image, consistently
  across every real template. Since the mismatch is 100% uniform
  across the entire mockup set, this reads as a deliberate sanitized
  name for the public GitHub Pages demo rather than a drifted gap —
  confirmed with the user, left untouched.

**`scripts/seed_demo_data.py`** — added alongside this pass: boots its
own server against a scratch `DATA_DIR` and populates a full realistic
dataset (three submissives named after the mockups' own example
people — Riley, Sam, Jordan — with devices, confinement sessions,
catalog templates, proof submissions in multiple review states, points
and a pending redemption, rated limits, a safety alert, a full
play-session lifecycle, check-ins, and a recurring task rule) purely
over the real HTTP API, the same way a browser would. Exists because
comparing an empty real instance against the mockups' always-populated
example screens was itself a major source of "looks like a huge gap"
that had nothing to do with the actual UI — most of the app looks
much closer to the mockups once it has realistic data in it.
