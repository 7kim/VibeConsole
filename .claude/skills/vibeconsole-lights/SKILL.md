---
name: vibeconsole-lights
description: Use when designing or explaining status-light patterns: what the Akai pads, the Xbox button light and rumble, and the Razer keyboard lights do for each agent state.
---

# Status lights

One agent state, up to four devices. Patterns live in Settings, Agents, Status light patterns.

| Device | What lights | Needs |
|---|---|---|
| Akai MPK mini 3 | octave and tempo lights on a verified program | a verified program; restored on exit |
| Xbox controller | the Xbox button light (modes, brightness, tempo) and rumble | the xone driver and its udev rule for the light; rumble only while VibeConsole owns the controller |
| Razer keyboard | Caps Lock, Scroll Lock and the M light | the M light needs OpenRazer |
| Razer keyboard RGB | a colour effect per state | OpenRazer; the effect can't be read back, so on idle, pause, quit and error the resting effect the user picked comes back |

## Designing a pattern

- `needs-input` and `needs-decision` should be the loudest (fast blink, rumble for a decision); `thinking` a slow breathe; `idle` off; `finished` a short steady light.
- Never rely on colour alone: also change the blink rate.
- Every light is restored when VibeConsole quits or pauses. Say so when the user asks.
