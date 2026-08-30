# Verification Workflow

This is the central loop of the application: prove → review →
(pass | redo | fail-and-consequence). Written out in more detail than
the API table alone conveys, because the ordering and atomicity
matter for the guarantees the system is supposed to provide.

## 1. Setting the policy

A Keyholder sets one `verification_policies` row per link
(`PUT /keyholder/submissives/{id}/verification-policy`):

- `frequency_kind = interval_hours`, e.g. every 24h.
- `frequency_kind = fixed_times_daily`, e.g. 09:00 and 21:00 local
  (submissive's `timezone`).
- `frequency_kind = random_within_window`, e.g. once between 08:00
  and 22:00 at an unpredictable time within that window — supports a
  "no warning, no gaming the schedule" mode.
- `frequency_kind = on_demand_only` — no scheduled prompts; a code
  exists only when the submissive requests one, e.g. for a
  Keyholder who prefers to ask verbally/out-of-band and have the
  submissive respond digitally.

Independent of `frequency_kind`, a Keyholder can also just message
their submissive out-of-band ("prove now") — the submissive presses
"request code" and the same on-demand path fires, subject to the
policy allowing it (`on_demand_only` always allows it; the scheduled
kinds may also allow an extra on-demand request, or restrict to
schedule-only — a `allow_on_demand: bool` sits inside
`frequency_value` for the scheduled kinds).

## 2. Code issuance

A Tokio background task wakes on a short tick (e.g. every minute),
and for each active link whose policy says a code is due and which
doesn't already have a live unconsumed code, inserts a new
`verification_codes` row:

- `code`: CSPRNG-generated, short enough to hand-write into a photo
  (e.g. 6-8 characters, avoiding visually ambiguous characters like
  `0`/`O`/`1`/`I`).
- `expires_at = issued_at + policy.code_ttl_seconds`.
- The submissive sees it via `GET /submissive/verification-codes/current`
  (poll, or the page simply reloads it) and via whatever notification
  mechanism exists later (see `06-future-extensions.md` — v1 has no
  push notifications, so the submissive must check the app; this is a
  known limitation for `random_within_window` mode where a missed
  window may need the Keyholder to notice and prompt out-of-band).

On-demand requests go through `POST /submissive/verification-codes`
which performs the same insert synchronously and rejects
(`409`) if an unconsumed, unexpired code already exists — a
submissive cannot stockpile codes.

Expiry is enforced at read/consume time by comparing to the current
server time, not by a cleanup job; an expired, unconsumed code simply
becomes unusable and (optionally) a new one is issued on the next
tick per policy.

## 3. Submitting proof

`POST /submissive/proof-submissions`, multipart:

1. Server resolves the caller's active link.
2. If `verification_code_id` is present: load it, confirm
   `link_id` matches, `consumed_at IS NULL`, and `now <= expires_at`
   (or within `expires_at + grace_period_seconds` if the policy
   allows late-but-graced submissions to still count as "on time" in
   review notes, though still ultimately reviewed by the Keyholder).
   If the code is missing/expired/consumed/foreign, `409`.
3. If `verification_code_id` is absent, the submission is treated as
   an unscheduled `note`/status update, not tied to a compliance
   window — still stored, still reviewable, but not something whose
   absence counts as a missed check-in.
4. Insert `proof_submissions` row, `status='pending'`, and — if step 2
   applied — set `verification_code_value = verification_codes.code`
   on that same row, so the code the submissive was told to display
   is stored directly alongside this proof, not only reachable via
   the `verification_code_id` foreign key (see
   `01-data-model.md` §5 for why this is copied rather than joined).
5. Stream each uploaded file to the private blob directory under a
   fresh UUID filename, compute `sha256` as it streams, insert
   `proof_attachments` rows.
6. If step 2 applied, mark the code `consumed_at=now()`,
   `consumed_by_submission_id=<new id>` in the same DB transaction as
   the submission insert (which also carries the `verification_code_value`
   snapshot from step 4), so a code can never be double-consumed even
   under concurrent requests (SQLite's single-writer semantics plus
   wrapping both writes in one transaction is sufficient here — no
   separate locking scheme needed given the write volume this
   application will see).
7. Missed-window handling: if a scheduled code simply expires with no
   submission, that fact is queryable (`verification_codes` where
   `consumed_at IS NULL AND expires_at < now`) and surfaced on the
   Keyholder's dashboard as a distinct "missed" indicator, separate
   from "reviewed" states, since the submissive never got the chance
   to be marked failed by a Keyholder — this is a scheduling miss,
   not a reviewed compliance failure, and the UI should not conflate
   the two.

## 4. Review: verified / redo / failed

`POST /keyholder/proof-submissions/{id}/review`. This is the same
endpoint for both `purpose='verification'` and
`purpose='punishment_completion'` submissions (`01-data-model.md`
§5) — the branching below is for the ordinary verification case;
§4a covers what's different when reviewing a punishment's completion
proof instead. Single DB transaction:

1. Confirm the submission's `link_id` belongs to the calling
   Keyholder and `status = 'pending'` (reviewing an already-reviewed
   submission is rejected `409` — prevents a double-review race if
   two browser tabs are open).
2. Update `status`, `reviewed_by_user_id`, `reviewed_at`,
   `review_notes`.
3. If `status = 'failed'` and the request body includes a
   `punishment` object: insert an `assignments` row
   (`kind='punishment'`, `triggered_by_submission_id = this
   submission`) in the same transaction, using `template_id` (which
   supplies `effect_kind`/`completion_type`/deadline math/escalation
   automatically, per `01-data-model.md` §6) or the inline
   `title`/`description`/`effect_kind`/`completion_type`/
   `default_deadline_seconds`/`time_extension_seconds` for an ad-hoc
   one-off (and, if the Keyholder also checked "save to catalog," a
   separate, non-transactional follow-up insert into
   `reward_punishment_templates` — saving a reusable template is not
   required for the punishment itself to take effect, so it doesn't
   need the same atomicity guarantee). An ad-hoc `effect_kind`
   `time_extension` punishment applies immediately, same as via the
   plain assignments endpoint.
4. If `status = 'failed'` and no `punishment` object is included, the
   fail stands with no consequence attached — the Keyholder may
   attach one later via the plain assignments endpoint, or may
   deliberately waive one. The API does not force a punishment on
   every fail.
5. If `status = 'redo'`: no assignment is created; the submissive is
   expected to submit again. The follow-up submission is created
   through the normal create endpoint with
   `redo_of_submission_id` set client-side to the original id (server
   validates that submission belongs to the caller and is in `redo`
   status before accepting the link).
6. If `status = 'verified'`: no assignment is created automatically,
   but the Keyholder is free to separately create a `reward`
   assignment (either from this same review screen calling the
   assignments endpoint right after, or later) — rewards are not
   forced into the review transaction the way a same-action
   punishment is, since "verified" is the expected/default outcome
   and doesn't need a mandatory consequence prompt the way "failed"
   does.

## 5. Interaction with safety alerts

A safety alert is orthogonal to this whole flow and never blocked by
it: `POST /submissive/safety-alert` works regardless of whether a
verification is due, pending, or overdue. On the Keyholder side, an
unresolved safety alert is surfaced above the normal review queue in
the dashboard, not merged into it — it is a different kind of
priority (physical wellbeing) from a compliance review (behavioral).

## 6. Why review status doesn't live on the code, only on the submission

`verification_codes.consumed_at` only proves *a* submission happened
before expiry — it says nothing about whether the photo was
acceptable. Keeping `status` on `proof_submissions` (not on the code)
means the code's job stops at "was this on time and not reused," and
the review's job is entirely "was this photo actually correct," which
keeps each table's invariants simple to reason about and test.

## 7. Reviewing a punishment's completion proof

Referenced from §4: when the submission being reviewed has
`purpose='punishment_completion'` (`01-data-model.md` §5) instead of
the default `'verification'`, the same `POST
/keyholder/proof-submissions/{id}/review` transaction does everything
in §4 above (update the submission's own `status`/`reviewed_at`/
`reviewed_via`/etc.) **plus** one extra step, atomically:

1. Load the assignment via `proof_submissions.assignment_id`
   (`01-data-model.md` §6) and confirm it's still in
   `proof_submitted` status (`409` otherwise — same double-review
   protection as §4 step 1, one level up).
2. `status = 'verified'` → the assignment moves to `completed`. This
   *is* the Keyholder's completion confirmation for a
   `proof_required` punishment — there's no separate "mark completed"
   click the way there is for an `acknowledge_only` one, since
   verifying the proof already required the Keyholder to look at it.
3. `status = 'redo'` → the assignment is left as-is (still
   effectively awaiting a valid submission); the submissive submits
   again via `POST /submissive/assignments/{id}/proof`
   (`03-api-design.md` §7), same `redo_of_submission_id` chaining as
   an ordinary verification redo. The assignment's original
   `deadline_at` is not extended automatically by a redo — see
   `08-punishments-and-deadlines.md` for what happens if the clock
   runs out mid-redo, and how a Keyholder who wants to be lenient
   about that should just extend the deadline directly.
4. `status = 'failed'` → the assignment moves to `failed`, and, if
   its `on_failure_template_id` is set, triggers the same escalation
   the deadline sweeper would trigger for an auto-failed punishment
   — one failure path, two possible causes (a rejected proof, or
   time simply running out), fully detailed in
   `08-punishments-and-deadlines.md`.

No separate endpoint exists for this — reusing the verification
review pathway means a Keyholder's "things waiting on me to look at"
queue can show punishment-completion proofs and verification proofs
side by side, filtered by `purpose` when they want to separate them
(`03-api-design.md` §6), rather than needing two different screens
for what is, mechanically, the same action: look at submitted proof,
decide verified/redo/failed.
