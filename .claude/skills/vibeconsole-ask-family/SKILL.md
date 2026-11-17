---
name: vibeconsole-ask-family
description: Use when VibeConsole's 0 AI asks for an action family ("Ask … for an action family"): a named group of existing actions the user picks from in /configure.
---

# Ask for an action family

An action family is a named group of actions ("Browser keys": new tab, close tab, louder). In /configure › Your families the user picks the family, then one action, and it is copied onto the control they learned. Families only group actions that already exist; they never add a new kind.

VibeConsole sends: what the family should group, the action forms there are (each with an example and what it does), and the user's own actions (keys, or the action itself).

## Steps

1. Pick 3–16 actions that belong together for the wish. Prefer the user's own actions; add others only from the forms listed.
2. Name the family (1–32 characters) and each action (field 2, short: "Close tab").
3. Put the actions on Bank A pads in order (note 36, 37, 38… in field 7): these are placeholders, not where they will be mapped.
4. Reply with only the file. Line 2: `profile<TAB><family name><TAB>-<TAB><count>`.

## Fields per action line

16 tab-separated fields: `0`, the action's name, `pad`, `09e8:1049`, `note`, `10`, the note (36, 37…), `0`, behaviour (`hold` or `toggle` for a shortcut, `trigger` otherwise), keys (`Key+Key` for a shortcut, else `-`), `-`, `-`, `-`, the action (`shortcut` or a form), `-`, `-`.

## Example

```reply
vibeconsole-profile-v1
profile	Browser keys	-	3
0	New tab	pad	09e8:1049	note	10	36	0	hold	Ctrl+T	-	-	-	shortcut	-	-
0	Close tab	pad	09e8:1049	note	10	37	0	hold	Ctrl+W	-	-	-	shortcut	-	-
0	Louder	pad	09e8:1049	note	10	38	0	trigger	-	-	-	-	volume-up:5	-	-
```

## What VibeConsole refuses

A terminal command (`command:`), a risky action (needs a one-second hold, or quits an app), an action form that doesn't exist, a wrong field count, a count on line 2 that doesn't match, a family name that already exists.

## Before it's saved

The actions are listed by name; Save writes `families/<name>.family`.
