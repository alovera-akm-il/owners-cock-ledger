# Implementation Roadmap

`00`–`14` specify *what* to build. This document is the companion that
says *what state the design is actually in* and *in what order to
build it* — an honest evaluation plus a phased build order, written
once the doc set had grown large enough that "just start coding"
stopped being a real plan.

## 1. Current state

Everything real that exists as of this document is the design: `docs/`
(15 files, ~5,000 lines) and `mockups/` (26 interactive HTML/JS pages).
The actual project scaffold is `Cargo.toml` with no dependencies and
`src/main.rs` printing `Hello, world!`. This is a 100% pre-implementation
codebase — the roadmap below starts from that literal baseline, not
from an assumed head start.

## 2. Evaluation

### Strengths

- **Patterns repeat instead of each domain inventing its own shape.**
  Template→instance with copy-at-write-time (rewards/punishments/
  tasks, check-ins, play sessions), soft-delete via `retired_at`, an
  append-only ledger next to a cached balance (points), and `*_via`
  columns distinguishing human/automated/system actions all recur
  across `01-data-model.md`. An implementer who internalizes four or
  five patterns can predict the shape of a domain section they
  haven't read yet, which lowers the real cost of the doc set's size.
- **The mockups are doing real spec work, not just UI decoration.**
  The play-session scheduling gap and the missing `cancelled` state
  (found and fixed this session) were logic gaps in the *design*,
  surfaced only because the mockups were built interactively enough
  to expose them before a line of Rust existed — cost-avoidance most
  projects don't get until integration testing, achieved here for
  free by treating the mockups as executable spec.
- **Security and safety are load-bearing from the start.** EXIF
  stripping, the field-encryption candidate list, the safety-alert
  path that bypasses every other gate, the deliberate refusal to
  auto-escalate a RED check-in without an explicit per-template
  opt-in (`13-checkins.md` §6) — these are the calls that are
  expensive to walk back once real users and real history exist, and
  they're already decided rather than deferred.
- **The tech stack is sized to the actual problem.** SQLite, single
  binary, in-process background tasks, no queue/cache/microservices
  (`07-tech-stack.md`). For one Keyholder and a handful of
  submissives, that's correct restraint, not under-engineering.

### Risks and gaps

- **No testing strategy exists anywhere in the doc set.** Not a
  philosophy, not a coverage target, nothing. For a system whose core
  mechanism is a background sweeper that unilaterally extends
  someone's confinement time on a deadline miss
  (`08-punishments-and-deadlines.md`), this is the single biggest gap
  between "well-designed" and "safe to actually run."
- **No CI exists beyond publishing the mockups to Pages**
  (`.github/workflows/pages.yml`). Nothing currently stops a
  regression from landing once code starts flowing.
- **Scope has outgrown a first release.** The doc set covers 15
  domains with no marked v1 cut line — `06-future-extensions.md`
  tracks what's *deferred*, but everything currently designed reads
  as equally "ready to build." Tasks/verification/confinement are the
  stated original purpose (`README.md`); points, the toy catalog,
  check-ins, and play sessions are substantial elaborations layered
  on in one later session. Treating all 15 as one build is a real
  risk of shipping nothing before the scope stops moving.
- **SQLite + single-process is a real ceiling**, correctly accepted
  for the stated scale (`07-tech-stack.md` §2) — worth remembering
  only if the scope ever grows past one deployer and a handful of
  submissives.

## 3. Phase 0 foundation decisions — resolved

Originally two open spikes; both are now settled, plus two related
constraints that came up while resolving them and apply just as much
to Phase 0. All four in `07-tech-stack.md`, summarized here so the
reasoning doesn't have to be re-derived:

1. **`rusqlite` over `sqlx`** for the DB layer. `sqlx`'s compile-time
   query checking is real value, but it requires either a live
   `DATABASE_URL` or a committed offline cache regenerated (via a
   command that itself needs `DATABASE_URL`) whenever a query
   changes — some implicit relationship between `cargo build` and a
   database, on some machine, at some point. The bar here is zero
   such relationship, ever, including local dev — `rusqlite` gives
   that unconditionally. Trade-off accepted explicitly: with no test
   suite yet (§2 above), query correctness now leans more on care and
   eventual integration tests than on the compiler.
2. **`askama` over `tera`** for templates, same underlying logic —
   compile-time-checked, and this app's single-operator,
   redeploy-to-change-anything model never needed `tera`'s
   runtime-editable-template advantage.
3. **No CDN dependency anywhere** — not just the Tailwind/jQuery
   vendoring already specified, but fonts too. A Google Fonts
   `<link>` tag is easy to leave as a default without registering it
   as the same category of third-party dependency as a JS framework;
   Inter's `.woff2` files get vendored under `static/fonts/` instead.
4. **All persistent app data under `~/.config/<app-name>/`** — the
   SQLite file, its WAL companions, and the blob directory in one
   place, not a repo-relative `data/` folder and not scattered across
   XDG's config/data/cache split. Resolved via the `directories`
   crate, overridable by env var for deployments that want a
   different location.

None of these four are architecturally significant on their own —
the schema, route shapes, and domain boundaries are identical either
way — but each had to be picked before the first line of `db/` or
`web/` code, so they're grouped here as the actual Phase 0 starting
conditions rather than left as open questions.

## 4. Phased build order

Each phase closes against the corresponding mockup(s) as the
acceptance check — every button in that mockup should have a real
counterpart, the same discipline used in this session's mockup gap
audit.

### Phase 0 — Skeleton (no user-facing feature)

Apply the four foundation decisions above. Project layout per
`07-tech-stack.md` §4. SQLite pool + `rusqlite_migration` tooling
against `~/.config/<app-name>/`. Session-cookie auth (Argon2id,
no 2FA yet) and the role middleware (`02-roles-and-permissions.md`
§1). `audit_log` and `safety_alerts`
(`01-data-model.md` §7/§8) wired in **now**, as cross-cutting
infrastructure every later domain writes to — retrofitting audit
logging after five domains already exist is the expensive way to do
it. `GET /health` (`10-operations.md` §2). CI: `cargo test`/`clippy`/
`fmt` gating every push. A test harness (temp SQLite DB per test)
exists before the first domain does, not after.

### Phase 1 — Identity & relationship

Users, invites, `keyholder_submissive_links`
(`01-data-model.md` §2–3). Login + invite redemption wired to
`mockups/login.html`. Roster page wired to
`mockups/keyholder-dashboard.html` with real data.

### Phase 2 — The original core loop

Chastity devices, confinement sessions/adjustments
(`01-data-model.md` §4), verification policies/codes/proof
submissions (§5, `04-verification-workflow.md`), the review workflow
(`mockups/proof-review.html`). This is the app's stated original
purpose (`README.md`) and should stand alone.

**Recommended v0.1 cut line — ship here** if the goal is something
real running before the rest of the design is built.

### Phase 3 — Tasks, rewards, punishments

`reward_punishment_templates`/`assignments` with the `kind='task'`
unification, the deadline sweeper, success/failure escalation chains
(`01-data-model.md` §6, `08-punishments-and-deadlines.md`,
`11-tasks-and-rewards.md` §1–2). `mockups/catalog.html` and the task
half of `mockups/submit-proof.html`.

### Phase 4 — Automation & self-service ops

API tokens (`01-data-model.md` §9), two-factor authentication, session
self-management, the backup CLI, the `admin` CLI recovery commands
(force password reset, force-disable-2FA, force-unlock, force-end-link
— `10-operations.md` §5), background-task health monitoring
(`10-operations.md`). Lower urgency than it looks — nothing later
structurally depends on this phase, so it's the right place to absorb
schedule slip if one is needed.

### Phase 5 — Notifications

In-app feed first (no new infrastructure), Web Push second (needs
VAPID keys + a service worker — genuinely more moving parts; don't
bundle it with the feed). See `09-notifications.md`.

### Phase 6 — The four subsystems designed this session

In dependency order:

1. **Toy catalog** (`12-toy-catalog.md`) — no dependencies beyond
   Phase 1.
2. **Points** (`11-tasks-and-rewards.md` §3) — depends on Phase 3
   tasks and Phase 2 verification as point sources.
3. **Check-ins** (`13-checkins.md`) — depends on the toy catalog for
   the `source:"devices"` field type, and on tasks/confinement for
   attachment points.
4. **Play sessions** (`14-play-sessions.md`) — depends on the toy
   catalog, check-ins (mid-session schedule), and rewards/punishments
   (judgement).

Reward redemption requests slot into step 2. This is the point to
seriously decide whether all four ship together or points/play
sessions slip to a later release — this is real scope, not a
footnote, and is the biggest lever available for shrinking a first
release without touching the original core loop at all.

### Phase 7 — Real-time

The SSE live-check-in channel (`13-checkins.md` §5), last. The docs
frame this explicitly as delivering existing data faster, not a new
capability, so it should never block anything upstream of it.

## 5. The one decision worth making before Phase 0 starts

Whether all four Phase 6 subsystems belong in a v1, or whether the
honest first release is Phases 0–5 with Phase 6–7 clearly scoped as a
v2. Given the gap between what's designed (15 domains) and what
exists today (a scaffold), the smaller cut is the default recommendation
unless there's a specific reason the full set has to ship together.
