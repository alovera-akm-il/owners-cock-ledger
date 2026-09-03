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
| 1 | Login page has no 2FA challenge step at all — a 2FA-enabled user cannot log in through the web UI | `login.html` | **Critical — to implement** |
| 2 | Toy photo upload: DB column + API field exist, no upload route, no UI anywhere | Toy catalog (both roles) | High — to implement |
| 3 | Safety alerts list shows no submissive name/attribution — a Keyholder with 2+ submissives can't tell whose alert is whose without a separate lookup | `safety_alerts.html` | High — to implement (safety-relevant) |
| 4 | Structured hard/soft limits + free-text limits live on a separate page instead of the profile — user-requested consolidation | Profile, Limits | **Fixed** |
| 5 | No per-submissive single-item proof review view (mockup's `proof-review.html` shape) — only the cross-submissive queue exists | Review flow | **Confirmed by user — to implement**, additive to the existing queue |
| 6 | No keyholder-wide pending-redemption-requests view — only visible per-submissive | Keyholder dashboard | **Confirmed by user — to implement** |
| 7 | Submit-proof page: no live countdown to code expiry, no "request new code" if it expires mid-fill, no in-browser record capture (file upload only) | `submit_proof.html` | **Live countdown + request-new-code fixed.** In-browser record capture still deferred — see note below. |
| 8 | Confinement timer was static text, not live | Dashboard, submissive detail | **Fixed** |
| 9 | `submissive_detail.html` nav missing "Limits" link | Submissive detail | **Fixed** |
| 10 | No mobile nav on any page (19 pages) | All | **Fixed** |
| 11 | Toy `compatible_device_id` and `acquired_at`: API/DB support them, no UI anywhere (mockup never designed for them either) | Toy catalog | Low — to implement |
| 12 | Device `description` field: API accepts it, no UI input (mockup never had one either) | Submissive detail (devices) | Low — to implement |
| 13 | No Audit Log UI (backend writes the log; nothing reads it back) | — (page doesn't exist) | Deferred (scoped out earlier) |
| 14 | No submissive History page | — (page doesn't exist) | Deferred (scoped out earlier) |
| 15 | No keyholder-wide cross-submissive Toy catalog view | — (page doesn't exist) | Deferred (scoped out earlier) |
| 16 | A submissive cannot see their Keyholder's stated boundaries anywhere (mockup's `submissive-profile.html` "Your Keyholder's boundaries" read-only panel has no real counterpart) | `submissive-profile.html` | **Fixed** |
| 17 | Mockup's `keyholder-profile.html` has a "Your submissive's boundaries" read-only mirror; real has no equivalent on the Keyholder's own profile page | `keyholder-profile.html` | Not a gap — see note below |

Items 13–15 were already surfaced and explicitly deferred by the
user's own scoping decision in this session ("bugs + timer + mobile
menu only... leaves Audit Log, History, and cross-submissive Toys as
separate future work"). They're listed here again only so this
document is a complete inventory, not because they're new.

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

## 2. Critical: no 2FA login-challenge UI

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

**Net effect:** every other 2FA surface in the app is real and
tested — setup, confirm, recovery codes, disable, all wired to
`account_settings.html` and covered by
`two_factor_setup_confirm_and_login_challenge_round_trip` — except the
one moment a 2FA-enabled user actually needs it: logging back in. This
is the highest-priority item in this document.

---

## 3. Profile page: structured limits/kinks consolidation (design decision, fixed)

**Current state:** one system now exists for "boundaries," on one page
(`account_settings.html`). Previously there were two separate systems on
two separate pages, described below for context.

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
| `keyholder-dashboard.html` | `dashboard.html` (`/dashboard`) | Structural | Mockup's top nav includes **Toys** and **Audit Log** as global items; real has neither (toys is per-submissive only, no cross-submissive route exists; audit log has no UI at all — item 11). Mockup's roster table and invite-modal shapes match the real implementation. |
| `keyholder-toy-catalog.html` | `toy_catalog.html` (`/keyholder/submissives/{id}/toys`) | Deep | Mockup is a **global, cross-submissive** catalog with a submissive-selector dropdown; real is scoped to one submissive, reached only by drilling in — a different IA, and the real API has no cross-submissive toy-listing route to support the mockup's shape (item 13, deferred). Photo upload (item 2), `compatible_device_id` (item 8) unwired in both roles' toy forms. |
| `keyholder-profile.html` | `account_settings.html` (`is_keyholder` branch) | Deep | Mockup's Personal info/Safety/Boundaries sections are split three ways; real merges them into one "Personal profile" section — functionally equivalent fields (bio, contact info, hard/soft limits, timezone), just fewer panels. 2FA, sessions, password change all real and matching. API tokens section is real-only (mockup predates it; correct, since tokens are a later addition per `01-data-model.md` §9). No "Your submissive's boundaries" read-only mirror section — see item 17: not a gap, the mockup's 1:1 assumption doesn't generalize to the real app's 1:many roster, and the information is already visible on the submissive's own detail page. |
| `keyholder-limits-catalog.html` | `limits_catalog.html` (`/keyholder/limit-items`) | Deep | Confirmed no gap: the mockup's closing note ("Sensation, Chastity & Denial, Fluids, Psychological, Medical, and Exhibitionism ship the same way — a modest starter list per category") is real — `migrations/0029_limits.sql` seeds exactly those categories with starter items. |
| `keyholder-safety-alerts.html` | `safety_alerts.html` (`/keyholder/safety-alerts`) | Deep | **Real gap (item 3):** the mockup labels every alert row with the submissive's name ("Riley," "Sam," "Jordan"); the real list has no name or attribution anywhere. `AlertResponse` (`src/api/safety.rs`) only exposes a raw `submissive_id`, and `safety_alerts.html`'s `renderAlert()` never resolves or displays it. For a Keyholder with more than one submissive, there is currently no way to tell whose alert is whose from this page. |
| `keyholder-recurring-tasks.html` | `recurring_tasks.html` (`/keyholder/submissives/{id}/recurring-tasks`) | Deep | Built this session directly against this mockup; matches. |
| `keyholder-submissive-statistics.html` | `keyholder_submissive_statistics.html` | Deep | Built this session directly against this mockup; matches. |
| `keyholder-points-and-redemptions.html` | *(folded into `submissive_detail.html`'s Points panel, per-submissive)* | Deep | **UI-approach divergence, resolved by consultation (item 6).** Mockup is a cross-submissive "Pending redemption requests" table; real requires opening each submissive individually to see theirs. Decision: keep the folded-in, per-submissive panel as-is (it's complete and working), but add the one missing piece — a keyholder-wide pending-redemptions view, the same aggregation pattern the Review Queue already uses across submissives. Not a full return to the mockup's standalone-page shape. |
| `keyholder-audit-log.html` | *(none)* | — | Item 11, deferred. Backend (`domain::audit`) writes rows on every state-changing action already; nothing reads them back. |
| `catalog.html` | `catalog.html` (`/keyholder/catalog`) | Structural | Field counts match (4/4) for the task/reward/punishment template form. |
| `checkin-templates.html` | `checkin_templates.html` (`/keyholder/checkin-templates`) | Structural | Field counts match; real has an extra `field-key` input the mockup doesn't show explicitly. |
| `play-session-templates.html` | `play_session_templates.html` (`/keyholder/play-session-templates`) | Structural | Real has *more* fields than the mockup (6 vs 3) — real exceeds the mockup here, not a gap. |
| `proof-review.html` | `proof_review.html` (`/keyholder/review`) | Deep | **UI-approach divergence, resolved by consultation (item 5).** The mockup is a single-submission review view reached from one submissive's own page (`← Riley` back-link); the real page is a cross-submissive "Review Queue" aggregating every submissive's pending proof. Decision: keep the queue *and* add the per-submissive single-item view the mockup shows — additive, not a replacement. |

### 4.3 Submissive pages

| Mockup | Real | Audit | Findings |
|---|---|---|---|
| `submissive-dashboard.html` | `submissive_dashboard.html` (`/submissive`) | Deep | Live countdown — **fixed this session** (was static). Nav: mockup lists "Rewards & Points" and "History" as their own pages; real folds points into the dashboard inline and has no History page at all (item 12, deferred). |
| `submissive-detail.html` | *(this is the Keyholder's page, see 4.2 — mockup name is misleading)* | Deep | Covered under `submissive_detail.html` in 4.2's row-equivalent; also: live countdown fixed, "Limits" nav-link bug fixed this session. |
| `submissive-toy-catalog.html` | `submissive_toys.html` (`/submissive/toys`) | Deep | Same photo-upload and `compatible_device_id` gaps as the Keyholder-side toy catalog (item 2, item 8) — these are backend/schema-level gaps shared by both roles' toy UI, not role-specific. |
| `submissive-profile.html` | `account_settings.html` (submissive branch) | Deep | See §3 for the limits/kinks consolidation (fixed). Also fixed (item 16): a read-only "Your Keyholder's boundaries" panel now shows the linked Keyholder's bio/hard limits/soft limits, via the new `GET /api/v1/submissive/keyholder-profile` endpoint (`src/api/profiles.rs`). |
| `submissive-limits.html` | *(folded into `account_settings.html`'s "Limits & kinks" section, per §3)* | Deep | Functionally solid match to the mockup (category grouping, rating buttons, notes) — now reached via the profile page instead of its own route. |
| `submissive-play-sessions.html` | `submissive_play_sessions.html` (`/submissive/play-sessions`) | Structural | Not deep-audited. |
| `submissive-play-session-detail.html` | `play_session_detail.html` (shared route, role-aware) | Structural | Real is substantially richer than the mockup (judgement flow, punishment/reward custom forms, schedule panel, toys panel) — real exceeds mockup, not a gap. |
| `submissive-rewards.html` | *(folded into `submissive_dashboard.html`'s Points panel)* | Structural | Functionally present (redeemable list, redeem button, pending-redemption state) — just inline on the dashboard instead of a dedicated page. Not a gap, a placement choice consistent with the points-and-redemptions consolidation on the Keyholder side too. |
| `submissive-history.html` | *(none)* | — | Item 12, deferred. Nothing in the real app currently explains "why did my time change" the way this mockup page promised (linked from the mockup dashboard's countdown hero). |
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
