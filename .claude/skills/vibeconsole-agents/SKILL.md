---
name: vibeconsole-agents
description: Use when the user asks about agent states, status lights, which agent is working or needs input, agent apps (Orca IDE, T3 Code, herdr, tmux) or how a hook reports a state.
---

# Agents and states

- An agent reports one of five states: `idle`, `thinking`, `needs-input`, `finished`, `needs-decision`. Hooks call `vibeconsole agent-status <agent> <state>`.
- Sources: agents started in the tmux session `vibe` (`tmux attach -t vibe`), Claude Code and Codex hooks, and hosted agents in Orca IDE, T3 Code and herdr.
- A state becomes a light pattern per device; see `vibeconsole-lights`.
- `finished` clears once the user looks at that agent's window.
- Hooks are installed by `vibeconsole install`; `vibeconsole doctor` says whether they are.

When an agent's light is wrong, check in this order: `vibeconsole doctor` (hooks), the agent's name in `vibeconsole agent-status`, then the Status lights setting.
