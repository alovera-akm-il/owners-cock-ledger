# Security and Privacy

The content this system stores (intimate photos, health-adjacent
device-wear data, D/s dynamic details) is unusually sensitive even by
the standards of a normal personal web app. The architecture treats
that as the primary constraint, ahead of features.

## 1. Deployment assumptions

- Single small trusted user base (one Keyholder, their submissive(s)).
  Not designed or hardened for open internet registration of
  strangers.
- Intended deployment is a private server (home server, VPS reachable
  only via VPN/Tailscale-style overlay network, or LAN-only) — the
  architecture does not itself provide DDoS protection, WAF, or
  multi-region failover, and shouldn't need to.
- TLS is required regardless of network placement (private network is
  not an excuse to run plaintext HTTP) — terminate TLS either in the
  Rust process (`rustls`) or in a reverse proxy in front of it
  (Caddy/nginx); either is acceptable, document whichever is chosen
  in the deployment README once built.

## 2. Authentication & session management

- Passwords hashed with Argon2id, per-user random salt, tuned cost
  parameters appropriate to the deployment hardware.
- Session-cookie auth, not JWT: sessions are stored server-side
  (`sessions`, `01-data-model.md` §2), and the cookie carries only an
  opaque, unguessable session id. This is chosen over stateless JWTs
  specifically because **revocation matters more than statelessness
  here** — if a device is lost, or a dynamic ends abruptly, being able
  to invalidate every outstanding session immediately (delete the
  row) is more important than avoiding a DB lookup per request.
  Session lookups are cheap against SQLite for this user scale. A user
  can also revoke sessions themselves — see `10-operations.md` §1.
- Cookie flags: `HttpOnly`, `Secure`, `SameSite=Strict`. No token is
  ever placed in a URL, query string, or `localStorage`.
- Login rate limiting and account lockout (`failed_login_count`,
  `locked_until` from the data model) to blunt credential-stuffing/
  brute force, given how damaging unauthorized access to this data
  would be.
- **Account enumeration**: `POST /auth/login` returns the identical
  `401` body ("invalid email or password") whether the email doesn't
  exist at all or exists with a wrong password — the two cases are
  indistinguishable from the response. The password-verification hash
  runs either way (against a fixed dummy Argon2 hash when the email
  doesn't exist), so a nonexistent-email request doesn't return
  measurably faster than a wrong-password one — a naive
  "look up user, if not found return early" implementation would leak
  which emails have accounts purely through response timing, which
  matters here more than on a typical site given what having an
  account on this one implies. The same short-circuit-timing risk
  doesn't apply to `POST /auth/invites/redeem`'s "email already in
  use" `409` — that one does legitimately need to say so for the
  submissive completing signup to fix their input, and it only
  discloses whether *any* account uses that email, not which role or
  which Keyholder, so it's an accepted, minor exception rather than
  an oversight.
- CSRF: since auth is cookie-based, all state-changing endpoints
  require a CSRF token (double-submit cookie or synchronizer token)
  in addition to the session cookie.
- Password reset for someone who can still log in is the ordinary
  authenticated "change password" action. For a locked-out account,
  two paths now exist: `owners-cock-ledger admin reset-password`
  (`10-operations.md` §5, always available, no configuration needed)
  and, if the deployer has opted into outbound email (§11 below),
  self-service via `POST /auth/password-reset/request`. Outbound
  email is not part of the base architecture — it's the one opt-in
  addition that changes "there's no email system at all" from an
  absolute statement to a deployer choice.
- Changing the account **email** (the login identifier) requires
  re-entering the current password in the same request
  (`POST /auth/email/change`, `03-api-design.md` §1), even though the
  caller already holds a valid session — an already-authenticated
  browser tab isn't proof the person at the keyboard right now is the
  account holder (shared devices, a session left open), and the email
  change is exactly the kind of action (it changes what credential
  logs the account in next time) worth that extra check.
- API tokens (§9 below) are a **separate** bearer-auth mechanism
  layered on top of this, for Keyholder automation only — they don't
  change anything about session/cookie handling above.
- **Two-factor authentication** (optional, either role — full design
  in `10-operations.md` §2) introduces the one credential in this
  schema that's deliberately stored in the clear rather than hashed:
  `two_factor_credentials.secret`. This is unavoidable, not an
  oversight — verifying a TOTP code requires *computing* the current
  expected value from the secret, which a one-way hash makes
  impossible. It's flagged here as a first candidate for
  application-level field encryption (`05-security-and-privacy.md`
  §5), alongside `keyholder_notes`/`safeword`/`hard_limits`/`soft_limits`,
  for the same reason those are flagged: sensitive plaintext at rest
  that a hash can't protect the way `password_hash` protects a
  password. Recovery codes are the opposite case — high-entropy
  generated values, not something the server needs to recompute — so
  `two_factor_recovery_codes.code_hash` is hashed exactly like an API
  token (SHA-256, no Argon2 needed, §9). The login-flow's second-step
  endpoint (`POST /auth/2fa/verify`) gets its own attempt-count
  lockout scoped to the specific challenge (`01-data-model.md` §2),
  separate from and stricter than the account-level login lockout —
  brute-forcing a 6-digit code needs tighter throttling than
  brute-forcing an arbitrary-length password.

## 3. Authorization

Covered in depth in `02-roles-and-permissions.md`; the two properties
worth restating here as *security* properties rather than just
product rules:

- Every scoped query resolves ownership from the session, never from
  a client-supplied id, which is what actually prevents IDOR
  (insecure direct object reference) issues on endpoints like
  `/keyholder/proof-submissions/{id}` and the attachment-download
  routes — the row's owning link is checked against the caller on
  every single request, not cached or assumed from a prior check.
- Attachment downloads are never served as static files from a
  web-exposed path. They're a handler that (a) authenticates, (b)
  authorizes against the submission's link, (c) streams the file from
  the private blob directory. This also means the blob directory
  itself should have OS-level permissions restricting it to the
  server process's user.
- The live check-in **SSE stream** (`GET /play-sessions/{id}/checkin-stream`,
  `03-api-design.md` §10, `13-checkins.md` §5) is authenticated
  identically to every other route — session cookie or API token,
  same middleware, no separate token or query-string secret invented
  for it. The one property worth stating explicitly: it's read-only
  fan-out of data the caller could already fetch via ordinary
  `GET /checkins/{id}` polling, scoped to one session the caller
  already has (keyholder\*/submissive\*) access to — the stream
  doesn't grant any capability beyond what the caller's normal
  ownership check already permits, it just delivers updates faster.
  The server drops a connection the moment its play session leaves
  `in_progress`, so a stream can't be left open as a standing
  side-channel past the point it's meant to exist.

## 4. Data at rest

- SQLite file and the blob directory live outside any directory the
  web server would ever serve statically, and ideally on a
  filesystem/volume the deployer encrypts at rest (LUKS, or
  provider-level disk encryption) — the application does not
  implement its own at-rest encryption layer; that's delegated to the
  deployment environment, called out explicitly so it isn't
  silently skipped.
- Uploaded files are renamed to a random UUID on write; the original
  filename is retained only as metadata in the DB (visible to the
  Keyholder, useful for context, not used as a path).
- EXIF/metadata stripping: photos should have EXIF (which can include
  GPS location) stripped server-side on ingest before being persisted
  — this matters more here than in most apps, since a leak of raw
  originals could geolocate a person. This is a processing step in
  the proof-submission handler, not a UI toggle.
- Backups: whatever backup strategy the deployer uses for the SQLite
  file must also cover the blob directory, and both should be treated
  as equally sensitive — a backup of the DB without the blobs is
  useless, and a backup of the blobs without the DB defeats all the
  access control metadata around them.

## 5. Data minimization & retention

- `proof_submissions.verification_code_value` (the snapshot described
  in `01-data-model.md` §5) is stored in plaintext, unlike an active
  credential would be. This is deliberate, not an oversight: by the
  time a submission row exists, the code has already been consumed
  (`verification_codes.consumed_at` is set in the same transaction)
  and is single-use, so the stored value can no longer authorize
  anything — it has purely evidentiary value from that point on
  (showing what code the picture was supposed to display), not
  security value. It should still be excluded from ephemeral
  request/process logs per §7, the same as any other stored field
  that isn't meant to be casually grepped.
- `keyholder_notes`, `safeword`/`hard_limits`/`soft_limits`/
  `emergency_contact`, and now `two_factor_credentials.secret` (§2)
  are the clearest "handle with care" plaintext fields; they're
  modeled as ordinary columns (not encrypted-at-application-layer) in
  v1, but flagged here as the first candidates if application-level
  field encryption is added later (e.g. via `sqlcipher` instead of
  plain SQLite, or per-field envelope encryption) — noted as a
  future hardening step rather than built now, to avoid overbuilding
  before the base system exists. The TOTP secret is arguably the
  highest-value of that list to encrypt first, precisely because
  unlike the others it's a live credential (compromise it and 2FA is
  defeated going forward), not just sensitive personal content.
- **Voice proof recordings** (`11-tasks-and-rewards.md` §2) join this
  candidate list, and arguably belong nearer the top of it than
  photo/video attachments do: a voice recording is far more directly
  identifying than a cropped or anonymized photo, since a voice is
  itself biometric-adjacent data that's comparatively easy to tie to
  a real person. It gets the same private-blob-storage, EXIF-adjacent
  (i.e. no embedded location/device metadata retained) handling as
  photo/video attachments in §4, with the size/duration caps noted
  there being the main defense against an oversized or misused
  upload.
- **Check-in field data** (`13-checkins.md`) — the structured
  skin-status/comfort/incident fields a check-in can capture are
  health-adjacent in the same way `hard_limits`/`soft_limits` are,
  and inherit the same handling: ordinary columns in v1
  (`checkins.field_values`, a JSON blob, isn't singled out for
  per-key encryption — encrypting one key inside a JSON column
  loses the query/filter ability the column exists for), flagged
  as in-scope if/when field-level encryption is actually built,
  not before.
- **Amended by Web Push, and worth stating plainly rather than
  quietly walking back**: this architecture previously talked to
  nothing but its own SQLite file and filesystem. Push notifications
  (`09-notifications.md`) break that — delivering a browser push
  notification *requires* an outbound HTTPS call to whichever push
  relay the user's browser uses (e.g. Google's for Chrome, Mozilla's
  for Firefox), because that relay, not this server, is what wakes the
  browser. This is a structural property of the Web Push standard,
  not a design choice this app is making casually, and it's one of
  two exceptions to "no third-party network calls" in the whole
  architecture — outbound email (§11) is the other, added later and
  under the same standard: opt-in, deployer-configured, never a
  default. Two things keep push specifically from being a real privacy
  regression: (1) the payload is end-to-end encrypted (RFC 8291) — the
  relay operator sees only routing metadata (destination endpoint,
  rough payload size/timing), never notification content, which is
  what §9's delivery-mechanics section spells out; (2) it is entirely
  opt-in per user, per device — a user who wants zero third-party
  contact, even at the metadata level, simply never registers a push
  subscription and uses the in-app feed (`GET /notifications`)
  instead, which never leaves this server. Any *other* future
  third-party call (e.g. cloud OCR for auto-verification) should
  still be flagged and opt-in the same way, not default.
- No endpoint exists to export/dump another user's data in bulk;
  each role's own data is only ever readable in the shapes the API
  above defines.
- Ended links: history is retained (see `02-roles-and-permissions.md`
  §4) rather than deleted, on the theory that a submissive's own
  historical record belongs to them and shouldn't vanish because a
  dynamic ended — but the architecture should support an explicit
  "delete my account and all my data" action for a submissive (and
  symmetrically for a Keyholder's own account and everything it
  owns) even though it isn't in the v1 endpoint table above; flagged
  here as a near-term addition, not deferred indefinitely, given how
  sensitive this data is and how much a person may want the option to
  fully erase it.

## 6. Input handling

- **Idempotency keys are scoped per-user, not global**
  (`idempotency_keys.user_id`, `01-data-model.md` §11) — a replay
  lookup always includes `WHERE user_id = caller`, so one user can
  never collide with, inspect, or replay another user's cached
  response by guessing or reusing their `Idempotency-Key` value. The
  cached `response_body` is exactly what the original caller would
  have received, so no additional data is exposed by the replay path
  that the original request didn't already return to that same user.
- File uploads: enforce an allowed MIME/extension list, a max file
  size, and re-encode/validate images server-side (don't trust the
  client's declared content-type) before persisting — both a
  correctness and a security measure (malformed/oversized uploads,
  disguised file types).
- All JSON bodies validated against strict schemas (reject unknown
  fields) so a client can't smuggle extra fields like
  `status: "verified"` into a create-submission call and have it
  accidentally honored by a careless handler — this is exactly why
  §4 of the workflow doc calls out that the create endpoint always
  forces `status='pending'` server-side regardless of input.
- SQL access exclusively through parameterized queries (via
  `sqlx`/`rusqlite` bound parameters) — no string-built SQL,
  anywhere.

## 7. Logging

- The audit log (`audit_log` table) is the durable, structured record
  of state changes and is treated as part of the data model, not
  ephemeral ops logging.
- Ephemeral process/request logs (e.g. via `tracing`) must not log
  photo bytes, full file paths that double as secrets, raw passwords,
  session ids, verification codes, or raw API tokens (bearer header
  values) in plaintext — log identifiers instead (user id, submission
  id, token id/prefix), and redact/omit the sensitive payload fields
  explicitly in the logging middleware rather than relying on nothing
  sensitive ever reaching a `Debug` derive.

## 8. Threats considered

| Threat | Mitigation |
|---|---|
| Submissive forges a "verified" status | Status only settable by the review endpoint, Keyholder-only, ignores client-sent status on create |
| Submissive replays/pre-stages a photo | Time-bound, single-use, server-generated verification codes |
| One Keyholder account browses another Keyholder's submissive | Every query joins through the caller's own `keyholder_submissive_links` row; no cross-link read path exists |
| Stolen session cookie | HttpOnly/Secure/SameSite cookie, server-side revocable sessions, short session lifetime + idle timeout |
| Brute-force login | Rate limiting + account lockout |
| Direct object reference on attachment URLs | Random UUID filenames + per-request ownership check on the streaming handler, never a static file path |
| Leaked photo reveals location via EXIF | Server-side EXIF stripping on ingest |
| Malicious/oversized file upload | Type/size validation + re-encoding before persistence |
| CSRF via cookie-based auth | CSRF token required on state-changing requests |
| Data loss/exposure via careless logging | Structured audit log for state, redacted ephemeral logs for ops |
| Physical/medical emergency while confined | Always-available, unblocked safety-alert endpoint, surfaced above normal review queue |
| Leaked/overscoped API token | Scopes limit blast radius per token, immediate revocation, short default expiry, never logged, hashed at rest like a password would be — see §9 |
| CSRF via a stolen/replayed API token | Not applicable — bearer tokens are sent explicitly in an `Authorization` header by the calling script, never attached automatically by a browser the way a cookie is, so the CSRF threat model that applies to session auth doesn't apply to token auth |
| Leaked VAPID private key | Stored as a server secret (env var/secrets file, never in the DB or version control) like any other server-side key; alone it lets an attacker construct requests that *pass sender verification* at a push relay, but without also having a specific subscriber's `endpoint`/`p256dh`/`auth` (only obtainable from the DB) it can't produce a payload any real subscription would accept — meaningful harm requires the DB compromise too, at which point far worse is already possible |
| A punishment escalation chain or repeated time-extensions used to extend confinement in a way the submissive didn't actually consent to | Not a technical vulnerability — a safety/consent concern. Mitigated the same way any other overreach in the dynamic is: the always-available, unblockable safety alert (`04-verification-workflow.md` §5) and the mutually-visible hard/soft limits (`01-data-model.md` §2) are the system's designed escape valves, not a technical limit on what a Keyholder can configure |

## 9. API tokens

Keyholder API tokens (`01-data-model.md` §9, `03-api-design.md` §12)
widen the attack surface deliberately, in exchange for real
automation value, so the mitigations here are treated with the same
seriousness as password handling:

- **At rest**: `token_hash` is a SHA-256 digest of the full raw token.
  Unlike `users.password_hash`, this doesn't need Argon2-style slow
  stretching — the input is already CSPRNG-random with far more
  entropy than any human-chosen password, so a fast cryptographic
  hash is sufficient to make the stored value useless without the
  original. `token_prefix` (a handful of characters, stored in the
  clear) exists purely so a Keyholder can tell their tokens apart in
  a list; it is far too short to meaningfully narrow a brute-force
  search.
- **In transit**: only ever sent over TLS (§1), in an `Authorization`
  header — never as a query parameter (query strings end up in
  server access logs, browser history, and proxy logs; headers
  generally don't).
- **On display**: the raw token is returned exactly once, in the
  `POST /keyholder/api-tokens` response body, and is unrecoverable
  after that — the same "shown once" discipline as the invite tokens
  in `01-data-model.md` §2.
- **Least privilege by default**: a newly created token has whatever
  scopes were explicitly requested and nothing else (§3's scope
  catalog); the creation UI should default to a narrow starting
  selection rather than pre-checking everything.
- **Expiry**: the creation flow defaults to a finite `expires_at`
  (e.g. 90 days) rather than indefinite, so an abandoned integration's
  token goes stale on its own rather than remaining a permanently
  valid credential someone has to remember exists.
- **Revocation is immediate** (`02-roles-and-permissions.md`, final
  scenario bullet) — a `DELETE` takes effect on the token's very next
  use, with no grace window.
- **Rate limiting applies per-token**, separately from the per-IP/
  per-account login limiter in §2 — this is defense-in-depth against
  a misbehaving or compromised automation script hammering the
  server, not an anti-brute-force measure (the token is already a
  valid, high-entropy secret by the time it's making requests).
- **Accountability**: every write made via a token is recorded with
  enough context to distinguish it from a manual action —
  `reviewed_via`/`assigned_via` on the affected row
  (`01-data-model.md` §5/§6) and `detail.auth = {"type":"api_token",
  "token_id":...}` in the corresponding `audit_log` entry — so a
  Keyholder auditing their own history later can always tell which
  decisions were theirs in the moment versus their automation's.
- **Scope leakage risk worth naming explicitly**: `manage:invites`
  lets a token create new submissive accounts and links. Of every
  scope in the catalog, this is the one most worth withholding from
  routine automation (a notification bot, a dashboard reader) and
  granting only to an integration that genuinely needs to originate
  new accounts — a leaked token with this scope could be used to
  create and link an account under the compromised Keyholder's
  identity, which none of the read-only or review-only scopes permit.

## 10. Push notification security

Covered in full in `09-notifications.md`; the points that are
specifically *security* (as opposed to delivery mechanics) properties:

- **VAPID private key** is a server secret like any other (§1's TLS
  cert, the session/CSRF signing material) — generated once at
  deploy time, stored outside the DB and outside version control, and
  rotating it invalidates nothing retroactively (it only affects
  future sends' sender-verification, not stored data).
- **Payload confidentiality**: per-subscription encryption
  (RFC 8291) means even this server's own operator, if they were also
  positioned to intercept traffic to the push relay, would see
  ciphertext — the encryption keys live in the `push_subscriptions`
  row and the recipient's browser, not anywhere the relay or a
  network observer can reach.
- **Subscription data is exactly as sensitive as a session cookie
  in spirit** (it's a standing way to reach a specific person's
  specific device) even though it can't be used to authenticate API
  requests — `push_subscriptions` rows should get the same "don't log
  this" treatment as session ids (§7), and are deleted (not merely
  disabled) once a push service reports them gone (`09-notifications.md`
  §2), rather than accumulating stale reachable-device records
  indefinitely.
- **No new cross-user exposure**: subscription registration and the
  notification feed are both strictly self-scoped
  (`02-roles-and-permissions.md` §5's final bullet) — this feature
  adds a delivery mechanism, not a new way for either role to see
  data about the other beyond what the rest of the API already
  permits.

## 11. Outbound email (password reset)

Second and last exception to "no third-party network calls"
(§5), added specifically to give self-hosted, LAN-only deployments a
real self-service password-reset path — `admin reset-password`
(`10-operations.md` §5) always works, but requires whoever's locked
out to reach the person running the server. Entirely opt-in: unset
the SMTP config and this capability doesn't exist, `admin
reset-password` remains the only path, exactly as before this section
was designed.

- **Relay, not a mail server.** The deployer supplies credentials for
  an existing mailbox (an app-scoped password, e.g. Fastmail's
  per-integration app passwords) and the daemon submits mail through
  it via standard SMTP AUTH over TLS
  (`07-tech-stack.md` for the crate/config specifics) — it never runs
  its own SMTP server or accepts inbound mail. This is also the
  *correct* answer for a LAN-only host specifically, not just the
  easy one: a home connection has no outbound-port-25 access and no
  sender reputation of its own, so directly sending mail would mostly
  get silently dropped by the recipient's provider. Relaying through
  an established mailbox's own SPF/DKIM/DMARC is what makes the mail
  actually arrive.
- **The relay credential is a server secret**, handled exactly like
  the VAPID private key (§10) — env var or secrets file, never in the
  DB, never in version control, never logged (§7).
- **Timing can't leak account existence.** `POST
  /auth/password-reset/request` (`03-api-design.md` §1) responds with
  the same generic message immediately, before the email lookup or
  send even happens — the actual work runs in a background task after
  the response is already gone. This is the same threat
  `POST /auth/login`'s dummy-hash timing-equalization (§2) defends
  against, solved here structurally instead: there's no "do the slow
  thing only if it's worth doing" branch for an attacker's timing
  measurement to distinguish, because the response never waits on the
  slow thing at all.
- **Rate-limited per-IP and per-email**, same posture as login (§2) —
  otherwise this is both a spam vector (mail-bombing someone's real
  inbox with reset links they didn't request) and a resource-
  exhaustion vector against the relay account's own sending limits.
- **Content minimization.** The email body carries only the reset
  link and its expiry — no account status, no Keyholder/submissive
  context, nothing that would out the nature of the app to someone
  glancing at a notification preview. The sender name is "The Ledger"
  (already a deliberately neutral product name, `00-overview.md`),
  which is enough for the recipient to trust it isn't phishing without
  the subject or body saying anything more specific.
- **Failure is invisible to the caller, not to the operator.** If the
  relay rejects the send (bad credentials, the account's own rate
  limit, a network blip), that's logged for whoever runs the server to
  notice — the public endpoint's response never changes either way,
  by the same discipline as the rest of this section.
