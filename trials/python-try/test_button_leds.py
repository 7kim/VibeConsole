import time
import mido


# ---------------------------------------------------------
# MPK Mini 3 MIDI output
# ---------------------------------------------------------

def find_mpk_output():
    for name in mido.get_output_names():
        if "MPK mini 3" in name:
            return name

    raise RuntimeError("MPK Mini 3 MIDI output not found")


OUTPUT = find_mpk_output()


# ---------------------------------------------------------
# LED helpers
# ---------------------------------------------------------

def led(port, note: int, on: bool, channel: int = 0):
    """
    Send a MIDI Note On.

    We are testing whether the MPK Mini 3 uses incoming
    MIDI notes to control button LEDs.

    velocity 127 = ON
    velocity 0   = OFF
    """

    velocity = 127 if on else 0

    port.send(
        mido.Message(
            "note_on",
            channel=channel,
            note=note,
            velocity=velocity,
        )
    )


def flash(port, note: int, count=6, delay=0.25):
    for _ in range(count):
        led(port, note, True)
        time.sleep(delay)

        led(port, note, False)
        time.sleep(delay)


# ---------------------------------------------------------
# OLD MPK MINI CANDIDATE VALUES
#
# These are NOT confirmed for Mk3.
# They are here only because older MPK Mini models used them.
# ---------------------------------------------------------

ARP_ON_OFF = 0
TAP_TEMPO = 4
OCTAVE_DOWN = 5
OCTAVE_UP = 6
FULL_LEVEL = 7

CC = 26
PROGRAM_CHANGE = 27


# ---------------------------------------------------------
# Individual tests
# ---------------------------------------------------------

def test_known_candidates(port):

    print("\nTesting older MPK Mini LED mappings.")
    print("Watch the controller carefully.\n")

    tests = [
        ("ARP ON/OFF", ARP_ON_OFF),
        ("TAP TEMPO", TAP_TEMPO),
        ("OCTAVE DOWN", OCTAVE_DOWN),
        ("OCTAVE UP", OCTAVE_UP),
        ("FULL LEVEL", FULL_LEVEL),
        ("CC", CC),
        ("PROGRAM CHANGE", PROGRAM_CHANGE),
    ]

    for name, note in tests:

        print(f"Testing {name:15} -> MIDI note {note}")

        led(port, note, True)

        time.sleep(1)

        led(port, note, False)

        time.sleep(1)


# ---------------------------------------------------------
# Desired demo
# ---------------------------------------------------------

def demo(port):

    print("\n--- DEMO ---")

    print("ARP ON")
    led(port, ARP_ON_OFF, True)
    time.sleep(1)

    print("Tap Tempo flash")
    flash(port, TAP_TEMPO, count=4)

    print("Alternate Octave - / +")

    for _ in range(6):

        led(port, OCTAVE_DOWN, True)
        led(port, OCTAVE_UP, False)

        time.sleep(0.3)

        led(port, OCTAVE_DOWN, False)
        led(port, OCTAVE_UP, True)

        time.sleep(0.3)

    led(port, OCTAVE_DOWN, False)
    led(port, OCTAVE_UP, False)

    print("Full Level ON")
    led(port, FULL_LEVEL, True)

    print("CC ON")
    led(port, CC, True)

    time.sleep(3)

    print("Turning everything OFF")

    for note in [
        ARP_ON_OFF,
        TAP_TEMPO,
        OCTAVE_DOWN,
        OCTAVE_UP,
        FULL_LEVEL,
        CC,
        PROGRAM_CHANGE,
    ]:
        led(port, note, False)


# ---------------------------------------------------------
# Mk3 discovery scanner
# ---------------------------------------------------------

def scan_notes(port, start=0, end=40):

    print()
    print("Scanning MIDI notes for LED responses.")
    print()
    print("For each number:")
    print("  - watch the MPK")
    print("  - see which LED lights")
    print()
    print("Ctrl+C to stop.")
    print()

    for note in range(start, end + 1):

        print(f"NOTE {note:3}")

        led(port, note, True)

        time.sleep(0.7)

        led(port, note, False)

        time.sleep(0.3)


# ---------------------------------------------------------
# Main
# ---------------------------------------------------------

with mido.open_output(OUTPUT) as port:

    print("MPK output:")
    print(OUTPUT)

    print()
    print("1 = test old candidate mappings")
    print("2 = run requested LED demo")
    print("3 = scan MIDI notes 0-40")
    print("4 = scan all MIDI notes 0-127")

    choice = input("\nChoose: ").strip()

    if choice == "1":
        test_known_candidates(port)

    elif choice == "2":
        demo(port)

    elif choice == "3":
        scan_notes(port, 0, 40)

    elif choice == "4":
        scan_notes(port, 0, 127)

    else:
        print("Invalid option")