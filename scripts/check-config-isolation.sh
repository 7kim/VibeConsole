#!/usr/bin/env bash
# v7.7 ticket 244: does the test suite leave the real config alone? (ticket 222's method)
#   scripts/check-config-isolation.sh          the whole suite once
#   scripts/check-config-isolation.sh --each   also each test alone, naming the ones that read
# The suite runs with XDG_CONFIG_HOME pointed at a temporary sentinel folder (the shipped
# defaults plus a prompts/ file), all access times set to 2000-01-01. On a relatime or
# strictatime mount a read moves an access time older than a day, so reads show; under
# noatime it says reads can't be seen and checks writes only. It prints the files read and
# the files created, changed or removed, and exits 1 on any write, here or in the real config
# (which it only hashes, before and after). The sentinel is removed at the end.
set -euo pipefail
cd "$(dirname "$0")/.."
each=0
if [ "${1:-}" = "--each" ]; then each=1; fi

real="${XDG_CONFIG_HOME:-$HOME/.config}/vibeconsole"
sentinel=$(mktemp -d)
trap 'rm -rf "$sentinel"' EXIT
mkdir -p "$sentinel/vibeconsole/prompts"
cp core/defaults/*.json "$sentinel/vibeconsole/"
echo "Sentinel prompt." > "$sentinel/vibeconsole/prompts/sentinel.md"

hashes() { [ -d "$1" ] && (cd "$1" && find . -type f -print0 | sort -z | xargs -0 -r sha256sum) || true; }
age() { find "$sentinel" -type f -exec touch -a -d 2000-01-01 {} +; }
read_files() { find "$sentinel" -type f -newerat 2001-01-01 | sed "s|$sentinel/||" | sort; }

atime=yes
if findmnt -no OPTIONS -T "$sentinel" | tr , '\n' | grep -qx noatime; then
    atime=no
    echo "Reads can't be seen: $sentinel is on a noatime mount. Checking writes only."
fi

real_before=$(hashes "$real")
sentinel_before=$(hashes "$sentinel")
age
echo "Running the test suite against the sentinel…"
XDG_CONFIG_HOME="$sentinel" cargo test --locked -q --workspace >/dev/null
if [ "$atime" = yes ]; then
    echo "Read by the suite:"
    read_files | sed 's/^/  /' | grep . || echo "  nothing"
fi

if [ "$each" = 1 ] && [ "$atime" = yes ]; then
    echo "Tests that read it, each run alone:"
    while read -r binary; do
        "$binary" --list 2>/dev/null | sed -n 's/: test$//p' | while read -r name; do
            age
            XDG_CONFIG_HOME="$sentinel" "$binary" --exact "$name" -q >/dev/null 2>&1 || true
            if [ -n "$(read_files)" ]; then
                echo "  $(basename "$binary" | sed 's/-[0-9a-f]*$//') $name"
            fi
        done
    done < <(cargo test --locked -q --workspace --no-run --message-format=json 2>/dev/null |
        uv run --no-project python -I -c 'import sys, json
for line in sys.stdin:
    try: m = json.loads(line)
    except ValueError: continue
    if m.get("profile", {}).get("test") and m.get("executable"): print(m["executable"])')
fi

status=0
written=$(diff <(echo "$sentinel_before") <(hashes "$sentinel") || true)
if [ -n "$written" ]; then
    echo "WRITTEN in the sentinel (created, changed or removed):"
    echo "$written" | sed -n 's/^[<>] [0-9a-f]* *\.\//  /p' | sort -u
    status=1
fi
if [ "$real_before" != "$(hashes "$real")" ]; then
    echo "CHANGED: the real config $real differs after the run"
    status=1
fi
if [ "$status" = 0 ]; then
    echo "No writes: the sentinel and the real config are unchanged."
fi
exit "$status"
