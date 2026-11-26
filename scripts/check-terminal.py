# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""Check real terminal navigation/restoration with isolated files and no desktop output.
Run after cargo build --locked: uv run scripts/check-terminal.py (or mise run pty)
"""
import fcntl
import os
from pathlib import Path
import pty
import re
import select
import shutil
import signal
import struct
import subprocess
import tempfile
import termios
import time

binary = Path(__file__).resolve().parents[1] / "target/debug/vibeconsole"
# v7.7 ticket 150: each tab's rows that open something, by the tab's number key (headings
# are skipped). v7.7 ticket 205: the session starts on 1 Live with the top agent selected
# (claude, from the old config's default agents, once its row appears), else Release all;
# never on Pause. No scenario presses Enter on an agent: that would run the real tmux.
TABS = {
    b"1": ["▶ Resume", "Release all", "Quit"],
    b"2": ["/configure", "/list", "Presets", "Undo last change", "History…", "⚠ Reset settings"],
    # v7.7 ticket 152: 3 Agents, 4 Prompts, 6 and 7 list the config's own items; no scenario opens them.
    b"5": ["/prog-select", "/program-name", "/feedback-idle", "/feedback-check", "/prog-select (Xbox)",
           "/program-name (Xbox)", "/detect", "/get", "MIDI device"],
    b"8": ["Start at login", "Start Wispr Flow with VibeConsole", "Start or repair Wispr Flow", "Sound combos", "Startup variables", "User variables"],
}
# A connected Xbox controller (or any input device) must not reach a scenario: it could run
# the session and send real shortcuts. bwrap hides /dev/input from the app (v7.7 ticket 150).
# v7.7 ticket 178: without bwrap nothing runs, unless the user opts out on a machine with no
# input devices and takes the risk.
# v7.7 ticket 252: its own network namespace, so its copies (which never open a device:
# fake lsusb below) don't meet the login service's per-user lock and run while it does.
# v7.7 ticket 203: nor the real journal: every scenario runs with no journal socket, so each
# also checks that a run without a journal starts and shows no error.
HIDE_INPUT = ["bwrap", "--dev-bind", "/", "/", "--unshare-net", "--tmpfs", "/dev/input", "--tmpfs", "/run/systemd/journal"] if shutil.which("bwrap") else []
if not HIDE_INPUT:
    if os.environ.get("VIBECONSOLE_PTY_NO_BWRAP") != "1":
        raise SystemExit(
            "bwrap not found: a connected controller could reach these scenarios and send real "
            "shortcuts, so nothing was run. Install it: sudo dnf install bubblewrap "
            "(or set VIBECONSOLE_PTY_NO_BWRAP=1 on a machine with no input devices)."
        )
    print("VIBECONSOLE_PTY_NO_BWRAP=1: running without bwrap; any connected controller can reach "
          "these scenarios. You take that risk.")
# The names scenarios use for Live's buttons (v7.7 ticket 151 relabelled them).
LIVE = {"Run": "▶ Resume", "Pause": "❚❚ Pause", "/release-all": "Release all", "/quit": "Quit",
        "Reset settings": "⚠ Reset settings"}
RESUME, PAUSE = "> ▶ Resume".encode(), "> ❚❚ Pause".encode()
# The command screen's hint line ("Tab ←→ 1–0: tabs" or "Tab 1–0 · …"): back at the tabs.
MENU = "1–0".encode()


def go(master, source, target):
    """Open the target row by name: its tab's number key, then Down until it is highlighted
    (v7.7 ticket 177: a tab opens on the row it was left on, so the row is found by name, not
    counted from the first), then Enter; `source` is the highlighted row. Flow commands would
    launch the real application, so no scenario may open them."""
    assert target != "Start or repair Wispr Flow", target
    if source == target:  # already highlighted, and an unchanged row isn't redrawn
        os.write(master, b"\r")
        return
    name = LIVE.get("Run" if target == "Pause" else target, target)
    key = next(key for key, rows in TABS.items() if name in rows)
    os.write(master, key)
    pick(master, "Start Wispr Flow with V" if target == "Start Wispr Flow with VibeConsole" else LIVE.get(target, target))


def signal_app(child, number):
    """Signal VibeConsole itself: under bwrap, `child` is bwrap and the app is its child."""
    found = subprocess.run(["pgrep", "-P", str(child.pid)], capture_output=True, text=True).stdout.split()
    for pid in [int(pid) for pid in found] if HIDE_INPUT else [child.pid]:
        os.kill(pid, number)


def read_until_any(master, tokens, seconds=3):
    data = b""
    deadline = time.monotonic() + seconds
    while not any(t in data for t in tokens) and time.monotonic() < deadline:
        if select.select([master], [], [], 0.1)[0]:
            data += os.read(master, 65536)
    assert any(t in data for t in tokens), (tokens, data)
    return data


def preserved(before, config):
    """v7.7 ticket 197: mappings.tsv is moved into JSON files once and kept as it was, and
    every old mapping is in mappings.json with its control and keys."""
    import json
    if (config / "mappings.tsv").read_bytes() != before:
        return False
    saved = json.loads((config / "mappings.json").read_text())["mappings"]
    # An old line `device kind channel id behaviour keys …` keeps its control and its keys.
    lines = [line.split(b"\t") for line in before.splitlines()[1:] if line.startswith(b"09e8:1049\t")]
    kept = all(any(m["message"] == f[1].decode() and m["channel"] == int(f[2]) and m["id"] == int(f[3])
                   and m["behavior"] == f[4].decode() and m["keys"] == f[5].decode() for m in saved) for f in lines)
    return kept


def read_until(master, token, seconds=3):
    data = b""
    deadline = time.monotonic() + seconds
    while token not in data and time.monotonic() < deadline:
        if select.select([master], [], [], 0.1)[0]:
            data += os.read(master, 65536)
    assert token in data, (token, data)
    return data


def pick(master, label, limit=20):
    """Press Down until the row named `label` is highlighted, then Enter. Finds the row by its
    label, so a menu that gains rows doesn't send the check somewhere else (v8 ticket 135)."""
    token = b"> " + label.encode()
    for _ in range(limit):
        try:
            read_until(master, token, seconds=0.5)
            break
        except AssertionError:
            os.write(master, b"\x1b[B")
    else:
        raise AssertionError(f"no row named {label!r} within {limit} rows")
    os.write(master, b"\r")


# v7.7 ticket 213: what the shell may leave queued when VibeConsole starts: the Enter that
# started it, a CRLF, a kitty key-release report, or a held Enter's repeats (31 ms apart).
STARTUP = {"startup_cr": b"\r", "startup_crlf": b"\r\n", "startup_kitty_release": b"\x1b[13;1:3u",
           "startup_enter_held": b"\r" * 10}
for ending in (*STARTUP, "quit", "sigint", "sigterm", "configuration_error", "current_settings", "contexts", "default_running", "run_subcommand", "toggle_run_pause", "modified_navigation", "flow_setting", "panes", "narrow_resize", "toggle_panel", "idle_no_device", "unverified_run"):
    with tempfile.TemporaryDirectory(prefix="vibeconsole-pty-") as directory:
        config = Path(directory) / "vibeconsole"
        config.mkdir()
        path = config / "mappings.tsv"
        contents = b"vibeconsole-mappings-v1\n09e8:1049\tnote\t10\t36\thold\tShift\n"
        if ending == "default_running" or ending in STARTUP:
            contents = b"vibeconsole-mappings-v4\nidle\t-\t-\t-\n"
        if ending == "current_settings":
            contents = b"vibeconsole-mappings-v2\nidle\t0\t240\ton\n09e8:1049\tnote\t10\t36\ttoggle\tShift\tabsolute:1\t240\ton\n"
        if ending == "toggle_panel":
            contents = b"vibeconsole-mappings-v1\n09e8:1049\tnote\t10\t36\ttoggle\tShift\n09e8:1049\tnote\t10\t37\ttoggle\tCtrl+K\n"
        path.write_bytes(contents)
        # No PTY scenario may reach real MIDI, even on a connected developer desktop.
        bin_dir = Path(directory) / "bin"
        bin_dir.mkdir()
        (bin_dir / "lsusb").write_text("#!/bin/sh\nexit 1\n")
        (bin_dir / "lsusb").chmod(0o755)
        # Nor the real login service: Quit asks whether it starts at login (v7.7 ticket 153).
        # The fake says no and logs every call.
        (bin_dir / "systemctl").write_text('#!/bin/sh\necho "$@" >> "$(dirname "$0")/systemctl.log"\nexit 1\n')
        (bin_dir / "systemctl").chmod(0o755)
        if ending == "configuration_error":
            # Every config file's staging file is taken, so a save fails (v7.7 ticket 197). A
            # folder, since a save clears a stale staging file but can't remove a folder.
            for name in ("settings", "devices", "lights", "mappings", "agents"):
                (config / f"{name}.json.tmp").mkdir()
        master, slave = pty.openpty()
        original = termios.tcgetattr(slave)
        os.write(master, STARTUP.get(ending, b""))  # typed before the first frame
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 100, 0, 0))
        child = subprocess.Popen(
            [*HIDE_INPUT, binary, *(["run"] if ending == "run_subcommand" else [] if ending in ("default_running", "toggle_run_pause", "idle_no_device", *STARTUP) else ["--paused"])], stdin=slave, stdout=slave, stderr=slave,
            env=dict(os.environ, PATH=str(bin_dir) + os.pathsep + os.environ["PATH"], XDG_CONFIG_HOME=directory, XDG_RUNTIME_DIR=directory, XDG_DATA_HOME=directory, XDG_DATA_DIRS=directory, XDG_STATE_HOME=directory, TERM="xterm-256color", **({"NO_COLOR": "1"} if ending == "narrow_resize" else {})),
        )
        try:
            # v7.7 ticket 155: these configs are older than 7.7, so What moved shows first; Esc
            # closes it and marks it seen.
            initial_screen = read_until_any(master, [MENU, b"What moved in 7.7"])
            if ending in STARTUP:
                if ending == "startup_enter_held":  # still held after the first frame
                    for _ in range(32):
                        os.write(master, b"\r")
                        # Read while typing, as a terminal does: unread output fills the PTY, the
                        # app blocks mid-redraw and the queued repeats then look like new presses
                        # (v7.8 ticket 73).
                        end = time.monotonic() + 0.031
                        while (left := end - time.monotonic()) > 0:
                            if select.select([master], [], [], left)[0]:
                                initial_screen += os.read(master, 65536)
                # Nothing queued reached a screen: What moved is still open, nothing paused.
                time.sleep(0.3)
                while select.select([master], [], [], 0.2)[0]:
                    initial_screen += os.read(master, 65536)
                assert b"What moved in 7.7" in initial_screen, initial_screen[-300:]
                assert b"PAUSED" not in initial_screen, initial_screen[-300:]
            if b"What moved in 7.7" in initial_screen:
                os.write(master, b"\x1b")
                initial_screen += read_until(master, MENU)
            if ending in STARTUP:
                assert PAUSE not in initial_screen and b"RUNNING" in initial_screen
                assert b"PAUSED" not in initial_screen
                # Idle on Live: nothing changes, so nothing is written between ticks.
                time.sleep(1)
                while select.select([master], [], [], 0)[0]:
                    os.read(master, 65536)
                assert not select.select([master], [], [], 2)[0], "wrote while idle"
                go(master, "Pause", "/quit")
            elif ending == "unverified_run":
                go(master, "Release all", "Run")
                notice = read_until(master, b"Select /prog-select before running")
                assert b"Operation failed" not in notice
                assert b"PAUSED" in initial_screen
                assert b"RUNNING" not in notice
                assert b"Registered VibeConsole keyboard" not in notice
                os.write(master, b"\r")
                read_until(master, MENU)
                go(master, "Run", "/quit")
            elif ending == "toggle_run_pause":
                assert b"RUNNING (detecting program" in initial_screen
                assert PAUSE not in initial_screen and "❚❚ Pause".encode() in initial_screen
                go(master, "Release all", "Pause")
                paused = read_until(master, RESUME)
                assert b"PAUSED" in paused
                go(master, "Run", "Run")
                read_until(master, b"Select /prog-select before running")
                os.write(master, b"\r")
                read_until(master, MENU)
                go(master, "Run", "/quit")
            elif ending == "modified_navigation":
                # Shift+Up until Resume: Live starts on the top agent once its row is
                # there, else on Release all (v7.7 ticket 205).
                for _ in range(10):
                    os.write(master, b"\x1b[1;2A")  # Shift+Up
                    try:
                        read_until(master, RESUME, seconds=0.5)
                        break
                    except AssertionError:
                        pass
                else:
                    raise AssertionError("Shift+Up never reached Resume")
                os.write(master, b"\x1b[1;5B")  # Ctrl+Down
                read_until(master, b"> Release all")
                os.write(master, b"\x1b[1;3A")  # Alt+Up
                read_until(master, RESUME)
                os.write(master, b"\x1b[13;5u")  # Ctrl+Enter
                read_until(master, b"Select /prog-select before running")
                os.write(master, b"\x1b[27;3u")  # Alt+Esc
                read_until(master, RESUME)
                go(master, "Run", "Reset settings")
                read_until(master, b"Type reset")
                os.write(master, b"ReSeT\r")
                read_until(master, "> ⚠ Reset settings".encode())
                assert not list(config.glob("mappings.tsv.bak-*"))
                go(master, "Reset settings", "/quit")
            elif ending == "flow_setting":
                assert b"Wispr Flow not installed" in initial_screen
                go(master, "Run", "Start Wispr Flow with VibeConsole")
                read_until(master, "Start Wispr Flow with VibeConsole · Off".encode())
                assert b'"start_flow": false' in (config / "settings.json").read_bytes()
                go(master, "Start Wispr Flow with VibeConsole", "/quit")
            elif ending == "idle_no_device":
                go(master, "Run", "/feedback-idle")
                read_until(master, b"> No hardware feedback")
                os.write(master, b"\x1b")
                read_until(master, b"> /feedback-idle")
                go(master, "/feedback-idle", "/quit")
            elif ending in ("default_running", "run_subcommand"):
                assert b"RUNNING (detecting program" in initial_screen
                # Startup retains intent, but no output is created before verified selection.
                go(master, "Pause", "/prog-select")
                failure = read_until(master, b"Operation failed")
                if b"unavailable" not in failure:  # error notice, pinned below in the details pane
                    read_until(master, b"unavailable")
                os.write(master, b"\r")
                read_until(master, MENU)
                go(master, "/prog-select", "/quit")
            elif ending == "quit":
                go(master, "Run", "/quit")
            elif ending == "configuration_error":
                go(master, "Run", "/configure")
                read_until(master, b"Configure controls")
                pick(master, "Remove saved assignment")
                read_until(master, b"Saved assignments")
                os.write(master, b"\r")
                pick(master, "Remove assignment")
                read_until(master, b"Operation failed")
                assert path.read_bytes() == contents
                assert b'"keys": "Shift"' in (config / "mappings.json").read_bytes()
                os.write(master, b"\r")
                read_until(master, MENU)
                go(master, "/configure", "/quit")
            elif ending in ("panes", "narrow_resize"):
                go(master, "Run", "/configure")
                wide = read_until(master, b"Tab / Left / Right")
                assert b"Learn a piano key" in wide and b"Learn a knob" in wide
                # v7 ticket 109: a MIDI keyboard pane (with its Xbox controller part) beside the saved one.
                assert re.search(rb"MIDI keyboard[^\n]*Saved assignments", wide)
                assert b"Learn an Xbox button" in wide
                # The cursor row is bold, filled with the accent (9e09296 replaced v7.7 ticket 150's
                # selection background; v7.8 ticket 73).
                assert b";1;38;2;26;27;38;48;2;122;162;247m" in wide or ending == "narrow_resize" or "NO_COLOR" in os.environ
                if ending == "narrow_resize":
                    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 18, 50, 0, 0))
                    narrow = read_until(master, b"Tab / Left / Right")
                    assert not re.search(rb"\x1b\[[0-9;]*m", narrow)
                    frame = narrow.rsplit(b"\x1b[H", 1)[-1]
                    plain = re.sub(rb"\x1b\[[0-9;?]*[A-Za-z]", b"", frame).decode()
                    # Tiled panes and the unclipped notice leave Configure in its scrolling short mode here.
                    assert "> Learn a pad" in plain and "Ctrl+C: quit" in plain, plain
                    assert len(plain.splitlines()) == 18
                    assert all(len(line) < 50 for line in plain.splitlines())
                    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 12, 30, 0, 0))
                    read_until(master, b"Resize terminal")
                    os.write(master, b"\t\t\r")  # Confirm is suppressed below the minimum size.
                    time.sleep(0.1)
                    # 19 rows: the tab bar (v7.7 ticket 150) takes one; at 18 Configure is short.
                    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 19, 100, 0, 0))
                    restored = read_until(master, b"Tab / Left / Right")
                    assert b"> Learn a pad" in restored and b"Finish" in restored
                    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 100, 0, 0))
                    read_until(master, b"Tab / Left / Right")
                    os.write(master, b"\t\x1b[B")
                    read_until(master, b"> Remove saved assignment")
                    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 16, 50, 0, 0))
                    # 50×16 with the tab bar: the rows scroll with the selection; the pane
                    # hint line is below them.
                    read_until(master, b"> Remove saved assignment")
                    os.write(master, b"\x1b")
                    read_until(master, b"/configure")
                    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 100, 0, 0))
                    read_until(master, b"> /configure")
                    os.write(master, b"\r")
                    read_until(master, b"Tab / Left / Right")
                os.write(master, b"\t\r")  # Saved pane -> Edit, without MIDI.
                read_until(master, b"Saved assignments")
                os.write(master, b"\x1b")
                read_until(master, b"Tab / Left / Right")
                os.write(master, b"\t\t\r")  # Finish centered below both panes.
                read_until(master, MENU)
                go(master, "/configure", "/quit")
            elif ending == "toggle_panel":
                if b"Ctrl+K" not in initial_screen:  # Toggles is Live's last pane (v7.7 ticket 151)
                    initial_screen += read_until(master, b"Ctrl+K")
                assert b"Release all" in initial_screen and "Toggles · 0 of 2 on".encode() in initial_screen
                assert b"Shift" in initial_screen and b"Ctrl+K" in initial_screen
                assert "[○] Toggle 1:".encode() in initial_screen and "[○] Toggle 2:".encode() in initial_screen
                fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 18, 50, 0, 0))
                # Long raw names wrap rather than truncate; the pane shows what fits and points to /list.
                # 50 columns: the Toggles pane stays, its rows cut to fit.
                narrow = read_until(master, "Toggles · 0 of 2 on".encode())
                assert "[○] Toggle 1:".encode() in narrow
                fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 100, 0, 0))
                read_until(master, b"Release all")
                go(master, "Run", "/quit")
            elif ending == "contexts":
                go(master, "Run", "/prog-select")
                failure = read_until(master, b"Operation failed")
                if b"unavailable" not in failure:  # error notice, pinned below in the details pane
                    read_until(master, b"unavailable")
                os.write(master, b"\r")
                read_until(master, MENU)
                go(master, "/prog-select", "/quit")
            elif ending == "current_settings":
                go(master, "Run", "/feedback-idle")
                read_until(master, b"> Octave")
                os.write(master, b"\r")
                read_until(master, b"> Absolute octave")
                os.write(master, b"\r")
                read_until(master, b"Value: 0")
                os.write(master, b"\r")
                read_until(master, b"> Octave")
                pick(master, "Tempo")
                read_until(master, b"> Set BPM")
                os.write(master, b"\r")
                read_until(master, b"Value: 240")
                os.write(master, b"\x1b")
                read_until(master, b"> Tempo")
                pick(master, "Arpeggiator")
                read_until(master, b"> On")
                os.write(master, b"\x1b")
                read_until(master, b"> Arpeggiator")
                os.write(master, b"\x1b")
                read_until(master, b"> /feedback-idle")
                go(master, "/feedback-idle", "/quit")
            else:
                go(master, "Run", "/configure")
                read_until(master, b"Configure controls")
                signal_app(child, signal.SIGINT if ending == "sigint" else signal.SIGTERM)
            # Drain full-screen frames while quitting; a PTY is a bounded output buffer.
            restored = read_until(master, b"\x1b[?1049l")
            assert b"\x1b[?25h" in restored
            # v7.7 ticket 159: mouse reporting was on while drawn and is off on every exit.
            assert b"\x1b[?1000h" in initial_screen or ending in ("sigint", "sigterm")
            assert b"\x1b[?1000l" in restored, restored[-200:]
            assert b"journal" not in initial_screen + restored  # no journal: no error shown
            child.wait(timeout=3)
            assert child.returncode == (1 if ending in ("sigint", "sigterm") else 0)
            assert termios.tcgetattr(slave) == original
            if ending == "flow_setting":
                assert b'"start_flow": false' in (config / "settings.json").read_bytes()
                assert path.read_bytes() == contents
            else:
                assert preserved(contents, config)
            calls = (bin_dir / "systemctl.log").read_text() if (bin_dir / "systemctl.log").exists() else ""
            assert " start " not in f" {calls} ", calls
            print(f"{ending}: PASS (terminal restored, saved mappings preserved)")
        finally:
            if child.poll() is None:
                child.terminate()
                try:
                    read_until(master, b"\x1b[?1049l")
                    child.wait(timeout=3)
                finally:
                    if child.poll() is None:
                        child.kill()
                        child.wait(timeout=3)
            os.close(master)
            os.close(slave)
