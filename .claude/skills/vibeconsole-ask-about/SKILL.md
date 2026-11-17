---
name: vibeconsole-ask-about
description: Use when VibeConsole's 0 AI asks a question about the user's setup ("Ask … about my setup", e.g. "Why didn't Pad 6 fire?"): answer from the facts sent, change nothing.
---

# Ask about my setup

VibeConsole sends: the user's question, what `vibeconsole describe` says (each device's controls and what each does, per program, layer and app profile) and the problems `vibeconsole doctor` sees, each with its stage.

You answer in plain words, short, from those facts only. You change nothing.

## Steps

1. Find the control or feature the question names in the describe text. Use the names it uses (`Bank A Pad 6`, `Xbox Y`).
2. Give the reason from the facts. The usual reasons a control does nothing, in the order to check:
   - VibeConsole is paused (the header says PAUSED), or the screen open is one where nothing fires (Learn, Detect, a menu that takes over).
   - The control has no mapping in the program, layer or app profile in use.
   - The Akai program isn't verified (lights and pads wait for a verified program).
   - A risky mapping needs a one-second hold.
   - A problem doctor lists (uinput not writable, the window extension off, an app not installed).
3. Say the one fix ("Map it in 2 Mappings › /configure", "Resume on 1 Live"). When the facts don't tell, say so and what to look at.
4. Optionally, when one app profile would fix it, add it after your answer as a file in a ``` block. It goes through the profile review; nothing is saved without the user's Save.

## A proposed profile

```reply
Bank A Pad 1 has no mapping while Firefox is focused. This profile gives it New tab:
vibeconsole-profile-v1
profile	Firefox	firefox.desktop	1
0	Bank A Pad 1	pad	09e8:1049	note	10	36	0	hold	Ctrl+T	-	-	-	shortcut	-	-
```

The file follows the profile format exactly: line 2 is `profile<TAB><name><TAB><desktop ID><TAB><count>`, then one 16-field line per control copied from the describe text's names. No terminal commands and no risky actions: VibeConsole refuses them.
