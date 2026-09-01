# Toy Catalog

Schema reference: `01-data-model.md` §13 (`toys`). This document
covers what fields a toy record should hold and why, and the
permission split between roles.

## 1. Why per-submissive, not a shared Keyholder catalog

`toys` is scoped by `submissive_id`, the same way `chastity_devices`
is — a physical toy belongs to a person, not to a reusable definition
the way `reward_punishment_templates` or `checkin_templates` do.
This matters for the play-session design (`14-play-sessions.md`): a
*template* can only suggest toy *categories* in the abstract, because
the actual inventory differs per submissive — there's no single
"the vibrator" a Keyholder's reusable template could point at across
every submissive they oversee.

## 2. What fields a toy record needs, and why

Researched against how BDSM/sex toy inventories are typically tracked
(care requirements, compatibility, and safety notes matter more here
than for a generic possessions list):

| field | why it matters |
|---|---|
| `name` | required, the only truly mandatory descriptive field |
| `category` | free text with a suggested list in the UI (vibrator, cage, plug, restraint, impact, estim, etc.) rather than a rigid enum — the space of toy categories is large and Keyholder-specific naming ("my punishment paddle" vs. "impact — paddle") shouldn't be forced into a fixed taxonomy |
| `material` | care-relevant (silicone vs. steel vs. wood have different cleaning/storage needs) and the one field with a plausible future tie-in to hard-limit cross-checking (`06-future-extensions.md` §1) if a submissive's limits ever name a material |
| `brand` | useful for reordering/replacement, and some brands have known sizing/quality reputations worth tracking |
| `size_notes` | free text rather than structured — length/diameter/circumference conventions vary so much by category that a fixed set of numeric fields would fit some toys and be meaningless for others |
| `color` | mostly for quick visual identification in a list/photo view |
| `compatible_device_id` | optional FK to `chastity_devices` — for toys that are specifically an attachment/accessory to a particular cage (a specific ring, spacer, or add-on), so the catalog can show "goes with: [cage name]" |
| `storage_location` | practical, especially relevant for anything the submissive is expected to retrieve or present without asking where it is |
| `care_instructions` | cleaning/maintenance notes — free text since requirements vary hugely by material |
| `usage_notes` | safety- and usage-relevant notes distinct from care, e.g. "requires extra lubricant," "check battery before session," "not for prolonged wear" — this is the field a Keyholder would reference before assigning a play session using the toy |
| `tags` | freeform JSON array for anything that doesn't fit the above and benefits from filtering — e.g. `travel-friendly`, `quiet`, `beginner`, `intense` |
| `photo_attachment_path` | one reference photo per toy, stored the same private-blob-storage way as proof/verification photos (`05-security-and-privacy.md` §4) — not embedded as a BLOB column, consistent with every other image reference in this schema |
| `acquired_at` | mostly informational (how long it's been in the catalog) |

Deliberately **not** included: a numeric "intensity" or "size" rating
as a structured field — toy intensity/size conventions are too
inconsistent across categories and brands to encode meaningfully as
a single number; `size_notes`/`usage_notes` free text covers this
better without a schema that implies false precision.

## 3. Permission split: add vs. delete

Requirement: a Keyholder has full CRUD; a submissive can add entries
but cannot delete one without the Keyholder's permission.

This is modeled the same way this schema handles every other
"submissive wants to change something, but the Keyholder is the final
authority" case (compare: ending a link, `06-future-extensions.md`
§2) — not a direct delete right gated by a runtime permission check,
but a **request-then-approve** flow with its own visible state:

- `added_by_user_id` records who created the row (either role).
- A submissive calls a "request removal" action, which sets
  `retirement_requested_at` — the toy is now flagged as
  pending-removal but still fully visible and usable; nothing is
  hidden or soft-deleted yet.
- Only a Keyholder action sets `retired_at` (+ `retired_by_user_id`),
  which is the actual soft-delete — same pattern as
  `chastity_devices.retired_at`. A Keyholder can also retire a toy
  directly, with no prior request, since they have full CRUD.
- A Keyholder can also *decline* a pending request — this doesn't
  need its own column; clearing `retirement_requested_at` back to
  `NULL` (with an audit-log entry recording the decline, per
  `01-data-model.md` §8) is enough state to represent "request seen
  and declined."

Soft-delete rather than a hard delete for the same reason it's used
everywhere else in this schema: a toy referenced by a past
`play_session_toys` row shouldn't become a dangling reference just
because it's no longer in active use — session history should still
be able to say what toy was actually used at the time.

### Why not just let the submissive delete outright

Considered and rejected: it would be the one place in this entire app
where a submissive can unilaterally destroy Keyholder-visible
records (every other submissive-initiated write — proof submissions,
self-reports, added toys — is additive, never destructive). Given how
consistently this schema puts the Keyholder as final authority on
consequential state changes, an unreviewed submissive delete would be
the odd one out, not a natural extension of "submissives can add."

## 4. Full CRUD summary

| action | Keyholder | Submissive |
|---|---|---|
| Add | yes | yes |
| View | yes (their submissives') | yes (their own) |
| Update | yes | yes, for a toy they're allowed to view — editing care notes, tags, etc. doesn't need extra gating |
| Delete | yes, directly | no — can only request (`retirement_requested_at`); Keyholder approves or declines |

Full permission-matrix rows and edge cases are in
`02-roles-and-permissions.md`.

## 5. Gaps considered (first pass)

Not built, but worth recording now rather than rediscovering later:

- **Multiple photos per toy.** `photo_attachment_path` (§2) is a
  single field — fine for "here's what it looks like," not enough for
  something where condition, packaging, or a specific attachment
  configuration matters across more than one angle. The natural fix
  follows a pattern already established elsewhere in this schema
  rather than inventing a new one: a child `toy_photos` table
  (`id`, `toy_id`, `attachment_path`, `position`, `created_at`),
  the same one-to-many shape `proof_attachments` already uses for
  `proof_submissions`. `photo_attachment_path` itself would become
  redundant at that point (the first `toy_photos` row, ordered by
  `position`, covers the "primary photo" case), so this would replace
  the single column rather than sit alongside it.
- **Usage stats** — surfacing which play sessions a toy has actually
  been used in, and deriving "last used" / times-used from that. Not
  a schema gap: `play_session_toys` (`01-data-model.md` §15) already
  records exactly this relationship once play sessions exist, so this
  is a pure reporting/read concern (a query against existing rows,
  the same "no new tables, compute on read" posture `03-api-design.md`
  §15 already takes for the stats endpoints) — worth adding to a
  toy's detail view once there's enough session history for it to be
  more than an empty list.

Deliberately not pursued: consumables/inventory tracking (lube,
batteries, and similar), a wishlist/shopping-list concept, and
barcode/lookup convenience — none of these are being carried forward
as open questions.
