---
name: vibeconsole-ask-profile
description: Use when VibeConsole's 0 AI or 6 Apps & profiles asks for an app profile ("Ask … for a profile"): pads and Xbox buttons that change while one app is focused.
---

# Ask for a profile

VibeConsole sends: the app's desktop ID (`firefox.desktop`), the user's wish (may be empty), and for each device the user picked (the Akai MPK mini 3, the Xbox controller, or both) every control by name with a ready line. Nothing else about the computer.

You reply with one app profile: overrides that apply only while that app is focused. Every other control keeps its Default meaning.

## Steps

1. Read the wish and the app. Pick the functions that app's users press most (tabs, undo, search, play/pause), unless the wish says otherwise.
2. For each function, pick a control from the request's list on each chosen device. The same function may sit on both devices: one line per device.
3. Give the profile a name of 1–32 characters (not `Default`) and put the app's desktop ID in field 3 of line 2.
4. Reply with only the file. Prose around it is ignored; the file is what counts.

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
profile	Firefox	firefox.desktop	2
0	Bank A Pad 1	pad	09e8:1049	note	10	36	0	hold	Ctrl+T	-	-	-	shortcut	-	-
239	Xbox A	pad	xbox	button	1	0	0	hold	Ctrl+W	-	-	-	shortcut	-	-
```

## What VibeConsole refuses

The whole reply is refused, and nothing is saved, when any line has: a terminal command (`command:`, also inside a sequence), a risky action (one that needs a one-second hold, or quits an app), a control the request didn't list, a light, a wrong field count, or a count on line 2 that doesn't match. The user can add commands themselves in /configure; you can't.

## Before it's saved

VibeConsole shows the profile by device and in plain words ("Bank A Pad 1 on the Akai MPK mini 3 presses Ctrl+T in firefox.desktop."). The user saves or cancels. A profile name that already exists is refused.
