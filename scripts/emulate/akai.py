"""An emulated Akai MPK mini 3 for agent tests (v7.8 ticket 01). Standard library only.

    from akai import Akai
    with Akai() as akai:
        akai.program(1)          # PROG SELECT + pad 1: RAM becomes stored program 1
        akai.pad(1, velocity=100)
        akai.knob(3, 64)
        akai.joystick(0.0, 1.0)
        akai.key(60)

It is one ALSA sequencer port, readable and writable like the real one, named
`VibeConsole emulator · MPK mini 3`: VibeConsole's `is_akai` never matches it, so only a run
started with `vibeconsole --emulated <dir>` picks it. It answers the MPK mini 3 program
read, write and select SysEx from its stored programs (real captures in
testdata/mpk-mini3-programs.hex), so the feedback gates can be tested.

Bytes sent, from the program in RAM (offsets as VibeConsole reads them, from real captures):
pads: note on/off on the pad channel (byte 0x10), notes at 0x24 + 3k; knobs: CC 0x55 + 20k on
channel 1, the value as given; keys: note on/off on channel 1. Joystick: X sends pitch bend
when byte 0x1E is 0, else CC byte 0x1F; Y sends CC byte 0x22. The joystick and key channel
bytes are inferred, not verified on the device: an emulator pass is an agent-test pass only.

python-rtmidi can't make this port: its virtual input and output are two separate ALSA
clients, and VibeConsole sends SysEx back to the port id it reads from (as the real one has).
So libasound's virtual raw MIDI is used through ctypes on one duplex port.
"""
import ctypes
import os
from pathlib import Path
import threading
import time

NAME = "VibeConsole emulator · MPK mini 3"
PROGRAMS = Path(__file__).resolve().parents[2] / "testdata/mpk-mini3-programs.hex"
KEYS_CHANNEL = 0  # channel 1, the factory default

_CAP_READ, _CAP_WRITE, _CAP_DUPLEX, _CAP_SUBS_READ, _CAP_SUBS_WRITE = 1, 2, 16, 32, 64
_TYPE_MIDI_GENERIC, _TYPE_APPLICATION = 1 << 1, 1 << 20
_SEQ_OPEN_DUPLEX, _RAWMIDI_NONBLOCK = 3, 2


def load_programs(path=PROGRAMS):
    """Nine 245-byte payloads: RAM (slot 0), then stored programs 1–8."""
    lines = [l for l in path.read_text().splitlines() if l.strip() and not l.startswith("#")]
    payloads = [bytearray.fromhex(l) for l in lines[:9]]
    assert len(payloads) == 9 and all(len(p) == 245 for p in payloads), path
    return payloads


class Akai:
    def __init__(self, programs=None):
        self.slots = programs or load_programs()
        self.received = []  # every SysEx frame VibeConsole sent, in order
        self._lib = lib = ctypes.CDLL("libasound.so.2")
        lib.snd_rawmidi_read.restype = ctypes.c_ssize_t
        lib.snd_rawmidi_read.argtypes = [ctypes.c_void_p, ctypes.c_void_p, ctypes.c_size_t]
        lib.snd_rawmidi_write.restype = ctypes.c_ssize_t
        lib.snd_rawmidi_write.argtypes = [ctypes.c_void_p, ctypes.c_void_p, ctypes.c_size_t]
        lib.snd_rawmidi_drain.argtypes = [ctypes.c_void_p]
        lib.snd_rawmidi_close.argtypes = [ctypes.c_void_p]
        lib.snd_seq_set_client_name.argtypes = [ctypes.c_void_p, ctypes.c_char_p]
        lib.snd_seq_create_simple_port.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_uint, ctypes.c_uint]
        lib.snd_seq_client_id.argtypes = [ctypes.c_void_p]
        # snd_rawmidi_virtual_open is exported by libasound but not in its public header:
        # (inputp, outputp, name, seq_handle, port, merge, mode).
        lib.snd_rawmidi_virtual_open.argtypes = [
            ctypes.c_void_p, ctypes.c_void_p, ctypes.c_char_p, ctypes.c_void_p,
            ctypes.c_int, ctypes.c_int, ctypes.c_int,
        ]
        seq = ctypes.c_void_p()
        self._check(lib.snd_seq_open(ctypes.byref(seq), b"default", _SEQ_OPEN_DUPLEX, 0), "snd_seq_open")
        name = NAME.encode()
        self._check(lib.snd_seq_set_client_name(seq, name), "snd_seq_set_client_name")
        caps = _CAP_READ | _CAP_WRITE | _CAP_DUPLEX | _CAP_SUBS_READ | _CAP_SUBS_WRITE
        port = lib.snd_seq_create_simple_port(seq, name, caps, _TYPE_MIDI_GENERIC | _TYPE_APPLICATION)
        self._check(port, "snd_seq_create_simple_port")
        self.client = lib.snd_seq_client_id(seq)
        self._in, self._out = ctypes.c_void_p(), ctypes.c_void_p()
        self._check(lib.snd_rawmidi_virtual_open(
            ctypes.byref(self._in), ctypes.byref(self._out), b"vibeconsole-emulator", seq, port, 1,
            _RAWMIDI_NONBLOCK,
        ), "snd_rawmidi_virtual_open")
        self._lock = threading.Lock()
        self._stop = threading.Event()
        self._reader = threading.Thread(target=self._read, daemon=True)
        self._reader.start()

    @staticmethod
    def _check(result, what):
        if result < 0:
            raise OSError(-result, f"{what}: {os.strerror(-result)}")

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.close()

    def close(self):
        self._stop.set()
        self._reader.join(timeout=1)
        for handle in (self._in, self._out):
            if handle:
                self._lib.snd_rawmidi_close(handle)
        self._in = self._out = None

    # What the device sends.
    def send(self, *data):
        """Raw bytes out of the device, as one message."""
        message = bytes(data)
        with self._lock:
            self._check(self._lib.snd_rawmidi_write(self._out, message, len(message)), "snd_rawmidi_write")
            self._lib.snd_rawmidi_drain(self._out)

    @property
    def ram(self):
        return self.slots[0]

    def _pad_note(self, n, bank):
        if not 1 <= n <= 8 or bank not in ("A", "B"):
            raise ValueError("pad 1–8 in bank A or B")
        k = n - 1 + (8 if bank == "B" else 0)
        return self.ram[0x10], self.ram[0x24 + 3 * k]

    def pad_down(self, n, velocity=100, bank="A"):
        channel, note = self._pad_note(n, bank)
        self.send(0x90 | channel, note, max(1, min(127, velocity)))

    def pad_up(self, n, bank="A"):
        channel, note = self._pad_note(n, bank)
        self.send(0x80 | channel, note, 0)

    def pad(self, n, velocity=100, bank="A", hold=0.05):
        """Press and release pad n (1–8) of bank A or B."""
        self.pad_down(n, velocity, bank)
        time.sleep(hold)
        self.pad_up(n, bank)

    def knob(self, n, value):
        """Knob n (1–8) sends `value` (0–127) on its CC."""
        if not 1 <= n <= 8 or not 0 <= value <= 127:
            raise ValueError("knob 1–8, value 0–127")
        self.send(0xB0 | KEYS_CHANNEL, self.ram[0x55 + 20 * (n - 1)], value)

    def joystick(self, x, y):
        """Both axes, each -1.0 (left, down) to 1.0 (right, up); 0.0 is the centre."""
        if not (-1 <= x <= 1 and -1 <= y <= 1):
            raise ValueError("joystick axes are -1.0 to 1.0")
        if self.ram[0x1E] == 0:
            bend = round(8192 + x * (8191 if x > 0 else 8192))
            self.send(0xE0 | KEYS_CHANNEL, bend & 0x7F, bend >> 7)
        else:
            self.send(0xB0 | KEYS_CHANNEL, self.ram[0x1F], round((x + 1) / 2 * 127))
        self.send(0xB0 | KEYS_CHANNEL, self.ram[0x22], round((y + 1) / 2 * 127))

    def key_down(self, note, velocity=100):
        self.send(0x90 | KEYS_CHANNEL, note, max(1, min(127, velocity)))

    def key_up(self, note):
        self.send(0x80 | KEYS_CHANNEL, note, 0)

    def key(self, note, velocity=100, hold=0.05):
        """Press and release a piano key (MIDI note 0–127)."""
        if not 0 <= note <= 127:
            raise ValueError("note 0–127")
        self.key_down(note, velocity)
        time.sleep(hold)
        self.key_up(note)

    def program(self, n):
        """PROG SELECT + pad n: stored program n (1–8) is loaded into RAM. The device sends
        nothing; VibeConsole sees it by reading RAM."""
        if not 1 <= n <= 8:
            raise ValueError("program 1–8")
        self.slots[0] = bytearray(self.slots[n])

    # What the device answers.
    def _answer(self, frame):
        self.received.append(frame)
        head = frame[:5]
        if head == bytes([0xF0, 0x47, 0x7F, 0x49, 0x66]) and len(frame) == 9 and frame[7] <= 8:
            slot = frame[7]
            self.send(0xF0, 0x47, 0x00, 0x49, 0x67, 0x01, 0x76, slot, *self.slots[slot], 0xF7)
        elif head == bytes([0xF0, 0x47, 0x7F, 0x49, 0x64]) and len(frame) == 254 and frame[7] <= 8:
            self.slots[frame[7]] = bytearray(frame[8:253])
        elif head == bytes([0xF0, 0x47, 0x7F, 0x49, 0x62]) and len(frame) == 9 and 1 <= frame[7] <= 8:
            self.program(frame[7])

    def _read(self):
        buffer, frame = ctypes.create_string_buffer(4096), None
        while not self._stop.is_set():
            count = self._lib.snd_rawmidi_read(self._in, buffer, len(buffer))
            if count <= 0:
                time.sleep(0.001)
                continue
            for byte in buffer.raw[:count]:
                if byte == 0xF0:
                    frame = bytearray([byte])
                elif frame is not None:
                    frame.append(byte)
                    if byte == 0xF7:
                        self._answer(bytes(frame))
                        frame = None
