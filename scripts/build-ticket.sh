#!/usr/bin/env bash
# The version and ticket(s) `vibeconsole --version` names, from commit subjects on stdin
# (newest first): the first subject that starts with "v<version> <ticket>" ("v7 128"), with
# "ticket"/"tickets" between them ("v7.8 ticket 38:", "v7.8 tickets 61, 62:", v7.8 ticket 63),
# or with a version folder name ("mise 10", mise ticket 11: docs/<version>/tickets,
# git-ignored, so a clone without docs/ matches only v<number>). Prints nothing on no match.
set -euo pipefail
cd "$(dirname "$0")/.."
versions=$(for d in docs/*/tickets; do [ -d "$d" ] && basename "$(dirname "$d")"; done | grep -vx skipped | sed 's/[.]/[.]/g' | paste -sd'|')
names="v[0-9.]+${versions:+|$versions}"
# awk reads every line, so `git log` upstream never meets a closed pipe.
awk -v re="^($names) (tickets? )?[0-9]" '!found && $0 ~ re { print; found = 1 }' |
    sed -E "s/^(($names) (tickets? )?[0-9, ]*[0-9]).*/\\1/"
