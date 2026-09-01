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
- **DB access**: `sqlx` with the `sqlite` feature, compile-time-
  checked queries, and its migration support for schema evolution
  (each table in `01-data-model.md` becomes one migration).
  `rusqlite` is a reasonable alternative if a more synchronous,
  simpler-to-audit DB layer is preferred over `sqlx`'s async pool —
  worth a quick spike before committing, but not architecturally
  significant either way since the schema and query shapes are the
  same regardless of driver.
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
- **Blob storage**: plain filesystem directory, structure e.g.
  `data/proofs/<uuid>.<ext>`, outside any statically-served path.
- **Migrations**: `sqlx migrate` (or equivalent) — one migration file
  per table/change, matching the table list in `01-data-model.md`.

## 3. Frontend

Per the request: Tailwind CSS, jQuery, vanilla JS — server-rendered
HTML pages (Axum can render templates, e.g. via `askama` or `tera`)
progressively enhanced with jQuery for API calls (AJAX submissions,
dynamic status updates, review-queue interactions) rather than a full
client-side SPA framework. This fits an app of this size: a handful
of page types (dashboard, submissive detail, review queue, profile,
catalogs, submit-proof form) each mostly server-rendered, with jQuery
handling the interactive bits (file upload with progress, review
action buttons calling `POST .../review` without a full page reload,
polling `verification-codes/current`).

- **Tailwind**: compiled at build time (CLI or a bundler step) into a
  static CSS file the server serves — no runtime CDN dependency, both
  for privacy (no third-party requests from a page displaying this
  content) and reliability offline/self-hosted.
- **jQuery**: vendored locally (served from the app itself), same
  reasoning — no CDN dependency for a privacy-sensitive app.
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
templates/               — askama/tera templates
static/
  css/                   — compiled Tailwind output
  js/                    — vendored jQuery + app JS
  sw.js                  — service worker (push notifications only)
docs/                    — this document set
mockups/                 — static HTML mockups (see mockups/README)
data/                    — sqlite file + proofs/ blob dir (gitignored)
```

This mirrors the domain boundaries used throughout `01-data-model.md`
through `10-operations.md`, so each doc section maps to one
`domain/` module plus its `api/` handlers.
