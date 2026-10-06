# MPK Mini 3 Button and LED Control Investigation

## Goal

Investigate which non-pad controls and LEDs on the Akai MPK Mini 3 can be controlled from Python or Rust.

Controls of interest:

```text
Bank A / Bank B
CC
Arpeggiator On/Off
Tap Tempo
Full Level
Note Repeat
Octave + / -
```

The important distinction is that three different operations may exist:

```text
1. stored configuration
2. runtime feature state
3. LED state
```

They must not be assumed to be the same thing.

---

# 1. Known confirmed control

## Octave

Octave configuration is confirmed controllable through Akai program SysEx.

Known payload field:

```text
payload[0x13]
```

Known mapping:

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

Changing this value produces a physical hardware-visible octave change.

Target API:

```python
mpk.set_octave(-1)
mpk.set_octave(0)
mpk.set_octave(+2)

mpk.octave_down()
mpk.octave_up()
mpk.reset_octave()
```

---

# 2. Stored configuration controls

The `sysex-controls` project shows that multiple MPK Mini 3 settings are stored in the program data.

These are strong candidates for software control through the same read-modify-write SysEx method.

Examples include:

```text
Transpose
MIDI channel
Arpeggiator configuration
Pad Note assignment
Pad CC assignment
Pad Program Change assignment
```

The correct technique is:

```text
1. read current 245-byte program
2. change one setting using sysex-controls
3. compare before and after
4. identify changed offset
5. reproduce it in Python
```

This is how the octave byte was found.

---

# 3. Arpeggiator

The MPK Mini 3 configuration UI includes an Arpeggiator section.

The source contains an `Enabled` setting.

Therefore an Arpeggiator enable/disable configuration byte is a strong candidate for the same SysEx program format.

Desired test:

```text
Read program
-> set Arpeggiator Enabled OFF
-> capture program
-> set Arpeggiator Enabled ON
-> capture program
-> compare
```

If exactly one or a few bytes change, document them.

Possible final API:

```python
mpk.arp_on()
mpk.arp_off()
```

Other possible stored Arpeggiator parameters can be mapped in the same way.

Potential API:

```python
mpk.set_arp_rate(...)
mpk.set_arp_mode(...)
mpk.set_arp_octaves(...)
```

Only implement fields after confirming their offsets.

---

# 4. Bank A / Bank B

We already know the stored pad definitions for both banks.

Program payload:

```text
Bank A starts at 0x24
Bank B starts at 0x3C
```

Each bank contains eight pads.

However:

```text
stored Bank A/B pad definitions
```

are not necessarily the same thing as:

```text
currently active physical Bank A/B runtime state
```

The active bank may be firmware runtime state.

Therefore investigate two separate capabilities:

```python
mpk.set_pad_note(bank="A", pad=1, note=60)
```

versus:

```python
mpk.select_bank("A")
```

The first is already strongly supported by the program layout.

The second is not yet confirmed.

---

# 5. CC button / CC mode

The exact meaning of the physical CC-related control should be tested separately from stored pad CC assignments.

We already know each pad stores a CC value.

Example API for configuration:

```python
mpk.set_pad_cc(bank="A", pad=1, cc=74)
```

But that does not automatically prove we can remotely toggle the physical CC mode or LED.

If the CC button has a light, test it as a runtime LED separately.

---

# 6. Tap Tempo

Tap Tempo appears to be a runtime action rather than ordinary stored program configuration.

Possible distinct operations:

```python
mpk.tap_tempo()
```

and:

```python
mpk.tap_tempo_led(True)
```

These are not necessarily the same command.

A host may be able to light the button without causing a tap event.

Likewise a tap event may exist without direct LED control.

Test them independently.

---

# 7. Full Level

Full Level also appears to be a runtime mode.

Potential operations:

```python
mpk.full_level_on()
mpk.full_level_off()
```

and separately:

```python
mpk.full_level_led(True)
mpk.full_level_led(False)
```

Again, do not assume feature state and LED state share the same command.

---

# 8. Note Repeat

Potential runtime feature:

```python
mpk.note_repeat_on()
mpk.note_repeat_off()
```

Potential LED-only control:

```python
mpk.note_repeat_led(True)
mpk.note_repeat_led(False)
```

These need separate testing.

---

# 9. Important clue from previous MIDI test

When we sent ordinary MIDI messages to the MPK while trying to light the pads:

```text
pads did not light
```

but some other physical button LEDs reacted.

This is important.

It means at least some non-pad LEDs may respond to incoming MIDI.

Therefore the best next experiment is to map:

```text
incoming MIDI message
->
specific physical LED response
```

instead of starting with SysEx.

---

# 10. MIDI LED mapping experiment

Create a small Python test that sends one MIDI message at a time.

For each MIDI note number:

```text
0-127
```

send:

```text
Note On
wait briefly
Note Off
```

Observe which hardware LED reacts.

Do this carefully and record only LED behavior.

Suggested data table:

| MIDI channel | Note | Velocity | Hardware LED |
|---:|---:|---:|---|
| 1 | ? | 127 | Arp |
| 1 | ? | 127 | Tap Tempo |
| 1 | ? | 127 | Full Level |
| 1 | ? | 127 | Note Repeat |
| 1 | ? | 127 | Bank A |
| 1 | ? | 127 | Bank B |

Because previous experimentation already caused non-pad LEDs to react, this is a justified mapping experiment.

---

# 11. Safer scanning strategy

Do not send arbitrary SysEx commands.

For ordinary MIDI note scanning, use a controlled process.

For example:

```text
Channel 1
Note 0
Note Off

Channel 1
Note 1
Note Off

...
```

Pause between messages so the physical response can be observed.

Record:

```text
note number
channel
velocity
LED that lights
whether LED stays latched
whether Note Off clears it
```

If nothing useful is found on one channel, repeat only on channels that make sense.

---

# 12. Separate LED state from device behavior

For every discovered message, test:

```text
Does only the light change?
```

or:

```text
Does the actual hardware mode change too?
```

For example:

```text
incoming MIDI lights Full Level LED
```

does not prove:

```text
Full Level processing is enabled
```

Test by playing pads and observing velocity behavior.

Similarly:

```text
Bank B LED lights
```

does not prove that Bank B pad assignments became active.

---

# 13. Suggested API architecture

Keep configuration, runtime behavior, and LEDs separate.

```python
class MPKMini3:
    # Program configuration
    def set_octave(self, octave): ...
    def set_transpose(self, semitones): ...
    def set_pad_note(self, bank, pad, note): ...
    def set_pad_cc(self, bank, pad, cc): ...
    def set_pad_program(self, bank, pad, program): ...

    # Runtime features
    def arp_on(self): ...
    def arp_off(self): ...
    def select_bank(self, bank): ...
    def full_level_on(self): ...
    def full_level_off(self): ...
    def note_repeat_on(self): ...
    def note_repeat_off(self): ...
    def tap_tempo(self): ...

    # LEDs
    def arp_led(self, enabled): ...
    def tap_tempo_led(self, enabled): ...
    def full_level_led(self, enabled): ...
    def note_repeat_led(self, enabled): ...
    def bank_led(self, bank, enabled): ...
```

Only methods backed by confirmed commands should be implemented.

---

# 14. Recommended order of investigation

## First

Map the non-pad LEDs that already reacted to incoming MIDI.

Priority:

```text
Arpeggiator
Tap Tempo
Full Level
Note Repeat
Bank A
Bank B
```

## Second

Map Arpeggiator stored configuration by comparing SysEx program payloads.

## Third

Map other stored configuration such as:

```text
Transpose
Arp rate
Arp mode
Arp octave range
```

## Fourth

Investigate whether runtime button actions can be remotely triggered.

Examples:

```text
select active Bank A/B
Full Level mode
Note Repeat mode
Tap Tempo event
```

---

# 15. Best immediate experiment

The next experiment should focus only on LED mapping.

Goal:

```text
Find which incoming MIDI messages light which non-pad LEDs.
```

Then create a confirmed mapping such as:

```text
MIDI Note X -> Arp LED
MIDI Note Y -> Tap Tempo LED
MIDI Note Z -> Full Level LED
...
```

Once that table exists, Python and Rust implementations become straightforward.

---

# Desired final result

A clean control library could eventually expose:

```python
mpk.set_octave(-1)

mpk.arp_on()
mpk.arp_led(True)

mpk.select_bank("B")
mpk.bank_led("B", True)

mpk.full_level_on()
mpk.full_level_led(True)

mpk.note_repeat_on()
mpk.note_repeat_led(True)

mpk.tap_tempo()
mpk.tap_tempo_led(True)
```

But the implementation must preserve the distinction between:

```text
configuration
runtime state
LED state
```

and each command should be added only after it is experimentally confirmed.
