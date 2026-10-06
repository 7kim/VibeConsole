import time
import mido


AKAI_MANUFACTURER = 0x47
HOST_TO_DEVICE = 0x7F
MPK_MINI_3_ID = 0x49

QUERY_PROGRAM = 0x66
RECEIVE_PROGRAM = 0x67
SEND_PROGRAM = 0x64

PROGRAM_ID = 0x00


# ---------------------------------------------------------
# Known MPK Mini 3 Program 0 offsets
# ---------------------------------------------------------

OCTAVE_OFFSET = 0x13
ARP_ENABLED_OFFSET = 0x14

TEMPO_HIGH_OFFSET = 0x1B
TEMPO_LOW_OFFSET = 0x1C


# ---------------------------------------------------------
# MIDI device discovery
# ---------------------------------------------------------

def find_mpk_input():
    for name in mido.get_input_names():
        if "MPK mini 3" in name:
            return name

    raise RuntimeError("MPK Mini 3 MIDI input not found")


def find_mpk_output():
    for name in mido.get_output_names():
        if "MPK mini 3" in name:
            return name

    raise RuntimeError("MPK Mini 3 MIDI output not found")


# ---------------------------------------------------------
# Read current Program 0
# ---------------------------------------------------------

def read_program(inport, outport):
    request = [
        AKAI_MANUFACTURER,
        HOST_TO_DEVICE,
        MPK_MINI_3_ID,
        QUERY_PROGRAM,
        0x00,
        0x01,
        PROGRAM_ID,
    ]

    outport.send(
        mido.Message(
            "sysex",
            data=request,
        )
    )

    start = time.time()

    while time.time() - start < 2:

        msg = inport.receive(block=False)

        if msg is None:
            time.sleep(0.005)
            continue

        if msg.type != "sysex":
            continue

        data = list(msg.data)

        # Expected:
        #
        # F0
        # 47
        # 00
        # 49
        # 67
        # size_hi
        # size_lo
        # program_id
        # payload...
        # F7

        if (
            len(data) >= 8
            and data[0] == AKAI_MANUFACTURER
            and data[1] == 0x00
            and data[2] == MPK_MINI_3_ID
            and data[3] == RECEIVE_PROGRAM
            and data[6] == PROGRAM_ID
        ):
            payload = data[7:]

            if len(payload) != 245:
                raise RuntimeError(
                    f"Unexpected payload size: {len(payload)}"
                )

            return payload

    raise RuntimeError("Timed out waiting for MPK Mini 3")


# ---------------------------------------------------------
# Write Program 0
# ---------------------------------------------------------

def write_program(outport, payload):

    if len(payload) != 245:
        raise ValueError(
            f"Expected 245 byte payload, got {len(payload)}"
        )

    size = len(payload) + 1

    message = [
        AKAI_MANUFACTURER,
        HOST_TO_DEVICE,
        MPK_MINI_3_ID,
        SEND_PROGRAM,

        (size >> 7) & 0x7F,
        size & 0x7F,

        PROGRAM_ID,

        *payload,
    ]

    outport.send(
        mido.Message(
            "sysex",
            data=message,
        )
    )


# ---------------------------------------------------------
# MPK Controller
# ---------------------------------------------------------

class MPKMini3:

    def __init__(self):

        self.input_name = find_mpk_input()
        self.output_name = find_mpk_output()

        self.inport = None
        self.outport = None

        self.payload = None


    def connect(self):

        print("Input :", self.input_name)
        print("Output:", self.output_name)

        self.inport = mido.open_input(
            self.input_name
        )

        self.outport = mido.open_output(
            self.output_name
        )

        self.refresh()


    def close(self):

        if self.inport:
            self.inport.close()

        if self.outport:
            self.outport.close()


    def refresh(self):

        print("Reading Program 0...")

        self.payload = read_program(
            self.inport,
            self.outport,
        )

        print(
            f"Loaded {len(self.payload)} bytes"
        )


    def save(self):

        write_program(
            self.outport,
            self.payload,
        )

        time.sleep(0.05)


    # -----------------------------------------------------
    # Octave
    # -----------------------------------------------------

    def set_octave(self, octave):

        if not -4 <= octave <= 4:
            raise ValueError(
                "Octave must be between -4 and +4"
            )

        value = octave + 4

        self.payload[OCTAVE_OFFSET] = value

        self.save()

        print(
            f"Octave -> {octave:+d} "
            f"(0x{value:02X})"
        )


    # -----------------------------------------------------
    # Arpeggiator
    # -----------------------------------------------------

    def set_arp(self, enabled: bool):

        self.payload[ARP_ENABLED_OFFSET] = (
            1 if enabled else 0
        )

        self.save()

        print(
            "Arpeggiator ->",
            "ON" if enabled else "OFF"
        )


    def arp_on(self):
        self.set_arp(True)


    def arp_off(self):
        self.set_arp(False)


    # -----------------------------------------------------
    # Tempo
    # -----------------------------------------------------

    def set_tempo(self, bpm):

        bpm = int(bpm)

        if not 1 <= bpm <= 300:
            raise ValueError(
                "Tempo must be between 1 and 300 BPM"
            )

        high = (bpm >> 7) & 0x7F
        low = bpm & 0x7F

        self.payload[TEMPO_HIGH_OFFSET] = high
        self.payload[TEMPO_LOW_OFFSET] = low

        self.save()

        print(
            f"Tempo -> {bpm} BPM "
            f"({high:02X} {low:02X})"
        )


# ---------------------------------------------------------
# Demo
# ---------------------------------------------------------

def main():

    mpk = MPKMini3()

    try:
        mpk.connect()

        print()
        print("Starting demo...")
        print()

        # Normal octave
        mpk.set_octave(0)
        time.sleep(1)

        # Octave down
        mpk.set_octave(-1)
        time.sleep(1)

        # Octave up
        mpk.set_octave(+1)
        time.sleep(1)

        # Back to normal
        mpk.set_octave(0)
        time.sleep(1)

        # Arpeggiator on
        mpk.arp_on()
        time.sleep(1)

        # Tap Tempo LED should blink according to BPM
        mpk.set_tempo(121)

        print()
        print("Watch Tap Tempo blink at 121 BPM...")
        time.sleep(5)

        mpk.set_tempo(240)

        print()
        print("Watch Tap Tempo blink faster at 240 BPM...")
        time.sleep(5)

        # Turn arp off
        mpk.arp_off()

        print()
        print("Finished.")

    finally:
        mpk.close()


if __name__ == "__main__":
    main()