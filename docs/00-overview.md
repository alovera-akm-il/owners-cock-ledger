# Owner's Cock Ledger — Architecture Overview

## 1. Purpose

A self-hosted web application used privately by one Keyholder and the
submissive(s) in their care to track chastity device status, collect
time-stamped photo proof, review that proof, and manage a catalog of
rewards/punishments tied to verification outcomes. It is not a public
SaaS product — it is designed to be deployed on a private server or
home network and used by a small, known set of accounts.

This document set describes the system architecture only. No
application code is included here; see `mockups/` for UI mockups
produced from this design.

## 2. Goals

- Track each submissive's chastity cage status over time (locked,
  unlocked, device changes, duration).
- Let the Keyholder define a verification policy (how often proof is
  required) per submissive.
- Generate time-bound, single-use verification codes the submissive
  must include in a proof submission, so a photo can't be replayed or
  pre-staged.
- Accept photo proof uploads (plus arbitrary structured metadata —
  not only images) to the server, persisted with SQLite for
  relational data and the filesystem for binary blobs.
- Let the Keyholder review each submission and mark it `verified`,
  `redo`, or `failed`.
- On `failed`, let the Keyholder attach a punishment from a reusable
  catalog or define a new one on the spot. Symmetrically, let the
  Keyholder grant rewards from a catalog at any time.
- Track not just *whether* a task was carried out but *how*
  (a simple acknowledgement, or actual submitted proof) and *by when*
  — the server enforces task deadlines itself, automatically marking
  a missed one `failed` and applying whatever consequence the
  Keyholder configured for that failure, up to and including a
  punishment (`08-punishments-and-deadlines.md`,
  `11-tasks-and-rewards.md`).
- Track how long a submissive is *actually* locked (measured, from
  lock to unlock) separately from how long they're *supposed* to be
  locked (a Keyholder-set, Keyholder-modifiable countdown target) —
  and let a punishment's consequence be extending that countdown
  directly, not just assigning another task.
- Notify each role, via the browser (Web Push) and always via an
  in-app feed, when something needs their attention — a code is due,
  a proof was reviewed, a deadline is approaching or was missed, a
  safety alert was raised (`09-notifications.md`).
- Give both roles a profile space, and give the Keyholder rollup and
  per-submissive drill-down views.
- Strictly isolate the two roles: a submissive can only ever see and
  act on their own data; a Keyholder can only see and act on
  submissives linked to them.
- Leave explicit room to add "play session" logging later without a
  schema rewrite.

## 3. Non-goals (for this iteration)

- Multi-keyholder collaboration on a single submissive (co-owners) —
  noted as a future extension, not designed in depth here.
- Public internet-facing multi-tenant hosting for strangers — the
  security model assumes a small, trusted user base and a privately
  operated instance.
- Automated (OCR/ML) verification-code reading from the photo itself.
  MVP verification is a human (the Keyholder) visually confirming the
  code appears in the submitted photo — this holds for task
  completion proof too (`08-punishments-and-deadlines.md`), not just
  the original chastity check-in.
- A native mobile app, or any delivery channel beyond Web Push and
  the in-app feed (no email/SMS) — see `09-notifications.md` §6.

## 4. Roles

Two roles exist, `keyholder` and `submissive`. A user is one or the
other; a single login is not both.

- **Keyholder** — the administrative role. Full read/write over every
  submissive linked to them: profile, cage status, verification
  policy, proof review, rewards/punishments catalog and assignment,
  audit log. No visibility into other Keyholders' submissives.
- **Submissive** — self-scoped only. Can manage their own profile
  (within limits), view their own cage-status history, view their
  active verification requirement, submit proof, and view (but not
  set) their own rewards/punishments and outcomes.

Full permission matrix: see `02-roles-and-permissions.md`.

## 5. High-level components

```
                    +-------------------------------------+
                    |            Rust Web Server            |
                    |        (async, single binary)         |
                    |                                        |
   Browser  <------>|  HTTP layer (routing, auth             |
  (Tailwind,         |  middleware, session + bearer-token   |
   jQuery, vanilla   |  auth, CSRF, session mgmt)            |
   JS + service      |                                        |
   worker for push;  |  Application/service layer             |
   server-rendered   |  (per-domain modules: users, links,    |
   pages + JSON      |  chastity/timer, verification,         |
   APIs)             |  rewards/punishments, safety, audit,   |
                    |  api tokens, notifications)             |
                    |                                        |
                    |  Background tasks (Tokio intervals):    |
                    |  verification-code issuance,            |
                    |  task deadline sweeper                   |
                    |                                        |
                    |  Data access layer (rusqlite             |
                    |  over SQLite, migrations)                |
                    |                                        |
                    |  Blob storage (filesystem dir,           |
                    |  outside web root, served only            |
                    |  via authenticated endpoints)             |
                    +-------------------------------------+
                          |                        |
                          v                        v
              SQLite database file        Browser vendor's Web Push
              (WAL mode) + /data/         relay (outbound only, opt-in,
              proofs/<uuid> blobs         end-to-end encrypted payload)
```

Single deployable binary, single SQLite file, single blob directory.
The one external network dependency is opt-in Web Push delivery
(`09-notifications.md`); everything else — including the in-app
notification feed that always works regardless — talks to nothing but
the server's own SQLite file and filesystem. This keeps the trust
boundary small, which matters given the sensitivity of the data
stored; see `05-security-and-privacy.md` §5 for the privacy tradeoff
Web Push introduces and why it's an explicit, opt-in exception rather
than a quiet contradiction of that stance.

## 6. Document index

- `01-data-model.md` — entities, relationships, SQLite schema sketch.
- `02-roles-and-permissions.md` — full authorization matrix and rules.
- `03-api-design.md` — REST API surface, request/response shapes.
- `04-verification-workflow.md` — code generation, proof submission,
  review, and the failed→punishment path in detail.
- `05-security-and-privacy.md` — auth, storage, transport, and
  data-handling stance given the sensitivity of the content.
- `06-future-extensions.md` — play sessions and other planned growth,
  and how the current schema/API leaves room for them.
- `07-tech-stack.md` — concrete crate/tooling choices and project
  layout.
- `08-punishments-and-deadlines.md` — escalation ladders, the
  deadline sweeper, time-extension effects, and the confinement timer.
- `09-notifications.md` — push notification trigger matrix, Web Push
  delivery, and the in-app feed.
