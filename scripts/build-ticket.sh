#!/usr/bin/env bash
# The version and ticket(s) `vibeconsole --version` names, from commit subjects on stdin
# (newest first): the first subject that starts with "v<version> <ticket>" ("v7 128"), with
# "ticket"/"tickets" between them ("v7.8 ticket 38:", "v7.8 tickets 61, 62:", v7.8 ticket 63),
# or with a version folder name ("mise 10", mise ticket 11: docs/<version>/tickets,
# git-ignored, so a clone without docs/ matches only v<number>), then " +N" when N commits
# follow it. Prints nothing on no match.
set -euo pipefail
cd "$(dirname "$0")/.."
versions=$(for d in docs/*/tickets; do if [ -d "$d" ]; then basename "$(dirname "$d")"; fi; done | { grep -vx skipped || true; } | sed 's/[.]/[.]/g' | paste -sd'|')
names="v[0-9.]+${versions:+|$versions}"
# v7.8 ticket 78: the subjects above the match are the commits since it, shown as " +N".
# awk reads every line, so `git log` upstream never meets a closed pipe.
awk -v re="^($names) (tickets? )?[0-9]" '!found && $0 ~ re { print NR - 1 "\t" $0; found = 1 }' |
    sed -E "s/^([0-9]+)\t(($names) (tickets? )?[0-9, ]*[0-9]).*/\\2 +\\1/; s/ \+0$//"
