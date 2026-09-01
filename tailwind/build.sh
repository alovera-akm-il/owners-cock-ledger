#!/usr/bin/env bash
# Compiles tailwind/input.css against templates/**/*.html into
# static/css/app.css (07-tech-stack.md §3: "compiled at build time... no
# runtime CDN dependency"). Not wired into `cargo build` — CSS output
# doesn't change unless a template's classes change, so this is a
# manual step, same posture as the rest of this stack's "no implicit
# build-time external dependency" preference. Re-run after editing any
# template.
set -euo pipefail
cd "$(dirname "$0")"

BIN=../.build-tools/tailwindcss
if [ ! -x "$BIN" ]; then
  mkdir -p ../.build-tools
  # Linux x64 standalone CLI, pinned to the same v3.4.17 the mockups
  # vendor (mockups/vendor/tailwind.js) — swap the asset name for other
  # platforms, see https://github.com/tailwindlabs/tailwindcss/releases
  curl -sL -o "$BIN" \
    https://github.com/tailwindlabs/tailwindcss/releases/download/v3.4.17/tailwindcss-linux-x64
  chmod +x "$BIN"
fi

"$BIN" -i ./input.css -o ../static/css/app.css --minify -c ./tailwind.config.js
