"""Check real terminal navigation/restoration with isolated files and no desktop output.
Run after cargo build --locked: python3 scripts/check-terminal.py
"""
import fcntl
import os
from pathlib import Path
import pty
import re
import select
import signal
import struct
import subprocess
import tempfile
import termios
import time

binary = Path(__file__).resolve().parents[1] / "target/debug/vibeconsole"
# Grouped command order (headings are skipped); the cursor starts on Run/Pause.
ORDER = ["Run", "/pause", "/release-all", "/quit", "/configure", "/list", "Agents", "Reset settings",
         "/prog-select", "/program-name", "/feedback-idle", "/feedback-check",
         "Start Wispr Flow with VibeConsole", "/start-flow", "/repair-flow", "/detect", "/get"]


def go(master, source, target):
    """Move from the highlighted command to the target by name; Enter only once it is highlighted.
    Flow commands would launch the real application, so no scenario may open them."""
    assert target not in ("/start-flow", "/repair-flow"), target
    if source != target:  # otherwise the target is already highlighted
        index = lambda name: 0 if name == "Pause" else ORDER.index(name)
        os.write(master, b"\x1b[B" * ((index(target) - index(source)) % len(ORDER)))
        visible = "Start Wispr Flow with V" if target == "Start Wispr Flow with VibeConsole" else target
        read_until(master, b"> " + visible.encode())
    os.write(master, b"\r")


def read_until(master, token):
    data = b""
    deadline = time.monotonic() + 3
    while token not in data and time.monotonic() < deadline:
        if select.select([master], [], [], 0.1)[0]:
            data += os.read(master, 65536)
    assert token in data, (token, data)
    return data


for ending in ("quit", "sigint", "sigterm", "configuration_error", "current_settings", "contexts", "default_running", "run_subcommand", "toggle_run_pause", "pause_alias", "modified_navigation", "flow_setting", "panes", "narrow_resize", "toggle_panel", "idle_no_device", "unverified_run"):
    with tempfile.TemporaryDirectory(prefix="vibeconsole-pty-") as directory:
        config = Path(directory) / "vibeconsole"
        config.mkdir()
        path = config / "mappings.tsv"
        contents = b"vibeconsole-mappings-v1\n09e8:1049\tnote\t10\t36\thold\tShift\n"
        if ending == "default_running":
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
        if ending == "configuration_error":
            (config / "mappings.tmp").write_text("occupied")
        master, slave = pty.openpty()
        original = termios.tcgetattr(slave)
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 100, 0, 0))
        child = subprocess.Popen(
            [binary, *(["run"] if ending == "run_subcommand" else [] if ending in ("default_running", "toggle_run_pause", "pause_alias", "idle_no_device") else ["--paused"])], stdin=slave, stdout=slave, stderr=slave,
            env=dict(os.environ, PATH=str(bin_dir) + os.pathsep + os.environ["PATH"], XDG_CONFIG_HOME=directory, XDG_RUNTIME_DIR=directory, XDG_DATA_HOME=directory, XDG_DATA_DIRS=directory, TERM="xterm-256color", **({"NO_COLOR": "1"} if ending == "narrow_resize" else {})),
        )
        try:
            initial_screen=read_until(master, b"/quit")
            if ending == "unverified_run":
                go(master, "Run", "Run")
                notice = read_until(master, b"Select /prog-select before running")
                assert b"Operation failed" not in notice
                assert b"PAUSED" in initial_screen
                assert b"RUNNING" not in notice
                assert b"Registered VibeConsole keyboard" not in notice
                os.write(master, b"\r")
                read_until(master, b"/quit")
                go(master, "Run", "/quit")
            elif ending == "toggle_run_pause":
                assert b"RUNNING (detecting program" in initial_screen
                assert b"> Pause" in initial_screen
                go(master, "Pause", "Pause")
                paused = read_until(master, b"> Run")
                assert b"PAUSED" in paused
                go(master, "Run", "Run")
                read_until(master, b"Select /prog-select before running")
                os.write(master, b"\r")
                read_until(master, b"/quit")
                go(master, "Run", "/quit")
            elif ending == "pause_alias":
                go(master, "Pause", "/pause")
                paused = read_until(master, b"  Run")
                assert b"PAUSED" in paused
                go(master, "/pause", "/quit")
            elif ending == "modified_navigation":
                os.write(master, b"\x1b[1;2B")  # Shift+Down
                read_until(master, b"> /pause")
                os.write(master, b"\x1b[1;3A")  # Alt+Up
                read_until(master, b"> Run")
                os.write(master, b"\x1b[13;5u")  # Ctrl+Enter
                read_until(master, b"Select /prog-select before running")
                os.write(master, b"\x1b[27;3u")  # Alt+Esc
                read_until(master, b"> Run")
                go(master, "Run", "Reset settings")
                read_until(master, b"Type reset")
                os.write(master, b"ReSeT\r")
                read_until(master, b"> Reset settings")
                assert not list(config.glob("mappings.tsv.bak-*"))
                go(master, "Reset settings", "/quit")
            elif ending == "flow_setting":
                assert b"Wispr Flow not installed" in initial_screen
                go(master, "Run", "Start Wispr Flow with VibeConsole")
                read_until(master, b"Off. Enter to turn on")
                assert b"start-flow\toff\n" in path.read_bytes()
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
                read_until(master, b"/quit")
                go(master, "/prog-select", "/quit")
            elif ending == "quit":
                go(master, "Run", "/quit")
            elif ending == "configuration_error":
                go(master, "Run", "/configure")
                read_until(master, b"Configure controls")
                os.write(master, b"\x1b[B" * 5 + b"\r")
                read_until(master, b"Saved assignments")
                os.write(master, b"\r")
                read_until(master, b"Remove assignment")
                os.write(master, b"\x1b[B\r")
                read_until(master, b"Operation failed")
                assert path.read_bytes() == contents
                os.write(master, b"\r")
                read_until(master, b"/quit")
                go(master, "/configure", "/quit")
            elif ending in ("panes", "narrow_resize"):
                go(master, "Run", "/configure")
                wide = read_until(master, b"Tab / Left / Right")
                assert b"Learn a piano key" in wide and b"Learn a knob" in wide
                assert re.search(rb"Learn controls[^\n]*Saved assignments", wide)
                assert b"\x1b[1;38;2;26;27;38;48;2;122;162;247m" in wide or ending == "narrow_resize" or "NO_COLOR" in os.environ
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
                    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 18, 100, 0, 0))
                    restored = read_until(master, b"Tab / Left / Right")
                    assert b"> Learn a pad" in restored and b"Finish" in restored
                    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 100, 0, 0))
                    read_until(master, b"Tab / Left / Right")
                    os.write(master, b"\t\x1b[B")
                    read_until(master, b"> Remove saved assignment")
                    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 16, 50, 0, 0))
                    short = read_until(master, b"Tab / Left / Right")
                    assert b"> Remove saved assignment" in short
                    os.write(master, b"\x1b")
                    read_until(master, b"/configure")
                    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 100, 0, 0))
                    read_until(master, b"/quit")
                    os.write(master, b"\r")
                    read_until(master, b"Tab / Left / Right")
                os.write(master, b"\t\r")  # Saved pane -> Edit, without MIDI.
                read_until(master, b"Saved assignments")
                os.write(master, b"\x1b")
                read_until(master, b"Tab / Left / Right")
                os.write(master, b"\t\t\r")  # Finish centered below both panes.
                read_until(master, b"/quit")
                go(master, "/configure", "/quit")
            elif ending == "toggle_panel":
                if b"Ctrl+K" not in initial_screen:  # the live pane renders after the command pane
                    initial_screen += read_until(master, b"Ctrl+K")
                assert b"/prog-select" in initial_screen and b"Toggles 2/2" in initial_screen
                assert b"Shift" in initial_screen and b"Ctrl+K" in initial_screen
                assert "[○] Toggle 1:".encode() in initial_screen and "[○] Toggle 2:".encode() in initial_screen
                fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 18, 50, 0, 0))
                # Long raw names wrap rather than truncate; the pane shows what fits and points to /list.
                narrow = read_until(master, b"Toggles 1/2 \xc2\xb7 /list")
                assert "[○] Toggle 1:".encode() in narrow and b"(Shift)" in narrow
                fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 100, 0, 0))
                read_until(master, b"/prog-select")
                go(master, "Run", "/quit")
            elif ending == "contexts":
                go(master, "Run", "/prog-select")
                failure = read_until(master, b"Operation failed")
                if b"unavailable" not in failure:  # error notice, pinned below in the details pane
                    read_until(master, b"unavailable")
                os.write(master, b"\r")
                read_until(master, b"/quit")
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
                os.write(master, b"\x1b[B\r")
                read_until(master, b"> Set BPM")
                os.write(master, b"\r")
                read_until(master, b"Value: 240")
                os.write(master, b"\x1b")
                read_until(master, b"> Tempo")
                os.write(master, b"\x1b[B\r")
                read_until(master, b"> On")
                os.write(master, b"\x1b")
                read_until(master, b"> Arpeggiator")
                os.write(master, b"\x1b")
                read_until(master, b"> /feedback-idle")
                go(master, "/feedback-idle", "/quit")
            else:
                go(master, "Run", "/configure")
                read_until(master, b"Configure controls")
                child.send_signal(signal.SIGINT if ending == "sigint" else signal.SIGTERM)
            # Drain full-screen frames while quitting; a PTY is a bounded output buffer.
            restored = read_until(master, b"\x1b[?1049l")
            assert b"\x1b[?25h" in restored
            child.wait(timeout=3)
            assert child.returncode == (1 if ending in ("sigint", "sigterm") else 0)
            assert termios.tcgetattr(slave) == original
            if ending == "flow_setting":
                assert b"start-flow\toff\n" in path.read_bytes()
            else:
                assert path.read_bytes() == contents
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
