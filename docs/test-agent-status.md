# Testing agent status lights by hand

`vibeconsole agent-status` sets an agent's state, just as Claude's and Codex's hooks do. Use it to test the status lights (MPK, Xbox button light and rumble, keyboard LEDs) without waiting for a real agent. Run it in any terminal while VibeConsole is running (interactive **Run**, or the login service) with Settings → Status lights **On**.

```
vibeconsole agent-status <agent> <state>
```

## Agents

`<agent>` is the name of an agent in Settings → Agents. On this machine:

| Agent | Host | Octave light |
|---|---|---|
| `claude` | tmux | +4 |
| `codex` | tmux | −4 |
| `orca-claude` | Orca | +4 |

A name that isn't in Settings → Agents is refused: `unknown agent "…"`.

## States

| State | Meaning | Default pattern (Settings → Agents → Status light patterns) |
|---|---|---|
| `needs-decision` | Waits for you to approve something (most urgent) | MPK fast tempo + alternating octave; Xbox fast blink + short rumble; keyboard fast blink |
| `needs-input` | Waits for your answer | MPK fast tempo + steady octave; Xbox fast blink; keyboard fast blink |
| `thinking` | Working | MPK slow tempo; Xbox slow pulse; keyboard slow pulse |
| `finished` | Done, not looked at yet | MPK octave blinks; Xbox blink + short rumble; keyboard blink |
| `idle` | Nothing to show (lights back to normal) | — |
| `ended` | Same as `idle` (what the SessionEnd hook sends) | — |

When several agents are busy, the most urgent state wins (top of the table first).

## Commands to copy

```bash
# One state at a time
vibeconsole agent-status claude needs-decision
vibeconsole agent-status claude needs-input
vibeconsole agent-status claude thinking
vibeconsole agent-status claude finished
vibeconsole agent-status claude idle

# The other agents
vibeconsole agent-status codex thinking
vibeconsole agent-status codex idle
vibeconsole agent-status orca-claude needs-input
vibeconsole agent-status orca-claude idle

# Two agents at once: the more urgent state shows (needs-input beats thinking)
vibeconsole agent-status claude thinking
vibeconsole agent-status codex needs-input

# Walk through every state, 3 seconds each, then back to idle
for s in needs-decision needs-input thinking finished idle; do
  echo "claude: $s"; vibeconsole agent-status claude "$s"; sleep 3
done

# Clear everything
for a in claude codex orca-claude; do vibeconsole agent-status "$a" idle; done
```

## A session outside VibeConsole (folder lights)

The hooks also report sessions VibeConsole didn't start (an agent you opened yourself, or one in Orca). Those light the folder's light, or "Every other session" (Settings → Status lights). To fake one, pass the hook's JSON on stdin with the agent `-`:

```bash
echo '{"session_id":"manual-test","cwd":"'"$PWD"'"}' | vibeconsole agent-status - thinking
echo '{"session_id":"manual-test","cwd":"'"$PWD"'"}' | vibeconsole agent-status - needs-input
echo '{"session_id":"manual-test","cwd":"'"$PWD"'"}' | vibeconsole agent-status - idle    # clears it
```

Run these outside a VibeConsole tmux window: inside one, `-` reports for that window's own agent instead.

## Seeing the result

- **MPK:** tempo and octave lights (needs a verified program).
- **Xbox:** the button light (needs xone and the light rule, Settings → Xbox tuning) and rumble (Settings → Xbox tuning → Rumble On).
- **Keyboard:** the LEDs chosen in Status light patterns → Keyboard lights that show it.
- Lights follow only while VibeConsole runs and isn't paused. Each device has its own switch in Status light patterns.
