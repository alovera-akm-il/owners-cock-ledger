# Roles and Permissions

## 1. Principles

1. **Role is fixed at account creation.** No endpoint changes a
   user's role. If a person needs to be both, they hold two separate
   accounts with two separate emails.
2. **Ownership, not global admin.** A Keyholder is not a superuser
   over the whole system — they are an admin *scoped to the
   submissives currently linked to them* (`keyholder_submissive_links`
   with `status='active'`, plus read access to `paused`/`ended`
   links they were party to, for history). Every Keyholder-role query
   in the service layer must join through that link table; there is
   no query that lets a Keyholder list or touch a submissive without
   a link row proving the relationship.
3. **Submissive is self-scoped.** Every submissive-role query is
   implicitly filtered to `submissive_id = current_user.id`. A
   submissive can never pass another user's ID and get data back —
   this is enforced server-side (checked against the session's user
   id), not trusted from any client-supplied field.
4. **Default deny.** Middleware resolves `(user, role)` from the
   session on every request; a route handler explicitly declares
   which role(s) may call it and, where relevant, which ownership
   check applies. No handler is reachable pre-auth except
   `/auth/login` and invite redemption.
5. **The Keyholder-only private field stays private even from the
   Keyholder's own submissive-facing exports.** e.g.
   `submissive_profiles.keyholder_notes` is filtered out at the
   serialization layer for any response destined for the submissive,
   not just hidden in the UI.

## 2. Permission matrix

Legend: **RW** = read+write, **R** = read-only, **R\*** = read-only,
own record(s) only, **RW\*** = read+write, own scope only, **—** =
no access.

| Capability | Keyholder | Submissive |
|---|---|---|
| Own profile: view/edit | RW | RW |
| Own credentials: change password/email | RW | RW |
| Own `hard_limits`/`soft_limits` | RW | RW |
| Submissive profile: view/edit basic fields | RW\* (their linked submissives) | RW\* (self only) |
| Submissive profile: `keyholder_notes` field | RW\* | — (never returned) |
| Submissive profile: `safeword`/`hard_limits`/`soft_limits`/emergency contact | R\* (view only) | RW\* (self only) |
| Keyholder's own `hard_limits`/`soft_limits` | RW (own) | R\* (view only, for their currently/formerly linked Keyholder) — mutual visibility, same reasoning as the row above, see `01-data-model.md` §2 |
| Create invite for new submissive | RW | — |
| View list of linked submissives | R\* (own links only) | — |
| End/pause a keyholder↔submissive link | RW\* | — (may request via out-of-band means; not a self-service API action, see §4) |
| Chastity device records | RW\* | R\* (self) |
| Confinement session (lock/unlock) create/edit | RW\* | R\* by default; RW\* only if the Keyholder enabled self-report for that link |
| Confinement `target_release_at` (the "supposed to be locked until" timer) | RW\* (manual adjustments) | R\* (self) — sees the countdown and its adjustment history, can't set it |
| Confinement lock-timer pause/resume (`clock_paused_at`) | RW\* | R\* (self) — sees "Paused," can't pause or resume it themselves |
| Pause message (`clock_pause_message`) | RW\* (set at pause, editable while paused) | R\* (self) — reads it, can't write it; it's written *for* them, not by them |
| Verification policy (schedule/frequency) | RW\* | R\* |
| Verification code: system-issued | (system-generated per policy; Keyholder can view issued codes) R\* | R\* (sees own active code) |
| Verification code: on-demand request | — (not needed) | create\* (self, subject to policy's `on_demand_only`/always-available rule) |
| Proof submission: create | — | create\* (self) |
| Proof submission: view (single submissive) | R\* (their submissives') | R\* (self) |
| Proof submission: view (cross-roster feed, `GET /keyholder/proof-submissions`) | R\* (own links only — see §5) | — (a submissive has only one "self," so this endpoint doesn't exist on their side) |
| Proof submission: review (set verified/redo/failed) | RW\* | — |
| Reward/punishment/task templates: manage catalog (incl. `kind`/`effect_kind`/`completion_type`/`proof_media_types`/deadlines/`on_success_template_id`/`on_failure_template_id`/`points_delta`) | RW | — |
| Reward/punishment/task templates: view | RW | R (read-only visibility into templates their Keyholder maintains, so they know what exists — see §5 caveat) |
| Assignment (actual reward/punishment/task given): create | RW\* | — (except a reward redemption request, see the points row below) |
| Assignment: acknowledge (submissive has seen it) | R\* | update\* (self, `assigned`→`acknowledged` only, `kind='task'` only) |
| Assignment: submit completion proof (photo/video/voice, per `proof_media_types`) | R\* (reviews it) | create\* (self, `completion_type='proof_required'` only, before `deadline_at`) |
| Assignment: mark completed/failed/revoked, review completion proof | RW\* | — |
| Assignment: edit `deadline_at` on an open task/punishment | RW\* | R\* (sees the current deadline and any edits to it) |
| Assignment: auto-`failed` on missed deadline, auto-escalation to `on_success_template_id`/`on_failure_template_id` | *(system-performed, not a role action — see `08-punishments-and-deadlines.md`)* | |
| Safety alert: raise | R\* (receives) | create\* (self, always available) |
| Safety alert: acknowledge/resolve | RW\* | R\* (self) |
| Audit log | R\* (entries scoped to their links) | R\* (entries about themself only) |
| API tokens: create/list/edit/revoke | RW (own tokens only) | — (no submissive-facing token mechanism in v1) |
| Push subscriptions: register/list/remove own device | RW (own) | RW (own) — both roles can opt into push, see `09-notifications.md` |
| In-app notification feed: read, mark read | R\*/update\* (own notifications only) | R\*/update\* (own notifications only) |
| Points: enable/disable for a link, view balance/ledger, manual adjustment | RW\* | R\* (self) |
| Points: request reward redemption | — (approves/denies requests, doesn't create them) | create\* (self, only if `points_enabled` and `points_balance` covers the reward's `points_cost`) |
| Toy catalog: add | RW\* | create\* (self) |
| Toy catalog: view/update | RW\* | RW\* (self, update covers care notes/tags/etc., not retirement) |
| Toy catalog: retire (soft-delete) | RW\* (direct, or approve/decline a pending request) | — (can only `request` retirement, see `12-toy-catalog.md` §3) |
| Check-in templates: manage (`checkin_templates`/`checkin_template_fields`) | RW | — |
| Check-in templates: view | RW | R (so they can see what a template will ask before filling it in) |
| Check-in instance: create/update (standalone or task-attached) | RW\* | RW\* (self) |
| Check-in instance: create/update (live, in-progress play session) | RW\* (real-time via SSE, `13-checkins.md` §5) | RW\* (self, real-time via SSE) |
| Play session templates: manage | RW | — |
| Play session: create/assign from template or ad-hoc | RW\* | — |
| Play session: start/log check-ins/end (live or retrospective) | RW\* | RW\* (self, on a session for their own link — either role can be the one running a live session) |
| Play session: judgement (assign reward/punishment) and mark completed | RW\* | — |
| Play session: view | R\* | R\* (self) |

`*` scope is always resolved from the authenticated session, never
from a client-supplied `keyholder_id`/`submissive_id` in the request
body — path/body IDs are used only to select *which of the caller's
own permitted rows* to act on, and every handler re-validates that
the target row actually belongs to the caller's scope before acting.

## 3. Why templates are keyholder-only to *edit* but submissive-visible to *read*

Letting the submissive see the punishment catalog (titles/
descriptions, not the decision of when to apply them) is intentional
— it's transparency about what's possible, not the same as giving
them any control over it. If a given Keyholder wants a fully opaque
catalog instead, that's a per-Keyholder configuration knob
(`catalog_visible_to_submissive` boolean on the Keyholder's settings,
default true), not a hardcoded platform behavior.

## 4. Ending a relationship

There is deliberately no `DELETE` on `keyholder_submissive_links`.
"Ending" sets `status='ended'`, `ended_at=now()`, and:

- The submissive's account is **not** deleted — their historical
  data (confinement history, submissions, assignments) stays intact
  and readable by them, but a Keyholder without an active/paused link
  to that submissive can no longer see or act on it (an `ended` link
  still grants the *former* Keyholder read-only access to history
  they created, for accountability, but no write access and no
  access to anything the submissive does after the link ends).
- The submissive account, once unlinked, may redeem a new invite from
  a different Keyholder to establish a new active link. The old
  history stays associated with the old (ended) link and is not
  visible to the new Keyholder.
- Only a Keyholder can end a link via the API in v1. A submissive
  wanting out of a dynamic where the Keyholder is unresponsive is a
  real scenario, handled by the end-request/escalation flow in
  `06-future-extensions.md` §2, whose actual last resort is
  `owners-cock-ledger admin force-end-link` (`10-operations.md` §5) —
  not a self-service endpoint, but a real named command rather than
  a hand-waved "out-of-band action."

## 5. Scenarios considered while designing the matrix

- **A Keyholder with several submissives must not see cross-
  submissive data by accident.** Every list/detail endpoint takes the
  target submissive's ID and re-checks the active-link join; there is
  no "list everything" admin mode that flattens across submissives
  other than the Keyholder's own dashboard rollup, which is still
  scoped to their own links.
- **A submissive should not be able to enumerate other submissives**
  (not even ones under the same Keyholder) — there is no submissive-
  facing endpoint that lists users at all.
- **A submissive should not be able to forge a "verified" status.**
  Only the review endpoint (Keyholder-only) transitions
  `proof_submissions.status`; the create-submission endpoint always
  inserts `status='pending'` regardless of what the client sends.
- **A submissive should not be able to fabricate a verification
  code.** Codes are generated server-side only; the submission
  endpoint checks that the supplied `verification_code_id` (a) exists,
  (b) belongs to the submissive's own active link, (c) is unexpired,
  and (d) is unconsumed, before accepting the upload.
- **A Keyholder marking `failed` must be able to assign a punishment
  in the same action** without a race where another request reviews
  the same submission twice — the review endpoint is a single
  transaction that updates `proof_submissions.status` and, if a
  punishment payload is present, inserts the `assignments` row
  atomically. See `04-verification-workflow.md` §4.
- **A disabled/locked-out account** (`users.disabled_at` or
  `locked_until`) fails auth entirely, regardless of role — a
  disabled Keyholder can't act on their submissives, and a disabled
  submissive can't submit proof, but existing historical data for
  either is untouched.
- **Self-report toggle for confinement sessions** exists because some
  Keyholder/submissive pairs may want the submissive to be able to
  log "I put it back on" themselves (e.g. after a supervised cleaning
  break) while others want the Keyholder to be the sole source of
  truth. Default is off (Keyholder-only writes) so the strict case is
  the safe default; the permissive case is opt-in per link.
- **Template deletion vs. history.** Deleting/deactivating a
  `reward_punishment_templates` row must never remove or orphan
  `assignments` that already reference it — hence assignments copy
  `title`/`description` at creation time rather than joining live to
  the template for display.
- **The cross-roster feed (`GET /keyholder/proof-submissions`) is not
  a new authorization path** — it applies the exact same
  `keyholder_submissive_links` join as every per-submissive query,
  just with `link_id IN (caller's link ids)` instead of
  `link_id = :one_link_id`. A date-range or `days` filter (see
  `03-api-design.md` §11) narrows *within* that already-scoped set;
  it can never be used to reach a submission belonging to a link the
  caller doesn't hold, and the endpoint has no submissive-facing
  equivalent since a submissive's "roster" is always just themself.
- **An API token is not a second identity** — it authenticates as
  the Keyholder who created it and inherits that Keyholder's ownership
  scope exactly (§2's `*` rule applies to token-authenticated requests
  identically to session requests). Scopes can only *narrow* what a
  token may do relative to the Keyholder's own full permissions, never
  grant it anything the Keyholder couldn't already do themselves — a
  token is not a way to delegate access to some other, less-trusted
  party with a different permission ceiling than the Keyholder has.
- **Scopes control action categories, not which submissives.** There
  is no v1 mechanism to mint a token restricted to a single
  submissive within a Keyholder's roster — a token with
  `review:proof-submissions` can review for *any* of that Keyholder's
  linked submissives, same as the Keyholder's own session can. A
  per-link-scoped token is flagged as a reasonable future refinement
  in `06-future-extensions.md`, not built now, since it adds real
  complexity (scope resolution would need to check both "which
  actions" and "which link") for a need that isn't concrete yet.
- **Automation erodes the "a human reviewed this" guarantee, by
  design choice, not by accident.** Granting a token
  `review:proof-submissions` or `manage:assignments` makes it possible
  for a Keyholder to script their own verification/punishment
  decisions, which is in tension with the human-judgment stance in
  `06-future-extensions.md` §5. The system doesn't prevent this — it's
  the Keyholder's own account and own call — but it does make the
  fact visible: `reviewed_via`/`assigned_via` on the record itself
  (`01-data-model.md` §5/§6) show whether a given verified/failed
  status or assignment came from a session or a token, rather than
  presenting automated and manual decisions identically.
- **Revoking a token is immediate**, unlike ending a
  keyholder-submissive link (§4) which is deliberately a soft,
  historical-preserving transition — a leaked or no-longer-needed
  token has no legitimate reason to keep working even briefly, so
  `DELETE /keyholder/api-tokens/{id}` takes effect on the very next
  request rather than draining gracefully.
- **A submissive can never mark their own task `completed`,
  even for `acknowledge_only` ones, and can never review their own
  completion proof.** Both stay Keyholder-only, matching the
  pre-existing reward/punishment confirmation rule — the deadline
  system changes *when* a task can auto-fail, not who gets to
  say it succeeded.
- **Auto-failure and auto-escalation are system actions, not a new
  authorization path for anyone.** The deadline sweeper
  (`08-punishments-and-deadlines.md`) runs with its own internal
  privilege, scoped to exactly the one assignment it's evaluating —
  it isn't reachable by any user-facing role and can't be invoked
  early or skipped by either role. `assigned_via='system'` on the
  resulting row (`01-data-model.md` §6) makes this visible rather
  than looking like a Keyholder acted at 3am.
- **A Keyholder editing `deadline_at` or `target_release_at` on an
  open item is always a manual, logged action** (`confinement_adjustments`
  for the timer, the normal audit log for a deadline edit) — neither
  field silently drifts; every value either came from a template
  default, a Keyholder's explicit edit, or an automatic
  punishment-time-extension effect, and which one it was is always
  recoverable.
- **Push subscriptions and notifications are self-scoped exactly like
  a submissive's own data** — there is no capability, for either
  role, to list another user's registered devices or read another
  user's notification feed. A Keyholder does not get visibility into
  *whether* their submissive has push enabled; they only see the
  domain events themselves (a missed check-in, a failed punishment)
  through the normal roster/detail views, same as before
  notifications existed.
- **A toy retirement request is not a delete right in disguise.** A
  submissive calling the "request removal" action only ever sets
  `retirement_requested_at` — it never transitions `retired_at`
  itself, regardless of what the request payload contains. Only a
  Keyholder-authenticated call can set `retired_at`, whether that's
  approving a pending request or retiring a toy with no request at
  all (`12-toy-catalog.md` §3).
- **A reward redemption request is the one place a submissive can
  self-assign something, and it's deliberately narrow.** It only
  creates a *pending* request row against a balance the Keyholder's
  own templates and grants already built up — it never creates the
  `assignments` row directly, and it's a no-op if `points_enabled` is
  off for that link or the balance is short. The Keyholder's approval
  step is what actually creates the assignment and debits the ledger,
  so this doesn't weaken "submissives never self-assign" as a general
  rule (`11-tasks-and-rewards.md` §3).
- **Check-in templates follow the same edit/view split as reward and
  punishment templates** (§3) — a submissive can see what a check-in
  will ask before filling one in, but only a Keyholder authors or
  edits the field list. Filling in and updating an actual check-in
  instance is symmetric between roles, including live ones — the
  real-time SSE channel (`13-checkins.md` §5) is read-only fan-out on
  top of the same authorization as the ordinary `PATCH`, not a
  separate write path with different permissions.
- **Either role can start or log a live play session for their own
  link**, unlike task/reward assignment which stays Keyholder-only —
  a submissive plausibly starts a session on their device in the
  moment. What stays Keyholder-only is everything after the session
  reaches `pending_judgement`: assigning a reward/punishment and
  marking the session `completed` (`14-play-sessions.md` §5). A
  submissive can view but not alter the judgement.
- **Play session templates cannot leak toy inventory across
  submissives.** `suggested_toy_categories` is plain text, never a
  reference to a specific `toys` row, precisely so a template built
  around one submissive's catalog can't accidentally expose or
  imply another submissive's inventory when reused
  (`14-play-sessions.md` §2).
