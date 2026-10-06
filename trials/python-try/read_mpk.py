import time
import mido

AKAI_MANUF_ID = 0x47
AKAI_SEND = 0x7F
MPK3_ID = 0x49

# These are the Mk3 commands used by sysex-controls
QUERY_CMD = 0x66
RECV_CMD = 0x67

PROGRAM_ID = 0x00

def find_mpk_input():
    return next(name for name in mido.get_input_names() if "MPK mini 3" in name)

def find_mpk_output():
    return next(name for name in mido.get_output_names() if "MPK mini 3" in name)

def request_program(outport):
    raw = [
        0xF0,
        AKAI_MANUF_ID,
        AKAI_SEND,
        MPK3_ID,
        QUERY_CMD,
        0x00,
        0x01,
        PROGRAM_ID,
        0xF7,
    ]

    msg = mido.Message("sysex", data=raw[1:-1])
    outport.send(msg)

def extract_payload(msg):
    raw = [0xF0, *msg.data, 0xF7]

    # Expected response header:
    # F0 47 00 49 67 ...
    if len(raw) < 10:
        return None

    if raw[0] != 0xF0:
        return None
    if raw[1] != AKAI_MANUF_ID:
        return None
    if raw[3] != MPK3_ID:
        return None
    if raw[4] != RECV_CMD:
        return None

    # Same layout sysex-controls uses:
    # header is 5 bytes
    # then 3 bytes metadata
    # payload begins at byte 8
    # final byte is F7
    return raw[8:-1]

def diff_payload(old, new):
    changes = []

    for i, (a, b) in enumerate(zip(old, new)):
        if a != b:
            changes.append((i, a, b))

    return changes

input_name = find_mpk_input()
output_name = find_mpk_output()

print("INPUT :", input_name)
print("OUTPUT:", output_name)
print()
print("Instructions:")
print("1. Leave all pads alone for a few reads.")
print("2. Hold Pad 1 so its LED stays lit.")
print("3. Keep holding it for 3-4 reads.")
print("4. Release it.")
print()

previous = None
count = 0

with mido.open_input(input_name) as inp, mido.open_output(output_name) as out:
    while True:
        count += 1

        request_program(out)

        deadline = time.time() + 1.0
        payload = None

        while time.time() < deadline:
            for msg in inp.iter_pending():
                if msg.type == "sysex":
                    payload = extract_payload(msg)
                    if payload is not None:
                        break

            if payload is not None:
                break

            time.sleep(0.01)

        if payload is None:
            print(f"[{count}] No valid program response")
        else:
            print(f"[{count}] Program read: {len(payload)} bytes")

            if previous is not None:
                changes = diff_payload(previous, payload)

                if not changes:
                    print("    no byte changes")
                else:
                    for offset, old, new in changes:
                        print(
                            f"    offset 0x{offset:02X}: "
                            f"{old:02X} -> {new:02X}"
                        )

            previous = payload

        time.sleep(1)