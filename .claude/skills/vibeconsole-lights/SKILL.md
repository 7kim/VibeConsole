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
| RGB lights: Razer keyboard, Nommo speakers, Firefly mousepad, Logitech G502 logo | one shared effect per state and one resting effect that every switched-on light follows; Per device gives a light its own effects instead | OpenRazer (Razer), logirgb (G502); a Razer effect can't be read back, so idle, pause, quit and error show the resting effect; the G502 goes back to its own stored lighting |

RGB effects (`lights.json`, words like `static #ff8800`): off, static, breath (one colour; Razer also two colours or random; the G502 logo also period, waveform, intensity), reactive (keyboard, speed 1–4), wave (Razer, direction 1–2), spectrum. A colour is a name (blue, orange, red, green, purple, white) or the user's own, typed as Red, Green and Blue (each 0–255) or as a hex code `#rrggbb`; it is saved as `#rrggbb`. The shared list offers only effects every switched-on light shows (with the G502 on: off, static, breath, spectrum); an effect a light lacks is named and never written.

## Designing a pattern

- `needs-input` and `needs-decision` should be the loudest (fast blink, rumble for a decision); `thinking` a slow breathe; `idle` off; `finished` a short steady light.
- Never rely on colour alone: also change the blink rate.
- Every light is restored when VibeConsole quits or pauses. Say so when the user asks.
