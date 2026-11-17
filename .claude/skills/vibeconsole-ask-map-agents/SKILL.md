---
name: vibeconsole-ask-map-agents
description: Use when VibeConsole's 0 AI asks to map agents ("Ask … to map agents"): jump buttons for each agent, a needs-me button and an octave light per agent.
---

# Ask to map agents

VibeConsole sends: the agents (name, host: tmux, orca or herdr, octave light), the allowed actions written out (`- jump to <agent>: <action>` for each agent, and `- needs-me …`), the user's wish, and for each picked device every control by name with a ready line and what it does now.

You reply with two parts: octave lines first, then a mappings file.

## Steps

1. One `agent-octave<TAB><name><TAB><octave>` line per agent: -4…-1 or 1…4, each agent a different one. The octave light tells the user which agent is busy.
2. One jump button per agent, on an `(empty)` control. Copy the action exactly as the request wrote it for that agent: `tmux-jump:<window>` (tmux), `orca-jump:<kind>:<folder>` (Orca), `herdr-focus:<name>` (herdr).
3. One needs-me button (Orca's agent waiting on the user): copy its action exactly as the request wrote it.
4. In the file, line 2 stays `profile<TAB>AI<TAB>-<TAB><count>`. Action lines use behaviour `trigger` and keys `-`.

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
agent-octave	claude	1
agent-octave	codex	-1
vibeconsole-profile-v1
profile	AI	-	1
239	Xbox B	pad	xbox	button	1	1	0	trigger	-	-	-	-	tmux-jump:claude	-	-
```

## What VibeConsole refuses

Everything in the file rules, plus: any action other than a jump to an agent that is set up (the same window, folder or name) or needs-me, an octave line for an agent that doesn't exist, an octave outside -4…4 or 0.

## Before it's saved

Each fill as `(was empty)`, each replacement on its own `Replace?` screen, each octave change as `old → new`; then Cancel or Save.
