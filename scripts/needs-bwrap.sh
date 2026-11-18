#!/usr/bin/env bash
# v7.7 ticket 265: `mise run pty` starts here, so no check runs half-way. Without bwrap a
# connected controller could reach the checks' copies of VibeConsole and send real shortcuts,
# so nothing runs, unless VIBECONSOLE_PTY_NO_BWRAP=1 (a machine with no input devices; the
# same opt-out and warning as scripts/check-terminal.py).
# Run it: bash's `command -v` also reports a bwrap on PATH that can't be executed.
if bwrap --version >/dev/null 2>&1; then
    exit 0
fi
if [ "${VIBECONSOLE_PTY_NO_BWRAP:-}" = 1 ]; then
    echo "VIBECONSOLE_PTY_NO_BWRAP=1: running without bwrap; any connected controller can reach these scenarios. You take that risk."
    exit 0
fi
echo "bwrap not found: a connected controller could reach these scenarios and send real shortcuts, so nothing was run. Install it: sudo dnf install bubblewrap (or set VIBECONSOLE_PTY_NO_BWRAP=1 on a machine with no input devices)." >&2
exit 1
