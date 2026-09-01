# Tasks, Punishments, Deadlines, and Escalation

This document is the workflow companion to `01-data-model.md` §6
(`reward_punishment_templates`/`assignments`) — read that first for
the schema; this is the *mechanics*: how a deadline/escalation ladder
is built once, how an individual task or punishment moves through its
lifecycle, and what the server does on its own when a deadline
passes.

Everything below was originally written for `kind='punishment',
effect_kind='task'` specifically, before `kind='task'` existed as its
own thing (`11-tasks-and-rewards.md` §1). It applies unchanged to
`kind='task'` rows generally — a task assigned proactively goes
through exactly the same deadline/proof/sweeper mechanics as a task
reached as a punishment's consequence, the only difference being
*why* it was assigned, not how it behaves once assigned. Where this
document says "punishment," read it as "a task-shaped
`assignments` row" unless a passage is explicitly about the
consequence side (`effect_kind='time_extension'`) of a plain
`kind='punishment'` row. The one genuinely new piece this document
didn't cover before: a task can also resolve *successfully* into
`on_success_template_id` (§6a) — the escalation machinery described
here for failure has a mirror-image success path now, not just a
failure one.

Bare rewards/punishments (`kind IN ('reward','punishment')`,
`effect_kind='grant'`, no completion workflow of their own) are
intentionally out of scope here — they don't have deadlines or
failure states (`06-future-extensions.md` §11 covers why that
asymmetry is deliberate, not an oversight).

## 1. Building an escalation ladder in the catalog

A Keyholder designs consequences once, as templates, not per
incident. A minimal but complete example:

```
Template: "cold shower, 5 min, video required"
  kind = task
  completion_type = proof_required
  proof_media_types = ["video"]
  default_deadline_seconds = 86400        (24h)
  on_failure_template_id -> "extra day locked"

Template: "extra day locked"
  kind = punishment, effect_kind = time_extension
  time_extension_seconds = 86400          (24h)
  on_failure_template_id = NULL           (nothing to fail into — see §6)
```

Assigning "cold shower" to a submissive who then never submits proof
(or submits something the Keyholder rejects) automatically produces a
brand-new "extra day locked" assignment against that same submissive,
with no further Keyholder action required in the moment. The
Keyholder built the ladder once; every future assignment from "cold
shower" inherits it.

A template can also have `on_failure_template_id = NULL` from the
start — meaning "if this fails, I'll decide what happens myself,"
which is a completely valid, common choice, not a lesser one. Nothing
about the system assumes every punishment must auto-escalate.

## 2. Assigning a punishment (instantiation)

`POST /keyholder/submissives/{id}/assignments` (`03-api-design.md`
§7). Whether from a template or ad-hoc, the created `assignments`
row copies (or receives inline) `effect_kind`, `completion_type`,
`time_extension_seconds`, and `on_failure_template_id`, and computes
`deadline_at`:

- From a template: `deadline_at = assigned_at + template.default_deadline_seconds`,
  unless the Keyholder passes an explicit `deadline_at` override in
  the same request.
- Ad-hoc: the Keyholder must supply either `default_deadline_seconds`
  (used the same way) or a direct `deadline_at`.
- `effect_kind = time_extension`: no deadline at all — see §5.

`on_failure_template_id` is resolved and stored **on the assignment**
at this moment, copied from the template (or explicitly overridden).
This copy-at-creation-time choice matters: if the Keyholder later
edits or deactivates that template, an *already-assigned* punishment's
escalation path doesn't silently change out from under it — see §6.

## 3. The deadline sweeper

A Tokio background task, structurally identical to the
verification-code-issuance task in `04-verification-workflow.md` §2
— a short interval tick (e.g. every minute), scanning for work rather
than being invoked by any client request. Each tick:

1. **Auto-fail pass**: find every `assignments` row where
   `kind='task'`, `status='assigned'`
   (i.e. the submissive has done *nothing* — not acknowledged, not
   submitted proof), and `deadline_at < now`. For each, in one
   transaction: set `status='failed'`, `status_updated_at=now`, write
   an `audit_log` entry, and — if `on_failure_template_id` is set —
   perform the escalation in §6. `assigned_via` on the *escalation*
   row (not the failed one, which keeps whatever it already had) is
   set to `'system'`. Note: pausing the confinement clock (§9) has no
   effect here — punishment deadlines are a separate concern and keep
   running on their own schedule regardless of whether the lock timer
   is paused.
2. **Deadline-approaching pass**: purely informational — it
   never changes `status`. The naive version of this ("remind 1 hour
   before `deadline_at`") breaks down for a short-fuse punishment: a 20-minute
   deadline would need its reminder *before* the punishment was even
   assigned, and a 90-minute one would fire almost immediately after
   assignment, which is just noise on top of the
   `punishment.assigned` notification that already told the submissive
   about it. So the window scales with the punishment's own length
   instead of being a flat constant:
   - `total = deadline_at - assigned_at`, `window = min(1 hour, total / 2)`,
     `reminder_at = deadline_at - window`.
   - If `reminder_at` would fall less than 5 minutes after
     `assigned_at`, **no reminder is scheduled at all** — for a
     punishment that short, the assignment notification *is* the
     warning, and a second one seconds later adds nothing.
   - Otherwise, each tick finds `assignments` still `status='assigned'`
     where `now >= reminder_at` and no
     `punishment.deadline_approaching` notification has been recorded
     yet for this assignment (checked against `notifications` by
     `related_entity_id`, rather than a new column on `assignments` —
     one less piece of state to keep in sync), and enqueues exactly
     one such notification.
   - A deadline edited after assignment (`PATCH .../deadline`,
     `03-api-design.md` §7) recomputes `reminder_at` from the new
     `deadline_at` and the *original* `assigned_at` on the next tick —
     a Keyholder who extends a deadline can end up with a fresh
     reminder scheduled further out, which is the correct behavior,
     not a bug to guard against.

Only `status='assigned'` is swept for auto-failure. `acknowledged`
and `proof_submitted` are **not** — once the submissive has acted
(acknowledged, or submitted proof), the deadline's job is done; what
happens next is on the Keyholder's own timeliness reviewing it, which
this system doesn't (and shouldn't) penalize the submissive for. See
`02-roles-and-permissions.md` §5 for this as an explicit design
choice, not an oversight.

## 4. The two ways a task fails (and the one way it succeeds)

1. **Deadline auto-fail** (§3 above) — the submissive didn't act in
   time. `assigned_via='system'` on the resulting escalation,
   `reviewed_by_user_id` stays NULL on the original (nobody reviewed
   anything; there was nothing submitted to review).
2. **Keyholder-judged fail** — for a `proof_required` task, the
   Keyholder reviews a submitted completion proof and rejects it
   (`04-verification-workflow.md` §7). Here a human did look at
   something and decided it didn't count.

Both land the assignment in the same terminal `status='failed'` and
trigger the same escalation logic (§6) — the *record* of which path
it took lives in whether `proof_submission_id` is set and what that
submission's own review trail shows, not in a separate status value,
since from the submissive's perspective ("I failed this task and now
have a new one") the practical outcome is identical.

Success is simpler and has only one path: an `acknowledge_only` task
the Keyholder marks `completed`, or a `proof_required` task whose
submitted proof the Keyholder reviews as `verified`. Either lands the
assignment in terminal `status='completed'` and, if
`on_success_template_id` is set, triggers the success-path escalation
in §6a — the mirror image of §6, using the same mechanics.

## 5. Applying a `time_extension` effect

Whether reached as a fresh assignment or as an escalation, applying a
`time_extension` punishment is one transaction:

1. Find the submissive's current open `confinement_sessions` row
   (`ended_at IS NULL`). If there isn't one (the submissive currently
   isn't locked at all), the extension has nothing to extend — insert
   the assignment anyway (status `applied`, for the record — the
   punishment still "happened"), but skip the `confinement_adjustments`
   insert and surface this clearly to the Keyholder (e.g. a note on
   the assignment: "no active confinement session to extend"). This
   is a real scenario (a punishment escalates to a time extension
   after the submissive was already unlocked) and shouldn't be
   silently swallowed or crash the transaction.
2. If there is an open session: insert a `confinement_adjustments`
   row (`reason='punishment_time_extension'`,
   `caused_by_assignment_id` = this assignment,
   `delta_seconds = time_extension_seconds`,
   `adjusted_by_user_id = NULL` — see `01-data-model.md` §4, nobody
   clicked anything in this moment), and update the session's
   `target_release_at += delta_seconds`.
3. Set the assignment's `status='applied'`.

This is exactly the same shape as a Keyholder's manual timer
adjustment (`03-api-design.md` §4) — same target table, same delta
pattern — the only difference is who/what initiated it and that
`reason` records which.

## 6. Escalation mechanics (failure path)

Triggered from §3 step 1 (deadline auto-fail) or from a
Keyholder-judged fail via `04-verification-workflow.md` §7 (a
rejected completion proof) — either path in §4 above. Given a
just-failed assignment `F` with `F.on_failure_template_id = T`:

1. Load template `T`. **Note:** `T` is used even if
   `T.active = false` — deactivating a template only stops it from
   being offered as a *new* choice in the Keyholder's UI going
   forward; it doesn't retroactively break an escalation chain that
   was already wired up when `F` (or an ancestor of `F`) was created.
   If a Keyholder genuinely wants to sever a chain, they edit the
   assignment or the upstream template's `on_failure_template_id`
   directly — deactivation is a different, weaker signal ("don't let
   people pick this anymore") and conflating the two would make
   catalog cleanup accidentally dangerous.
2. Create a new `assignments` row from `T`, exactly as §2 describes
   for any template-based assignment, with `link_id` copied from `F`,
   `escalated_from_assignment_id = F.id`, and `assigned_via='system'`
   (or `'session'`/`'api_token'`/preserved-from-`F` if this escalation
   was itself triggered synchronously inside a Keyholder's own review
   call rather than the background sweeper — either way it's
   accurately attributed, never misattributed as a fresh manual pick).
3. If `T.effect_kind = 'time_extension'`, apply it immediately per
   §5, in the *same* transaction as creating the row — and leave the
   resulting `confinement_adjustments.keyholder_reviewed_at` NULL
   (`01-data-model.md` §4). This is the one step in the whole
   escalation pipeline where a number that was pre-configured, maybe
   weeks ago, gets applied to a real person's confinement time with
   no Keyholder present in the moment. Applying it immediately still
   matters (§7 — the consequence shouldn't wait on the Keyholder
   noticing a notification), but "applied automatically" and
   "reviewed" are kept as two separate facts rather than one, so a
   default nobody has looked at in a while doesn't just keep firing
   unexamined.
4. Send **two** notifications (`09-notifications.md`):
   - `punishment.assigned` to the submissive, same as any new
     punishment, flagged as arriving via escalation
     (`escalated_from_assignment_id`) so the notification/UI can say
     "because you didn't complete X."
   - if (and only if) `T.effect_kind = 'time_extension'`: a
     `confinement.time_extension_needs_review` notification to the
     **Keyholder** — this is new, and closes a real gap: nothing
     before this told the Keyholder that an automatic timer change
     had just happened at all when it came from a `system`-attributed
     escalation rather than their own click. Body: submissive name,
     the amount just applied, and which punishment caused it; `Push:`
     yes, since it's actionable, not just informational. `link_path`
     goes straight to the confinement timer's adjust control
     (`03-api-design.md` §4) with this specific adjustment in view —
     not a generic "something changed" pointer.
   No notification of this kind fires for a **manually assigned**
   time-extension punishment, only an escalated one — a Keyholder who
   just picked "extra day locked" from the review screen already saw
   the amount before confirming it (`04-verification-workflow.md`
   §4); asking them to re-review their own just-completed action a
   second later would be noise, not a safeguard. The gap this closes
   is specifically "a consequence applied without anyone present,"
   and a manual assignment doesn't have that gap.

The Keyholder resolves the flag one of two ways, both ending in
`keyholder_reviewed_at` getting set: `PATCH
.../timer-adjustments/{id}/review` with no further change ("6 hours
was right, leave it"), or a follow-up manual delta on the same
session via the ordinary `PATCH .../timer` endpoint (§4 in
`03-api-design.md`) — correcting it is itself the review, so applying
a manual adjustment on a session with an outstanding unreviewed one
marks that prior row reviewed as a side effect rather than requiring
a separate acknowledgment click first.

Recursion is inherent, not special-cased: if `T` itself has an
`on_failure_template_id`, the newly-created assignment can *later*
fail and escalate again, by the exact same steps. Nothing needs a
depth counter or recursion guard — progressing further down a chain
always requires either real time passing (another `deadline_at`) or
another Keyholder judgment call (another proof rejection), so a chain
cannot run away with itself in a tight loop the way naive recursive
code sometimes can. A `time_extension`/`time_reduction` leaf (§1)
can't fail or succeed at all (`status='applied'` is terminal), so
every chain has at least one natural place it's guaranteed to stop.

## 6a. Escalation mechanics (success path)

Triggered when a task lands in `status='completed'` (§4) with
`on_success_template_id` set. Mechanically identical to §6 with
"success" substituted for "failure" throughout: given just-completed
assignment `C` with `C.on_success_template_id = T`, the server loads
`T` (again, even if `T.active = false`, same reasoning as §6 step 1),
creates a new `assignments` row from it with `escalated_from_assignment_id
= C.id` and `assigned_via` attributed the same way, applies it
immediately if `T.effect_kind` is `time_reduction` (per §5's
mirror-image reasoning, flagging `keyholder_reviewed_at` NULL the
same way an auto-applied `time_extension` does), and sends the
equivalent notifications — `reward.assigned` (or `task.assigned`, if
`T` is itself a task) to the submissive, and, only for an escalated
`time_reduction`, a `confinement.time_reduction_needs_review`
notification to the Keyholder, for the identical reason §6 step 4
flags an escalated `time_extension`: a number applied to real
confinement time with no Keyholder present in the moment deserves a
visible, resolvable flag, not silent trust.

The one asymmetry worth naming: success chains are expected to be
shallow in practice (task → reward, most commonly, rather than a long
ladder) since nothing about "doing well" has the same open-ended
escalation logic that repeated non-compliance does — but the schema
doesn't enforce shallowness, and a Keyholder who wants a multi-step
success chain (task → task → reward) can build one exactly the same
way a failure ladder is built in §1.

## 7. Interaction with the confinement timer

`target_release_at` (`01-data-model.md` §4) is what "how long they're
supposed to be locked" means in this system, and §5/§6 above are the
*only* two ways a punishment moves that number — a task-based
punishment never touches it directly, only a `time_extension` one
does, whether reached directly or via escalation. Displaying "why is
my time longer than I expected" is always answerable by walking
`confinement_adjustments` for the current session (`03-api-design.md`
§4's `timer-adjustments` endpoint), which for a punishment-caused row
links straight back to the assignment (and, via
`escalated_from_assignment_id`, the whole chain that led there).

## 8. Edge cases considered

- **Keyholder edits `deadline_at` while the sweeper is mid-tick.**
  The sweeper's auto-fail pass reads `deadline_at` fresh on each tick
  and only acts on rows still `status='assigned'` past whatever
  `deadline_at` currently says — there's no cached/stale deadline
  anywhere, so an extension applied a second before the sweeper would
  have fired is honored, not raced.
- **The link ends (or is paused) mid-chain.** The sweeper's queries
  are not filtered by link status the way interactive API calls are
  (`02-roles-and-permissions.md` §5 rule about `active` links) —
  deadlines and escalations for a `paused` or even `ended` link still
  resolve, because the punishment was real and already in flight; a
  paused/ended link stops new *manual* Keyholder actions, not the
  natural conclusion of consequences already set in motion. This is
  a judgment call, flagged as such — a Keyholder who wants a clean
  break on pausing a link should also revoke any open punishments for
  that submissive at the same time (`PATCH .../assignments/{id}` →
  `revoked`), which does stop the chain (§ next bullet).
- **Revoking never escalates.** Explicit, from `01-data-model.md` §6:
  `revoked` is a Keyholder deciding "never mind," not a failure, so it
  is never treated as one no matter how it's reached.
- **A redo drags past the original deadline.** Not specially handled
  — the sweeper only acts on `status='assigned'`, and a punishment
  awaiting redo is `proof_submitted` (unaffected) up until the
  Keyholder actually reviews it as `failed` (§4-of-workflow-doc, a
  judged fail, not a swept one). A Keyholder who wants "you have to
  get this right by midnight, redos included" should extend
  `deadline_at` to account for review turnaround, or simply reject
  promptly — this is a coordination expectation between the two
  people, not something the deadline mechanism enforces on its own.
- **Multiple punishments with overlapping/simultaneous deadlines** —
  no interaction between them; each assignment is swept independently.
  A submissive can have several open punishments at once, each with
  its own clock.
- **Timezone**: `deadline_at` is stored and compared as UTC epoch
  seconds like every other timestamp in this schema
  (`01-data-model.md` intro) — display formatting into the
  submissive's or Keyholder's local timezone (`timezone` on their
  respective profiles) is a client-side/API-boundary concern, not a
  storage one.

## 9. Pausing the lock timer

A Keyholder can freeze the confinement countdown for a submissive —
`target_release_at` stops advancing until resumed. Scope is
deliberately narrow: **this affects only the chastity cage's lock
timer** on `confinement_sessions` — nothing about punishment
deadlines, verification scheduling, or anything else in the system
changes because of this. It is not a first step toward a broader
"pause everything" mode; see the note on scope at the end of this
section for why that's a deliberate boundary, not an oversight.

The motivating case: the Keyholder is genuinely unavailable (travel,
illness, anything), and the submissive shouldn't get "credit" toward
release for time the Keyholder wasn't actually supervising — the
required duration shouldn't quietly erode just because nobody was
there to extend it manually.

This is a different axis entirely from
`keyholder_submissive_links.status`, which pauses the *administrative
relationship* — see `01-data-model.md` §4 for why the two are
deliberately separate fields on different tables with deliberately
dissimilar names, not two values of one enum.

### Pausing

`POST /keyholder/submissives/{id}/confinement-sessions/{sessionId}/pause`
(`03-api-design.md` §4), body `{message?}`. `409` if there's no open
session, or it's already paused. Sets `clock_paused_at = now()` and,
if given, `clock_pause_message` on the session. That's the entire
effect at pause time — no other row is touched, no deltas computed
yet. The countdown displays as "Paused" rather than a frozen number
that quietly goes stale or, worse, silently keeps ticking in the UI
while meaning nothing — and, when a message was given, shows it right
alongside "Paused" rather than leaving the submissive staring at a
frozen number with no idea why.

`message` is optional — a Keyholder can pause without explaining
anything, same as they can extend the timer with an empty `notes`
field on a manual adjustment. When present it's shown to *both*
roles identically (`01-data-model.md` §4): the Keyholder isn't
writing a private note to themselves here, they're writing something
meant to be read by the person whose countdown just froze.

While still paused, `PATCH .../confinement-sessions/{sessionId}/pause-message`
(`03-api-design.md` §4) lets the Keyholder update or clear the
message without resuming and re-pausing — "still away, back Monday
instead of Friday" doesn't need to be modeled as ending one pause and
starting another.

The submissive cannot end their own confinement regardless of any of
this — self-unlock was never a capability in the first place unless
`self_report_allowed` is set for the link (`01-data-model.md` §4), so
"the submissive stays locked" while paused was already guaranteed by
the existing model; what pausing specifically adds is that the
*required duration* doesn't erode during the time nobody was
supervising it.

### Resuming

`POST /keyholder/submissives/{id}/confinement-sessions/{sessionId}/resume`.
`409` if not currently paused. Computes `elapsed = now -
clock_paused_at`, then in one transaction: insert a
`confinement_adjustments` row (`reason='clock_pause'`,
`delta_seconds = elapsed`, `notes = clock_pause_message` (carrying the
message forward into permanent history rather than discarding it,
`01-data-model.md` §4), `keyholder_reviewed_at = now()` — the
Keyholder resuming is itself the confirmation, same as any other
`manual`-flavored row), apply `elapsed` to `target_release_at`, and
clear both `clock_paused_at` and `clock_pause_message`.

### Notifications

Both actions notify the submissive (`09-notifications.md`) —
`confinement.clocks_paused` and `confinement.clocks_resumed`. The
pause notification's body includes `clock_pause_message` when one was
given ("Your Keyholder paused your lock timer: 'traveling for work,
back Friday'"), falling back to a generic "your lock timer has been
paused" when it wasn't. Resume is push-worthy since it changes a real
number (their displayed release date just moved forward by the pause
length); pause itself is a real state change too and gets the same
treatment for consistency, even though nothing about the submissive's
obligations changes at the moment of pausing itself. Updating the
message mid-pause (the `PATCH` above) does **not** send a fresh push
— it's a feed-only update, since it's a refinement of something
already communicated, not a new event on the level of pausing or
resuming.

### Still-paused reminder

The whole premise of pausing is that the Keyholder is away — which is
exactly the situation where they're most likely to forget it's still
paused once they're back. Nothing about pausing itself expires or
nags on its own, so a separate, lightweight check closes that loop:
on the same tick as the deadline sweeper (§3 — this doesn't warrant a
third background task, just another pass in the existing one), find
every `confinement_sessions` row where `clock_paused_at IS NOT NULL`
and at least 24 hours have elapsed since it was set. For each, check
whether a `confinement.clock_still_paused` notification has already
been recorded for this session within the last 24 hours (the exact
same existing-notification lookup already used to dedupe the
deadline-approaching pass, §3) — if not, send one, **to the
Keyholder**, not the submissive. This is the one pause-related
notification aimed at the Keyholder rather than the submissive: the
submissive already knows their timer is paused (they were told when
it happened); what's missing is reminding the *Keyholder* that it's
still true. Left unresolved, this repeats roughly once every 24 hours
for as long as the pause continues, rather than firing once and being
forgotten, or never firing again and letting a pause silently persist
indefinitely.

### Scope: why this doesn't also touch punishment deadlines

It's tempting to generalize "pause time for this submissive" into one
switch that also freezes the deadline sweeper (§3) and verification
issuance (`04-verification-workflow.md` §2), but that was explicitly
asked to be narrowed to just the lock timer, and there's a real reason
that narrower scope is defensible rather than merely "what was
requested": the lock timer is a single, unambiguous quantity per
submissive with one obvious pause semantics (stop counting toward one
target). Punishment deadlines are plural, independent, and each
already has its own Keyholder-editable `deadline_at`
(`03-api-design.md` §7) — a Keyholder who wants a specific open
punishment's clock to wait can already extend that one deadline
directly, without needing a separate pause/resume concept layered on
top. If a genuine need for pausing punishment deadlines (or
verification scheduling) as their own concept shows up later, it's an
additive feature — its own `pause`/`resume` pair, following exactly
this same pattern — not a reason to have generalized this one
prematurely. See `06-future-extensions.md` for this noted as a
considered-and-deferred extension rather than an unconsidered gap.