# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""v7.8 ticket 01: the real binary, run with --emulated, against the emulated Akai.
Run after cargo build --locked: uv run scripts/emulate/check-akai.py

Checks: a pad mapped to F1 shows on Live and output.log records F1 down and up; the program
query verifies against the emulator's stored program; the login service is left as it was;
a normal (not emulated) run never picks the emulator's port. Nothing reaches the desktop:
keys go to output.log, external programs to tools.log, and /dev/input is hidden from the run.
"""
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, str(Path(__file__).resolve().parent))
from akai import Akai  # noqa: E402
from run import Run, describe, hide, mapping  # noqa: E402

if not shutil.which("bwrap"):
    raise SystemExit("bwrap not found: install it (sudo dnf install bubblewrap); nothing was run.")


def service():
    """The login service's state, read only."""
    active = subprocess.run(["systemctl", "--user", "is-active", "vibeconsole.service"],
                            capture_output=True, text=True).stdout.strip()
    runtime = os.environ.get("XDG_RUNTIME_DIR", f"/run/user/{os.getuid()}")
    state = Path(runtime) / "vibeconsole/run-state"
    return active, state.read_text() if state.exists() else None


def main():
    before = service()
    with tempfile.TemporaryDirectory() as directory, Akai() as akai:
        akai.program(1)
        pad = mapping(0, "Bank A Pad 1", "note", akai.ram[0x10] + 1, akai.ram[0x24], "F1")
        # v7.8 ticket 10: a Music & Sound media key.
        media = mapping(0, "Bank A Pad 2", "note", akai.ram[0x10] + 1, akai.ram[0x27], None, "trigger",
                        "media:next")
        # v7.8 ticket 12: Fedora System, recorded (never run) in the emulated run.
        dark = mapping(0, "Bank A Pad 3", "note", akai.ram[0x10] + 1, akai.ram[0x2A], None, "trigger",
                       "system:dark-style-on")
        suspend = mapping(0, "Bank A Pad 4", "note", akai.ram[0x10] + 1, akai.ram[0x2D], None, "trigger",
                          "system:suspend") | {"risky": True}
        with Run(directory, [pad, media, dark, suspend], hide()) as run:
            # The run detects the program, then fires on the next press.
            run.read_until("Pads fire on their next press", 15)
            queries = [f for f in akai.received if f[4] == 0x66]
            assert queries, "VibeConsole never queried the program"
            run.read_until("Program 1")
            print(f"ok: program verified against the emulator ({len(queries)} program reads)")
            mark = run.mark()
            akai.pad(1, hold=0.5)
            run.read_until("◆Pad 1 F1", 5, since=mark)  # Live's controller map: Pad 1 held
            run.wait_recorded("key KEY_F1 up")
            assert run.recorded()[:2] == ["key KEY_F1 down", "key KEY_F1 up"], run.recorded()
            print("ok: akai.pad(1) shows on Live and output.log records F1 down and up")
            akai.pad(2)
            run.wait_recorded("key KEY_NEXTSONG up")
            assert run.recorded()[2:4] == ["key KEY_NEXTSONG down", "key KEY_NEXTSONG up"], run.recorded()
            print("ok: a pad mapped to Next track taps KEY_NEXTSONG once")
            tools = lambda: [t for t in run.recorded("tools.log") if "gsettings" in t or "systemctl" in t]
            akai.pad(3)
            deadline = time.monotonic() + 5
            while not tools() and time.monotonic() < deadline:
                time.sleep(0.05)
            assert tools() == ['["gsettings", "set", "org.gnome.desktop.interface", "color-scheme", "prefer-dark"]'], tools()
            akai.pad(4, hold=0.3)  # let go early: nothing
            time.sleep(1.5)
            assert len(tools()) == 1, tools()
            akai.pad(4, hold=1.4)  # held past one second: suspend (recorded, never run)
            deadline = time.monotonic() + 5
            while len(tools()) < 2 and time.monotonic() < deadline:
                time.sleep(0.05)
            assert tools()[1:] == ['["systemctl", "suspend"]'], tools()
            print("ok: Dark style runs its gsettings; Suspend only after a one-second hold (recorded)")
            described = describe(hide()).lower()
            assert "emulator" not in described and "multiple" not in described, described
            print("ok: a normal run ignores the emulator's port")
        print(f"tools recorded instead of run: {len(run.recorded('tools.log'))}")
    after = service()
    assert after == before, (before, after)
    print(f"ok: the login service is unchanged ({after[0]})")


if __name__ == "__main__":
    main()
