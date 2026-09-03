# Operations: Sessions, Two-Factor Auth, Health, and Backups

Four previously-thin areas, addressed together because they're all
"keeping the deployed instance trustworthy over time" rather than
core domain behavior: letting a user manage their own login sessions,
letting them add a second factor to login itself, making it possible
to tell from the outside whether the server is actually doing its
job, and not losing data.

## 1. Self-service session management

Sessions (`01-data-model.md` §2) were always fully revocable
server-side — that was the whole reason session-cookie auth was
chosen over JWT (`05-security-and-privacy.md` §2). What was missing
was a way for a user to exercise that themselves, rather than it only
being a capability a server operator has via direct DB access.

- `GET /auth/sessions` (`03-api-design.md` §1) lists a user's own
  active sessions with `user_agent` and `last_seen_at`, so "where am
  I logged in" is answerable from the app itself.
- `DELETE /auth/sessions/{id}` revokes one — the "I left it open on
  the hotel computer" case.
- `DELETE /auth/sessions` revokes everything except the caller's
  current session — the "I think something's wrong, kill everything
  else" case, in one action rather than clicking through a list.
- `POST /auth/password/change` also revokes every *other* session as
  a side effect, unconditionally, without the user needing to
  separately remember to do so — a password change is exactly the
  moment a user has reason to believe an old session might be
  compromised, so cleaning up automatically is the safer default.

None of this is reachable via API token (§9 in `03-api-design.md`) —
session management is inherently about interactive login sessions;
an API token has nothing analogous to manage.

## 2. Two-factor authentication

Optional, opt-in, available to either role — this was flagged early
as a gap (password-only auth for content this sensitive) and left for
later; this is that later. TOTP (the standard "6-digit code from an
authenticator app" scheme, RFC 6238) rather than SMS — SMS requires
an outbound SMS provider (a third-party dependency this architecture
has otherwise avoided, `05-security-and-privacy.md` §5) and is
generally considered weaker (SIM-swap risk) for comparable effort.

**Setup is a two-step commit, not instant**, to avoid a broken
half-state: `POST /auth/2fa/setup` generates a secret and shows it as
a QR code, but `two_factor_credentials.confirmed_at` stays `NULL` —
2FA is **not yet enforced on login** — until `POST /auth/2fa/confirm`
proves the user actually captured the secret correctly by entering a
real code back. Without this two-step shape, a user who fat-fingers
the QR scan (or whose authenticator app clock is off) could lock
themselves out on the very next login with no way back in.

**Recovery codes exist for exactly that "next login" risk anyway** —
ten single-use codes issued the moment setup is confirmed, shown once
(`05-security-and-privacy.md` §2), hashed at rest like an API token.
Losing the authenticator device is a real, common failure mode for
TOTP; without a recovery path, that failure mode would be a full
account lockout. If the recovery codes are *also* exhausted or lost,
`admin disable-2fa` (§5 below) is the actual last resort.

**Disabling requires the password *and* a code**, not password alone
(`03-api-design.md` §1) — deliberately more friction than enabling
does. The threat this defends against is specific: a session that's
already been hijacked (which bypasses the password check entirely)
quietly turning 2FA back off so the real owner's next password change
doesn't actually lock the attacker out. Requiring a live code means
the attacker also needs the authenticator itself, not just an open
tab.

**Login flow**: `POST /auth/login` behaves exactly as before when the
account has no confirmed 2FA. When it does, a correct password
returns a `two_factor_login_challenges` row's token instead of a
session (`03-api-design.md` §1) — the password was necessary but not
sufficient. `POST /auth/2fa/verify` completes the login with either a
TOTP code or a recovery code; either is accepted at that endpoint,
since from the login flow's perspective they serve the identical
purpose (prove possession of the second factor).

**Every enable/disable/recovery-regeneration writes an `audit_log`
entry** and sends the account holder a notification
(`09-notifications.md`) — these are exactly the kind of
security-relevant event where the useful failure mode is "the real
owner notices something happened that they didn't do," same
motivating logic as the account-enumeration and session-revocation
protections elsewhere in this document set.

## 3. Background task health

Two Tokio interval tasks now carry real system guarantees:
verification code issuance (`04-verification-workflow.md` §2) and the
task deadline sweeper (`08-punishments-and-deadlines.md` §3).
If either silently stops — a panic that isn't caught, a deadlock, the
process being in a bad state without actually crashing — the failure
mode is invisible from the outside: no codes get issued, or
tasks simply never auto-fail, and nothing in the UI would look
obviously wrong until someone noticed a much later symptom (a
submissive who should have failed three tasks by now hasn't).
That's a bad property for something a Keyholder is relying on to
enforce consequences.

- Each task upserts one row in `background_task_runs`
  (`01-data-model.md` §11) at the *end* of every tick — `last_run_at`,
  whether it completed without error, and how many rows it touched.
  Because it's a single upserted row per task rather than an
  append-only log, checking health is a two-row lookup, not a scan.
- `GET /health` (`03-api-design.md` §14) reads those two rows and
  calls a task `healthy` if `last_run_at` is within, say, 3× its
  expected tick interval (a minute-interval task is unhealthy if it
  hasn't reported in 3+ minutes) — generous enough to absorb normal
  jitter (a slow tick under load) without generous enough to miss a
  task that's actually stopped.
- The endpoint also does a trivial DB round-trip (e.g. `SELECT 1`) to
  distinguish "background task is unhealthy" from "the whole process
  is in trouble," since those call for different responses from
  whoever's watching.
- This is deliberately a **pull** model (an external uptime monitor —
  even something as simple as a cron job curling `/health` and
  emailing on `503` — polls the server) rather than the server trying
  to push an alert itself. Push-based alerting (email/SMS/webhook on
  failure) needs its own delivery mechanism and its own failure modes
  (what if *alerting* is what's broken?); a dumb external poller
  checking a dumb endpoint has fewer ways to fail silently, and is
  consistent with this architecture generally preferring to not take
  on outbound integrations it doesn't strictly need
  (`05-security-and-privacy.md` §5).
- If a Keyholder wants an actual notification when the server itself
  is unhealthy (as opposed to routine in-app notifications the server
  sends about domain events), that's a job for whatever external
  monitoring they point at `/health` — this architecture provides the
  signal, not the alerting pipeline on top of it.

## 4. Backups

Not a new background job. Deliberately not one, in fact: an
always-on internal backup scheduler would mean the application
process itself decides when to compete with live traffic for disk
I/O, on a schedule the deployer doesn't control and might not even
know about. Backups are an infrastructure concern with an
infrastructure-shaped answer:

- The binary exposes a **backup subcommand**
  (e.g. `owners-cock-ledger backup --out <dir>`), not a scheduled
  task or an HTTP endpoint. It reads from `~/.config/<app-name>/`
  (`07-tech-stack.md` §2, overridable the same way the server itself
  resolves it) and performs a live, safe copy using SQLite's own
  backup API (which is online-safe against a concurrently-running
  server in WAL mode — no need to stop the service) and copies the
  blob directory (`05-security-and-privacy.md` §4) alongside it into
  the same output directory, as one unit.
- The deployer wires this into whatever scheduling mechanism they
  already trust — a cron entry or a systemd timer calling the binary
  in backup mode — rather than this application inventing its own
  scheduler for a problem the OS already solves well. This keeps the
  decision of *when* and *how often* (and where the backup ends up —
  another disk, another host) in the deployer's hands, where it
  belongs for a self-hosted single-operator system.
- Restore is symmetric: stop the server, replace the SQLite file and
  blob directory with a backup's copies, start the server. No
  in-place restore-while-running is supported or needed at this
  deployment scale.
- This is a documented **operational recommendation**, not a
  guarantee the application enforces — nothing stops a deployer from
  never running it. `05-security-and-privacy.md` §4 already states
  backups are the deployer's responsibility; this section is what
  makes doing it easy and correct rather than a bespoke script every
  deployer has to write themselves.

## 5. Admin CLI: forced account recovery

Every other doc's mention of "an admin can fix this via direct DB
access" (`05-security-and-privacy.md` §2, `02-roles-and-permissions.md`
§5, `06-future-extensions.md` §2) was always true but never actually
designed — genuinely a gap, not a deliberate omission. This section is
that design: the commands below are the **always-available** recovery
path, independent of any configuration. A deployer who additionally
opts into outbound email (`05-security-and-privacy.md` §11) gets a
second, self-service path for password reset specifically
(`POST /auth/password-reset/request`) — the two are not alternatives
to choose between, they coexist: self-service for the common case,
`admin reset-password` as the fallback that always works even with no
email configured, a failed send, or a submissive who'd rather ask
their Keyholder than wait on an email.

### Trust model: why no extra in-app auth layer

`owners-cock-ledger admin <verb>` subcommands run locally on the
server host, as the same OS user the server process runs as — the
same access level already required to open the SQLite file directly.
For the self-hosted, LAN-only deployment this system assumes
(`05-security-and-privacy.md` §1), that access is the actual security
boundary, not a second authentication layer bolted onto the CLI.
Requiring the CLI to separately log in would be theater: anyone who
can run it already has unmediated read/write access to every table it
would touch. What the CLI adds over raw SQL isn't a permission check —
it's correctness (the right transaction, the right side effects, the
right audit trail) and a record that something happened at all,
which hand-written SQL against a live database gives none of.

This is also why none of the commands below are reachable from any
HTTP endpoint, even a `keyholder`-scoped one, and never will be —
exposing them over the network (LAN or otherwise) would turn "you
need to be on the box" into "you need to be on the network," which is
a materially bigger trust boundary than this design accepts anywhere
else. `POST /auth/password-reset/request` isn't an exception to this —
it's a separate mechanism entirely (a public endpoint that emails a
token, `05-security-and-privacy.md` §11), not `admin reset-password`
made network-reachable; the CLI command still only ever runs on the
box.

### Commands

Every account in this system after the first is created via invite
(`invites`, `01-data-model.md` §2) — but an invite can only be issued
by an existing Keyholder, which means invite-only signup has no answer
for how the *first* account ever comes to exist. That was a genuine gap
(discovered building Phase 1, not a deliberate omission): nothing in
the design created the initial Keyholder a fresh deployment needs
before invite-based signup can even start. `admin create-keyholder`
closes it, as a sibling of the recovery commands below rather than a
separate subsystem — same trust model, same confirmation/audit
behavior, the only CLI command that creates an account instead of
recovering one.

| Command | Effect |
|---|---|
| `admin create-keyholder <email> [--display-name <name>]` | Creates a `role='keyholder'` account with a fresh cryptographically-random password, printed once to stdout — the deployer relays it to the real Keyholder over whatever channel they already trust, same "shown once" discipline as an invite token. The account holder should change it via the ordinary authenticated `POST /auth/password/change` on first login rather than keep the CLI-generated one. Meant to run exactly once per deployment, for the first account; every account after that goes through the normal invite flow. |
| `admin reset-password <email>` | Issues a single-use password-reset token for the account (see below); prints it once to stdout. Does **not** set a password itself — the account holder still chooses their own. |
| `admin disable-2fa <email>` | Force-clears `two_factor_credentials` and every `two_factor_recovery_codes` row for the account — the actual last resort for the lost-device-and-exhausted-recovery-codes case (§2 above). Doesn't touch the password; run `reset-password` too if both are needed. |
| `admin unlock-account <email>` | Clears `failed_login_count` and `locked_until` immediately. A convenience, not a necessity — the account already self-unlocks once `locked_until` passes; this is for "I know it's really them, don't make them wait." |
| `admin force-end-link <link_id>` | Already specified in `06-future-extensions.md` §2 — grouped here as a sibling command, not redesigned. |

Every command above (backup excluded — it's read-only against live
data) shares three behaviors:

- **Confirmation required.** Prints what it's about to do and the
  target account's email, and requires typing the email back to
  proceed — the same "type the name to confirm" friction used for
  any other action this consequential elsewhere in software, absent
  from this app until now only because nothing this destructive was
  CLI-reachable yet. A `--yes` flag skips the prompt for scripted use.
- **Audit-logged like any other action.** Writes a normal `audit_log`
  row (`01-data-model.md` §8) scoped to the affected account/link, not
  a silent side channel — see the actor-marking fix below.
- **Never touches the app's HTTP surface.** Pure CLI, operating
  directly against the database the running server also uses — SQLite
  in WAL mode tolerates this concurrently, the same property the
  backup subcommand already relies on (§4 above).

### Password reset: token, not a set password

`reset-password` deliberately doesn't let the admin choose or see the
account's new password — it issues a **single-use reset token**
(`requested_via='admin_cli'`) the admin relays to the account holder
through whatever out-of-band channel they already have open (LAN
chat, walking over, a phone call), who then sets their own new
password with it. This mirrors invite redemption (`03-api-design.md`
§1) rather than inventing a new shape — and it's the same table and
the same redeem endpoint that `POST /auth/password-reset/request`
(`requested_via='self_service'`, `05-security-and-privacy.md` §11)
also writes into, when a deployer has opted into outbound email.
Schema is `password_reset_tokens`, defined once in
`01-data-model.md` §2 rather than repeated here.

#### `POST /auth/password-reset/redeem` (new endpoint, `03-api-design.md` §1)

Public, requires a valid/unexpired/unconsumed token — `{token,
new_password}`. Reachable through a page at `/password-reset/redeem`
(paste the token, choose a new password) as well as directly, so the
account holder never has to be handed a raw API call to complete this.
Sets `password_hash`, consumes the token, and
**revokes every other existing session for this account in the same
transaction**, exactly like `POST /auth/password/change` already
does. That last part matters here specifically: a password reset is
often needed *because* something's wrong (a shared device, a
suspicion the account's compromised), so old sessions — including
whatever caused the reset to be needed — shouldn't quietly survive it.

No email-enumeration concern beyond what invite redemption already
accepts: the token itself is the proof of legitimacy, not the email
address. A `requested_via='admin_cli'` token is never guessable or
requestable without CLI access; a `requested_via='self_service'` one
is requestable by anyone who knows the email, but that request's
response and timing are identical whether or not the account exists
(`05-security-and-privacy.md` §11) — the redeem step doesn't need to
re-derive that guarantee, it inherits it from how the token was
issued.

### Audit log: distinguishing an admin from the sweeper

`01-data-model.md` §8 previously argued `audit_log.actor_user_id`
being `NULL` was unambiguous on its own — true when the *only* thing
that could write a `NULL`-actor row was the deadline sweeper
(`08-punishments-and-deadlines.md` §3). It no longer is, now that an
admin-CLI action is a second, genuinely different `NULL`-actor case (a
deliberate human decision made outside the app, not an automated
system tick) — conflating the two would hide a person's action behind
the same signal used for "nobody did this, the schedule did." Every
`NULL`-actor row now sets `detail.actor_type` to either `"system"`
(the sweeper) or `"admin_cli"` (one of the commands above), closing
that gap the same way `assigned_via`/`reviewed_via`/`raised_via`
already distinguish automated from human action everywhere else in
this schema.
