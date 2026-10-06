import mido
import time

AKAI_MANUF_ID = 0x47
AKAI_SEND = 0x7F
MPK3_ID = 0x49

QUERY_CMD = 0x66
RECV_CMD = 0x67
SEND_CMD = 0x64

PROGRAM_ID = 0x00

# Change this:
TARGET_OCTAVE = 0


def find_mpk_input():
    for name in mido.get_input_names():
        if "MPK mini 3" in name:
            return name
    raise RuntimeError("MPK Mini 3 input not found")


def find_mpk_output():
    for name in mido.get_output_names():
        if "MPK mini 3" in name:
            return name
    raise RuntimeError("MPK Mini 3 output not found")


def octave_to_byte(octave):
    if octave < -4 or octave > 4:
        raise ValueError("Octave must be between -4 and +4")

    return octave + 4


def read_program(inport, outport):
    request = [
        AKAI_MANUF_ID,
        AKAI_SEND,
        MPK3_ID,
        QUERY_CMD,
        0x00,
        0x01,
        PROGRAM_ID,
    ]

    outport.send(mido.Message("sysex", data=request))

    start = time.time()

    while time.time() - start < 2:
        msg = inport.receive(block=False)

        if msg and msg.type == "sysex":
            data = list(msg.data)

            if (
                len(data) > 8
                and data[0] == AKAI_MANUF_ID
                and data[2] == MPK3_ID
                and data[3] == RECV_CMD
            ):
                # SysEx structure:
                # F0 47 00 49 67 size_hi size_lo program_id [payload...] F7
                return data[7:]

        time.sleep(0.01)

    raise RuntimeError("Timed out waiting for MPK program data")


def write_program(outport, payload):
    size = len(payload) + 1

    message = [
        AKAI_MANUF_ID,
        AKAI_SEND,
        MPK3_ID,
        SEND_CMD,
        (size >> 7) & 0x7F,
        size & 0x7F,
        PROGRAM_ID,
        *payload,
    ]

    outport.send(mido.Message("sysex", data=message))


def main():
    input_name = find_mpk_input()
    output_name = find_mpk_output()

    print("Input :", input_name)
    print("Output:", output_name)

    with mido.open_input(input_name) as inport, \
         mido.open_output(output_name) as outport:

        print("Reading current program...")

        payload = read_program(inport, outport)

        print("Program size:", len(payload), "bytes")

        old_byte = payload[0x13]

        print(f"Current octave byte: 0x{old_byte:02X}")
        print(f"Setting octave to: {TARGET_OCTAVE:+d}")

        payload[0x13] = octave_to_byte(TARGET_OCTAVE)

        write_program(outport, payload)

        print("SysEx sent.")
        print(f"New octave byte: 0x{payload[0x13]:02X}")


if __name__ == "__main__":
    main()
