# Operations: Sessions, Health, and Backups

Three previously-thin areas, addressed together because they're all
"keeping the deployed instance trustworthy over time" rather than
core domain behavior: letting a user manage their own login sessions,
making it possible to tell from the outside whether the server is
actually doing its job, and not losing data.

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

## 2. Background task health

Two Tokio interval tasks now carry real system guarantees:
verification code issuance (`04-verification-workflow.md` §2) and the
punishment deadline sweeper (`08-punishments-and-deadlines.md` §3).
If either silently stops — a panic that isn't caught, a deadlock, the
process being in a bad state without actually crashing — the failure
mode is invisible from the outside: no codes get issued, or
punishments simply never auto-fail, and nothing in the UI would look
obviously wrong until someone noticed a much later symptom (a
submissive who should have failed three punishments by now hasn't).
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

## 3. Backups

Not a new background job. Deliberately not one, in fact: an
always-on internal backup scheduler would mean the application
process itself decides when to compete with live traffic for disk
I/O, on a schedule the deployer doesn't control and might not even
know about. Backups are an infrastructure concern with an
infrastructure-shaped answer:

- The binary exposes a **backup subcommand**
  (e.g. `owners-cock-ledger backup --out <dir>`), not a scheduled
  task or an HTTP endpoint. It performs a live, safe copy using
  SQLite's own backup API (which is online-safe against a
  concurrently-running server in WAL mode — no need to stop the
  service) and copies the blob directory (`05-security-and-privacy.md`
  §4) alongside it into the same output directory, as one unit.
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