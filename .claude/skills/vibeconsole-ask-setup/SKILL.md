---
name: vibeconsole-ask-setup
description: Use when VibeConsole's 0 AI asks for a whole setup ("Ask … for a whole setup"): pads and buttons for a goal such as "I stream" or "I code with Claude", saved as Default mappings.
---

# Ask for a whole setup

The user names a goal ("I stream", "I code with Claude", "I edit video"). You turn it into Default mappings for the devices they picked.

VibeConsole sends: the goal, and for each picked device every control by name with a ready line and what it does now (`(empty)` or `(now: …)`).

## Steps

1. List what someone with that goal presses all day (streaming: scene switches, mute mic, start/stop; coding with Claude: new tab, run tests, accept, interrupt).
2. Put the most frequent on the easiest controls (Xbox A/B/X/Y, Bank A pads 1–4).
3. Controls in use may be replaced where the goal needs them; the user confirms each replacement on its own screen and can keep the old one.
4. Keep line 2 as `profile<TAB>AI<TAB>-<TAB><count>`. Reply with only the file.

## The line format

A reply's file is tab-separated. Line 1 is `vibeconsole-profile-v1`. Line 2 is `profile<TAB><name><TAB><apps><TAB><count>`, where `<count>` is the exact number of lines that follow. Every following line has exactly 16 fields:

| # | Field | What to write |
|---|---|---|
| 1 | program | as in the request's line: `0` for the Akai's Program 1, `239` for the Xbox |
| 2 | label | the control's name from the request (`Bank A Pad 1`, `Xbox A`) |
| 3–8 | input, device, message, channel, id, direction | copied from the request's line, unchanged |
| 9 | behaviour | `hold` or `toggle` for a shortcut; `trigger` for any other action |
| 10 | keys | `Key+Key` names (`Ctrl+Shift+T`) for a shortcut; `-` otherwise |
| 11–13 | light, octave, tempo | `-` (no feedback) |
| 14 | action | `shortcut`, or an action form the request allows |
| 15–16 | risky, status | `-` |

The request gives a ready line for every control you may use. Copy that line and change only fields 9, 10 and 14. A control the request didn't list doesn't exist for you.

## Example

```reply
vibeconsole-profile-v1
profile	AI	-	2
0	Bank A Pad 1	pad	09e8:1049	note	10	36	0	hold	Ctrl+T	-	-	-	shortcut	-	-
239	Xbox A	pad	xbox	button	1	0	0	hold	Ctrl+W	-	-	-	shortcut	-	-
```

## What VibeConsole refuses

The whole reply is refused, and nothing is saved, when any line has: a terminal command (`command:`, also inside a sequence), a risky action (one that needs a one-second hold, or quits an app), a control the request didn't list, a light, a wrong field count, or a count on line 2 that doesn't match. The user can add commands themselves in /configure; you can't.

## Before it's saved

Each fill as `(was empty)`, each accepted replacement as `old → new`; then Cancel or Save.
