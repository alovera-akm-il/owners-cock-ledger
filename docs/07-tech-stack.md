# Tech Stack & Project Layout

This is a decisions record, not code. Concrete crate/version choices
belong in `Cargo.toml` when implementation starts; this documents the
reasoning so implementation doesn't have to re-derive it.

## 1. Backend

- **Language/runtime**: Rust, async, Tokio (already implied by the
  README and `Cargo.toml`).
- **Web framework**: Axum. Reasons: integrates directly with Tokio
  and `tower` middleware (useful for the auth/session/CSRF layers
  described in `05-security-and-privacy.md`), has first-class
  multipart upload support (needed for proof attachments), and keeps
  routing declarative and easy to map 1:1 to the endpoint table in
  `03-api-design.md`.
- **DB access**: `rusqlite`, synchronous, wrapped in
  `tokio::task::spawn_blocking` at the call sites inside async
  handlers — settled over `sqlx` specifically because `sqlx`'s
  compile-time query checking needs either a live `DATABASE_URL` or a
  committed `.sqlx/` offline cache regenerated (via `cargo sqlx
  prepare`, itself requiring `DATABASE_URL`) whenever a query changes;
  the requirement to decide is "never, on any machine, for any
  build" — no implicit relationship between `cargo build` and a
  database, full stop. This trades away compile-time SQL validation,
  which matters more given the project has no test suite yet
  (`15-implementation-roadmap.md` §2), so query correctness leans more
  on care and (eventually) integration tests than on the compiler.
  SQLite itself is single-writer regardless of driver (WAL mode gives
  concurrent readers, not concurrent writers), so `sqlx`'s async pool
  was never buying real concurrency here — the async-vs-sync framing
  was closer to a wash than the "async is better" default might
  suggest.
- **Migrations**: `rusqlite_migration` — plain numbered `.sql` files
  in `migrations/`, applied in order and tracked in a
  `schema_migrations`-style table, no build-time or `DATABASE_URL`
  dependency of any kind, consistent with the DB-access choice above.
- **Password hashing**: `argon2` crate.
- **API token hashing**: `sha2` crate (SHA-256) — deliberately not
  `argon2` for these; see `05-security-and-privacy.md` §9 for why a
  fast hash is the right (and faster, at auth time) choice for an
  already-high-entropy CSPRNG token versus a human-chosen password.
  Recovery codes (`10-operations.md` §2) hash the same way, for the
  same reason.
- **TOTP**: the `totp-rs` crate (RFC 6238) for generating/verifying
  codes and building the `otpauth://` provisioning URI; QR rendering
  is client-side (any small JS QR library, or a server-rendered PNG
  via the `qrcode` crate if avoiding a frontend dependency is
  preferred) — the server never needs to render an image, only hand
  back the URI/secret.
- **Session storage**: `sessions` (`01-data-model.md` §2, security
  rationale in `05-security-and-privacy.md` §2) plus a small
  middleware layer resolving the session cookie to `(user_id, role)`
  on every request; no external session store needed at this scale.
  `GET/DELETE /auth/sessions` (`10-operations.md` §1) are thin reads/
  writes against this same table.
- **Bearer token auth**: a second `tower` auth layer, tried when a
  request carries `Authorization: Bearer …` instead of a session
  cookie, resolving to `(keyholder_id, scopes)` against `api_tokens`
  (`01-data-model.md` §9) the same way the session layer resolves
  `(user_id, role)`. Downstream handlers see one unified
  "authenticated as X, permitted to Y" context regardless of which
  layer produced it, so route logic doesn't need to know or care
  which auth method was used — only `reviewed_via`/`assigned_via`
  (`03-api-design.md` §§6–7) and the audit log record that
  distinction.
- **CSRF**: `tower`-layer double-submit-cookie check, or an
  equivalent minimal implementation — no need for a heavyweight
  dependency for this. Applies only to the session/cookie auth layer;
  bearer-token requests are exempt (`05-security-and-privacy.md` §8).
- **Background scheduling**: two Tokio `interval` tasks spawned at
  server startup, running in-process — no external job queue.
  1. Verification code issuance (`04-verification-workflow.md` §2).
  2. The punishment deadline sweeper (`08-punishments-and-deadlines.md`
     §3) — auto-fail and deadline-approaching passes. Structurally
     identical to #1 (scan-and-act on a timer), so both can share one
     small internal "scheduled task" helper rather than being two
     independently-written loops. Both upsert a heartbeat row into
     `background_task_runs` (`01-data-model.md` §11) on every tick, so
     `GET /health` (`10-operations.md` §2) can tell a stalled task from
     a healthy one instead of assuming silence means everything's fine.
- **Backup**: a CLI subcommand on the same binary
  (`owners-cock-ledger backup --out <dir>`), not a background task or
  HTTP endpoint — `10-operations.md` §4 has the full reasoning
  (deployer controls timing via cron/systemd, not the app). Uses
  SQLite's backup API for an online-safe DB copy plus a filesystem
  copy of the blob directory.
- **Idempotency-key storage**: `idempotency_keys` (`01-data-model.md`
  §11) — a plain table lookup keyed on `(user_id, endpoint, key)`
  before executing the handful of endpoints listed in
  `03-api-design.md`'s conventions section, no separate cache layer
  needed at this request volume.
- **Image processing** (EXIF stripping, re-encode/validate on
  upload): `image` crate, or a minimal EXIF-stripping crate if full
  re-encoding is judged unnecessary overhead — decide during
  implementation based on measured cost, not upfront here.
- **Voice proof attachments**: no new crate needed for storage or
  playback — a voice recording is stored and streamed exactly like a
  video attachment (opaque file in the blob directory, browser
  `<audio>` playback), just with a smaller size cap. The one addition
  is validating the uploaded container/codec (e.g. accept
  `audio/webm`/`audio/mp4`, reject anything else) at the same
  multipart-handling layer that already validates image/video
  content-type, not a separate subsystem.
- **Real-time live check-ins**: Axum's built-in SSE support
  (`axum::response::sse`), no separate crate. A small in-process
  broadcast channel (`tokio::sync::broadcast`) per open live play
  session is enough to fan an update out to the (at most two)
  connections subscribed to it — no external pub/sub needed at this
  scale, consistent with the rest of this stack's "in-process,
  single-instance" posture (`10-operations.md`). See
  `13-checkins.md` §5 for the scoping (in-progress play sessions
  only).
- **Web Push**: the `web-push` crate (handles RFC 8291 payload
  encryption and RFC 8292 VAPID JWT signing) rather than
  hand-rolling either — this is exactly the kind of narrow
  cryptographic protocol implementation not worth re-deriving.
  VAPID keypair generated once at first deploy and stored as a server
  secret (`05-security-and-privacy.md` §10), not regenerated per
  restart (that would invalidate every existing subscription's
  sender verification).
- **Outbound email (password reset)**: the `lettre` crate, async
  transport (`AsyncSmtpTransport` over Tokio) speaking plain SMTP AUTH
  over implicit TLS to whatever relay the deployer configures — no
  provider-specific SDK, since any standard mailbox with app-password
  support (Fastmail, and most others) works identically through this
  one path. Configured entirely via environment variables, all
  optional as a group — unset any of them and outbound email is
  simply off, per `05-security-and-privacy.md` §11:
  - `SMTP_RELAY_HOST` / `SMTP_RELAY_PORT` — e.g. `smtp.fastmail.com` / `465`.
  - `SMTP_USERNAME` / `SMTP_APP_PASSWORD` — the relay account's login
    and app-scoped password, handled as a secret like the VAPID key.
  - `SMTP_FROM_ADDRESS` — defaults to `SMTP_USERNAME` if unset; lets a
    deployer send from a subaddress or alias instead of the bare
    mailbox address.
  - `PUBLIC_BASE_URL` — the server's own externally-reachable URL,
    needed to build a clickable reset link in the email body. Nothing
    prior to this needed the app to know its own public address
    (invite tokens are handed over directly, not linked), so this is
    the first config surface of its kind.
  Sending itself runs inside a `tokio::spawn`ed task off the request
  path, never awaited before responding — see
  `05-security-and-privacy.md` §11 for why that's a security property
  here, not just a performance one.

## 2. Data layer

- **Database**: SQLite, single file, WAL journal mode, `foreign_keys`
  pragma on.
- **Location**: `~/.config/<app-name>/` (e.g.
  `~/.config/owners-cock-ledger/`) is the default root for everything
  persistent — the SQLite file, its `-wal`/`-shm` companions, and the
  blob directory all live under this one path, not scattered across
  XDG's usual config/data/cache split. One directory to know about,
  secure (`chmod 700`), and point a backup at. Overridable via an env
  var (e.g. `DATA_DIR`) for deployments that want a different
  location (a mounted volume in a container, say); resolved via the
  `directories` crate rather than hand-rolled `$HOME` string-building,
  so it degrades sensibly if `$HOME` isn't set the way a systemd
  service account's environment might need `Environment=HOME=...`
  configured explicitly for this to resolve at all.
- **Blob storage**: plain filesystem directory,
  `<data-dir>/blobs/<uuid>.<ext>`, outside any statically-served path.
  Named generically (not `proofs/`) since it already needs to hold
  more than proof photos — voice recordings today, toy photos if
  `12-toy-catalog.md` §5's gap ever gets built.
- **Migrations**: `rusqlite_migration` (§1) — numbered `.sql` files in
  `migrations/`, one per table/change, matching the table list in
  `01-data-model.md`, applied against `<data-dir>/`'s database file on
  startup.

## 3. Frontend

Per the request: Tailwind CSS, jQuery, vanilla JS — server-rendered
HTML pages (Axum rendering `askama` templates, compiled in at build
time — settled over `tera` for the same compile-time-safety-net
reasoning as the `rusqlite`/`sqlx` call in §1, and because this app's
single-operator, redeploy-to-change-anything deployment model never
needed `tera`'s runtime-editable-templates advantage in the first
place) progressively enhanced with jQuery for API calls (AJAX
submissions, dynamic status updates, review-queue interactions)
rather than a full client-side SPA framework. This fits an app of
this size: a handful of page types (dashboard, submissive detail,
review queue, profile, catalogs, submit-proof form) each mostly
server-rendered, with jQuery handling the interactive bits (file
upload with progress, review action buttons calling `POST
.../review` without a full page reload, polling
`verification-codes/current`).

Every frontend asset is vendored and served by the app itself — no
CDN dependency anywhere, full stop, not just for Tailwind/jQuery:

- **Tailwind**: compiled at build time (CLI or a bundler step) into a
  static CSS file the server serves — no runtime CDN dependency, both
  for privacy (no third-party requests from a page displaying this
  content) and reliability offline/self-hosted.
- **jQuery**: vendored locally (served from the app itself), same
  reasoning — no CDN dependency for a privacy-sensitive app.
- **Fonts**: the Inter typeface (SIL Open Font License) is downloaded
  once and its `.woff2` files committed under `static/fonts/`,
  referenced via a local `@font-face` rule compiled into the Tailwind
  output — not a Google Fonts `<link>` tag. This is the one asset
  that's easy to miss (Tailwind and jQuery are the visible framework
  choices; a font `<link>` is easy to leave as the default CDN
  snippet without thinking of it as the same category of dependency),
  so it's called out explicitly here rather than assumed covered by
  the bullets above.
- **Service worker** (plain vanilla JS, no framework): required for
  Web Push (`09-notifications.md`) — registers on first load, handles
  `push` events by showing the OS notification and `notificationclick`
  by deep-linking into `link_path`, and is the one piece of this
  frontend that can't be jQuery-driven page logic, since it runs in
  its own worker context independent of any open tab. Everything else
  about push (requesting permission, calling `PushManager.subscribe()`,
  POSTing the subscription) is ordinary jQuery/vanilla JS in the
  regular page scripts.
- Two view "surfaces" driven by role, matching
  `02-roles-and-permissions.md`: a Keyholder-facing set of pages
  (roster, submissive detail/review, catalogs, audit log) and a
  submissive-facing set (own status, submit-proof, own history,
  own assignments) — enforced server-side by the same auth/role
  checks as the API, not just hidden nav links.
- **Motion**: functional, not decorative — every animation exists to
  communicate a state change, direct attention, or confirm an action
  landed, never as flourish. Three patterns, reused everywhere rather
  than invented per page (`mockups/app.css` is the reference
  implementation the real frontend's stylesheet should port
  directly):
  - **Flash on change** — a value that just updated (a new ledger
    row, a status badge moving to a new state after an action) gets a
    brief background highlight, not a silent snap. The default for
    "something changed as a direct result of what you just did."
  - **Urgent pulse** — reserved specifically for something needing a
    human's attention *right now*: an unacknowledged safety alert, a
    live/recording indicator. Deliberately rare, so it keeps that
    meaning — never used for routine "here's something new," only for
    the handful of genuinely urgent or live signals in the whole app.
  - **Panel fade** — swapping which panel is visible (e.g. a play
    session's state-dependent action panel) gets a short cross-fade
    instead of an instant `hidden`-class snap, so a state transition
    reads as a change rather than a glitch.
  All three collapse to nothing under `prefers-reduced-motion:
  reduce` — a real accessibility consideration, not an afterthought.
  Explicitly **not** done anywhere: entrance animation on page load,
  hover bounce/scale micro-interactions, or any animation that exists
  purely for visual flair — consistent with this app's restrained,
  utilitarian tone rather than a consumer-app feel. Ordinary
  hover/focus color transitions (Tailwind's `transition-colors`) are
  the one exception, already the baseline on every interactive
  element and not considered part of this "motion" category — they're
  standard affordance, not a communicated event.

## 4. Project layout (indicative, for when implementation starts)

```
src/
  main.rs              — startup, router assembly, background tasks
  db/                   — connection pool, migrations
  auth/                 — session middleware, password hashing, CSRF
  domain/
    users/
    links/               (keyholder_submissive_links)
    chastity/             (devices, confinement_sessions, timer adjustments)
    verification/         (policies, codes, background issuance task)
    proofs/               (submissions, attachments, review)
    rewards_punishments/  (templates, assignments, deadline sweeper, escalation, points ledger)
    toys/                  (catalog CRUD, retirement request/approve)
    checkins/              (templates, fields, instances, SSE fan-out for live sessions)
    play_sessions/         (templates, instances, toy attachment, check-in scheduling, judgement)
    safety/
    audit/
    api_tokens/
    notifications/        (push_subscriptions, notifications, Web Push dispatch)
    stats/                 (read-only aggregation queries, §15 in 03-api-design.md)
  api/                   — route handlers per domain, thin over domain/ services
  ops/                   — health check, background_task_runs heartbeat helper, backup subcommand
  web/                   — server-rendered page handlers/templates
  storage/               — blob directory read/write, streaming, EXIF stripping
migrations/
templates/               — askama templates
static/
  css/                   — compiled Tailwind output
  js/                    — vendored jQuery + app JS
  fonts/                 — vendored Inter .woff2 files
  sw.js                  — service worker (push notifications only)
docs/                    — this document set
mockups/                 — static HTML mockups (see mockups/README)
```

No `data/` directory in the repo tree — the SQLite file and blob
directory live under `~/.config/<app-name>/` (§2), outside the
project directory entirely, the same way any other Linux daemon's
runtime state isn't checked out alongside its source.

This mirrors the domain boundaries used throughout `01-data-model.md`
through `10-operations.md`, so each doc section maps to one
`domain/` module plus its `api/` handlers.
