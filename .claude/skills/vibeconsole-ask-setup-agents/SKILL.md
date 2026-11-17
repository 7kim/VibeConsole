---
name: vibeconsole-ask-setup-agents
description: Use when VibeConsole's 0 AI asks to set up agents ("Ask … to set up agents"): propose agents, a default folder and status-light folders from the CLIs and project folders it found.
---

# Ask to set up agents

VibeConsole sends: the names of the agent CLIs it found on PATH (looked up, never run: claude, codex, opencode, ollama, gemini, aider, goose, qwen, copilot, omp), the paths of git repositories under the home folder (3 folders deep, 40 at most), the agents already set up, and the user's wish. Nothing else.

You reply with tab-separated lines only. Each becomes a tmux agent VibeConsole starts later, a default folder, or a status-light source.

## Lines

- `agent<TAB><name><TAB><cli><TAB><folder><TAB><octave>`: a new agent. `<name>`: letters, digits, `-` and `_`, new (not in the agents already set up), also used as its tmux window. `<cli>`: exactly one CLI name that was found, bare, with no options; or `ollama run <model>` when `ollama` was found. `<folder>`: one of the folders sent, exactly as sent. `<octave>`: -4…-1 or 1…4, different for each agent (it is the agent's light on the controller).
- `agent-dir<TAB><folder>`: at most one; the folder agents start in when they have none.
- `light-folder<TAB><folder><TAB><octave>`: sessions working in that folder light that octave.

## Steps

1. One agent per CLI the user will use, each in the project folder the wish points to (or the most likely one).
2. Give each a different octave: +1 for the first, -1 for the second, then +2, -2.
3. Add `agent-dir` for the folder most agents share, and a `light-folder` for each other project worth telling apart.

## Example

With `claude` found and `/srv/code/api` sent:

```reply
agent	api	claude	/srv/code/api	2
agent-dir	/srv/code/api
light-folder	/srv/code/api	2
```

## What VibeConsole refuses

The whole reply, when any line has: a CLI that wasn't found, an option (`claude --dangerously-skip-permissions`), a pipe or any other command, a folder that wasn't sent, an octave outside -4…4 or 0, or a name already taken. Lines it doesn't recognise are ignored; a reply with no agent, folder or light line is refused.

## Before it's saved

Each change in words ("Agent api runs claude in tmux in /srv/code/api; octave light 2."), what was sent, and that nothing was run; then Cancel or Save.
