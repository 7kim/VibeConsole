# /// script
# requires-python = ">=3.12"
# dependencies = ["pyte"]
# ///
"""v7.8 ticket 02: the real binary, run with --emulated, against the emulated Xbox controller.
Run after cargo build --locked: uv run scripts/emulate/check-xbox.py

Checks: A (built-in default Enter) records Enter in output.log; RT mapped to F2 and pulled
straight to full fires once and lets go on release; a normal run beside the
emulator never lists it; 7 Xbox's Test rumble reaches the emulator with its strength and length
(ticket 79). The run sees only the emulator's own event node (bwrap hides the rest).
The emulator refuses to start while a VibeConsole older than ticket 01 runs (see xbox.py).
"""
from pathlib import Path
import shutil
import sys
import tempfile
import time

sys.path.insert(0, str(Path(__file__).resolve().parent))
from drive import Drive  # noqa: E402
from run import Run, describe, hide, mapping  # noqa: E402
from xbox import Xbox  # noqa: E402

if not shutil.which("bwrap"):
    raise SystemExit("bwrap not found: install it (sudo dnf install bubblewrap); nothing was run.")
GAMEPAD, RT = 239, 7  # mappings::GAMEPAD_CONTEXT, gamepad::BUTTONS' RT


def main():
    with tempfile.TemporaryDirectory() as directory, Xbox() as xbox:
        sandbox = hide(xbox.node)
        with Run(directory, [mapping(GAMEPAD, "Xbox RT", "button", 1, RT, "F2")], sandbox) as run:
            # v7.8 ticket 72: a controller there at startup says so (gilrs sends no Connected).
            run.read_until("Xbox controller ready: Microsoft Xbox One", 15)
            print("ok: the controller there at startup shows Xbox controller ready")
            run.xbox_ready(xbox)
            assert run.recorded()[:2] == ["key KEY_ENTER down", "key KEY_ENTER up"], run.recorded()
            print("ok: xbox.tap('A') records Enter in output.log")
            xbox.trigger("RT", 1.0)
            run.wait_recorded("key KEY_F2 down")
            for pull in (0.8, 0.6):
                xbox.trigger("RT", pull)
                time.sleep(0.1)
            assert run.recorded().count("key KEY_F2 down") == 1, run.recorded()
            assert "key KEY_F2 up" not in run.recorded(), run.recorded()
            xbox.trigger("RT", 0.0)
            run.wait_recorded("key KEY_F2 up")
            f2 = [line for line in run.recorded() if line.startswith("key KEY_F2 ")]
            assert f2 == ["key KEY_F2 down", "key KEY_F2 up"], f2
            print("ok: RT pulled straight to full fires once and lets go on release")
            # The built-in defaults name each button's key (gamepad::default_layout).
            for button, key in [("B", "ESC"), ("X", "BACKSPACE"), ("Y", "TAB")]:
                xbox.tap(button)
                run.wait_recorded(f"key KEY_{key} up")
            for direction in ("up", "down", "left", "right"):
                xbox.dpad(direction)
                time.sleep(0.05)
                xbox.dpad(None)
                run.wait_recorded(f"key KEY_{direction.upper()} up")
            print("ok: B, X, Y and the D-pad fire their built-in keys")
            assert "emulator" not in describe(sandbox).lower()
            print("ok: a normal run beside the emulator ignores it")
    # Detect shows the analog controls and the Xbox button as VibeConsole reads them.
    with tempfile.TemporaryDirectory() as directory, Xbox() as xbox:
        with Run(directory, [], hide(xbox.node), ["detect"]) as detect:
            detect.read_until("Microsoft Xbox One", 15)  # gilrs names the emulator by its ids
            time.sleep(0.5)
            for act, token in [(lambda: xbox.trigger("LT", 0.5), "LT 50%"),
                               (lambda: xbox.trigger("RT", 1.0), "RT 100%"),
                               (lambda: xbox.stick("left", 1.0, 0.0), "Left stick x +1.00 y +0.00"),
                               (lambda: xbox.stick("left", 0.0, 1.0), "Left stick x +0.00 y +1.00"),
                               (lambda: xbox.stick("right", -1.0, 0.0), "Right stick x -1.00"),
                               (lambda: xbox.press("Xbox"), "💡 ON Xbox")]:
                mark = detect.mark()
                act()
                detect.read_until(token, 5, since=mark)
            print("ok: Detect reads LT, RT, both sticks (up is up) and the Xbox button")
    bluetooth()
    layer_button()
    high_fire_point()
    test_rumble()


def test_rumble():
    """v7.8 ticket 79: 7 Xbox's Test rumble, as developer test 24 sets it (light 200 ms, then
    strong 1000 ms), plays that strength and length on the emulator. Length is measured from the
    first play to the last on gilrs's 50 ms ticks, so it may be one tick off."""
    for word, magnitude, ms in [("light", 20_000, 200), ("strong", 65_535, 1000)]:
        with Drive(xbox=True, tuning={"rumble_strength": word, "rumble_ms": ms}) as d:
            d.run.xbox_ready(d.xbox)
            d.type("7")
            d.wait(f"Rumble strength: {word}")
            d.wait(f"Rumble length: {ms} ms")
            d.pick("Test rumble")
            d.wait(f"Rumble sent: {word}, {ms} ms")
            deadline = time.monotonic() + ms / 1000 + 3
            while not (played := [line for line in d.recorded() if line.startswith("rumble ")]):
                assert time.monotonic() < deadline, d.recorded()
                time.sleep(0.05)
            time.sleep(0.5)
            played = [line.split() for line in d.recorded() if line.startswith("rumble ")]
            assert len(played) == 1, played
            _, _, strong, _, weak, length, _ = played[0]
            assert (int(strong), int(weak)) == (magnitude, 0), played
            assert abs(int(length) - ms) <= 60, played
            print(f"ok: Test rumble at {word}, {ms} ms played: {' '.join(played[0])}")


def high_fire_point():
    """v7.8 ticket 53: with RT's fire point at 90% (hard point 91, the default above it), a
    pull to 80% sends nothing; full travel sends one key-down, rest one key-up."""
    rt = mapping(GAMEPAD, "Xbox RT", "button", 1, RT, "F2")
    with tempfile.TemporaryDirectory() as directory, Xbox() as xbox:
        with Run(directory, [rt], hide(xbox.node), tuning={"trigger": 90, "hard_point": 91}) as run:
            run.xbox_ready(xbox)
            f2 = lambda: [line for line in run.recorded() if line.startswith("key KEY_F2 ")]
            xbox.trigger("RT", 0.8)
            time.sleep(1)
            assert f2() == [], f2()
            xbox.trigger("RT", 1.0)
            run.wait_recorded("key KEY_F2 down")
            xbox.trigger("RT", 0.0)
            run.wait_recorded("key KEY_F2 up")
            time.sleep(0.3)
            assert f2() == ["key KEY_F2 down", "key KEY_F2 up"], f2()
    print("ok: with RT firing at 90%, 80% sends nothing; full sends one down, rest one up")


def layer_button():
    """v7.8 ticket 23: with RB as the layer 2 button, LB fires its own mapping, RB held gives
    A its layer 2 mapping, and let go A does its default again."""
    LB, RB, A, LAYER_2 = 4, 5, 0, 238  # gamepad::BUTTONS indexes; mappings::GAMEPAD_LAYER_2
    rows = [mapping(GAMEPAD, "Xbox LB", "button", 1, LB, "F3"), mapping(LAYER_2, "Xbox A", "button", 1, A, "F4")]
    with tempfile.TemporaryDirectory() as directory, Xbox() as xbox:
        with Run(directory, rows, hide(xbox.node), tuning={"layer_button": "RB"}) as run:
            run.xbox_ready(xbox)
            enters = run.recorded().count("key KEY_ENTER up")
            xbox.tap("LB")
            run.wait_recorded("key KEY_F3 up")
            xbox.press("RB")
            time.sleep(0.2)
            xbox.tap("A")
            run.wait_recorded("key KEY_F4 up")
            xbox.release("RB")
            time.sleep(0.2)
            xbox.tap("A")
            deadline = time.monotonic() + 5
            while run.recorded().count("key KEY_ENTER up") == enters and time.monotonic() < deadline:
                time.sleep(0.05)
            assert run.recorded().count("key KEY_ENTER up") == enters + 1, run.recorded()
            assert run.recorded().count("key KEY_F4 down") == 1, run.recorded()
    print("ok: with RB as the layer 2 button, LB fires its mapping and RB held gives layer 2")


def bluetooth():
    """v7.8 ticket 04: the same controller over Bluetooth under xpadneo (its ids pick SDL's
    "Xbox One Elite 2" line in gilrs): Detect reads LT, RT and the right stick, and a mapping
    on RT fires."""
    with tempfile.TemporaryDirectory() as directory, Xbox("bluetooth") as xbox:
        with Run(directory, [], hide(xbox.node), ["detect"]) as detect:
            detect.read_until("Xbox One Elite 2", 15)
            time.sleep(0.5)
            for act, token in [(lambda: xbox.trigger("LT", 0.5), "LT 50%"),
                               (lambda: xbox.trigger("RT", 1.0), "RT 100%"),
                               (lambda: xbox.stick("right", -1.0, 0.0), "Right stick x -1.00"),
                               (lambda: xbox.stick("right", 0.0, 1.0), "Right stick x +0.00 y +1.00")]:
                mark = detect.mark()
                act()
                detect.read_until(token, 5, since=mark)
    print("ok: over Bluetooth (xpadneo's ids) Detect reads LT, RT and the right stick")
    with tempfile.TemporaryDirectory() as directory, Xbox("bluetooth") as xbox:
        with Run(directory, [mapping(GAMEPAD, "Xbox RT", "button", 1, RT, "F2")], hide(xbox.node)) as run:
            run.xbox_ready(xbox)
            for pull in (0.1, 0.5, 0.8, 1.0, 0.6):
                xbox.trigger("RT", pull)
                time.sleep(0.1)
            xbox.trigger("RT", 0.0)
            run.wait_recorded("key KEY_F2 up")
            assert run.recorded().count("key KEY_F2 down") == 1, run.recorded()
    print("ok: over Bluetooth a mapping on RT fires once and lets go on release")


if __name__ == "__main__":
    main()
