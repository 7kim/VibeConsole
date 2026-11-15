# File formats

## A mapping line (16 tab-separated fields)

| # | Field | Values |
|---|---|---|
| 1 | context | `0`-`7` Akai Programs 1-8; `239` the Xbox controller |
| 2 | label | the control's name as the user sees it |
| 3 | input | `pad`, a knob kind, `piano` or `joystick` |
| 4 | device | `09e8:1049` for the Akai, `xbox` for the Xbox controller |
| 5 | message | `note`, `cc`, `bend`, `button` |
| 6 | channel | MIDI channel (Xbox: `1`) |
| 7 | id | note, CC or button number |
| 8 | direction | `0`, or `1`/`-1` for a knob or stick side |
| 9 | behavior | `hold`, `toggle`, `pulse`, `tap`, `trigger` |
| 10 | keys | `Key+Key` names for a shortcut, else `-` |
| 11-13 | octave, tempo, arp | Akai light feedback, `-` when off |
| 14 | action | `shortcut` or an action string ([actions](actions.md)) |
| 15 | risky | `risky` fires only after a one-second hold, else `-` |
| 16 | status | `status` shows sent/running/stopped in the live pane, else `-` |

## A profile file

```
vibeconsole-profile-v1
profile<TAB>NAME<TAB>APP,APP<TAB>COUNT
<COUNT override lines, 16 fields each>
```

- The name is 1-32 characters and not `Default`. Apps are desktop IDs, never commands.
- Overrides are pads and Xbox buttons only, behaviour `hold` or `toggle`, no light feedback, no control twice.
- A profile applies while one of its apps is focused; everything else falls back to Default.
- Imports are checked in full before anything is saved. A bad line saves nothing.

## Names

Prompt, variable, sequence and profile names are letters, digits, `-` and `_`. They can't leave their folder.
