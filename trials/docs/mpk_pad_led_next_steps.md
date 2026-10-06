# MPK Mini 3 Pad LED Investigation Plan

## Goal

Find a safe way to control the Akai MPK Mini 3 pad LEDs from software.

Desired final API:

```python
mpk.pad_led(bank="A", pad=1, on=True)
mpk.pad_led(bank="A", pad=1, on=False)
```

If color is supported:

```python
mpk.pad_color(bank="A", pad=1, color=...)
```

---

## What is already ruled out

We already tested normal incoming MIDI Note On/Off messages using the same notes assigned to the physical pads.

Result:

```text
Pad LEDs did not light.
```

We also mapped the stored pad configuration.

Each pad uses only three bytes:

```text
Note
Program Change
CC
```

There is no obvious stored LED field.

We also repeatedly read the 245-byte program while pressing and holding pads.

Result:

```text
No program bytes changed.
```

Therefore pad LED state is not represented in the normal program-query payload.

---

## Most important remaining clue

The MPK Mini 3 exposes a vendor-specific HID interface:

```text
/dev/hidraw0
```

The HID descriptor defines:

```text
32-byte INPUT reports
32-byte OUTPUT reports
```

Physical pads did not generate input reports through this interface.

However, the 32-byte OUTPUT path remains unexplored.

That is currently the best remaining stock-firmware candidate for undocumented hardware-control commands.

---

# Recommended investigation order

## Phase 1 - Search for known HID protocol usage

Before sending any custom HID packets, search public code and binaries for:

```text
09e8:1049
MPK mini 3 hidraw
MPK mini 3 HID
Akai 0xFFA0
Akai 0xFFA1
32 byte report
hid_write
HIDIOCSFEATURE
```

Places worth checking:

```text
open-source MPK tools
Linux utilities
Akai editor applications
reverse-engineering repositories
USB/HID dumps
community projects
```

The objective is to find even one known 32-byte host-to-device report.

---

## Phase 2 - Capture an official Akai application if available

If an official Akai configuration application communicates through HID, capture its traffic while changing one setting at a time.

Ideal experiment:

```text
1. Start capture.
2. Open Akai software.
3. Change exactly one setting.
4. Stop capture.
5. Compare packets.
```

If an application exposes any LED-related test or visual feedback, that would be the best possible target.

---

## Phase 3 - Inspect process behavior instead of USB traffic

If raw usbmon capture remains unreliable, observe the application at the system-call level.

Useful Linux approaches may include:

```text
strace
```

Look for writes to:

```text
/dev/hidraw*
```

Example investigation concept:

```bash
sudo strace -f -xx -e trace=openat,read,write,ioctl <application>
```

The goal is to identify:

```text
open("/dev/hidraw0", ...)
write(..., 32-byte-buffer, 32)
ioctl(... HID feature ...)
```

This avoids having to decode lower-level USB packets.

---

## Phase 4 - Compare 32-byte reports

If known HID output reports are discovered, collect multiple examples.

For each action, record:

```text
Action
Report bytes
Changed offsets
Repeated constants
Checksums
Command byte
Pad index
State byte
```

Example table format:

| Action | Byte 0 | Byte 1 | Byte 2 | ... | Byte 31 |
|---|---:|---:|---:|---|---:|
| Known command A | | | | | |
| Known command B | | | | | |
| Known command C | | | | | |

Then infer fields.

---

## Phase 5 - Only send previously observed commands

Do not brute-force all 256 values.

Safe progression:

```text
1. Capture a known command.
2. Replay exactly the same command.
3. Confirm the same harmless result.
4. Modify one likely parameter.
5. Observe behavior.
```

For example, if a command appears to contain a pad index:

```text
original:
AA BB 00 ...

candidate:
AA BB 01 ...
```

Only test controlled one-byte changes after the packet format is understood.

---

# Potential architecture to test

One possible internal architecture is:

```text
Host
 |
 +--> USB MIDI
 |      |
 |      +--> notes
 |      +--> SysEx program configuration
 |
 +--> Vendor HID
        |
        +--> undocumented runtime hardware commands
               |
               +--> LEDs?
               +--> display?
               +--> device modes?
```

This is a hypothesis only.

The existence of a HID OUT report does not prove that it controls LEDs.

---

# Firmware path if HID fails

If no host LED protocol exists, the next level would be firmware reverse engineering.

That would require identifying:

```text
microcontroller
firmware update format
bootloader
firmware image
code signing / verification
LED GPIO or LED driver
pad scanning logic
LED update routine
```

Possible workflow:

```text
official firmware updater
        |
        v
extract update package
        |
        v
identify firmware image
        |
        v
inspect strings / vectors / code
        |
        v
find pad scan + LED routines
```

This is significantly more invasive and should only be attempted after the HID path is exhausted.

---

# Things not to do

Avoid:

```text
random SysEx opcode brute forcing
random 32-byte HID output packets
writing all-zero HID reports
blind firmware flashing
using firmware intended for another MPK generation
```

Especially do not flash firmware from MPK Mini Mk1 projects onto an MPK Mini 3.

---

# Success criteria

The pad investigation is considered successful if we can prove one of these:

## Level 1

```text
Host can turn a pad LED on/off.
```

## Level 2

```text
Host can independently control all 16 pad LEDs.
```

## Level 3

```text
Host can control brightness or color.
```

## Level 4

A clean API exists:

```python
mpk.pad_on("A", 1)
mpk.pad_off("A", 1)

mpk.all_pads_on()
mpk.all_pads_off()
```

If color is actually supported:

```python
mpk.pad_color("A", 1, value)
```

---

# Current status

The normal MIDI and normal program-SysEx paths do not provide confirmed pad LED control.

The strongest remaining stock-firmware path is:

```text
vendor-specific 32-byte HID OUTPUT reports
```

The next useful step is not random packet sending.

The next useful step is finding or capturing one legitimate HID output report and reverse engineering it from there.
