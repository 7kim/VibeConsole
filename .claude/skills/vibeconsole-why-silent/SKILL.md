---
name: vibeconsole-why-silent
description: Use when a pad or Xbox button did nothing: walks the screens and states where VibeConsole deliberately fires nothing and names the cause.
---

# Why did my pad not fire?

Ask which screen VibeConsole was on and whether the header said Running. Then:

| Where | Fires? | Why |
|---|---|---|
| Command list, Running | yes | normal |
| `/list`, `/configure` menus | yes | mapped controls run there |
| Header says Paused | no | `/pause` releases keys and stops output; pick Run |
| Program unverified (Akai) | no | choose the program with `/prog-select`; the Xbox alone still runs |
| Learn, shortcut recording | no | the press is captured, not fired |
| The editor opened from a menu | no | output is suspended |
| `/detect` | no | shows buttons live, fires nothing |
| Settings, Agents, `/prog-select`, `/program-name`, `/feedback-idle`, `/feedback-check` | not yet | they suspend output while open (a planned change makes them fire between controller writes) |
| First press after a transition | yes, the next one | queued presses are dropped on purpose; press again |
| A risky control tapped | no | hold it one second |
| The Xbox button (Guide) | never a mapping | it cycles profiles and locks |

Then check, in order: `vibeconsole doctor`, `vibeconsole list` (is the control mapped in this program?), the active profile (the focused app may have its own override).
