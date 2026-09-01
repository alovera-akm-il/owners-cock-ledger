# Architecture Documentation

Design-only documentation for owners-cock-ledger. No application code
lives here — see `mockups/` for the resulting UI mockups.

Read in order:

1. [`00-overview.md`](00-overview.md) — purpose, goals, non-goals, roles, high-level component diagram.
2. [`01-data-model.md`](01-data-model.md) — entities, relationships, SQLite schema sketch, ERD.
3. [`02-roles-and-permissions.md`](02-roles-and-permissions.md) — full authorization matrix and edge cases.
4. [`03-api-design.md`](03-api-design.md) — REST API surface.
5. [`04-verification-workflow.md`](04-verification-workflow.md) — code issuance, proof submission, review, and the failed→punishment path in detail.
6. [`05-security-and-privacy.md`](05-security-and-privacy.md) — auth, storage, transport, and data-handling stance.
7. [`06-future-extensions.md`](06-future-extensions.md) — play sessions and other planned growth, and how the current design accommodates them.
8. [`07-tech-stack.md`](07-tech-stack.md) — concrete crate/tooling choices and project layout.
9. [`08-punishments-and-deadlines.md`](08-punishments-and-deadlines.md) — escalation ladders (success and failure), the deadline sweeper, time-extension/time-reduction effects, and the confinement timer.
10. [`09-notifications.md`](09-notifications.md) — push notification trigger matrix, Web Push delivery, and the in-app feed.
11. [`10-operations.md`](10-operations.md) — self-service session management, two-factor authentication, background-task health monitoring, and the backup approach.
12. [`11-tasks-and-rewards.md`](11-tasks-and-rewards.md) — task templates with photo/video/voice proof, the success/failure escalation design, and the points system.
13. [`12-toy-catalog.md`](12-toy-catalog.md) — the toy/BDSM-gear catalog, field rationale, and the add/retire permission split.
14. [`13-checkins.md`](13-checkins.md) — check-in templates, the configurable field-type system, the color safety signal, and real-time live-session updates.
15. [`14-play-sessions.md`](14-play-sessions.md) — live and retrospective play-session logging, templates, toy attachment, and the judgement-before-completion workflow.
16. [`15-implementation-roadmap.md`](15-implementation-roadmap.md) — honest evaluation of the design's current state and a phased build order, read once the doc set above is settled and it's time to actually start writing code.
