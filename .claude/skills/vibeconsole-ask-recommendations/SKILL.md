---
name: vibeconsole-ask-recommendations
description: Use when VibeConsole's 0 AI asks for AI Recommendations: up to three one-control changes, each with what and why, from the user's usage counts and mappings.
---

# AI Recommendations

VibeConsole sends (only after the user agreed to share): 30 days of counts, `press<TAB><control><TAB><n>` per control and `family<TAB><family><TAB><n>` per action family, and the Default mappings, each control with its keys or only the kind of action. Never prompt text, keys typed, folders or agent output.

You suggest up to three changes that would save presses.

## Steps

1. Look for: a much-pressed control doing something a closer control could do; an action family used a lot with no control of its own; an app whose controls are pressed while a profile could make them easier; unused controls next to busy ones.
2. For each change write one line `recommendation<TAB><what to change, one sentence><TAB><why, one sentence naming the count>`, then the change as a file with exactly one override line.
3. For a Default mapping, line 2 is `profile<TAB>AI<TAB>-<TAB>1`. For an app profile, line 2 is `profile<TAB><new name><TAB><desktop ID><TAB>1`.

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
recommendation	Put Close tab on Xbox A	You pressed Bank A Pad 2 for Ctrl+W 40 times a day with the controller in hand.
vibeconsole-profile-v1
profile	AI	-	1
239	Xbox A	pad	xbox	button	1	0	0	hold	Ctrl+W	-	-	-	shortcut	-	-
```

## What VibeConsole refuses

The whole reply is refused, and nothing is saved, when any line has: a terminal command (`command:`, also inside a sequence), a risky action (one that needs a one-second hold, or quits an app), a control the request didn't list, a light, a wrong field count, or a count on line 2 that doesn't match. The user can add commands themselves in /configure; you can't.

A recommendation whose file has more than one line is dropped.

## On the screen

Each shows its what in bold and its why under it. Enter reviews and saves (a replacement asks first), Space hides it for 30 days, R asks again.
