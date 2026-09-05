# Owner's Cock Ledger

A self-hosted web app for a Keyholder and the submissive(s) in their care to
track chastity device status, exchange time-stamped proof, and manage a
catalog of tasks, rewards, and punishments tied to verification outcomes.
It's built for one small, known set of accounts on a private server or home
network — not a public multi-tenant service.

Server-rendered pages (Askama + jQuery, no client-side framework) over a
JSON API, backed by SQLite. Every mutating action is audit-logged; every
session is server-revocable; every background task reports its own health.

## Status

Phases 0–4 of the [implementation roadmap](docs/15-implementation-roadmap.md)
are built and tested:

- **Identity** — invite-only signup, Argon2id passwords, server-side
  sessions, roster management.
- **Core loop** — chastity devices, confinement sessions (pause/resume/timer
  adjustments), scheduled and on-demand verification codes, proof submission
  (photo/video/voice, with EXIF stripped) and review.
- **Tasks, rewards & punishments** — a reusable catalog, assignment state
  machines, escalation chains, and an automatic deadline sweeper.
- **Operations** — self-service session management, password reset (admin
  CLI + redeem endpoint), TOTP two-factor auth with recovery codes,
  Keyholder-issued scoped API tokens (session *or* bearer-token auth on
  every endpoint), a live backup subcommand, and admin CLI recovery
  commands.

Phases 5–7 (notifications, points/toy-catalog/check-ins/play-sessions,
real-time check-ins) are designed in `docs/` but not yet built.

## Getting started

```sh
cargo build --release
```

The server reads/writes `~/.config/owners-cock-ledger/` by default
(override with `DATA_DIR`), and listens on `127.0.0.1:8080` by default
(override with `LISTEN_ADDR`).

Every account after the first is created via invite, so bootstrap the first
Keyholder from the command line:

```sh
./target/release/owners-cock-ledger admin create-keyholder you@example.com
```

This prints a one-time temporary password — log in with it and change it
immediately from the Account & security page. Then start the server:

```sh
./target/release/owners-cock-ledger
```

By default this only serves plain HTTP, which is fine for `127.0.0.1`
directly on the same machine. The session cookie is `Secure`, though, so a
browser silently won't store it over plain HTTP from anywhere else (a LAN
IP, a tailnet hostname) — either set `INSECURE_COOKIES=1` (session cookie
travels unencrypted) or, better, terminate real TLS in front with Tailscale
serve or a Caddy + mkcert reverse proxy. See `docs/10-operations.md` §6 for
both setups.

### Admin CLI

Local-host-only, never HTTP-reachable — see `docs/10-operations.md` §5 for
the trust model. Each command (except `backup`) prompts for confirmation
unless run with `--yes`.

| Command | Purpose |
| --- | --- |
| `admin create-keyholder <email>` | Bootstrap the first account on a fresh deployment |
| `admin reset-password <email>` | Issue a one-time password-reset token |
| `admin disable-2fa <email>` | Force-clear 2FA (lost device + exhausted recovery codes) |
| `admin unlock-account <email>` | Clear a login lockout immediately |
| `admin force-end-link <link_id>` | Unilaterally end a Keyholder/submissive link |
| `backup --out <dir>` | Live, online-safe copy of the database + blob directory |

## Development

```sh
cargo test                              # 170+ integration + unit tests
cargo clippy --all-targets -- -D warnings
cargo fmt
```

Frontend assets are vendored locally (no CDN dependency). `static/css/app.css`
is a **compiled, purged Tailwind build** — it is not regenerated
automatically. After editing any file under `templates/`, rebuild it:

```sh
bash tailwind/build.sh
```

## Documentation

The full design — data model, API surface, roles/permissions, security
posture, and every subsystem's reasoning — lives in [`docs/`](docs/),
starting with [`docs/00-overview.md`](docs/00-overview.md). Interactive UI
mockups (including subsystems not yet built) are in [`mockups/`](mockups/).

## License

Apache 2.0 — see [`LICENSE`](LICENSE).
