# API Design

All endpoints are JSON over HTTPS (self-signed/private-CA cert
acceptable for a home deployment, but TLS is not optional — see
`05-security-and-privacy.md`), under prefix `/api/v1`. File bytes for
proof attachments are the one exception, handled via
`multipart/form-data` on upload and streamed on download.

Auth is a server-side session (opaque session id in an `HttpOnly`,
`Secure`, `SameSite=Strict` cookie), not a bearer JWT — see
`05-security-and-privacy.md` §2 for the reasoning. Every table below
implicitly requires a valid session unless marked "public."

A second, separate auth mechanism exists purely for Keyholder
automation: an `Authorization: Bearer <token>` header carrying a
Keyholder-issued API token (§12). Like sessions, these are opaque and
revocable server-side, not stateless JWTs — the "revocation matters
more than statelessness" reasoning in `05-security-and-privacy.md`
§2 applies equally here. Any endpoint markable "keyholder" or
"keyholder\*" below is reachable via either a session cookie or a
valid, unrevoked, sufficiently-scoped API token; submissive-role
endpoints only ever accept a session, since tokens are Keyholder-only
in v1.

Conventions:
- IDs in paths are UUIDs.
- Timestamps in JSON are ISO-8601 strings (converted from the
  integer epoch storage format at the API boundary).
- Every mutating endpoint writes an `audit_log` row server-side; that
  is not repeated per-endpoint below.
- Errors: standard 400/401/403/404/409/422, JSON body
  `{"error": {"code": "...", "message": "..."}}`. `403` is returned
  (not `404`) when the caller is authenticated but not permitted —
  except where noted below that a `404` is deliberately used to avoid
  confirming another user's row exists.
- **Idempotency**: `POST` endpoints whose side effect is a real-world
  consequence rather than just a database row — `POST
  /keyholder/submissives/{id}/assignments`, `POST
  .../confinement-sessions`, `POST /submissive/proof-submissions`
  (both kinds), `POST /keyholder/invites` — accept an optional
  `Idempotency-Key` header. A repeat request with the same key from
  the same user against the same endpoint returns the original
  response verbatim instead of re-executing (`01-data-model.md` §11
  backs this with `idempotency_keys`, 24h replay window). This
  matters more here than in a typical CRUD API: a retried
  `assignments` POST after a dropped connection wouldn't just create
  a duplicate row, it would apply a `time_extension` effect *twice*
  — genuinely extra confinement time, not just messy data — so the
  client (the web frontend itself, not only third-party token users)
  should always send this key on these specific calls. Endpoints not
  listed here don't support it, since a duplicate `PATCH` (e.g.
  re-applying the same deadline edit) is naturally idempotent already
  and needs no help.

## 1. Auth

| Method & path | Role | Notes |
|---|---|---|
| `POST /auth/login` | public | `{email, password}` → if the account has no confirmed `two_factor_credentials`, sets the session cookie directly, same as always. If it does, returns `{requires_2fa: true, challenge_token}` (`200`, no cookie set yet) instead — the password was correct, but that alone isn't enough to finish logging in. Rate-limited per-IP and per-account; increments `failed_login_count` on failure, locks after threshold. Returns the same generic `401` ("invalid email or password") whether the email doesn't exist or the password is wrong, and always runs the password-verification hash even for a nonexistent email (against a fixed dummy hash) — see `05-security-and-privacy.md` §2 on why, both for the error message and the timing. |
| `POST /auth/2fa/verify` | public (requires a valid `challenge_token`) | `{challenge_token, code}` — `code` is either a 6-digit TOTP code or a recovery code, checked either way. On success, sets the session cookie (exactly what a normal successful login would have done) and deletes the challenge row. On failure, increments the challenge's `attempts`; `410` once the challenge is expired or has hit its attempt limit, requiring a fresh `POST /auth/login`. |
| `POST /auth/logout` | any | invalidates the server-side session record. |
| `GET /auth/me` | any | returns caller's `id`, `role`, `display_name`. |
| `POST /auth/invites/redeem` | public | `{token, email, password, display_name}` → creates a `submissive` account, consumes the invite, creates the `keyholder_submissive_links` row with `status='active'`, and creates a default `verification_policies` row for that link (`04-verification-workflow.md` §1) — all in one transaction. |
| `POST /auth/password/change` | any | `{current_password, new_password}`. Revokes every *other* session for this user in the same transaction (`10-operations.md` §1) — the one exception to sessions only being revocable one at a time by explicit user action. |
| `POST /auth/email/change` | any | `{current_password, new_email}` — requires re-entering the current password even though the caller is already authenticated, since changing the login identifier is sensitive enough to warrant re-proving it's really the account holder at the keyboard. Fails `409` if `new_email` is already in use. |
| `GET /auth/sessions` | any | list the caller's own active sessions — `id`, `created_at`, `last_seen_at`, `user_agent`, and `is_current: bool` (whether it's the one making this request). Backs a "where am I logged in" view; see `10-operations.md` §1. |
| `DELETE /auth/sessions/{id}` | any\* | revoke one specific session (e.g. a lost or old device) — self-scoped, can't touch another user's session. Revoking the *current* session is allowed and behaves like logout. |
| `DELETE /auth/sessions` | any | `{except_current: true}` (default) — "log out everywhere else." Revokes every session for the caller except the one making the call, in one action, for the "I think something's wrong" moment rather than clicking revoke one at a time. |
| `GET /auth/2fa/status` | any | `{enabled: bool, pending_setup: bool, recovery_codes_remaining: int}`. |
| `POST /auth/2fa/setup` | any | no body. Generates a fresh TOTP secret, stores it with `confirmed_at=NULL`, and returns `{secret, otpauth_uri}` for the client to render as a QR code (and show as text for manual entry). Calling this again before confirming replaces the pending secret rather than erroring. |
| `POST /auth/2fa/confirm` | any | `{code}` — validates `code` against the pending secret from `setup`. `409` if there's no pending setup. On success: sets `confirmed_at`, generates 10 recovery codes, and returns them **exactly once** in this response (same "shown once" discipline as invite tokens and API tokens, `05-security-and-privacy.md` §2) — hashed at rest from this point on, unrecoverable if lost. |
| `POST /auth/2fa/disable` | any | `{current_password, code}` — requires **both** the password and a valid TOTP/recovery code, not just one. A password alone isn't enough specifically because the threat this guards against is a hijacked *session* (which already bypasses the password) trying to quietly turn off the second factor; requiring the code too means whoever disables 2FA has to currently possess the authenticator, not just be sitting in an already-open tab. Deletes `two_factor_credentials` and every remaining recovery code. |
| `POST /auth/2fa/recovery-codes/regenerate` | any | `{current_password, code}` — same dual-proof requirement as disabling. Invalidates every existing recovery code and issues a fresh set, shown once. For "I've used most of my codes" or "I think an old code leaked." |

## 2. Keyholder: submissive roster & invites

| Method & path | Role | Notes |
|---|---|---|
| `POST /keyholder/invites` | keyholder | `{expires_in_hours?}` → `{token, expires_at}`. Token shown once; only its hash is stored. |
| `GET /keyholder/invites` | keyholder | list own outstanding/used invites. |
| `DELETE /keyholder/invites/{id}` | keyholder | revoke an unused invite. |
| `GET /keyholder/submissives` | keyholder | roster: linked submissives (default `status=active`; `?status=` to include paused/ended) with a summary card per row (current lock state, last verification outcome, pending items count). |
| `GET /keyholder/submissives/{id}` | keyholder\* | full detail view (profile, current status, recent activity). 404 if no link exists to that id (see error-code note above). |
| `PATCH /keyholder/submissives/{id}/link` | keyholder\* | `{status: "paused"\|"ended"}` — only forward transitions; can't reopen an ended link (a new invite starts a fresh link instead). |
| `PATCH /keyholder/submissives/{id}/link/settings` | keyholder\* | `{self_report_allowed: bool, catalog_visible_to_submissive: bool}` |

## 3. Profiles

Both roles' profile page covers three things: account credentials
(§1, above — email/password), personal profile fields (this
section), and, for the Keyholder, API token management (§12).

| Method & path | Role | Notes |
|---|---|---|
| `GET /profile` | any | own profile, role-appropriate shape. |
| `PATCH /profile` | any | submissive can edit `bio`, `safeword`, `hard_limits`, `soft_limits`, `emergency_contact`, `timezone`; keyholder can edit `bio`, `contact_info`, `hard_limits`, `soft_limits`, `timezone`. |
| `GET /keyholder/submissives/{id}/profile` | keyholder\* | the linked submissive's profile, including `safeword`, `hard_limits`, `soft_limits`, and `keyholder_notes` (none of which are ever exposed to a *different* Keyholder, or to any submissive other than the profile's owner). |
| `PATCH /keyholder/submissives/{id}/profile/notes` | keyholder\* | `{keyholder_notes}` — the one field only the Keyholder can write on the submissive's profile. The Keyholder cannot write the submissive's `hard_limits`/`soft_limits`/`safeword` — those stay submissive-owned, read-only to the Keyholder (see `02-roles-and-permissions.md` §2). |
| `GET /submissive/keyholder-profile` | submissive | read-only view of the caller's currently-linked Keyholder: `display_name`, `bio`, `hard_limits`, `soft_limits`. The mirror image of the row above — mutual limits visibility only works if this direction exists too (see `01-data-model.md` §2). Does **not** expose `contact_info`, which stays Keyholder-only unless the Keyholder chooses to put reachability info in a field the submissive can already see. |

## 4. Chastity status

| Method & path | Role | Notes |
|---|---|---|
| `GET /keyholder/submissives/{id}/devices` | keyholder\* | |
| `POST /keyholder/submissives/{id}/devices` | keyholder\* | `{name, description?}` |
| `PATCH /keyholder/submissives/{id}/devices/{deviceId}` | keyholder\* | e.g. retire a device |
| `GET /submissive/devices` | submissive | self, read-only |
| `GET /keyholder/submissives/{id}/status` | keyholder\* | derived current status + open session if any, including `target_release_at`, `time_remaining_seconds` (negative if overdue), whether it's overdue, and `clock_paused_at`/`clock_paused` (bool)/`clock_pause_message` |
| `GET /submissive/status` | submissive | self equivalent — includes `clock_pause_message` too, since it's written to be read by the submissive |
| `POST /keyholder/submissives/{id}/confinement-sessions/{sessionId}/pause` | keyholder\* | `{message?}` — optional note shown to both roles while paused. `409` if there's no open session, or it's already paused. Sets `clock_paused_at=now()` and, if given, `clock_pause_message` — freezes the confinement countdown *only*; punishment deadlines and verification scheduling are unaffected on purpose. Full reasoning and exact scope boundary in `08-punishments-and-deadlines.md` §9. |
| `PATCH /keyholder/submissives/{id}/confinement-sessions/{sessionId}/pause-message` | keyholder\* | `{message}` (empty string clears it) — update the shown message without resuming and re-pausing. `409` if not currently paused. |
| `POST /keyholder/submissives/{id}/confinement-sessions/{sessionId}/resume` | keyholder\* | no body. `409` if not currently paused. Computes the elapsed pause duration, extends `target_release_at` by it (inserting a `confinement_adjustments` row, `reason='clock_pause'`, `notes` carrying forward whatever `clock_pause_message` was), and clears both `clock_paused_at` and `clock_pause_message` — all in one transaction. |
| `POST /keyholder/submissives/{id}/confinement-sessions` | keyholder\* | start a session: `{device_id, started_reason, target_release_at?, notes?}`; 409 if one is already open |
| `PATCH /keyholder/submissives/{id}/confinement-sessions/{sessionId}` | keyholder\* | close it: `{ended_reason, notes?}` |
| `PATCH /keyholder/submissives/{id}/confinement-sessions/{sessionId}/timer` | keyholder\* | `{delta_seconds, notes?}` — adjusts `target_release_at` by `delta_seconds` (positive extends, negative shortens) and inserts a `confinement_adjustments` row with `reason='manual'` in the same transaction. There is deliberately no endpoint to set `target_release_at` to an absolute value directly — every change is a *delta*, so it always has an explicit, logged magnitude rather than silently overwriting whatever was there. |
| `GET /keyholder/submissives/{id}/confinement-sessions/{sessionId}/timer-adjustments` | keyholder\* | history of every delta applied to this session's timer, manual and punishment-triggered alike; each row includes `keyholder_reviewed_at` (`01-data-model.md` §4) |
| `PATCH /keyholder/submissives/{id}/confinement-sessions/{sessionId}/timer-adjustments/{adjustmentId}/review` | keyholder\* | no body — marks one automatically-applied (`reason='punishment_time_extension'`) adjustment as reviewed, i.e. "the default amount was correct, leave it." `409` if it's already reviewed or isn't the kind of row that needs review (`reason='manual'` rows are reviewed at insert time and can't be re-reviewed). A follow-up `PATCH .../timer` delta on the same session marks any outstanding unreviewed adjustment reviewed too, as a side effect — see `08-punishments-and-deadlines.md` §6. |
| `GET /submissive/confinement-sessions/{sessionId}/timer-adjustments` | submissive | self equivalent, read-only — "why did my time change" always has an answer here. Includes `keyholder_reviewed_at` too, so a submissive can see whether an applied extension was a default nobody's confirmed yet or one the Keyholder actively signed off on — informational only, the submissive has no action to take on it. |
| `POST /submissive/confinement-sessions` / `PATCH .../{sessionId}` | submissive | same shape, only reachable when `self_report_allowed` is true for the caller's active link; 403 otherwise. The timer-adjustment endpoints stay Keyholder-only even when self-report is enabled — self-report covers "I put it back on," not "how long I'm supposed to stay in it" |
| `GET /keyholder/submissives/{id}/confinement-sessions` | keyholder\* | history, paginated |
| `GET /submissive/confinement-sessions` | submissive | self history |

## 5. Verification policy & codes

| Method & path | Role | Notes |
|---|---|---|
| `GET /keyholder/submissives/{id}/verification-policy` | keyholder\* | |
| `PUT /keyholder/submissives/{id}/verification-policy` | keyholder\* | `{frequency_kind, frequency_value, code_ttl_seconds, grace_period_seconds}` |
| `GET /submissive/verification-policy` | submissive | self, read-only |
| `GET /submissive/verification-codes/current` | submissive | returns the caller's currently-active unconsumed code if one exists, else `null` |
| `POST /submissive/verification-codes` | submissive | request a new code on demand; 409 if the policy doesn't allow on-demand *and* one isn't currently due, or if an unconsumed code already exists |
| `GET /keyholder/submissives/{id}/verification-codes` | keyholder\* | issued-code history (for audit, not for reading the code value to "help" — code is delivered to the submissive only) |

Scheduled code issuance (per `frequency_kind`) runs as an internal
background task inside the server process (a Tokio interval task),
not a client-triggered endpoint — see `04-verification-workflow.md`.

## 6. Proof submissions

| Method & path | Role | Notes |
|---|---|---|
| `POST /submissive/proof-submissions` | submissive | `multipart/form-data`: `verification_code_id` (or omitted for an unscheduled `note`-kind entry), `kind`, `metadata` (JSON string), `files[]` (0+ attachments, required if `kind` involves media). Always inserts `status='pending'`. The server resolves `verification_code_id` to its `code` value and stores that value on the new row (`verification_code_value`) in the same transaction that consumes the code — the client never supplies the code text itself, only the code's id, so it can't be spoofed on write. |
| `GET /submissive/proof-submissions` | submissive | self history, paginated, filterable by `status` and `purpose`. Each item includes `verification_code_value` alongside its attachments, so the code and the picture it belongs to are always returned together; a `purpose="punishment_completion"` item also includes `assignment_id`. |
| `GET /submissive/proof-submissions/{id}` | submissive\* | self only; see response shape below. |
| `GET /keyholder/submissives/{id}/proof-submissions` | keyholder\* | one submissive's history, newest first, paginated; filterable by `status`, `purpose`, and by `days`/`from`/`to` (see §11). Includes `verification_code_value` per item. |
| `GET /keyholder/proof-submissions` | keyholder | **cross-roster feed**: recent submissions across every submissive currently linked to the caller (`active` links by default; `?link_status=` to include `paused`), newest first. Same filters as the per-submissive list (`status`, `days`/`from`/`to`), plus an optional `submissive_id` to narrow back down to one person without switching endpoints. This is what backs an "everything from the last N days" view and the dashboard's aggregate pending-review count — see §11 and `02-roles-and-permissions.md` §5 for how it stays scoped to the caller's own links. |
| `GET /keyholder/proof-submissions/{id}` | keyholder\* | resolves ownership via the submission's `link_id`, not a submissive id in the path — used from a cross-submissive "pending review" queue. See response shape below. |
| `GET /keyholder/proof-submissions/{id}/attachments/{attachmentId}` | keyholder\* | streams the file bytes; requires the same ownership check as the submission itself, independent of the DB row (no direct static file serving of the blob directory). |
| `GET /submissive/proof-submissions/{id}/attachments/{attachmentId}` | submissive\* | same, self-scoped. |
| `POST /keyholder/proof-submissions/{id}/review` | keyholder\* | `{status: "verified"\|"redo"\|"failed", review_notes?, punishment?: {template_id?, title?, description?}}`. See workflow doc for the transactional detail. `redo` re-opens the door for a follow-up submission referencing `redo_of_submission_id`. Records `reviewed_via` as `"session"` or `"api_token"` based on how the request authenticated — see `01-data-model.md` §5 and `05-security-and-privacy.md` §9 on why a review made by an API-token-driven script is worth being able to tell apart from one a Keyholder actually looked at. |

**Response shape for `GET .../proof-submissions/{id}` (both roles):** a
single JSON object carrying the code and the picture *reference*
together in one payload — `verification_code_value` plus an embedded
`attachments: [{id, mime_type, original_filename, byte_size}, …]`
array — so one request tells the caller everything about the
submission, including which attachment id(s) to load. The Keyholder
review screen (`mockups/proof-review.html`) renders both from that
one call: the code is shown as plain text straight from the JSON,
while each attachment's actual bytes are then fetched with
`<img src="/api/v1/keyholder/proof-submissions/{id}/attachments/{attachmentId}">`
(or an equivalent authenticated fetch). That second request is a
streamed binary, not JSON, deliberately kept separate from the
metadata response — see `05-security-and-privacy.md` §3 on never
returning file bytes from a route that doesn't also re-run the
ownership check, and on not bloating/duplicating binary payloads
into JSON (e.g. as base64) when a plain authenticated stream does the
job. From the Keyholder's point of view this is still "getting both
at the same time": one page load, one detail fetch, code and photo
displayed side by side — it just isn't one HTTP response carrying
raw image bytes inline.

## 7. Rewards & punishments

### Catalog

| Method & path | Role | Notes |
|---|---|---|
| `GET /keyholder/templates` | keyholder | own catalog, `?kind=reward\|punishment` |
| `POST /keyholder/templates` | keyholder | rewards: `{kind:"reward", title, description?, severity?}`. Punishments: `{kind:"punishment", title, description?, severity?, effect_kind:"task"\|"time_extension", completion_type?, default_deadline_seconds?, time_extension_seconds?, on_failure_template_id?}` — `completion_type`+`default_deadline_seconds` required when `effect_kind="task"`; `time_extension_seconds` required when `effect_kind="time_extension"`. `422` if the required combination for the chosen `effect_kind` isn't present. |
| `PATCH /keyholder/templates/{id}` | keyholder\* | edit any of the above fields, or deactivate. Editing an existing template never rewrites past assignments (§6 in `01-data-model.md` — everything meaningful is copied at assignment time), so changing e.g. `default_deadline_seconds` only affects punishments assigned *after* the edit. |
| `GET /submissive/templates` | submissive | read-only, only if `catalog_visible_to_submissive` true for the caller's link. Includes the punishment-only fields (a submissive can see that "cold shower" requires proof within 24h and escalates to "extra day locked" — full transparency about the ladder, per the same reasoning as template read-visibility generally, `02-roles-and-permissions.md` §3) |

### Assignments

| Method & path | Role | Notes |
|---|---|---|
| `POST /keyholder/submissives/{id}/assignments` | keyholder\* | Rewards: `{kind:"reward", template_id? \| (title & description), notes?}`, unchanged. Punishments: `{kind:"punishment", template_id? \| (title, description, effect_kind, completion_type?, default_deadline_seconds?, time_extension_seconds?), on_failure_template_id?, deadline_at?, triggered_by_submission_id?, notes?}`. From a template, `effect_kind`/`completion_type`/deadline math/`on_failure_template_id` all default from it; any may be overridden inline for this one instance. `deadline_at` defaults to `assigned_at + default_deadline_seconds` and can be overridden directly instead of via the seconds offset. `effect_kind="time_extension"` punishments apply immediately (§6 in `01-data-model.md`) and return with `status:"applied"` already set — there's nothing further for anyone to do. Also the endpoint used internally by the verification-review flow's `punishment` payload (`04-verification-workflow.md` §4) and by the deadline sweeper's escalation logic (`08-punishments-and-deadlines.md`), which is why `assigned_via` can be `"system"` as well as `"session"`/`"api_token"`. |
| `GET /keyholder/submissives/{id}/assignments` | keyholder\* | one submissive's history, filterable by `kind`/`status`/`effect_kind` and by `deadline_before`/`deadline_after` (ISO-8601, see below); response includes `assigned_via`, `deadline_at`, `escalated_from_assignment_id` per row |
| `GET /keyholder/assignments` | keyholder | **cross-roster feed**, the same relationship `GET /keyholder/proof-submissions` (§6) has to its per-submissive equivalent: open/recent punishments and rewards across every submissive linked to the caller, newest first, same filters as above plus an optional `submissive_id`. This is what "what's due across my whole roster" actually needs — `GET /keyholder/assignments?status=assigned&deadline_before=2026-08-31T00:00:00Z` answers "everything due by midnight," which nothing in v1 could answer without an N-submissive fan-out before this endpoint existed. Scoped via the same `link_id IN (caller's links)` join as the proof-submissions feed (`02-roles-and-permissions.md` §5). |
| `GET /keyholder/assignments/{id}` | keyholder\* | single assignment detail, including its full escalation chain (walking `escalated_from_assignment_id` backward and any assignment with `escalated_from_assignment_id = this id` forward) — lets a Keyholder see "this is the 3rd link in a chain that started with X" at a glance |
| `GET /submissive/assignments` | submissive | self history, same filters/fields as the Keyholder list above (no separate cross-roster concept needed — a submissive's "roster" is always just themself, same reasoning as the proof-submissions feed) |
| `PATCH /submissive/assignments/{id}/acknowledge` | submissive\* | only legal transition a submissive can make directly: `assigned`→`acknowledged`. `409` if `effect_kind != "task"` or `completion_type != "acknowledge_only"` (a `proof_required` punishment is acted on via the proof endpoint below instead, not this one; a `time_extension` punishment has nothing to acknowledge — it's already `applied`) |
| `POST /submissive/assignments/{id}/proof` | submissive\* | `multipart/form-data`, same shape as `POST /submissive/proof-submissions` (§6) minus `verification_code_id`. Creates a `proof_submissions` row with `purpose="punishment_completion"` and `assignment_id` set, and moves this assignment to `proof_submitted` — both in one transaction. `409` if `completion_type != "proof_required"`, if the assignment isn't in `assigned` status, or if `deadline_at` has already passed (see `08-punishments-and-deadlines.md` for exactly when the sweeper beats a late submission to it). |
| `POST /keyholder/proof-submissions/{id}/review` | keyholder\* | **the same endpoint as §6** — when the reviewed submission has `purpose="punishment_completion"`, a `verified` result also moves the linked assignment to `completed`, `failed` also fails the assignment (triggering escalation per `on_failure_template_id`, see `08-punishments-and-deadlines.md`), and `redo` leaves the assignment as-is awaiting resubmission. No separate punishment-proof review endpoint exists — reuse, not a parallel pathway. |
| `PATCH /keyholder/assignments/{id}` | keyholder\* | `{status: "completed"\|"revoked", notes?}` — for rewards, and for `acknowledge_only` punishments once acknowledged. Not used for `proof_required` punishments, whose completion is decided by the proof review above, not this endpoint directly. |
| `PATCH /keyholder/assignments/{id}/deadline` | keyholder\* | `{deadline_at}` — extend or shorten an open punishment's deadline. `409` once the assignment has left `assigned`/`proof_submitted` (nothing left to extend). Every edit is a normal audited action; unlike the confinement timer (§4) this is a direct absolute-value set rather than a delta, since a deadline is a single point in time, not an accumulating quantity. |
| `PATCH /keyholder/assignments/{id}/escalation` | keyholder\* | `{on_failure_template_id}` (nullable — `null` clears it, meaning "no automatic escalation, I'll decide manually if this fails"). Lets a Keyholder reconsider the consequence after assigning, without revoking and recreating the punishment from scratch. Same `409` window as the deadline edit above — once the assignment has resolved, there's nothing left to escalate. |

`deadline_before`/`deadline_after` (ISO-8601) filter on
`assignments.deadline_at` specifically — distinct from the general
`days`/`from`/`to` convention in §11, which filters on `assigned_at`
(when the row was created). The two answer different questions:
"what was assigned recently" vs. "what's coming due soon," and a
Keyholder-facing dashboard needs both — see
`08-punishments-and-deadlines.md` §3 for how these same bounds are
what the deadline sweeper itself sweeps on internally.

## 8. Safety

| Method & path | Role | Notes |
|---|---|---|
| `POST /submissive/safety-alert` | submissive | `{message?}` — always reachable regardless of any other state (locked account excepted); intentionally minimal payload so it's fast to fire. |
| `GET /keyholder/safety-alerts` | keyholder | across all their active links; default filter unresolved-first. |
| `PATCH /keyholder/safety-alerts/{id}` | keyholder\* | `{acknowledged: true}` / `{resolved: true}` |

## 9. Audit log

| Method & path | Role | Notes |
|---|---|---|
| `GET /keyholder/submissives/{id}/audit-log` | keyholder\* | one submissive's entries, paginated, filterable by `action` |
| `GET /keyholder/audit-log` | keyholder | **cross-roster feed**, same pattern as the proof-submissions and assignments feeds — every audit entry across the caller's own links, newest first, filterable by `action` and `submissive_id`. Without this, "what's happened across my roster today" required opening every submissive individually; with it, it's one call. Scoped identically (`link_id IN (caller's links)`). |
| `GET /submissive/audit-log` | submissive | self-scoped entries only |

## 10. Reserved, not implemented yet

`/api/v1/play-sessions*` — namespace reserved per
`06-future-extensions.md`; no routes exist in this version.

## 11. Pagination & filtering convention

List endpoints share `?limit=&cursor=` (opaque cursor over
`(occurred_at/submitted_at, id)`) rather than offset pagination, so
results stay stable while new rows are inserted between page fetches
— relevant here since these are live, frequently-appended logs.

### Date/time range filtering

Any list endpoint ordered by a timestamp column (proof submissions,
audit log, assignments, safety alerts, notifications) additionally
accepts:

- `?from=&to=` — ISO-8601 timestamps, inclusive lower/upper bound on
  that endpoint's timestamp column (`submitted_at`, `occurred_at`,
  `assigned_at`, `raised_at` respectively). Either may be omitted for
  an open-ended bound.
- `?days=N` — shorthand equivalent to `from = now - N days` (`to`
  defaults to now). `N` is any positive integer; the Keyholder-facing
  UI surfaces **3, 5, and 7** as one-click presets (matching the
  question that prompted this), but the API itself doesn't restrict
  `days` to that set — a client can pass `days=1` or `days=30` just
  as validly.
- `from`/`to`/`days` are mutually composable with `status` and the
  cursor: they narrow the *set* being paginated, the cursor still
  walks newest-first through whatever that filtered set is. Passing
  both `days` and an explicit `from` is rejected `400` (ambiguous)
  rather than silently picking one.
- These are query-string filters only — never a way to bypass
  ownership scoping. `GET /keyholder/proof-submissions?days=7` still
  only ever returns rows whose `link_id` belongs to the caller (see
  `02-roles-and-permissions.md` §5); a submissive's equivalent calls
  are always additionally pinned to `submissive_id = caller`,
  regardless of any date filter supplied.

## 12. API tokens (Keyholder automation)

Backs `01-data-model.md` §9 (`api_tokens`). Keyholder-only — there is
no submissive-facing equivalent in v1 (see `06-future-extensions.md`).

| Method & path | Role | Notes |
|---|---|---|
| `POST /keyholder/api-tokens` | keyholder | `{label, scopes: [...], expires_in_days?}` → `{id, token, prefix, expires_at}`. The full raw `token` value is returned **exactly once**, in this response only; it is never retrievable again, only re-issuable as a brand new token. |
| `GET /keyholder/api-tokens` | keyholder | list own tokens — `id`, `label`, `token_prefix`, `scopes`, `created_at`, `expires_at`, `last_used_at`, `revoked_at`. Never returns the full token value. |
| `PATCH /keyholder/api-tokens/{id}` | keyholder\* | `{label?, scopes?}` — narrowing or renaming an existing token without having to rotate it. Widening scopes is allowed too; it's the same Keyholder granting themself the access, not a privilege escalation from anyone else's perspective. |
| `DELETE /keyholder/api-tokens/{id}` | keyholder\* | revoke: sets `revoked_at`, doesn't hard-delete the row (history stays visible — see `01-data-model.md` §9). Immediate: a revoked token fails auth on its very next request. |

### Scope catalog

Scopes are named `verb:resource` pairs mirroring the domain modules
in `01-data-model.md`, so adding a new domain later means adding a
new scope pair, not redesigning this system. Illustrative v1 set
(not necessarily exhaustive — the pattern is what matters):

| Scope | Grants |
|---|---|
| `read:submissives` | roster + per-submissive profile/status reads |
| `read:proof-submissions` | submission metadata, including `verification_code_value` |
| `read:proof-attachments` | the actual photo/video **bytes** — split out from the scope above deliberately, since a token built for e.g. a notification bot ("you have 2 pending reviews") has no reason to ever be able to pull the images themselves |
| `review:proof-submissions` | `POST .../review` — see the `reviewed_via` discussion in `01-data-model.md` §5 and `05-security-and-privacy.md` §9 before granting this one |
| `manage:chastity` | device records, confinement-session start/stop, and timer adjustments (`PATCH .../timer`) |
| `manage:verification-policy` | edit a submissive's verification policy |
| `read:catalog` / `manage:catalog` | read vs. create/edit reward-punishment templates, including the punishment-only `effect_kind`/`completion_type`/deadline/escalation fields |
| `manage:assignments` | create/update reward or punishment assignments, including deadline edits (`PATCH .../deadline`) — see the `reviewed_via`/`assigned_via` accountability note in `01-data-model.md` §5/§6 before granting this alongside `review:proof-submissions`, since together they let a token both fail a punishment's proof review *and* assign whatever comes next unattended |
| `read:audit-log` | |
| `read:safety-alerts` / `manage:safety-alerts` | read vs. acknowledge/resolve |
| `manage:invites` | create/revoke submissive invites — flagged in `05-security-and-privacy.md` §9 as the highest-risk scope to hand to an automated integration, since it can bring new accounts into existence |
| `read:notifications` | poll `GET /notifications` (§13) — an alternative to Web Push for a script that wants to react to events itself rather than receiving a browser notification |

A token created with no scopes at all is valid but can call nothing
except `GET /auth/me`-equivalent introspection — scopes are opt-in,
never a default-allow list.

### Request authentication

`Authorization: Bearer <raw_token>`. The server hashes the presented
value and looks it up by `token_hash` (exact match, not prefix scan);
on success it resolves to `(keyholder_id, scopes)` the same way a
session resolves to `(user_id, role)`, then every downstream
ownership check (§§2–9 above) proceeds identically to a session-
authenticated request. A route also checks the token carries the
scope it requires; missing a needed scope is `403`, not `401` (the
token is valid, it's simply not permitted to do this one thing).
`last_used_at` is updated (best-effort, not in the same transaction
as the actual request) on each successful auth.

## 13. Push notifications & in-app feed

Backs `01-data-model.md` §10. Full trigger matrix (which event
produces which notification, to whom) and Web Push delivery mechanics
are in `09-notifications.md`; this is just the API surface. Available
to both roles.

| Method & path | Role | Notes |
|---|---|---|
| `GET /notifications/vapid-public-key` | any | public, but only meaningful to an authenticated client — the VAPID public key the frontend needs to create a `PushSubscription` via the browser's Push API. Not a secret; it's designed to be public (see `05-security-and-privacy.md` §5). |
| `POST /notifications/push-subscriptions` | any | `{endpoint, keys: {p256dh, auth}, user_agent?}` — registers a browser-created `PushSubscription`. Idempotent on `endpoint`: re-registering the same endpoint updates `last_seen_at` rather than erroring. |
| `GET /notifications/push-subscriptions` | any | list own registered devices (`user_agent`, `created_at`, `last_seen_at`) — lets a user see and clean up stale devices; never returns the encryption keys back out. |
| `DELETE /notifications/push-subscriptions/{id}` | any\* | unregister a device — a user should do this from the device itself when disabling notifications, but can also do it remotely (e.g. "I lost my phone"). |
| `GET /notifications` | any | own in-app feed, paginated, newest first, filterable by `?unread=true` and by `days`/`from`/`to` (§11). This is the fallback that always works, independent of whether push is set up, permitted, or currently reachable. |
| `PATCH /notifications/{id}/read` | any\* | mark one read. |
| `PATCH /notifications/read-all` | any | mark everything currently unread as read, in one call — the common "clear the badge" action. |

Delivery itself (deciding a notification is warranted, writing the
`notifications` row, and attempting a Web Push send to every active
subscription) is not client-triggered — it happens inside the same
service-layer transaction as the event that caused it (a review, an
assignment, a missed deadline), the same way an `audit_log` row is
written alongside every state change rather than through a separate
endpoint. See `09-notifications.md` for exactly which actions trigger
which notification `type`.

## 14. Health & operations

Full rationale in `10-operations.md`. Session management lives in
§1 (Auth), not here, since it's user-facing rather than operational.

| Method & path | Role | Notes |
|---|---|---|
| `GET /health` | public (or bind to localhost only, deployer's choice) | `{status: "ok"\|"degraded", db: "ok"\|"error", background_tasks: {verification_issuance: {last_run_at, healthy}, deadline_sweeper: {last_run_at, healthy}}}`. `healthy` per task is `last_run_at` within a small multiple of its expected tick interval — see `10-operations.md` §2 for the exact threshold and why a crashed background task is the specific failure mode this exists to catch. Returns `503` when `status: "degraded"`, so an external uptime monitor can alert on it without parsing the body. |

## 15. Statistics

Backs `submissive-statistics.html`-style views for both roles. All of
this is computed on read from existing tables (`proof_submissions`,
`assignments`, `confinement_sessions`, `confinement_adjustments`) —
no new tables, no pre-aggregation, no scheduled rollup job. At this
app's scale (one Keyholder, a handful of submissives, years of
history at most) a handful of aggregate queries per request is cheap;
a materialized stats table would be solving a performance problem
this system doesn't have yet.

| Method & path | Role | Notes |
|---|---|---|
| `GET /submissive/stats` | submissive | self stats, `?period=all\|30d\|90d\|365d` (default `all`) — see shape below |
| `GET /keyholder/submissives/{id}/stats` | keyholder\* | identical shape, for one linked submissive — a Keyholder sees exactly what their submissive sees, not a different rollup, so there's one shared mental model of "the numbers" rather than two disagreeing views |

**Response shape** (all durations in seconds, alongside a
human-formatted string the server also computes so the client doesn't
need duration-formatting logic):

```json
{
  "period": "all",
  "current_streak_seconds": 1468800,
  "personal_best_streak_seconds": 8985600,
  "consistency_pct": 21,
  "session_lengths": { "shortest_seconds": 0, "longest_seconds": 8985600, "average_seconds": 4492800 },
  "verification": { "verified": 142, "failed": 9, "missed": 3 },
  "punishments": { "assigned": 18, "completed": 14, "failed": 4, "escalated": 3 },
  "rewards_given": 11,
  "timer_adjustments": { "added_seconds": 356400, "removed_seconds": 43200 },
  "lifetime_locked_seconds": 31449600
}
```

- `current_streak_seconds` / `personal_best_streak_seconds`: the open
  `confinement_sessions` row's elapsed time (0 if currently unlocked),
  and the longest `ended_at - started_at` across all of this
  submissive's sessions ever — the latter is **not** period-filtered,
  matching the reference design's "Global" vs. "Activity" split: a
  personal best doesn't reset just because you're looking at "last
  30 days."
- `consistency_pct`: total locked time within the selected period
  divided by the period's wall-clock length. For `period=all`, the
  denominator is time since the link's `started_at`, not account
  creation — consistency is about the dynamic, not the account.
- `verification`/`punishments` counts are scoped to the selected
  period by `submitted_at`/`assigned_at` respectively; `rewards_given`
  and `timer_adjustments` likewise.
- `lifetime_locked_seconds` is the one other never-period-filtered
  field, alongside the personal best — cumulative time locked across
  every session ever, for the same "Global, not Activity" reason.
