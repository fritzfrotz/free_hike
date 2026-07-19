#!/usr/bin/env bash

# add_spdx_headers.sh
#
# Mechanically prepends "// SPDX-License-Identifier: Apache-2.0" to every
# first-party source file in src/ and freehike-core/. Idempotent: files that
# already carry an SPDX tag anywhere in the first three lines are skipped.
#
# Deliberately excluded:
#   - freehike-core/*/target/          (build output)
#   - freehike-core/ffi/bindings/      (UniFFI-generated Swift/Kotlin/C headers)
#   - src/valhalla.js                  (vendored Emscripten output, MIT upstream)
#   - JSON/config files                (comments are not valid there)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "${SCRIPT_DIR}/.."

HEADER='// SPDX-License-Identifier: Apache-2.0'
count=0

while IFS= read -r -d '' file; do
  if head -n 3 "$file" | grep -q 'SPDX-License-Identifier'; then
    continue
  fi
  tmp="$(mktemp)"
  printf '%s\n' "$HEADER" | cat - "$file" > "$tmp"
  mv "$tmp" "$file"
  count=$((count + 1))
  echo "tagged: $file"
done < <(
  find src \( -name '*.ts' -o -name '*.tsx' \) -type f -print0
  find freehike-core \( -name '*.rs' \) -type f \
    -not -path '*/target/*' \
    -not -path '*/bindings/*' \
    -print0
)

echo "Done. Tagged ${count} file(s)."
