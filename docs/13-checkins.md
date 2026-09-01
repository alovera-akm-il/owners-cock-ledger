# Check-ins

Schema reference: `01-data-model.md` §14 (`checkin_templates`,
`checkin_template_fields`, `checkins`). This document covers the
color system, the configurable field-type system, how a check-in
attaches to a task or a play session, and the real-time requirement
for live sessions.

## 1. Why color is schema-level, not just another configurable field

Every check-in, regardless of which template produced it, carries a
mandatory `color` value (`green`=SAFE/OK, `yellow`=Near Limit,
`red`=immediate stop). This is deliberately **not** implemented as
just another entry in `checkin_template_fields` — it's a fixed column
on `checkins` itself, for two reasons:

- **Consistency across every template.** If color were configurable
  per template, a Keyholder could accidentally omit it, or word its
  options differently on different templates, undermining the one
  thing that's supposed to mean the same thing everywhere: "red always
  means stop, on every check-in, no exceptions." A submissive glancing
  at any check-in should never have to first figure out what that
  template's color options mean.
- **It's the one field the UI and notification system can always rely
  on existing**, regardless of what a given Keyholder configured. A
  RED check-in can trigger a consistent visual treatment (and a push
  notification, `09-notifications.md`) across the entire app, not just
  on templates that happened to include a color-like field.

Custom fields (§2) are for everything template-specific *beyond* that
universal signal.

## 2. Configurable field types

`checkin_template_fields` lets a Keyholder build a check-in template
field-by-field, each with a `field_type` and a JSON `config` shaping
how it's presented and validated:

| `field_type` | `config` shape | notes |
|---|---|---|
| `scale` | `{"min":0,"max":5,"min_label":"...","max_label":"..."}` | a numeric slider. `min_label`/`max_label` are the key piece the request specifically called for: the Keyholder decides whether `0` means good or bad per field — e.g. a "Comfort level" scale could be set up as `min_label:"very comfortable"` or `min_label:"in pain"` depending on how that Keyholder thinks about the number, and the UI always renders whatever labels are configured rather than assuming a direction |
| `select` | `{"options":["a","b"]}` or `{"source":"devices"}` | either a static option list, or a dynamically sourced one — `source:"devices"` populates the options from the submissive's actual `chastity_devices` at fill-in time, which is what the example "Device: which cage, ring size, configuration" field needs without hardcoding device names into the template |
| `number` | `{"unit":"hours"}` | free numeric entry with an optional display unit — used for things like "Duration: hours locked" |
| `text` | `{}` | free text — "Incidents," "Sleep quality" |
| `boolean` | `{}` | yes/no toggle |

`required` on each field controls whether the check-in can be
submitted without it — a template can mix required and optional
fields (e.g. color and skin status required, incidents optional).

### `label` vs. `description`

Every field has a short `label` (the prompt itself — "Cage comfort")
and an optional longer `description` shown underneath it to whoever's
filling the field in. This is a deliberate two-tier split, not
redundant with the template-level `checkin_templates.description`
(which describes what the *whole template* is for, e.g. "Paired with
the overnight cage photo-proof task") — a field's `description` is
scoped to that one field, and exists because a label alone often
can't carry enough context to answer consistently:

- A bare "Cage comfort" label with a 1–5 scale doesn't say what a 3
  actually means to the person answering it. `description: "1 =
  barely feel it during normal movement, 5 = actively painful — tell
  your Keyholder immediately if you're near this end"` does.
- A submissive filling this in at 6am, half-asleep, benefits from not
  having to remember or guess what a field was originally meant to
  capture — the description is right there every time, not something
  explained once when the template was set up and then forgotten.

`description` is optional per field — a self-explanatory field like
"Duration (hours)" doesn't need one, while anything with a scale, a
judgment call, or safety implications usually should.

### Worked example: the morning cage check-in from the request

The example given maps directly onto this system:

| field_key | label | description | field_type | config |
|---|---|---|---|---|
| *(built-in)* | Color | — | — | green/yellow/red |
| `skin_status` | Skin status | "Look at the skin under and around the cage, not just how it feels" | `select` | `{"options":["normal","mild redness","chafing","swelling","open skin"]}` |
| `cage_comfort` | Cage comfort | "1 = barely feel it, 5 = painful — anything above 3 should probably also be RED" | `scale` | `{"min":1,"max":5,"min_label":"barely feel it","max_label":"painful"}` |
| `incidents` | Incidents | "Any pressure points, nocturnal erections, or discomfort — even minor" | `text` | `{}` |
| `sleep_quality` | Sleep quality | — | `text` | `{}` |
| `device` | Device | — | `select` | `{"source":"devices"}` |
| `duration` | Duration | — | `number` | `{"unit":"hours"}` |

This template's `related_confinement_session_id` links it to the
overnight cage-wearing period it's checking in on, and it's the
check-in a photo-proof overnight task
(`11-tasks-and-rewards.md`) would require alongside its own proof
attachment — the task's proof photo and this template's structured
answers are two separate but co-required pieces of one morning
routine.

## 3. Additional proposed parameters

Beyond color, arousal level, and the comfort slider named in the
request, worth including as commonly useful built-in *suggestions*
(not mandatory fields — a Keyholder picks which ones a given template
actually uses):

- **Mood/headspace** (`select` or `text`) — useful context alongside
  a physical comfort reading, especially for longer confinement or
  post-scene check-ins.
- **Pain location** (`text`, only relevant when color is yellow/red)
  — where, specifically, distinct from the general "incidents" field.
- **Hydration/last meal** (`text` or `boolean`) — relevant for
  longer sessions or intense play, a basic physical-wellbeing signal.
- **Safeword invoked** (`boolean`) — distinct from color: a
  submissive could be at "yellow" comfort without having used the
  safeword, or could want to flag the safeword specifically as its
  own explicit signal rather than relying on red always being read
  the same way.
- **Free-form notes** — every template should probably default to
  including one plain `text` field for anything that doesn't fit the
  structured fields, mirroring the "structured fields plus a notes
  catch-all" pattern already used for proof submissions
  (`01-data-model.md` §5).

None of these need new `field_type` values — they're all expressible
with the five types already defined, which is the point of a
configurable field system: new *use cases* don't need new schema.

## 4. Attachment points: task, confinement session, or play session

A `checkins` row can relate to exactly one of three things (all
nullable FKs, at most one populated per row in practice, though the
schema doesn't hard-enforce mutual exclusivity since nothing requires
it to):

- `related_confinement_session_id` — a periodic check-in tied to an
  ongoing confinement period (the morning cage example).
- `related_assignment_id` — a check-in required alongside a task's
  proof submission (also the morning cage example, from the task
  side).
- `related_play_session_id` — a mid-session check-in during a play
  session (the Estim example), see `14-play-sessions.md` §3.

A check-in with none of these set is a standalone, on-demand
check-in either role can log at any time — useful for anything not
tied to a specific tracked activity.

## 5. Real-time updates for live sessions

The one case that needs genuinely real-time behavior: a check-in tied
to an **in-progress** play session (`related_play_session_id` set,
session `status='in_progress'`). The Estim example is exactly this —
either party can be the one typing, and the other should see the
update land immediately, not on next page refresh.

### Why this needs something beyond ordinary REST

Every other write in this app is fire-and-forget from the writer's
perspective — the other party finds out via the notification feed
(`09-notifications.md`) or by loading a page. That's the right model
for "Keyholder assigns a task" (not time-sensitive to the second) but
wrong for "we are both looking at the same check-in right now during
a session" — polling would either be too slow (a multi-second delay
feels broken mid-session) or wasteful (fast polling from every open
session view, most of which see no changes most of the time).

### Design: Server-Sent Events, scoped narrowly

Axum has native SSE support, so no new infrastructure dependency is
needed (`07-tech-stack.md`). The design:

- Writes still go through the ordinary REST endpoint
  (`PATCH /api/v1/checkins/{id}`, `03-api-design.md`) — there is no
  parallel write path. SSE is read-only fan-out.
- A client viewing an in-progress play session's check-in opens
  `GET /api/v1/play-sessions/{id}/checkin-stream` (or similar), an SSE
  connection scoped to that one session, authenticated the same way
  every other request is (session cookie or API token,
  `05-security-and-privacy.md`).
- On every successful `PATCH` to a check-in whose
  `related_play_session_id` matches an open stream, the server
  pushes the updated check-in as an SSE event to every connection
  subscribed to that session — in practice just the Keyholder's and
  the submissive's own views of it, since only two people are ever on
  one link.
- The connection closes (or the client stops trying to reconnect)
  once the session's `status` leaves `in_progress` — there's nothing
  to watch live once a session ends.

This is deliberately narrow: standalone check-ins, task-attached
check-ins, and completed-session history are all ordinary
create-once-rarely-edited REST resources with no SSE involved. Only
"someone is watching a check-in happen live" gets the real-time
treatment, and it's a distinct mechanism from Web Push — Web Push is
for async/background alerts to a device that may not have the app
open; SSE here is for two people already looking at the same page at
the same time.

## 6. Auto-escalation to a safety alert (per-template opt-in)

`checkin_templates.auto_escalate_on_red` (`01-data-model.md` §14,
default off) resolves a question this document originally left as a
deliberate non-feature: whether a RED check-in should ever
auto-create a `safety_alerts` row, or only ever produce a strong
notification.

Both sides of that original argument were right about different
templates, which is why the resolution is a per-template toggle
rather than one system-wide rule:

- `color='red'` is *defined* to mean "immediate stop" (§1) — for a
  template where that's genuinely what a RED answer signals (a
  mid-session Estim check-in, an overnight cage check where "open
  skin" is marked), treating it as equivalent to the submissive
  hitting the safety-alert button isn't presumptuous, it's honoring
  what the color was defined to mean.
- For a looser template — one where a Keyholder set up color
  thresholds more casually, or where RED on one specific field isn't
  actually an emergency-level signal — auto-firing the full
  safety-alert workflow every time would over-trigger relative to
  what was actually meant.

Putting the decision on the template's author, at authoring time,
means the Keyholder who best knows what a *specific* template's RED
threshold is supposed to signal is the one who decides whether it
gets the heavier treatment — not a single global assumption applied
identically to every template regardless of content.

### Mechanics

When a `checkins` row is created or updated with `color='red'` and its
template has `auto_escalate_on_red=true`, the same transaction that
writes the check-in also inserts a `safety_alerts` row
(`raised_via='system'`, `related_checkin_id` set, `message`
system-generated — e.g. "Auto-raised: RED on 'Overnight cage
check-in'"). This reuses the existing safety-alert table and Keyholder
workflow (`04-verification-workflow.md` §5) rather than inventing a
parallel escalation path — from the Keyholder's dashboard, an
auto-raised alert looks and behaves exactly like a manually-raised
one (surfaced above the normal review queue, acknowledge/resolve the
same way), with `raised_via` as the one visible marker distinguishing
"a person hit the button" from "the system did, because this
template says RED means that."

**Once per transition, not once per save**: the trigger condition is
the check-in's color *changing into* `red` (a fresh check-in created
directly as red, or an existing one updated from non-red to red) —
not simply "is currently red." A live-session check-in that's already
red and gets a follow-up edit (e.g. adding a note) doesn't raise a
second alert for the same ongoing red state; re-triggering requires
the color to leave red and come back, the same "meaningful state
change, not every write" principle already applied to deduping
reminder notifications elsewhere (`08-punishments-and-deadlines.md`
§3's deadline-approaching pass).

## 7. Gaps considered

- **Presence indicators** ("Keyholder is currently viewing") — a
  natural SSE extension, not built now; adds complexity for a nicety,
  not a safety-relevant signal.
