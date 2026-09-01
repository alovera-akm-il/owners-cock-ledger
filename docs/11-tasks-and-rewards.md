# Tasks, Rewards, and Points

Schema reference: `01-data-model.md` §6 (tasks/rewards/punishments)
and §12 (points). This document covers the research and rationale:
why tasks are a third `kind`, what proof-of-completion looks like,
what changed on the reward side, and whether a points system is
worth building.

## 1. Why tasks are a third `kind`, not a punishment variant

Before this pass, the schema had `reward_punishment_templates.kind IN
('reward','punishment')`, and a punishment could optionally have
`effect_kind='task'` — meaning "the consequence for X is: go do this
task." That shape is *structurally* a neutral, assignable task with a
failure path (`on_failure_template_id`) bolted on. It has no success
path, because nothing in a punishment's design ever needed one — a
punishment being completed just ends the punishment.

The request here is for tasks that are assigned proactively (not as
a consequence of something else) and that resolve to *either* a
reward *or* a punishment depending on whether they're completed
successfully or not — a genuine two-way branch, not a one-way
failure escalation. Retrofitting that onto `effect_kind='task'` would
mean either:

- Adding `on_success_template_id` only to punishment rows shaped as
  tasks (confusing — a field that means "reward" living on a row
  whose `kind` is `'punishment'`), or
- Duplicating the whole `completion_type`/`proof_media_types`/
  `default_deadline_seconds`/`on_failure_template_id` field set onto
  a *second* table for `kind='task'` and keeping punishment-as-task
  as a legacy path.

Both are worse than admitting these were always the same underlying
shape (an assignable, deadline-bearing, provable unit of work) with
one path missing. `kind='task'` unifies them: a task is that shape
with both `on_success_template_id` and `on_failure_template_id`
available. A punishment that just wants "go do this, or else" is a
task with only `on_failure_template_id` set. Nothing that worked
before changes — this is additive to the existing rows' shape, not a
breaking rename.

`kind='reward'` and `kind='punishment'` keep their original meaning:
a direct grant or consequence with no completion/proof workflow of
their own (`effect_kind IN ('grant','time_extension','time_reduction')`).
A task is the thing that gets *done*; a reward or punishment is the
thing that *happens as a result* (of a task, of a verification
outcome, of a play session judgement, or of nothing — a Keyholder can
still hand out a bare reward with no triggering event at all).

## 2. Proof of completion: photo, video, or voice

`reward_punishment_templates.proof_media_types` (task-only, JSON
array) and the matching `completion_type` field
(`acknowledge_only`/`proof_required`) generalize the verification
system's existing photo-proof pattern
(`04-verification-workflow.md`) rather than inventing a new one:

- `completion_type='acknowledge_only'` — a submissive marks the task
  done; no attachment required. Fine for tasks like "message your
  Keyholder before noon" where there's nothing to photograph.
- `completion_type='proof_required'` — one or more attachments
  required, and `proof_media_types` constrains *which* kinds
  (`["photo"]`, `["photo","video"]`, `["voice"]`, etc.) the template
  accepts. A task can accept more than one media type at once — e.g.
  a task template might accept either a photo or a short video, and
  the submissive picks whichever fits.

This reuses `proof_submissions`/`proof_attachments`
(`01-data-model.md` §5) as the storage mechanism — a task's proof
submission is stored exactly like a verification code's proof
submission is, same private-blob-storage path, same review flow
(Keyholder verifies/rejects). The only schema change needed is that
`proof_attachments.media_type` (already presumably
photo/video-agnostic, extended here) also accepts `'voice'`, and that
voice recordings get the same size/format constraints as photo/video
uploads in `05-security-and-privacy.md` §4 (a capped duration and
file size, not unbounded audio).

**Voice recording privacy note** (new, flagged for
`05-security-and-privacy.md`): a voice recording is a categorically
more identifying artifact than a photo of a cage or a body part — it
captures the submissive's actual voice, which is far easier to tie to
a real identity than an anonymized/cropped photo would be. It should
get the same private-storage treatment as photo/video, and is a
reasonable candidate to add to the field-level-encryption-candidates
list alongside `keyholder_notes` and the limits fields, higher
priority than photo/video given the identifiability difference.

## 3. Points system

### Should this be built at all?

The request was to "look into" whether points are needed, not a
firm requirement — so the honest answer is: **optional, opt-in per
link, not load-bearing for anything else.** Concretely:

- Rewards and punishments already work as direct, explicit grants —
  a Keyholder assigning a specific reward for a specific task doesn't
  need a points layer to function. Points don't replace any part of
  that mechanism; they sit alongside it.
- What points *do* add, that direct grants alone don't: a running
  sense of "how am I doing overall" that isn't tied to any single
  task, and a foundation for **submissive-initiated redemption** —
  today a submissive can never assign themselves anything; a points
  balance is the one thing that could justify letting them request a
  reward rather than always waiting to be given one.
- The risk of building it as mandatory: it adds a whole visible
  subsystem (balances, transaction history, redemption UI) that a
  Keyholder who just wants simple direct rewards/punishments now has
  to look past. That's why `keyholder_submissive_links.points_enabled`
  defaults to off (`01-data-model.md` §12) — same posture as
  `self_report_allowed`.

### Design chosen: append-only ledger + cached balance

`point_transactions` is the source of truth (every change is one
itemized row — task completed, task failed, verification outcome,
check-in logged, manual adjustment, redemption), and
`keyholder_submissive_links.points_balance` is a cached running total
kept in sync transactionally on every insert. This is a deliberate
exception to this schema's general "derive, don't cache" instinct
(confinement lock status, for example, is always computed from
`target_release_at`, never stored): a point balance is read on nearly
every dashboard load by both roles, and changes far less often than
it's read, so caching is the right tradeoff here specifically. The
ledger stays the audit trail, exactly like `confinement_adjustments`
is for the timer — "why do I have 42 points" always has a full,
itemized answer, never just a number with no history.

### What earns and spends points

`points_delta` lives on `reward_punishment_templates` (task-only, in
addition to reward/punishment effects) — a Keyholder can optionally
attach a point value to a task's success (and, separately, its
`on_failure_template_id` chain can carry its own negative
`points_delta` on the punishment side). Points are **additive to**
the reward/punishment mechanism, not a replacement — a task can grant
both a direct reward *and* points on completion.

Proposed earn/spend surface (`point_transactions.reason`):

| reason | direction | source |
|---|---|---|
| `task_completed` | + | a task's `points_delta`, on success |
| `task_failed` | − (optional) | a task's failure-path `points_delta`, if the Keyholder wants failures to cost points, not just trigger a punishment |
| `verification_verified` | + (optional) | small, consistent positive reinforcement for routine compliance — see below |
| `verification_failed` / `verification_missed` | − (optional) | mirror of the above |
| `checkin_logged` | + (optional, small) | rewards the habit of checking in at all, independent of what the check-in says |
| `manual_adjustment` | either | Keyholder can always hand-adjust the balance with a note — the same "Keyholder is the final authority" escape hatch used everywhere else in this app |
| `redemption` | − | the one submissive-initiated row, see below |

Tying routine verification check-ins into points (not explicitly
requested, but a natural extension once points exist) gives points a
steady trickle from ordinary compliance, not just occasional task
completions — this is the "give it a go with what you'd add" case for
points specifically, and it's entirely optional per Keyholder
(a template with no `points_delta` set simply never generates that
row).

### Redemption requests (new submissive capability)

If `points_enabled` is on and a reward template carries a
`points_cost`, a submissive with a sufficient `points_balance` can
request to redeem it: this creates a `reward_redemption_requests` row
in a `pending` state (not documented as a full table here since it's
a small, single-purpose one — see `03-api-design.md` for the
endpoint) that the Keyholder approves (creating a normal `assignments`
row and deducting the points via a `redemption` ledger entry) or
denies (no point deduction). This is the one deliberate exception to
"submissives never self-assign anything" in the whole app — scoped
narrowly enough that it doesn't weaken that principle: the submissive
is only ever *requesting* against a balance the Keyholder's own
templates and grants built up, and the Keyholder still has final
approval before anything is actually granted.

## 4. Gaps considered and deliberately not built

- **Point expiry / decay** — not built; adds real complexity (when do
  points expire, does that need its own sweeper) for a problem
  ("balances grow unbounded") that isn't a proven issue and can be
  handled with a manual adjustment if it ever comes up.
- **Leaderboards / multi-submissive comparison** — out of scope; this
  schema deliberately keeps every relationship pairwise
  (`link_id`-scoped) and has no precedent anywhere for cross-link
  aggregation or comparison views.
- **Automatic reward suggestion from templates near a point
  threshold** — plausible future UI nicety (surface "you're 20 points
  from being able to redeem X"), not a schema concern, left for
  `06-future-extensions.md` if wanted later.
