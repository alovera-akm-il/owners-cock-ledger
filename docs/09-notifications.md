# Push Notifications

Two channels, one source of truth. Every notification-worthy event
writes a `notifications` row (`01-data-model.md` §10) — that row is
the durable, always-available record, surfaced as an in-app feed
(`GET /notifications`, `03-api-design.md` §13). Web Push is an
additional, best-effort, opt-in delivery mechanism layered on top of
that same row, not a separate system with its own state.

This split matters because push is inherently unreliable at the
margins — a browser can be closed, a subscription can silently expire,
a user may never have granted permission, and (per the privacy
tradeoff in `05-security-and-privacy.md` §5) some users may choose
not to enable it at all. The in-app feed works regardless of any of
that.

## 1. Why Web Push, specifically

This is a self-hosted web app (`00-overview.md`), not a native mobile
app, so there's no app-store push service (APNs/FCM via a native SDK)
to hook into directly. The web-native equivalent is the **Web Push
API**: a browser-provided service worker registers a
`PushSubscription` (an opaque delivery endpoint chosen by the
browser/OS, plus an encryption keypair), the server sends
VAPID-signed, end-to-end-encrypted messages to that endpoint, and the
browser's own push service wakes the service worker to show a native
OS notification — this works even when the site's tab isn't open, as
long as the browser is running.

**Known limitations**, stated plainly rather than glossed over:
- Requires the frontend to register a service worker and request
  notification permission — a user can decline, and the app must work
  fully without it (hence the in-app feed always existing).
- iOS Safari only supports this for a site added to the home screen
  as an installed PWA (iOS 16.4+), not for a normal browser tab —
  worth calling out given how personal/on-the-go this app's use case
  is.
- Delivery is best-effort; the browser's push service, not this
  server, ultimately decides timing (though in practice it's normally
  near-instant).

## 2. Push subscription lifecycle

1. Frontend registers a service worker and calls
   `GET /notifications/vapid-public-key` (`03-api-design.md` §13) to
   get the public key needed to create a subscription via
   `PushManager.subscribe()`.
2. The resulting `PushSubscription` (`endpoint` + `keys.p256dh` +
   `keys.auth`) is POSTed to `POST /notifications/push-subscriptions`
   and stored (`push_subscriptions`, `01-data-model.md` §10).
3. A user can have several subscriptions (phone, laptop, tablet) —
   each is delivered to independently; there's no "primary device"
   concept.
4. **Cleanup**: when a push send to a given `endpoint` gets back an
   HTTP 404 or 410 from the browser's push service (the standard
   signal that the subscription is gone — uninstalled, permissions
   revoked, browser data cleared), the server deletes that
   `push_subscriptions` row rather than retrying it forever. A user
   can also remove a device explicitly
   (`DELETE /notifications/push-subscriptions/{id}`).

## 3. Trigger matrix

Every row here writes a `notifications` row for the listed
recipient(s); the "Push?" column notes whether it's also worth an
immediate push attempt (as opposed to something fine to only ever see
in the in-app feed next time someone opens it) — a judgment call
about urgency, listed explicitly so it's easy to revisit per-type
later rather than being an implicit all-or-nothing choice.

| Event | Recipient(s) | Notification `type` | Push? |
|---|---|---|---|
| Verification code issued (scheduled) | Submissive | `verification.code_issued` | Yes — the whole point is a timely prompt |
| Verification window expired unclaimed | Keyholder | `verification.missed` | Yes |
| Proof reviewed (verified/redo/failed) | Submissive | `verification.reviewed` | Yes |
| Proof submitted, awaiting review | Keyholder | `verification.proof_submitted` | Yes — this is their queue growing |
| Task assigned (incl. via success/failure escalation) | Submissive | `task.assigned` | Yes |
| Task deadline approaching (§3 in `08-punishments-and-deadlines.md`) | Submissive | `task.deadline_approaching` | Yes |
| Task auto-failed (deadline) or judged-failed | Both | `task.failed` | Yes |
| Task completion proof submitted | Keyholder | `task.proof_submitted` | Yes — this is their queue growing |
| Task completed/revoked | Submissive | `task.resolved` | No — informational, feed is enough |
| Reward given (direct grant, or via a task's `on_success_template_id`) | Submissive | `reward.given` | Yes (this one's meant to feel good in the moment) |
| Confinement timer adjusted (manual, or task/punishment-driven) | Submissive | `confinement.adjusted` | Yes if it's an *extension*; feed-only if it's a reduction (good news travels fine without an interrupt) |
| Time-extension effect applied **via automatic escalation** (`assigned_via='system'`, `01-data-model.md` §4's `keyholder_reviewed_at` starts NULL) | Keyholder | `confinement.time_extension_needs_review` | Yes — this is the one notification in the whole matrix that exists specifically to get a human to look at something the system just did on its own; see `08-punishments-and-deadlines.md` §6. Not sent for a manually-assigned time-extension, since the Keyholder already saw the amount before confirming it themselves. |
| Time-reduction effect applied via automatic escalation (`08-punishments-and-deadlines.md` §6a) | Keyholder | `confinement.time_reduction_needs_review` | Yes — the direct mirror of the row above; not sent for a manually-assigned time-reduction reward |
| Lock timer paused (body includes the Keyholder's `clock_pause_message` when one was given) | Submissive | `confinement.clocks_paused` | Yes |
| Lock timer resumed (release date just moved forward by the pause length) | Submissive | `confinement.clocks_resumed` | Yes — resuming changes a real number (their displayed release date), not just a status label, per `08-punishments-and-deadlines.md` §9. Scoped to the lock timer only — punishment deadlines are unaffected, see that section for why. |
| Lock timer still paused 24h+ later (repeats roughly daily while it stays paused) | **Keyholder** | `confinement.clock_still_paused` | Yes — the one pause-related notification aimed at the Keyholder instead of the submissive, since the failure mode it exists for is the Keyholder forgetting, not the submissive not knowing. See `08-punishments-and-deadlines.md` §9. |
| Safety alert raised | Keyholder | `safety.alert_raised` | Yes — highest urgency in the system, see `04-verification-workflow.md` §5 |
| Safety alert acknowledged | Submissive | `safety.acknowledged` | Yes — they should know they were heard, quickly |
| Invite redeemed (new submissive linked) | Keyholder | `link.established` | No |
| Two-factor authentication enabled | The account holder | `account.2fa_enabled` | Yes — a security-relevant event where the useful failure mode is the real owner noticing something happened that they didn't do, `10-operations.md` §2 |
| Two-factor authentication disabled | The account holder | `account.2fa_disabled` | Yes — same reasoning, arguably more urgent since this *reduces* protection |
| Recovery codes regenerated | The account holder | `account.2fa_recovery_codes_regenerated` | Yes |
| Toy retirement requested | Keyholder | `toy.retirement_requested` | No — not urgent, sits in the review-adjacent feed until the Keyholder gets to it |
| Toy retired or a retirement request declined | Submissive | `toy.retirement_resolved` | No |
| Check-in submitted (standalone or task-attached, not live) | Keyholder | `checkin.submitted` | Yes if `color='red'`, otherwise No — matches the "urgent only" posture the rest of this matrix uses for anything that's actually a safety-adjacent signal |
| Check-in logged at `color='red'`, template has `auto_escalate_on_red=false` (or unset) | Keyholder | `checkin.red_flag` | Yes, always — the one check-in notification that's never merely feed-only, regardless of context |
| Check-in transitions into `color='red'`, template has `auto_escalate_on_red=true` | Keyholder | `safety.alert_raised` (not a separate `checkin.red_flag`) | Yes — see `13-checkins.md` §6. The auto-raised safety alert's own notification covers "look at this now," so this case sends that one instead of also sending `checkin.red_flag` for the same event — two pushes for one red check-in would be noise, not extra safety |
| Live play-session check-in updated | *(no notification — delivered via the SSE stream instead, `13-checkins.md` §5)* | | |
| Play session assigned | Submissive | `play_session.assigned` | Yes |
| Play session started (by the other party) | Whichever role didn't start it | `play_session.started` | Yes — the other party should know a live session is now underway |
| Play session mid-session check-in due (per its schedule) | Both | `play_session.checkin_due` | Yes |
| Play session ended, awaiting judgement | Keyholder | `play_session.pending_judgement` | Yes — this is their queue growing, same reasoning as a proof submission |
| Play session judged/completed | Submissive | `play_session.judged` | Yes |
| Reward redemption requested | Keyholder | `points.redemption_requested` | Yes — this is their queue growing |
| Reward redemption approved/denied | Submissive | `points.redemption_resolved` | Yes |

This table is the place to add a row when a new domain gets built —
the pattern (event → row → optional push) doesn't change per-feature.

## 4. Delivery mechanics

- **Payload**: small JSON (`{type, title, body, link_path}` — the
  same fields as the stored `notifications` row minus the DB-only
  ones), encrypted client-side-key/server-side-implementation per
  RFC 8291 using the subscription's `p256dh`/`auth` keys, sent with a
  VAPID (RFC 8292) JWT signed by the server's private key so the push
  service can verify the sender without a shared secret per
  subscription.
- **What the push relay can and can't see**: the payload content is
  end-to-end encrypted — the browser vendor's push infrastructure
  (e.g. Google's or Mozilla's push relay, whichever the user's
  browser uses) cannot read notification content, only routing
  metadata (the target endpoint, approximate payload size, timing).
  This is the basis for the privacy carve-out in
  `05-security-and-privacy.md` §5.
- **Retry**: a single send attempt per event; on failure other than
  404/410 (transient network/relay issues), the notification is not
  re-queued for retry in v1 — the in-app feed row already exists
  regardless, so the information isn't lost, just not pushed. A retry
  queue is a reasonable enhancement if silent push failures turn out
  to matter in practice, not built preemptively.
- **Fan-out**: a user with multiple subscriptions gets the push sent
  to all of them in parallel; one device's failure doesn't affect
  delivery to the others.

## 5. In-app feed

`GET /notifications` (`03-api-design.md` §13) is the fallback and, for
a user who never enables push, the *only* channel — it must be a
first-class, complete way to use the app, not an afterthought next to
push. `link_path` on each notification lets the frontend deep-link
straight to the relevant screen (the specific submission to review,
the specific punishment, etc.) rather than dumping the user on a
generic dashboard.

## 6. What this doesn't cover

- Email digests / SMS — not part of this architecture; Web Push and
  the in-app feed are the only channels. Could be added later as
  additional consumers of the same `notifications` rows
  (`06-future-extensions.md` §4 already anticipated this shape).
- Per-notification-type preferences (e.g. "push me for safety alerts
  only, feed-only for everything else") — v1 is all-or-nothing per
  device (a user either has push subscriptions or doesn't); granular
  per-type toggles are a reasonable UI addition later that needs no
  schema change (`notifications.type` already exists to filter on),
  just a new preferences table and a check before sending — not
  designed further here since it's additive.