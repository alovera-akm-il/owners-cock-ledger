# Future Extensions

Things intentionally deferred, and how the current design leaves room
for each without a breaking rework.

## 1. Play sessions — implemented

Originally a reserved stub here. No longer deferred — fully designed
in `14-play-sessions.md`, schema in `01-data-model.md` §15
(`play_session_templates`, `play_sessions`, `play_session_toys`,
`play_session_checkin_schedule`). Both the live and the
logged-after-the-fact case are covered, along with the
judgement-before-completion workflow that reuses `assignments` via
the now-built `triggered_by_play_session_id` column (§ below) rather
than the generic entity-type/entity-id pair once flagged as the
fallback — a second concrete trigger source existed by the time this
was designed, so the dedicated-column path from
`confinement_adjustments.caused_by_assignment_id` was reused directly.

What's still genuinely deferred *within* play sessions — kept in sync
with the source list in `14-play-sessions.md` §6, not summarized
separately, so this doesn't drift out of date again:

- **Automatic hard-limit cross-checking** — flagging that a session's
  attached toys or notes touch a listed hard limit. Blocked on the
  same thing §9 below is: limits are free text today, not a
  structured, checkable taxonomy.
- **Aftercare follow-up automation** — e.g. auto-scheduling a
  check-in some hours after a session ends. Not built because the
  right default delay/cadence is a product decision better made from
  real usage than guessed now; a Keyholder can already do this
  manually with an ad-hoc check-in.
- **Session statistics / history views** (frequency, average
  duration, most-used toys, judgement outcomes over time) — a
  reporting concern layered on top of data this design already
  captures, not a schema gap; worth building once there's enough
  session history to make it useful.
- **Recurring/scheduled sessions** (e.g. "every Tuesday") — a
  scheduling concern distinct from templates (which are reusable
  *shapes*, not reusable *calendar entries*). The template/instance
  split already in place would support this cleanly later — a
  recurrence rule that spawns `play_sessions` rows from a template on
  a cadence — without needing to be designed against now.
- **Multi-submissive / group sessions** — out of scope; this schema is
  pairwise (`link_id`-scoped) throughout, the same boundary noted for
  points below and consistent with the single-active-link model (§3).
- **Session activity log as its own child table** (distinct
  timestamped events within one session, beyond notes and scheduled
  check-ins) — the original idea recorded here before the design
  existed, kept as its own item since it's a resolved-for-now design
  call rather than an open request: the scheduled check-ins plus
  `judgement_notes` cover what a session currently needs to record,
  revisit only if that stops being true in practice.

## 1a. Tasks, points, toy catalog, and check-ins — implemented

Also no longer speculative. Full designs:

- `11-tasks-and-rewards.md` — the `kind='task'` unification (a task
  is a `reward_punishment_templates` row with both an
  `on_success_template_id` and `on_failure_template_id` path),
  multi-media proof (`proof_media_types`), and the points system.
- `12-toy-catalog.md` — the `toys` table and the
  submissive-adds/Keyholder-deletes-or-approves permission split.
- `13-checkins.md` — Keyholder-authored check-in templates, the
  always-present color field, configurable custom fields, and the
  SSE-based real-time channel for live-session check-ins.

Schema for all three is in `01-data-model.md` §§12–14. What's still
deferred *within* each, kept in sync with each doc's own source list
the same way §1 now is:

**Tasks, rewards, and points** (`11-tasks-and-rewards.md` §4):

- **Point expiry/decay** — not built; adds real complexity (when do
  points expire, does that need its own sweeper) for a problem
  ("balances grow unbounded") that isn't a proven issue and can be
  handled with a manual adjustment if it ever comes up.
- **Leaderboards / multi-submissive comparison** — out of scope;
  pairwise (`link_id`-scoped) throughout, no precedent anywhere for
  cross-link aggregation.
- **Automatic reward suggestion near a point threshold** ("you're 20
  points from redeeming X") — plausible future UI nicety, not a
  schema concern.

**Toy catalog** (`12-toy-catalog.md` §5):

- **Multiple photos per toy** — the current single
  `photo_attachment_path` would become a child `toy_photos` table,
  mirroring how `proof_attachments` already does one-to-many photos
  for `proof_submissions`.
- **Usage stats** ("last used," times-used) — a read-only reporting
  concern over `play_session_toys` rows that already exist once play
  sessions are built, not a schema gap.
- Deliberately not pursued: consumables/inventory tracking,
  wishlist/shopping-list, barcode/lookup convenience.

**Check-ins** (`13-checkins.md` §7, plus §6 for what's now resolved):

- **Presence indicators** ("Keyholder is currently viewing") — a
  natural SSE extension, not built now; adds complexity for a nicety,
  not a safety-relevant signal.
- **Auto-escalation on a RED check-in — resolved, no longer a gap.**
  Originally deferred outright ("presumptuous to auto-raise a safety
  alert"), then reopened: `color='red'` is *defined* to mean
  "immediate stop" (`13-checkins.md` §1), so for a template where
  that's genuinely what RED signals, treating it as equivalent to the
  submissive hitting the safety-alert button is honoring the
  definition, not overstepping it — but a looser template's RED
  isn't always that severe, so one system-wide rule was wrong either
  way. Resolved as a **per-template opt-in**
  (`checkin_templates.auto_escalate_on_red`, `01-data-model.md` §14):
  the Keyholder who authored a given template is the one who knows
  whether *that template's* RED threshold warrants the full alert
  workflow. Full mechanics — the transition-only trigger (not
  re-firing on every subsequent edit while still red),
  `safety_alerts.raised_via='system'` accountability, and reuse of the
  existing safety-alert review flow — are in `13-checkins.md` §6.

## 2. Self-service link ending (submissive-initiated)

v1 makes ending a `keyholder_submissive_links` row Keyholder-only
(see `02-roles-and-permissions.md` §4), with an unresponsive-Keyholder
escape hatch left as an out-of-band/admin operation. This section now
has a full shape, not just the two bullet points it used to be — still
not built, but concrete enough to build without further product
decisions being invented mid-implementation.

### Why not just let the submissive end it directly

Rejected, deliberately, not just by default inertia: a self-hosted
app answering only to the two people on the link has no independent
party who could verify a request to sever the relationship is really
what the submissive wants versus, say, someone else with access to
their device clicking it, or a heat-of-the-moment action taken
without the Keyholder ever getting a chance to respond, discuss, or
object to a misunderstanding. Requiring the Keyholder to act (or a
timeout to elapse) keeps a human in the loop for a decision this
consequential, the same reasoning that keeps every other
consequential state change in this schema Keyholder-gated. It is
**not** the same reasoning as "the submissive shouldn't be able to
leave" — the design goal is a guaranteed, bounded path *out*, just
one that isn't a single unilateral click with no visibility to the
other party.

### Shape: request, not action

Extend `keyholder_submissive_links` (no new table needed — this is
one link's pending state, not a growing log) with:

| column | type | notes |
|---|---|---|
| end_requested_at | INTEGER NULL | set when the submissive requests; cleared on cancel, decline, or the link actually ending |
| end_requested_by_user_id | TEXT NULL FK -> users.id | always the submissive on this link, kept explicit for consistency with every other `*_by_user_id` column in this schema rather than being implied |
| end_request_reason | TEXT NULL | optional, shown to the Keyholder — not required, since demanding a justification to leave would undercut the point |
| end_request_escalated_at | INTEGER NULL | set once the unacknowledged-request timeout (below) passes; drives the recurring-reminder cadence, mirroring `clock_paused_at`'s relationship to the "still paused" reminder in `08-punishments-and-deadlines.md` §9 |

Deliberately **not** a new `status` value on the link — `status`
stays `active`/`paused` throughout a pending request; requesting to
end doesn't itself change what's operative today (open tasks,
confinement, verification all continue exactly as before). This
mirrors why the confinement pause fields live alongside
`confinement_sessions.status` rather than replacing it
(`01-data-model.md` §4): a pending request is metadata *about* the
relationship, not a new state the relationship is *in*.

### API surface (end request)

- `POST /submissive/link/end-request` — `{reason?}`. `409` if one is
  already pending or the link isn't `active`/`paused`.
- `DELETE /submissive/link/end-request` — the submissive withdraws
  their own request at any time, no confirmation-from-Keyholder
  needed to cancel (only to act on it).
- `POST /keyholder/submissives/{id}/link/end-request/decline` —
  `{response_note?}` — clears the request without ending the link;
  audit-logged (so a dismissed request is never just silently gone),
  and the optional note is delivered back to the submissive so
  declining isn't indistinguishable from being ignored.
- The existing `PATCH /keyholder/submissives/{id}/link {status:"ended"}`
  (`03-api-design.md` §2) is the acceptance path — no new endpoint
  for "approve," since ending the link already *is* the approval.
  Ending a link with a pending request clears the request fields as a
  side effect of the same transaction.

### Timeout and escalation

A fixed default (no per-link configuration needed for v1 — this is a
safety-net timer, not a tunable product setting): **7 days**
unacknowledged sets `end_request_escalated_at`, after which:

- **Tier 1 (built into the app):** the pending request becomes
  impossible to miss on every Keyholder page load (not just a
  dismissible notification), and a recurring reminder notification
  re-fires roughly every 24 hours thereafter — the exact same pattern
  already built for "lock timer still paused"
  (`08-punishments-and-deadlines.md` §9's "Still-paused reminder"),
  reused rather than invented fresh.
- **Tier 2 (genuinely out-of-band, but concretely specified rather
  than hand-waved):** the real escape hatch for a Keyholder who never
  responds at all is a server-admin CLI operation —
  `owners-cock-ledger admin force-end-link <link_id>` — that ends the
  link unilaterally, grouped as a sibling of the other admin recovery
  commands (`10-operations.md` §5). This is worth
  actually building alongside the rest of this feature, not leaving
  implicit, since it's the actual answer to "the Keyholder
  disappeared for good," not just a footnote.

The 7-day timer is specifically about ordinary unresponsiveness (busy,
traveling, slow to notice), **not** the safety-critical case — a
submissive who feels unsafe *right now* was never meant to wait a
week regardless of this feature; the always-available safety-alert
endpoint (`04-verification-workflow.md` §5) remains the immediate
path, and Tier 2's admin operation is available on request at any
time, not gated on the timer elapsing.

### Scenarios considered

- **No cooldown on re-requesting after a decline.** A submissive who
  keeps re-requesting after being declined isn't rate-limited or
  blocked from asking again — repeated requests are themselves a
  strong signal worth the Keyholder's attention, and an artificial
  cooldown could trap someone who has a genuine, urgent reason to keep
  asking.
- **A pending request changes nothing about ongoing authority.**
  Tasks can still be assigned, punishments still escalate, the
  confinement timer keeps running — the request doesn't freeze the
  relationship, only starts a clock on the Keyholder's response. This
  is a real, named tradeoff: it does not technically prevent a
  Keyholder from assigning something retaliatory during the pending
  window. The mitigation is the same one this schema already relies
  on for every other overreach scenario (`05-security-and-privacy.md`
  §8's punishment-escalation-consent row) — the safety alert and
  mutually-visible limits are the actual escape valves, not a
  technical restriction on what a Keyholder can do in the interim.
- **Declining isn't final.** A declined request can always be
  re-opened with a fresh `POST .../end-request` — decline means "not
  right now, let's talk," not "request denied, case closed."

Not fully speculative work: this is deliberately specified in enough
detail to build directly, unlike the other items in this document —
elaborated because "the Keyholder went quiet and now I'm stuck" is a
more serious gap than most of what's listed below it.

## 3. Co-keyholder / shared-oversight submissives

Current model: one *active* link per submissive at a time
(`01-data-model.md` §3). A trainer/switch/co-owner scenario where two
Keyholders legitimately share oversight of one submissive isn't
supported. If needed later:

- `keyholder_submissive_links` would need a `role_on_link` (e.g.
  `primary`,`secondary`) instead of assuming exactly one row, and the
  partial-unique-active-link constraint would need to change from "at
  most one active link" to "at most one `primary` active link."
- Every scoped query in the API layer already joins through this
  table, so widening it to permit multiple active rows per submissive
  is additive to the query pattern (change the uniqueness constraint
  and the "which link" resolution logic), not a rewrite of the
  authorization approach itself.

## 4. Notifications / push delivery — implemented

Originally flagged here as a known gap (especially for
`random_within_window` verification policies, where a submissive must
notice a code was issued with no prompt). No longer deferred — see
`09-notifications.md` for the full design: an in-app notification
feed backed by a `notifications` table, with Web Push as an opt-in
delivery layer on top of it (`01-data-model.md` §10,
`03-api-design.md` §13). What's still explicitly *not* built, and
left as genuinely future:

- **Email as a `notifications` delivery channel (digests) / SMS** —
  distinct from password-reset email, which *is* now designed
  (`05-security-and-privacy.md` §11, `10-operations.md` §5): that's a
  one-off transactional send outside the notification system
  entirely, not a consumer of `notifications` rows. A digest channel
  ("email me a daily summary") is still genuinely undesigned — but
  once password reset exists, the SMTP relay/credential/crate is
  already there, so adding a digest later is "a new email template
  and a trigger," not "add email support from scratch." SMS remains
  fully deferred, no transport exists for it at all.
- **Per-notification-type preferences** (push me for safety alerts
  only, feed-only for the rest) — v1 is all-or-nothing per device;
  see `09-notifications.md` §6.

## 5. Automated verification-code reading (OCR)

v1 is manual: the Keyholder looks at the photo and judges it. If
automated code-in-photo detection is added later, it would sit as an
optional, opt-in enrichment on top of the existing review step (e.g.
a `detected_code` field surfaced to the Keyholder as a hint, review
decision still made by a human) rather than an auto-approve path —
consistent with the principle in `05-security-and-privacy.md` §5 that
new third-party/ML dependencies should be opt-in, not default, given
the sensitivity of the images involved.

## 6. Field-level encryption for the most sensitive columns

Flagged in `05-security-and-privacy.md` §5 as a hardening step
(`sqlcipher` or application-level envelope encryption for
`keyholder_notes`, `safeword`, `hard_limits`, `soft_limits`,
`emergency_contact`) — deferred so the base system's data-access
patterns are established first, since encrypting fields changes how
they can be queried/filtered.

## 7. Full account deletion / data export

Flagged in `05-security-and-privacy.md` §5. Both roles should
eventually get a genuine "delete my account and everything I own"
action and a "export everything about me" action, independent of the
`ended` link's history-retention default — these are near-term, not
speculative, additions once the base CRUD surface exists, and should
be designed with an explicit answer to "what happens to a
Keyholder's assignment/audit rows that reference a submissive who
deleted their account" (likely: anonymize the reference, keep the
Keyholder's own audit trail intact) before being built.

## 8. Multi-file/rich metadata already accounted for

Not deferred — noted here only to make explicit that this one *was*
designed in from the start rather than left as a gap: proof
submissions are `kind` + JSON `metadata` + a child attachments table
specifically so that "not just photo proof but other info also can be
saved" (structured notes, mood/state fields, multiple files per
submission) doesn't require a schema change later — see
`01-data-model.md` §5.

## 9. Structured hard/soft limits checklist

v1 stores `hard_limits`/`soft_limits` as free text
(`01-data-model.md` §2) — matching how `safeword`/`emergency_contact`
are modeled, and good enough for a Keyholder and submissive to write
and read each other's boundaries in prose, but too easy to skim past
and impossible for anything else in the system to check against. Below
is a concrete shape for a structured version, worked out enough to
build, still deferred because nothing in scope today forces it — the
free-text fields keep working exactly as they do now regardless of
whether this ever gets built, since prose can always say something a
checklist can't ("okay, but only if we've talked about it that day").

### Catalog and ratings, kept separate

Two things, not one, mirroring the split this schema already uses for
rewards/punishments (a reusable catalog vs. a per-submissive
instance):

- **`limit_items`** — the catalog of things a rating can be given
  for: `id`, `keyholder_id NULL` (`NULL` = ships as a global default
  item every deployment starts with; non-null = a Keyholder's own
  addition, visible to their own submissives only), `category`
  (free text grouping for display, e.g. "Impact," "Bondage &
  Restraint," "Sensation," "Chastity & Denial," "Fluids,"
  "Psychological," "Medical," "Exhibitionism"), `label`, `description
  NULL`, `active`.
- **`submissive_limit_ratings`** — one row per submissive per item
  they've actually rated: `id`, `submissive_id`, `limit_item_id`,
  `rating TEXT CHECK IN ('hard','soft','okay')`, `notes NULL`,
  `updated_at`. **No row at all** is the default state for an item —
  read as "not discussed," never silently coerced into `okay`. This
  is the one invariant this feature cannot get wrong: an unrated item
  must never be presentable as a green light.
- Ownership matches the existing free-text fields exactly
  (`02-roles-and-permissions.md` §2): a submissive's own ratings are
  `RW*` (self), a Keyholder gets `R*` (view only) — rating a limit is
  exactly as submissive-owned an act as writing the free-text
  paragraph is today.

### Who can extend the catalog

Keyholder-only, matching the "Keyholder authors, submissive
participates" pattern used for every other template catalog in this
app (`reward_punishment_templates`, `checkin_templates`) — a
submissive rates items, but doesn't define new ones, which keeps the
vocabulary consistent enough for the cross-checking below to mean
anything. A submissive who wants something specific tracked that
isn't in the list yet asks their Keyholder to add it — the same
social workflow that already governs every other template catalog
here.

### The seed list

Ships once, globally, so a fresh deployment isn't a blank checklist —
category names above plus a modest handful of concrete items per
category (the kind of list already common in real munch/negotiation
checklists: things like "impact — paddle," "impact — cane," "sensory
deprivation," "public/outdoor exposure," "breath play," "fluids —
oral," "degradation/humiliation language," "medical/needle play").
Deliberately not exhaustive — a starting vocabulary a Keyholder
extends for what actually matters to their dynamic, not an attempt to
enumerate every possible activity up front.

### Cross-checking: advisory, and deliberately narrow in scope

The payoff feature this unlocks: flagging "this touches a listed hard
limit" on things that already carry a structured category string —
which today means **toys** (`toys.category`, `12-toy-catalog.md` §2)
and **play session templates** (`suggested_toy_categories`,
`14-play-sessions.md` §2). Matching is a case-insensitive comparison
against a submissive's `hard`/`soft`-rated `limit_items.category`/
`label` values, surfaced as a non-blocking warning banner on the
create/edit form and the item's detail view — never a hard block,
since the Keyholder remains the final authority on everything in this
system, same as every other guard-rail here. Deliberately **not**
extended to task/reward template titles or descriptions — those are
free text with no structured vocabulary, and keyword-matching prose
would produce enough false positives and false negatives that a
Keyholder would reasonably start ignoring the warnings entirely,
which is worse than not having them. If this ever gets built, it
gets built for toys and play sessions only, where a real structured
field already exists to match against — not as a general
free-text scanner.

### Why still deferred

The catalog/rating split, the ownership model, and the cross-check
scope are all decided above — what's left isn't design uncertainty,
it's that nothing in the current feature set actually depends on it
existing yet. Worth building whenever prose limits are reported as
easy to skim past in practice, or whenever the toy/play-session
cross-check becomes something a Keyholder actively asks for.

## 10. API token refinements

Two things intentionally left out of `03-api-design.md` §12's v1
token design, both flagged in `02-roles-and-permissions.md`'s
scenario list:

- **Per-link-scoped tokens** — restricting a token to one specific
  submissive within a Keyholder's roster, rather than every
  submissive the scopes' action-categories would otherwise reach.
  Would need scope resolution to check two things (action + link)
  instead of one; deferred until there's a concrete use case
  (e.g. "a script that only ever touches one particular submissive's
  data") to design the exact shape against.
- **Submissive-issued API tokens** — v1 deliberately gives automation
  only to the Keyholder role, since the Keyholder is the one described
  as wanting "to automate things." A submissive-side use case (e.g. a
  script that auto-submits a scheduled proof photo from a phone
  automation app) is plausible later, and would reuse the same
  `api_tokens` table/mechanism with `keyholder_id` generalized to
  `owner_user_id` — not a redesign, just widening who can hold one,
  with its own, narrower scope catalog (a submissive token should
  almost certainly never get anything beyond
  `create:proof-submissions` and maybe `read:own-status`).

## 11. Why rewards don't get deadlines or escalation (revised — tasks now do)

This section originally argued rewards shouldn't mirror punishments'
deadline/escalation machinery at all. That's now partially
superseded: `01-data-model.md` §6 introduces `kind='task'`, which
*does* get a success path (`on_success_template_id`) alongside the
pre-existing failure path (`on_failure_template_id`), because a task
is a third thing distinct from both a pure reward and a pure
punishment — see `11-tasks-and-rewards.md` §1 for the full
reasoning. The original argument still holds for **plain rewards**
(`kind='reward'`, no deadline, no `effect_kind='task'` shape) — a
Keyholder handing out an unprompted "nice work, here's a small
thing" still doesn't need a deadline or a success chain, and that
case is unchanged:

- **A missed deadline is still meaningless for a bare reward** —
  nobody needs to be "punished" for not claiming a reward in time.
  Tasks don't contradict this: a task's deadline governs the task's
  *completion*, not a reward being claimed.
- **Chaining plain-reward-completions** (an escalating reward ladder
  triggered by claiming a reward, as opposed to completing a task)
  is still not built and still not asked for — `on_success_template_id`
  exists only on `kind='task'` rows, not on `kind='reward'` rows, so
  this distinction is enforced by the schema, not just convention.
- A bare reward assignment stays exactly the two-step confirmation
  flow it always was (`assigned` → `acknowledged` →
  `completed`/`revoked`) — only tasks carry the fuller state machine
  (`01-data-model.md` §6, "Task state machine").

## 12. Smarter defaults for `time_extension_seconds` (and `time_reduction_seconds`)

The 6-hour default (`01-data-model.md` §6) is a flat constant applied
to every new time-extension template, regardless of what the
punishment is escalating *from* — a missed 20-minute task and a
missed week-long one get the same starting suggestion. Since
`time_reduction_seconds` was added for rewards
(`11-tasks-and-rewards.md` §2), it inherits the exact same problem and
should inherit whatever default policy this section settles on —
everything below applies to both, mirror-imaged.

Both approaches raised originally are worth keeping, for different
moments rather than picking one winner:

### Two-tier default, resolved

1. **Context-aware, when there is context.** When a template is being
   authored specifically to fill a task's `on_failure_template_id` (or
   a reward's `on_success_template_id`) — i.e. the UI already knows
   which task's `default_deadline_seconds` this consequence is
   escalating from — default to `clamp(originating_deadline_seconds,
   1800, 604800)` (a floor of 30 minutes so a trivially short task
   doesn't suggest a trivially short extension, a ceiling of 7 days so
   a single automatic escalation never suggests something extreme
   without the Keyholder deliberately choosing it). The intuition this
   encodes: *"you get roughly the time back that you were given and
   didn't use"* — proportional, and easy for a Keyholder to explain to
   themselves and to the submissive, rather than an arbitrary flat
   number.
2. **History-aware, when there isn't.** A template authored from the
   general catalog page with no specific originating task in view (no
   deadline to be proportional *to*) instead defaults to the median of
   that Keyholder's own last 10 `time_extension_seconds` (or
   `time_reduction_seconds`) values across their existing catalog — a
   Keyholder whose punishments cluster around 12 hours gets 12 hours
   suggested, not a generic 6. Falls back to the flat 6-hour constant
   only for a Keyholder's genuinely first template of that kind, where
   there's no history yet to learn from.

Both are **suggestions computed at template-creation time**, not a
stored formula or a new column — the Keyholder can freely override
before saving, exactly as today, and the actual stored value is still
just a plain `time_extension_seconds`/`time_reduction_seconds`
integer once chosen. That's what makes this a comparatively cheap
future addition despite the two-tier logic: **no schema change and no
migration**, purely a service-layer prefill computed from data that
already exists (the originating template's deadline, or a `SELECT`
over the Keyholder's own catalog) — worth keeping in mind as a reason
this could get picked up opportunistically even without a specific
trigger forcing it, unlike most of the other items in this document.

Still not built now for the reason originally given: it's tuning a
starting value the Keyholder can already freely override, and the
actual substantive change that mattered this iteration — flagging an
auto-applied extension for review at all
(`08-punishments-and-deadlines.md` §6/§6a) — doesn't depend on the
default being especially clever, just on one existing and being
visibly reviewable rather than either absent or silently trusted
forever.

## 13. Pausing task deadlines / verification scheduling

`08-punishments-and-deadlines.md` §9 deliberately scopes "pause the
clocks" to just the confinement lock timer, not task/punishment
deadlines or verification code issuance — a considered boundary, not
an oversight, since each of those already has its own per-item lever
a Keyholder can reach for today: a specific open task's deadline can
already be extended directly (`03-api-design.md` §7), and a
verification policy can already be relaxed to `on_demand_only` or
edited outright. Below is what the bulk version would actually look
like, worked out concretely rather than left as "would follow the
same pattern."

### The trigger this is for

One thing, specifically: a Keyholder going genuinely unreachable for a
stretch (travel, illness) who wants **everything** that assumes their
ongoing attention frozen at once — every open task/punishment's
deadline, and new verification code issuance — rather than having to
remember to extend N individual deadlines and separately relax the
verification policy, then undo both by hand on return.

### Shape: one umbrella pause, on the link

A new pair of fields on `keyholder_submissive_links` (not on any
individual task/session, since this needs to reach everything open
for a submissive at once): `oversight_paused_at INTEGER NULL`,
`oversight_pause_message TEXT NULL` — same two-field shape as the
existing confinement pause (`01-data-model.md` §4), one level up in
scope.

**While paused:**

- The deadline sweeper (`08-punishments-and-deadlines.md` §3) skips
  the auto-fail pass entirely for any `assignments` row whose link is
  oversight-paused. On **resume**, every still-open row
  (`status IN ('assigned','proof_submitted')`, `deadline_at IS NOT
  NULL`) for that link has its `deadline_at` shifted forward by the
  elapsed pause duration in one bulk update — mirroring exactly how
  resuming the confinement pause adds the elapsed time to
  `target_release_at` (§9 in that doc), for the same reason: without
  the shift, every open deadline would already be in the past the
  instant the pause lifts, and the sweeper's very next tick would
  auto-fail all of them simultaneously, which is the opposite of what
  pausing was for. Logged as **one** link-level audit entry
  summarizing the shift (which rows, how much) rather than one audit
  row per affected assignment — a bulk system action reads better as
  a single entry, the same judgment call already made for the
  confinement-resume's single `confinement_adjustments` row instead of
  per-second bookkeeping.
- The verification-code-issuance background task
  (`04-verification-workflow.md` §2) skips issuing **new** codes for a
  paused link. An already-outstanding code at the moment of pausing is
  left alone — not extended, not cancelled — but this is close to
  moot in practice: if the Keyholder is unreachable, there's no one to
  review a submission against it anyway, and no punishment consequence
  should ever be auto-applied off a pause-window verification gap in
  the first place. Scoping the pause to "stop issuing new ones" rather
  than trying to retroactively neutralize an in-flight code's
  consequences keeps the mechanism simple and avoids a second,
  separate "was this verification's failure exempted by a pause"
  branch anywhere in the review logic.
- **Cascades into the existing confinement pause automatically**, if
  there's an open confinement session: engaging the oversight pause
  also sets `clock_paused_at` (unless already paused), so a Keyholder
  going away doesn't have to remember to flip two separate switches
  for what is, in the "I'm unavailable" scenario, one intent. The
  standalone confinement-only pause stays available on its own for
  the narrower case that doesn't want the rest of this (e.g. a short
  supervised cleaning break, which is a different scenario handled by
  `self_report_allowed`, not this).

### API surface (oversight pause)

`POST /keyholder/submissives/{id}/oversight-pause` `{message?}`,
`POST .../oversight-resume`, `PATCH .../oversight-pause-message` —
directly mirroring the existing confinement-pause endpoint shapes
(`03-api-design.md` §4), same request/response conventions, so
there's no new pattern to learn, just a wider blast radius per call.

### Notifications

`oversight.paused`/`oversight.resumed` to the submissive, same
treatment as `confinement.clocks_paused`/`clocks_resumed`
(`09-notifications.md` §3) including surfacing
`oversight_pause_message` the same way. Reuses the existing
"still paused" 24-hour recurring-reminder pattern aimed at the
Keyholder (`08-punishments-and-deadlines.md` §9) rather than a new
mechanism, since it's the identical failure mode: the person who
paused something is the one most likely to forget it's still paused.

### Why still deferred despite being fully specified now

Building this adds a second, related-but-distinct pause concept a
Keyholder has to understand alongside the existing confinement-only
pause — real cognitive surface, not free. The actual trigger for
needing it (extended Keyholder unavailability spanning multiple open
tasks/punishments at once, often enough that per-item deadline edits
feel burdensome) hasn't shown up as a reported pain point yet. Having
the full design ready here means it can be built directly against
this spec the moment that trigger does show up, rather than needing
this reasoning re-derived from scratch.
