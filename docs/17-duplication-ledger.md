# Duplication Ledger

A running list of pages/sections where keyholder-facing and
submissive-facing content overlaps substantially enough that it's
worth consolidating behind one shared implementation, in the same
spirit as `16-mockup-implementation-gaps.md` tracks divergence from the
mockups. Five earlier rounds of this are already done, referenced here
for context rather than re-litigated:

- Unify the four confirm/detail modals behind one shared shell (`f8dbbe4`)
- Extract the shared top nav into one partial across all 20 pages (`05ec59e`)
- Consolidate the play-session status and check-in color maps into one source (`8c79504`)
- Share `fieldInput()` between `submit_checkin.html` and `checkin_live.html` (`710fe18`)
- Merge `proof_review.html` and `submissive_review.html` behind one `Option<submissive_id>` (`112d375`)
- Merge the three overlapping "limits" concepts on the account page (free-text hard/soft limits duplicated per role, a read-only mirror of the other party's limits, and a submissive-only structured rating grid) into one symmetric "Limits & boundaries" section both roles share, with role-conditional bits kept to "which endpoint to hit" and "whether a read-only reference box for the other party is shown" (`11976e8`)

This document is the next batch — found by a fresh audit pass, **not
yet fixed**. Read-only findings; no code changed by this document
itself.

## 1. Summary punch list, by size of overlap

| # | Finding | Page(s) | Size | Status |
|---|---|---|---|---|
| 1 | Nearly the entire file is byte-identical between the two roles' statistics pages — same inline script verbatim, only the endpoint and one label word differ | `keyholder_submissive_statistics.html`, `submissive_statistics.html` | Large | **Fixed** |
| 2 | Same toy-inventory feature, ~80% overlapping markup and JS (form fields, card-rendering function, device-list fetch/edit flow) | `toy_catalog.html`, `submissive_toys.html` | Large | **Fixed** |
| 3 | Three drill-down pages each hand-roll their own ~30-line nav bar (logo, bell, role badge, avatar, logout) instead of using the shared nav partial | `play_session_detail.html`, `checkin_live.html`, `submit_checkin.html` | Medium | **Fixed** |
| 4 | Confinement lock/unlock widget rendered by both roles for the same session, same "owner reads/writes, other party reads a mirror" shape as the limits merge | `submissive_dashboard.html`, `submissive_detail.html` | Medium | **Partially fixed** |

## 2. Statistics pages (largest overlap, fixed)

`templates/keyholder_submissive_statistics.html` and
`templates/submissive_statistics.html` are nearly byte-identical: the
nav-adjacent header and `<select>`, and the *complete* inline
`<script>` — `fmtDuration`, `statCard`, `dl`, `renderStatistics`,
`loadStats` (~100 lines of jQuery/DOM-building logic) — are
copy-pasted verbatim between the two files. The only real differences:

- The AJAX endpoint: `/api/v1/keyholder/submissives/{id}/stats` vs.
  `/api/v1/submissive/stats`.
- The page title.
- One label: "Your personal best" vs. "Personal best".

This is the purest "one thing, two files" case found — stronger than
the account-page limits case was, since there the two roles at least
had genuinely different editable fields. Here the content is
identical; only who it's about and which URL to fetch differ.

**Fixed.** The two template files stay separate (they carry genuinely
different Askama contexts — nav/subnav/back-link markup for the
keyholder's view, none of that for the submissive's own) but the
~100-line script is now one file, `static/js/statistics.js`, loaded by
both via `<script src>` instead of inlined twice. Each template's only
role-specific footprint is two `data-*` attributes on `#stats-root`
(`data-endpoint`, `data-best-label`) that the shared script reads
rather than branching on role itself. Caught along the way: moving
Tailwind classes out of an inline `<script>` block and into a separate
`static/js/*.js` file drops them from the purge scan, since
`tailwind/tailwind.config.js`'s `content` glob only covered
`templates/**/*.html` — fixed by adding `static/js/**/*.js` to that
glob, which matters for every later item on this list too, since all
of them involve the same "extract inline script to a shared file"
move.

## 3. Toy catalog (keyholder) vs. submissive toys (fixed)

`templates/toy_catalog.html` and `templates/submissive_toys.html`
render the same toy-inventory feature with ~80% overlapping markup and
JS: identical form fields, an identical card-rendering function
(`renderToy`), identical device-list fetch/edit flow. The differences
that remain are legitimately role-specific, not incidental:

- Endpoint prefix: `/keyholder/submissives/{id}/toys` vs. `/submissive/toys`.
- Keyholder can retire a toy instantly; a submissive can only request
  removal (the Keyholder approves/declines).
- A keyholder-only advisory: cross-checking a toy against the
  submissive's hard/soft limit ratings and surfacing a warning.
- A "show retired" toggle (keyholder-side management view).

Good fit for the same pattern used on the limits merge: one shared
template, with the role-conditional surface limited to which
endpoint/verb to hit and which of the above extra affordances show up.

**Fixed.** The two template files stay separate (different Askama
contexts, different grid-column count, different placeholder copy on
a few form fields — none of that is logic, so it stayed as literal
per-template markup), but the ~230-line shared script — form
reset/edit, photo upload/remove, save, card rendering, the list load —
is now one file, `static/js/toy_catalog.js`. Role-specific behavior
(the size/storage/usage-notes fields on the card, retire-instantly vs.
request-removal, the decline/approve actions, the advisory limit
cross-check) branches on an `IS_KEYHOLDER` flag and a couple of
optional endpoint data-attributes read once at the top of the file,
rather than existing as two parallel copies of the same functions.

## 4. Duplicated hand-rolled mini-nav (fixed)

`play_session_detail.html`, `checkin_live.html`, and
`submit_checkin.html` are drill-down pages reached by clicking in
rather than from the main nav, and by the shared nav partial's own doc
comment they're deliberately excluded from it. But instead of just
omitting nav chrome, each of the three independently hand-rolls its
own ~30-line `<nav>` — the same `is_keyholder`-branched logo/home-link,
notification bell, role badge, avatar, and logout button — three
parallel copies of the same markup rather than one. This is exactly
the kind of leftover the shared-nav-partial merge (`05ec59e`) didn't
catch, since these three pages opted out of the full nav rather than
using a trimmed variant of it. A small "minimal/back-link nav" partial
(logo + back-link + the same account/notification cluster, no primary
link set) would collapse this to one implementation.

**Fixed** as `templates/partials/minimal_nav.html`, taking `back_href`/
`back_label` (page-specific) and `nav_variant` ("full" — bell, role
badge, logout, used by `submit_checkin.html` and
`play_session_detail.html`; or "live" — a Live badge instead, no bell/
badge/logout, used only by `checkin_live.html`'s reduced chrome, which
turned out to genuinely be a smaller shape than the other two rather
than a plain copy). Caught along the way: all three hand-rolled navs
predated the primary-nav redesign that turned the profile corner into
a dropdown — they'd fallen out of sync with `partials/nav.html`'s
current shape (still separate always-visible name/avatar/logout
elements instead of one dropdown trigger). Left that divergence alone
for this pass rather than bundling an unrelated visual change into a
duplication fix; worth a follow-up if these three should adopt the
dropdown too.

## 5. Confinement-status widget (partially fixed)

`submissive_dashboard.html` and `submissive_detail.html` both render
the same locked/unlocked concept for the same confinement session —
same underlying conditions (`locked`, `locked_elapsed_text`,
`device_name`, `clock_paused`), the same shared `countdown_clock.html`
partial, and near-identical surrounding markup. The keyholder's view
(`submissive_detail.html`) adds control buttons (pause/resume/adjust/
end/start) and the unreviewed-adjustment callout; the submissive's
view (`submissive_dashboard.html`) is read-mostly plus its own
"request a code" affordance. Structurally this is the same "owner
reads/writes, other party reads a mirror" shape as the limits merge —
just for confinement status instead of limits, and worth the same
treatment.

**Investigated in more depth than the summary above suggests, and only
partially fixed.** A closer read found the two widgets diverge more
than "same shape, different permissions": the layouts differ (a
side-by-side flex row on the dashboard vs. a stacked column on the
detail page), the field sets differ (weekly punishment-added text and
the unreviewed-adjustments loop are keyholder-only, with no dashboard
equivalent), and one is read-only where the other has four action
buttons plus a start-session control. Forcing the whole widget behind
one shared partial would have meant either flattening those layout/
feature differences (a real behavior change) or writing a partial
riddled with enough conditionals to reproduce two different shapes,
which isn't really consolidation. Raised with the user rather than
picking unilaterally; the answer was to extract only the part that's
genuinely identical.

**Fixed**: the "🟢 Locked · 2d 4h / Device: X" header, via
`partials/confinement_status_header.html`. Caught in the process: the
two pages already disagreed on font-weight for the "Locked" label
(`font-semibold` on the dashboard, `font-medium` on the detail page) —
a pre-existing inconsistency, not something this pass introduced.
Normalized to `font-medium` (matching the detail page) as part of
sharing the markup, confirmed with the user first since picking one
technically changes the other page's rendered output, however
trivially. The countdown, action buttons, punishment-added line, and
surrounding layout remain separate per page — genuinely different
content, not duplication.

## 6. Verified clean — no action needed

Checked as part of this audit and confirmed to have no residual
duplication:

- `proof_review.html` — genuinely unified via `Option<submissive_id>`,
  no leftover per-role branching.
- The shared nav partial (`partials/nav.html`) — used cleanly by 19 of
  25 templates; the 6 exceptions are pre-auth pages (legitimately
  nav-less) plus the 3 pages in §4 above.
- The modal shell and status/check-in color-map consolidations — no
  leftover duplicate color maps or modal markup found elsewhere in the
  codebase.
- `fieldInput()` sharing — both `submit_checkin.html` and
  `checkin_live.html` call the one copy in `app-shell.js`; no local
  redefinitions crept back in.

## 7. Not findings — single-role by design

Excluded because they're genuinely role-specific, not overlapping:
`catalog.html`, `checkin_templates.html`, `recurring_tasks.html`,
`play_session_templates.html`, `limits_catalog.html` (keyholder-only
management/CRUD surfaces with no submissive-facing counterpart page),
`redemption_requests.html` (keyholder-only approval queue — the
submissive's points/ledger widget on their dashboard is a different
concept, not a duplicate of this), and `assignment_proof.html`/
`submit_proof.html` (submissive-only).
