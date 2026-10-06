# MPK Mini 3 Reverse Engineering Notes

## Scope

This document records only what we actually did and what we learned while investigating the Akai MPK Mini 3.

Device identified:

```text
AKAI Professional M.I. Corp. MPK mini 3
USB VID: 09e8
USB PID: 1049
```

Primary goals during this investigation:

- understand the MIDI behavior of the pads
- test whether the pad LEDs can be controlled from the host
- inspect the Akai SysEx program format
- find the octave control byte
- inspect USB interfaces and HID capabilities

---

## 1. Python MIDI environment

We used Python with:

```text
mido
python-rtmidi
```

The MPK appeared as:

```text
MPK mini 3:MPK mini 3 MIDI 1
```

The working Python version was:

```text
Python 3.13
```

The project was run using `uv`.

---

## 2. MIDI input behavior

We listened to all incoming MIDI messages from the controller.

### Pads

The physical pads send MIDI on:

```text
Mido channel = 9
Human-readable MIDI channel = 10
```

Pad notes:

```text
Bank A: 36-43
Bank B: 44-51
```

A normal pad press produced messages such as:

```text
note_on channel=9 note=36 velocity=127
polytouch channel=9 note=36 value=36
...
polytouch channel=9 note=36 value=0
note_off channel=9 note=36 velocity=0
```

We learned that the pads send polyphonic aftertouch while held.

### Joystick

The joystick produced pitch wheel data on channel 0.

Example:

```text
pitchwheel channel=0 pitch=-8192
...
pitchwheel channel=0 pitch=0
```

---

## 3. Normal MIDI output test

We tried sending MIDI Note On and Note Off messages back to the MPK using the real pad notes.

Example:

```python
port.send(
    mido.Message(
        "note_on",
        channel=9,
        note=36,
        velocity=127,
    )
)
```

### Result

The pads did not light.

However, some other physical button LEDs reacted to incoming MIDI.

This established an important distinction:

```text
Pad LED behavior != ordinary incoming Note On/Off behavior
```

A likely observed architecture is:

```text
Physical pad press
    |
    +--> firmware sends MIDI note / poly aftertouch to host
    |
    +--> firmware lights the pad locally
```

We did not find evidence that incoming note messages directly control the pad LEDs.

---

## 4. Physical Octave button behavior

Pressing the physical Octave buttons did not produce normal MIDI messages in our listener.

This strongly suggests that the physical Octave +/- buttons are handled internally by the MPK firmware rather than being exposed as ordinary MIDI events.

---

## 5. Open-source projects investigated

We checked multiple public projects.

### sysex-controls

Repository:

```text
soyersoyer/sysex-controls
```

This project explicitly supports the MPK Mini 3.

Important source files included:

```text
src/mpkmini3/amm3-program-page.ui
src/mpkmini3/amm3-pad-page.ui
src/mpkmini3/amm3-pad-bank-page.ui
src/mpkmini3/amm3-pad.ui
src/mpkmini3/amm3-keybed-page.ui
```

The program editor exposed settings including:

```text
MIDI Channel
Octave
Transpose
Arpeggiator settings
Pad Note
Pad CC
Pad Program Change
```

### Important hardware result

Changing the Octave value in `sysex-controls` changed the physical octave state on the MPK.

This proved that the MPK Mini 3 accepts configuration over Akai SysEx and that host-side software can change hardware-visible state.

---

## 6. Akai MPK Mini 3 SysEx protocol

From the `sysex-controls` source we identified the Akai protocol structure.

Important constants:

```text
Akai manufacturer ID = 0x47
Host -> device prefix = 0x7F
Device -> host prefix = 0x00
MPK Mini 3 device ID = 0x49
```

Known command values:

```text
Query program   = 0x66
Receive program = 0x67
Send program    = 0x64
```

### Program read request

The read request structure is:

```text
F0
47
7F
49
66
00
01
PROGRAM_ID
F7
```

For program 0:

```text
F0 47 7F 49 66 00 01 00 F7
```

### Program write structure

The program write message contains:

```text
F0
47
7F
49
64
SIZE_HI
SIZE_LO
PROGRAM_ID
PAYLOAD...
F7
```

The size is encoded as 7-bit high/low values.

---

## 7. Program payload

Reading Program 0 returned:

```text
245 bytes
```

A typical beginning looked like:

```text
0000: 4d 55 53 49 43 00 00 00 00 00 00 00 00 00 00 00
0010: 09 02 00 04 00 01 04 01 00 00 04 00 78 00 01 01
...
```

---

## 8. Octave byte discovered

By changing only the octave value in `sysex-controls` and comparing program data, we found:

```text
payload offset 0x13 = octave
```

Confirmed values:

```text
0x02 = -2
0x03 = -1
0x04 =  0
```

The consistent mapping is:

```text
-4 -> 0x00
-3 -> 0x01
-2 -> 0x02
-1 -> 0x03
 0 -> 0x04
+1 -> 0x05
+2 -> 0x06
+3 -> 0x07
+4 -> 0x08
```

Equivalent conversion:

```python
octave_byte = octave + 4
```

Valid range:

```text
-4 to +4
```

This is the first confirmed host-controlled hardware state we reverse engineered.

---

## 9. Pad program layout

The MPK Mini 3 program payload stores pad configuration.

### Bank A

Starts at:

```text
0x24
```

### Bank B

Starts at:

```text
0x3C
```

Each pad uses exactly 3 bytes.

From the UI and payload comparison, the layout is:

```text
byte 0 = Note
byte 1 = Program Change
byte 2 = CC
```

Example:

```text
0x24: 24 00 10
```

means:

```text
Pad A1
Note = 0x24 = 36
Program Change = 0
CC = 0x10 = 16
```

The next pad begins three bytes later:

```text
0x27
```

The complete two-bank pad region occupies 48 bytes:

```text
0x24 through 0x53
```

### Important conclusion

There is no extra per-pad byte in this region for:

```text
LED state
LED color
RGB value
brightness
```

The stored pad program data is only:

```text
Note
Program Change
CC
```

---

## 10. Program memory does not expose runtime button state

We repeatedly read the 245-byte program while physically interacting with the controller.

Tests included:

- holding Pad 1
- pressing Octave -
- pressing Octave +
- changing physical runtime state

Result:

```text
no program bytes changed
```

Therefore:

```text
245-byte program payload = stored/configuration state
```

It is not a live runtime-state dump.

This means physical pad illumination and physical Octave button state are not readable through the normal program-query response.

---

## 11. USB inspection

`lsusb` identified:

```text
Bus 003 Device 002
ID 09e8:1049
AKAI Professional M.I. Corp. MPK mini 3
```

`lsusb -t` showed three interfaces:

```text
Interface 0 = HID
Interface 1 = Audio
Interface 2 = Audio / MIDI Streaming
```

Further descriptor inspection showed the endpoint layout.

### HID interface

```text
IN  endpoint 0x81
OUT endpoint 0x01
```

### MIDI streaming interface

```text
OUT endpoint 0x02
IN  endpoint 0x83
```

Meaning:

```text
0x83 = MIDI data from MPK to computer
0x02 = MIDI / SysEx data from computer to MPK
```

---

## 12. usbmon / tcpdump / Wireshark attempts

We enabled:

```text
usbmon
```

and tried captures using:

```text
tcpdump
Wireshark
```

on:

```text
usbmon3
```

The captures contained USB descriptor/control traffic, but they did not give us useful live pad or Octave runtime packets.

Because of this, we stopped pursuing that route.

---

## 13. HID discovery

The MPK exposes:

```text
/dev/hidraw0
```

with:

```text
HID_ID=0003:000009E8:00001049
HID_NAME=AKAI MPK mini 3
```

We read the HID report descriptor.

Raw descriptor:

```text
06 a0 ff 09 01 a1 01 09
02 a1 00 06 a1 ff 09 03
09 04 15 18 25 7f 35 00
45 ff 75 08 95 20 81 02
09 05 09 06 15 80 25 7f
35 00 45 ff 75 08 95 20
91 02 c0 c0
```

Important decoded properties:

```text
Vendor-specific usage page
32-byte INPUT report
32-byte OUTPUT report
No explicit Report ID
```

Therefore the device exposes a vendor-specific host-to-device HID output channel.

---

## 14. HID input test

We opened:

```text
/dev/hidraw0
```

and attempted to read 32-byte HID input reports while:

- pressing Pad 1
- holding Pad 1
- pressing Octave -
- pressing Octave +

Result:

```text
no HID reports appeared
```

Therefore those controls are not exposed through the tested HID input path.

We intentionally did not send arbitrary HID output reports because the 32-byte protocol is undocumented and could contain commands unrelated to LEDs.

---

## 15. Current pad LED conclusion

What we tested:

```text
Normal MIDI Note On/Off        -> pad LEDs did not react
Program SysEx pad fields       -> no LED fields found
Program reads during pad press -> no runtime changes
USB capture                    -> no useful LED protocol found
HID input                      -> no pad/button events
HID output                     -> exists but protocol unknown
```

Current conclusion:

```text
The pad LEDs appear to be controlled internally by firmware,
or by an undocumented vendor-specific HID output protocol.
```

We did not find a safe confirmed command for direct pad LED control.

---

## 16. Current known architecture

The working model is:

```text
                    MPK Mini 3
                        |
        +---------------+----------------+
        |                                |
   MIDI / SysEx                    Internal firmware
        |                                |
        |                                +--> pad LEDs
        |                                +--> octave runtime
        |
        +--> host receives notes
        +--> host can write program configuration
```

A second undocumented channel also exists:

```text
Host
 |
 +--> /dev/hidraw0
      |
      +--> 32-byte vendor-specific OUTPUT report
```

Its purpose remains unknown.

---

## 17. What is confirmed to be controllable

Confirmed:

```text
Octave configuration
Pad Note assignments
Pad CC assignments
Pad Program Change assignments
```

The octave setting was confirmed to produce a physical hardware-visible change.

Pad assignment fields are confirmed in the stored program format.

---

## 18. Python octave control strategy

The safe strategy is:

```text
1. Read the current 245-byte program.
2. Change only payload[0x13].
3. Write the same program back.
```

This preserves the rest of the user's program configuration.

Example API target:

```python
mpk.set_octave(-2)
mpk.set_octave(0)
mpk.set_octave(+2)

mpk.octave_down()
mpk.octave_up()
mpk.reset_octave()
```

---

## Final status

### Solved

```text
MPK Mini 3 MIDI input mapping
Pad note ranges
Polyphonic aftertouch behavior
Akai program SysEx read/write protocol
245-byte program format
Octave byte
Pad Note / PC / CC layout
USB interface and endpoint layout
Vendor HID report sizes
```

### Not solved

```text
Direct pad LED control
Pad LED color control
Live runtime Octave button state reading
Meaning of vendor-specific 32-byte HID OUTPUT reports
```

The project currently has a solid, safe foundation for program-level control, especially octave and pad assignments, while direct pad LED control remains undocumented.
