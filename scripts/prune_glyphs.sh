#!/usr/bin/env bash

# prune_glyphs.sh
#
# The full Noto Sans Regular SDF glyph set is 256 ranges (~34MB), most of it
# CJK — dead weight for an Alps-focused app whose labels are Latin-script.
# This keeps only the ranges needed for Western/Central European labels and
# deletes the rest from both the working tree and the git index.
#
#   0-255       Basic Latin + Latin-1 Supplement (ä ö ü ß é è …)
#   256-511     Latin Extended-A/B (š ž ő ć …)
#   512-767     IPA extensions + spacing modifier letters
#   768-1023    Combining diacritical marks + Greek
#   7680-7935   Latin Extended Additional
#   8192-8447   General punctuation (– — ' " …) + super/subscripts + currency
#
# Re-run is a no-op. To restore the full set, re-download Noto Sans SDF
# glyphs from https://github.com/openmaptiles/fonts (OFL 1.1).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "${SCRIPT_DIR}/.."

GLYPH_DIR="public/glyphs/Noto Sans Regular"
KEEP="0-255 256-511 512-767 768-1023 7680-7935 8192-8447"

deleted=0
for f in "${GLYPH_DIR}"/*.pbf; do
  base="$(basename "$f" .pbf)"
  keep=false
  for k in ${KEEP}; do
    [ "$base" = "$k" ] && keep=true && break
  done
  if [ "$keep" = false ]; then
    git rm -q --ignore-unmatch -f "$f" 2>/dev/null || rm -f "$f"
    deleted=$((deleted + 1))
  fi
done

echo "Pruned ${deleted} glyph range(s); kept: ${KEEP}"
du -sh "${GLYPH_DIR}"
