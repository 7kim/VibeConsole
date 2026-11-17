# Every action

Generated from the code (`UPDATE_SKILLS=1 cargo test vibeconsole_skills`). `<US>` joins steps. A pad's default is `shortcut`.

- `shortcut`: Press the keys in the keys field (a pad's default)
- `sequence:keys:Ctrl+T<US>wait:500<US>keys:Ctrl+L`: Run steps in order: keys, waits, prompts, commands
- `saved-sequence:name`: Run a saved prompt sequence by name
- `nothing`: Silence a control
- `orca-keys:keys:Ctrl+K<US>wait:200`: Press keys inside Orca IDE only while it is focused
- `choice:prompt:a:@ctrl-v:paste<US>prompt:b:@ctrl-v:paste`: A knob that picks one of 2-16 prompts
- `value:2:0:100:/tmp/value`: A knob that writes a number between min and max to a file
- `prompt:review:@ctrl-v:paste`: Paste (or send with Enter) a saved prompt into an agent or the focused window
- `prompt-own:step1:@ctrl-v:paste:6869`: A sequence step's own prompt text (hex)
- `prompts:prompt:a:@ctrl-v:paste<US>prompt:b:@ctrl-v:paste`: Send prompts one per press
- `command:dev:/tmp:cargo test`: Type a command line in a tmux window (needs a one-second hold)
- `app:/usr/share/applications/firefox.desktop`: Launch an installed app
- `app-toggle:minimize:/usr/share/applications/firefox.desktop`: Launch it, or minimize it when focused
- `app-toggle:close:/usr/share/applications/firefox.desktop`: Launch it, or close it when focused
- `app-toggle:minimize+hold-quit:/usr/share/applications/firefox.desktop`: Tap launches or minimizes; a one-second hold quits
- `app-quit:/usr/share/applications/firefox.desktop`: Quit an app (needs a one-second hold)
- `volume-up:5`: Raise the default output volume by a percent
- `volume-down:5`: Lower the default output volume by a percent
- `output-mute`: Toggle the default output mute
- `microphone-mute`: Toggle the microphone mute
- `flow`: Start or repair Wispr Flow
- `orca-jump:claude:/tmp`: Jump to an agent in Orca IDE for a folder
- `t3:chat.new`: Run a T3 Code command
- `herdr-focus:claude`: Focus a herdr agent
- `tmux-jump:claude`: Bring a tmux agent's window to the front
