---
name: vibeconsole-doctor
description: Use when the user pastes or asks about `vibeconsole doctor` output, or something is "unavailable", "missing" or "unverified": reads each line and gives the one fix.
---

# Reading `vibeconsole doctor`

Run it, then go line by line. Report only what a line says; never guess.

| Line | Meaning | Fix |
|---|---|---|
| `uinput read/write` not ready | VibeConsole can't make a virtual keyboard | `sudo modprobe uinput` and `sudo setfacl -m "u:$USER:rw" /dev/uinput` |
| `MIDI unavailable` | no Akai or MIDI port | replug it; close other MIDI apps |
| `Xbox controllers: none connected` | no controller seen | plug it in or pair it; USB or Bluetooth is named per controller |
| `Xbox headset audio` | headset audio needs a USB driver | the Bluetooth firmware carries none; on USB use the xone driver in Settings |
| `Xbox button light` unavailable | the light needs the xone driver and its udev rule | Settings, Xbox tuning |
| `Keyboard M light` / `RGB keyboard` unavailable | OpenRazer's driver isn't loaded or its files aren't writable | install OpenRazer, join the `plugdev` group, log out and in |
| `Keyboard G light` unavailable | lighting it turns on game mode, which turns off the Super key | by design |
| `Login service` | whether it starts at login | `vibeconsole startup-enable` |
| `Window control extension` | launcher pads can minimize or close | log out and in once, then `gnome-extensions enable vibeconsole@vibeconsole.local` |
| hooks not installed | agent states won't update | run `vibeconsole install` |

Only one VibeConsole runs at a time: if the port is busy, run `vibeconsole startup-stop` first.
