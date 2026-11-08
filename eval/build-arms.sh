#!/usr/bin/env bash
# Assemble the cairn arms from their preambles plus the guide that ships.
#
# `eval/.gitignore` says `arms/cairn_full.md` is "the arm prompt assembled from a preamble
# plus SKILL.md at run time". That sentence described a step nothing performed: the file was
# assembled by hand once and then sat on disk while `skill/SKILL.md` grew, so the backend
# arm spent several rounds handing out a guide 1 379 characters out of date - missing the
# paragraphs on a *searched* miss and on `UNCHECKED` meaning "I did not look", both of which
# bear on scenarios 9 and 10. This is that step, so the comment is now true.
#
# Run it before a round. `skill::arm_tests::every_arm_embeds_the_guide_that_ships` fails if
# an arm that exists has drifted, which is the backstop for forgetting.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
skill="$here/../skill/SKILL.md"
[ -f "$skill" ] || { echo "no $skill" >&2; exit 1; }

for pair in "cairn_preamble.md:cairn_full.md" "cairn_preamble_frontend.md:cairn_full_frontend.md"; do
  pre="$here/arms/${pair%%:*}"
  out="$here/arms/${pair##*:}"
  [ -f "$pre" ] || continue
  { cat "$pre"; printf '\n---\n\n'; cat "$skill"; } > "$out"
  printf '%s  <- %s + skill/SKILL.md (%s bytes)\n' "${pair##*:}" "${pair%%:*}" "$(wc -c < "$out")"
done
