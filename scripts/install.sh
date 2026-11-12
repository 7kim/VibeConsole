#!/usr/bin/env bash
# Build the release binary and install it to ~/.local/bin.
# Builds first, so a failed build leaves VibeConsole running. Then it stops every running
# VibeConsole: the login service, and any terminal session (SIGTERM is its clean quit: keys
# released, controller feedback restored). It installs, then starts the login service again
# if it was running. A terminal session isn't reopened.
set -euo pipefail
cd "$(dirname "$0")/.."
unit=vibeconsole.service
active() { systemctl --user is-active --quiet "$unit"; }

cargo build --release --locked

restart=0
if active; then restart=1; systemctl --user stop "$unit"; fi
if pids=$(pgrep -x vibeconsole); then
    echo "Stopping VibeConsole terminal session(s): $(echo $pids)"
    kill -TERM $pids
    for _ in $(seq 50); do pgrep -x vibeconsole >/dev/null || break; sleep 0.2; done
    if pgrep -x vibeconsole >/dev/null; then
        echo "install: VibeConsole is still running ($(pgrep -x vibeconsole | xargs)); quit it and run again" >&2
        exit 1
    fi
fi
# A quitting terminal session hands back to the login service (ticket 20): stop that too.
if active; then restart=1; systemctl --user stop "$unit"; fi

./target/release/vibeconsole install
if [ "$restart" = 1 ]; then
    systemctl --user start "$unit"
    sleep 2
    echo "Login service: $(systemctl --user is-active "$unit")"
fi
