<h1 align="center">🎛️ VibeConsole</h1>

<p align="center">
  <b>Vibecode with the gear you already own.<br/>Your MIDI keyboard or game controller becomes the control surface for your AI coding agents.</b>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/open%20source-yes-brightgreen" />
  <img src="https://img.shields.io/badge/language-Rust%202024-orange" />
  <img src="https://img.shields.io/badge/devices-MIDI%20%7C%20Xbox-blueviolet" />
  <img src="https://img.shields.io/badge/agents-Claude%20Code%20%7C%20Codex-black" />
  <img src="https://img.shields.io/badge/desktop-Fedora%20GNOME%20Wayland-2563EB" />
</p>

---

## 💡 Why

You talk to your AI agent now. Speech-to-text writes the prompt, and the agent writes the code. What's left for your hands is a handful of moves: talk, send, approve, switch agent, stop. You don't need 104 keys for that.

OpenAI and Work Louder made the **Codex Micro** for this: a $230, limited-edition keypad for driving AI agents. It inspired VibeConsole. You probably already own something with buttons, knobs and sticks: a MIDI keyboard on your desk, a game controller in a drawer. VibeConsole turns it into your own Codex Micro, for free and in the open.

---

## ✨ What it does

- 🎙️ **Hold a pad to talk.** Push-to-talk for Wispr Flow, so you dictate instead of typing.
- 🤖 **Drive your agents.** Send saved prompts to Claude Code or Codex, step through a prompt sequence one tap at a time, and run commands. Risky actions fire only after a one-second hold.
- 🚦 **See what your agents are doing.** Controller lights show when an agent is thinking, needs you, or has finished.
- ⌨️ **Shortcuts and apps.** Any key combo, app launcher, volume or mic mute on any pad, button, knob or stick.
- 🗂️ **Profiles per app.** Your controls change with the focused app, like Logitech G HUB does for games.

---

## 🎮 Supported devices

| Device | Status |
|---|---|
| 🎹 Akai MPK Mini MK3 | ✅ Supported, with pad lights, octave and tempo as status lights |
| 🎹 Any MIDI controller | ✅ Supported |
| 🎮 Xbox controllers (USB) | ✅ Supported, with rumble, Xbox button light and headset |
| ⌨️ Your keyboard's Caps, Num and Scroll lights | ✅ As status lights |
| 🎮 PlayStation 5 DualSense | 🔜 Next version |
| 🕹️ Steam Deck | 🔜 Next version |
| 🎮 Xbox controllers over Bluetooth | 🔜 Planned |

Use a MIDI controller and an Xbox controller at the same time if you like.

---

## 🚀 Quick start

VibeConsole runs on Linux and needs [Rust](https://rustup.rs). It's tested on Fedora with GNOME on Wayland.

```bash
sudo dnf install alsa-lib-devel alsa-utils usbutils acl tmux wl-clipboard
git clone https://github.com/7kim/VibeConsole.git && cd VibeConsole
scripts/install.sh       # builds and installs to ~/.local/bin
vibeconsole doctor       # checks your setup
vibeconsole              # plug in your controller and go
```

Open **Configure**, press a pad or button, and choose what it does.

---

## 🛣️ Coming next

- A desktop app with a tray icon and a hints window that shows your controller in 3D, with what each button does.
- PlayStation 5 DualSense and Steam Deck support.
- Packages for Linux (AppImage, rpm, deb) and Windows.

---

## 🤝 Contributing

VibeConsole is open source, and contributions are welcome. Commands, configuration, architecture and testing are in [README-DEVELOPER.md](README-DEVELOPER.md).
