# Future Extensions

Things intentionally deferred, and how the current design leaves room
for each without a breaking rework.

## 1. Play sessions (explicitly requested for later)

Already sketched in `01-data-model.md` §12 (`play_sessions` table,
reserved) and `03-api-design.md` §10 (reserved namespace). When
built:

- New table `play_sessions`, scoped by `link_id` like every other
  per-relationship table — fits the existing ownership model with no
  changes to `keyholder_submissive_links` or the auth middleware.
- Likely wants its own lightweight "activity log" child table
  (similar shape to `proof_attachments`/`assignments`) if sessions
  need multiple timestamped notes/events within one session, rather
  than a single `notes` blob — worth revisiting once real usage
  patterns are known rather than guessing the shape now.
- Could reuse `assignments` (a session outcome triggering a reward)
  the same way `proof_submissions` does today via
  `triggered_by_submission_id` — would need a parallel
  `triggered_by_play_session_id` nullable column, or a more generic
  `triggered_by_entity_type`/`triggered_by_entity_id` pair if more
  trigger sources accumulate. Deferred until there's a second
  concrete trigger source to design against — one example
  (`proof_submissions`) isn't enough to generalize correctly yet.
- Safety-alert pattern (`safety_alerts`) generalizes cleanly to "raise
  during a play session" without change — it's already independent of
  any other flow.

## 2. Self-service link ending (submissive-initiated)

v1 makes ending a `keyholder_submissive_links` row Keyholder-only
(see `02-roles-and-permissions.md` §4), with an unresponsive-Keyholder
escape hatch left as an out-of-band/admin operation. A future version
could add a submissive-initiated "request to end" that:

- Doesn't unilaterally sever the link (to avoid a submissive
  panic-clicking something they didn't mean, and to keep the
  Keyholder in the loop), but
- Starts a visible, time-boxed pending state the Keyholder must act
  on, escalating to a support/admin path if unacknowledged past a
  deadline.

Not designed in full here because the right timeout/escalation policy
is a product decision best made with real usage feedback, not
guessed upfront.

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

- **Email digest / SMS delivery** — could be added as another
  consumer of the same `notifications` rows, same as Web Push is; not
  designed further since nothing in scope needs it yet.
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
and read each other's boundaries in prose. A structured version
(a fixed or Keyholder-extensible checklist of specific items, each
tagged hard/soft/okay per submissive, similar in spirit to how
`reward_punishment_templates` structures the rewards/punishments
catalog) would make limits easier to scan and could let the UI flag
"this proposed assignment touches a listed hard limit" — but that's a
real content taxonomy to design (what the default item list is, who
can extend it, whether it's global or per-Keyholder) and isn't
justified by anything in scope today. Free text first; revisit if
prose limits turn out to be too easy to skim past in practice.

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

## 11. Why rewards don't get deadlines or escalation

Punishments got substantially richer structure in
`01-data-model.md` §6 (`effect_kind`, `completion_type`, deadlines,
`on_failure_template_id`) — rewards deliberately did not get the
mirror image (no `on_success_template_id`, no reward deadlines). This
was an explicit instruction, not a default carried over by inertia,
but it's worth recording *why* it also makes sense structurally, in
case a future request asks to symmetrize them:

- **A missed deadline is meaningful for a punishment**
  (non-compliance has to be noticed and acted on) but has no
  equivalent bite for a reward — nobody needs to be "punished" for
  not claiming a reward in time, so there's nothing for a deadline
  mechanism to enforce.
- **Chaining rewards-on-completion** would mean success breeding more
  success automatically, which is a fundamentally different dynamic
  (an escalating reward ladder) than what was asked for here. If
  that's ever wanted, it's an additive, separate feature — a
  `on_completion_template_id` on the reward side — not a
  generalization of the punishment escalation machinery, because the
  *reason* punishment escalation exists (an enforcement backstop for
  non-compliance) doesn't apply to rewards at all.
- Kept simple, a reward assignment stays exactly the two-step
  confirmation flow it always was (`assigned` → `acknowledged` →
  `completed`/`revoked`), which is also just less for a Keyholder to
  configure for the common case ("nice work, here's a small thing")
  that doesn't need any of this machinery.

## 12. Smarter defaults for `time_extension_seconds`

The 6-hour default (`01-data-model.md` §6) is a flat constant applied
to every new time-extension template, regardless of what the
punishment is escalating *from* — a missed 20-minute task and a
missed week-long one get the same starting suggestion. A more
sophisticated default might scale off the originating punishment's
own `default_deadline_seconds`, or off the Keyholder's own historical
choices (most of their time-extension templates cluster around N
hours, so suggest N). Not built now because it's tuning a starting
value the Keyholder can already freely override, and the actual
substantive change this iteration is about — flagging an
auto-applied extension for review at all (`08-punishments-and-deadlines.md`
§6) — doesn't depend on the default being especially clever, just on
one existing and being visibly reviewable rather than either absent
or silently trusted forever.

## 13. Pausing punishment deadlines / verification scheduling

`08-punishments-and-deadlines.md` §9 deliberately scopes "pause the
clocks" to just the confinement lock timer, not punishment deadlines
or verification code issuance — a considered boundary, not an
oversight, since each of those already has its own per-item lever
a Keyholder can reach for today: a specific open punishment's deadline
can already be extended directly (`03-api-design.md` §7), and a
verification policy can already be relaxed to `on_demand_only` or
edited outright. If a genuine need for a dedicated pause/resume
concept on either of those shows up later — e.g. "pause every open
punishment's deadline for this submissive at once" as its own bulk
action, distinct from editing them one at a time — it would follow
the exact same pattern already established for the lock timer (a
`clock_paused_at`-style field plus a resume-time delta), not require
rethinking the approach. Not built now because nothing yet has asked
for the bulk version specifically.
