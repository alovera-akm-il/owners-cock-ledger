# Play Sessions

Schema reference: `01-data-model.md` §15 (`play_session_templates`,
`play_sessions`, `play_session_toys`, `play_session_checkin_schedule`).
This document covers live vs. retrospective logging, session
templates, toy attachment, mid-session check-in scheduling, the
judgement-before-completion workflow, and what more could be built on
top of this.

## 1. Templates vs. instances

Same pattern as everywhere else reusable-definition-vs-per-use
appears in this schema (`reward_punishment_templates` →
`assignments`, `checkin_templates` → `checkins`):
`play_session_templates` is Keyholder-authored and reusable across
every submissive they oversee; `play_sessions` is one actual instance,
optionally created from a template (`template_id`) or fully ad-hoc.

A template holds: `title`, `setup_notes` (prep/read-before-starting
instructions), `suggested_toy_categories` (informational only — see
§2 for why this can't reference actual toys), `planned_duration_seconds`,
and an optional `checkin_template_id` + `checkin_interval_seconds` for
generating a mid-session check-in schedule automatically when a
session is created from this template.

At assignment/creation time, the instance copies the template's title
and setup notes (the established "copy at write time" pattern — a
later template edit shouldn't rewrite a session that already
happened), and the Keyholder fills in the parts that only make sense
per-instance: actual toys from the actual submissive's catalog,
actual start/end time, and can adjust the copied duration/check-in
interval for this specific instance if needed.

## 2. Why templates can't reference actual toys

`toys` is per-submissive (`12-toy-catalog.md` §1), but a
`play_session_template` is meant to be reused across every submissive
a Keyholder oversees. If a template referenced specific `toys` rows,
it would only ever be usable for the one submissive whose toys those
are — defeating the point of it being a template. `suggested_toy_categories`
(a plain JSON array of category strings, e.g. `["vibrator","cock cage"]`)
keeps the template submissive-agnostic while still communicating
intent; the actual toy gets picked from the real catalog via
`play_session_toys` at the point an instance is created or started,
when there's a concrete submissive (and concrete inventory) to pick
from. A toy's `category` field (`12-toy-catalog.md` §2) is exactly
what lets the UI suggest matching toys from the submissive's own
catalog against a template's suggested categories.

## 3. Live vs. pre-done logging

`play_sessions.status` covers both cases with one state machine:

```
scheduled → in_progress → pending_judgement → completed
                ↓                                 ↑
            cancelled                    (retrospective entry
                                           can jump straight to
                                           pending_judgement)
```

- **Live**: a session is assigned or created as `scheduled`, someone
  (either role can start it — see `02-roles-and-permissions.md`) moves
  it to `in_progress` (setting `started_at`), mid-session check-ins
  happen in real time against the check-in schedule
  (`13-checkins.md` §5 for the SSE mechanism), and ending it
  (`ended_at` set) moves it to `pending_judgement`.
- **Pre-done / retrospective**: a session that already happened
  off-app can be logged directly — created with `started_at`,
  `ended_at`, and any check-ins entered as already-filled-in
  historical records (no live SSE fan-out needed, since nothing is
  "happening" during data entry), landing straight in
  `pending_judgement` without ever passing through `in_progress`
  from this app's point of view.

Either path converges on the same `pending_judgement` state, so the
judgement step (§4) doesn't need to know or care which path a session
took to get there.

## 4. Toys, duration, and mid-session check-in scheduling

At creation (from a template or ad-hoc), the Keyholder sets:

- **Toys used** — `play_session_toys` rows, each referencing a real
  `toys` row belonging to the session's submissive (enforced at the
  service layer, not just implied by convention).
- **Duration / start–end time** — `planned_duration_seconds` up
  front, `started_at`/`ended_at` filled in as the session actually
  runs (or, for retrospective entry, both set at creation time to
  whatever actually happened).
- **Number and interval of mid-session check-ins** — if a
  `checkin_template_id` and `checkin_interval_seconds` are set,
  `play_session_checkin_schedule` rows are generated up front
  (`sequence_number`, `planned_offset_seconds` from `started_at`) —
  e.g. a 60-minute session with a 20-minute interval generates three
  scheduled slots. Each slot's `fulfilled_checkin_id` stays `NULL`
  until someone actually fills in that check-in, so the UI can show
  "2 of 3 check-ins done" and flag an overdue one distinctly from one
  that hasn't come up yet.

Scheduling the slots ahead of time (rather than just letting either
party log ad-hoc check-ins during a session) is what makes "you're
due for a check-in" a checkable, notifiable condition
(`09-notifications.md`) instead of something that only exists if
someone remembers to do it.

## 5. Judgement before completion

A session in `pending_judgement` isn't done yet — the Keyholder still
has to make a call. This is the concrete workflow requested: "the
keyholder gets to make judgements, assign rewards or punishments...
before marking a play-session done."

The judgement step:

- The Keyholder reviews the session (notes, toys used, check-in
  history — especially any yellow/red check-ins logged during it) and
  writes `judgement_notes`.
- Optionally assigns a reward and/or a punishment. This **reuses**
  the existing `assignments`/`reward_punishment_templates` machinery
  rather than inventing a session-specific consequence system — the
  same templates a Keyholder already maintains for tasks and general
  conduct apply here too. The created `assignments` row's
  `triggered_by_play_session_id` points back at this session (the
  dedicated-column choice explained in `01-data-model.md` §1's ERD
  notes and `06-future-extensions.md` §1), and the session's own
  `reward_assignment_id`/`punishment_assignment_id` point forward at
  whichever were created, so the link is navigable from either side.
- Moving the session to `completed` is a separate, explicit action
  from creating the judgement — a Keyholder can write judgement notes
  and come back later to actually finalize it, though in the common
  case these likely happen in one step.

A session can also be `cancelled` from `scheduled` or `in_progress`
(started but aborted) — no judgement applies, and no reward/punishment
is expected for a cancelled session.

## 6. What more can be done

Beyond what was explicitly requested, considered and either included
above or explicitly deferred:

**Included in this design:**

- Reusable templates with pre-configured check-in scheduling (§1, §4).
- Both live and retrospective logging converging on one state machine
  (§3).
- Toy catalog integration via category-suggestion-at-template,
  concrete-selection-at-instance (§2).
- Judgement reusing the existing reward/punishment system rather than
  a parallel one (§5).

**Considered and deliberately deferred** (candidates for
`06-future-extensions.md` if wanted later):

- **Aftercare follow-up check-ins** — auto-scheduling a check-in some
  hours after a session ends, to catch delayed physical/emotional
  fallout. Not built because the right default delay is a product
  decision, not a schema one; a Keyholder can already do this
  manually today with an ad-hoc check-in.
- **Session statistics / history views** (frequency, average
  duration, most-used toys, judgement outcomes over time) — a
  reporting concern layered on top of data this design already
  captures, not a schema gap; worth building once there's enough
  session history to make it useful.
- **Automatic hard-limit cross-checking** against a session's toys or
  notes — blocked on hard/soft limits still being free text
  (`06-future-extensions.md` §9), same blocker as the toy catalog's
  version of this idea.
- **Recurring/scheduled sessions** (e.g. "every Tuesday") — a
  scheduling concern distinct from templates (which are reusable
  *shapes*, not reusable *calendar entries*); not requested and not
  built, but the template/instance split here would support it
  cleanly later (a recurrence rule that spawns `play_sessions` rows
  from a template on a cadence).
- **Multi-submissive / group sessions** — out of scope; this schema
  is pairwise (`link_id`-scoped) throughout, same boundary noted for
  points (`11-tasks-and-rewards.md` §4) and consistent with the
  single-active-link model (`06-future-extensions.md` §3).
