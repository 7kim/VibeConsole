# Codex setup (one time, per machine)

Codex CLI 0.172.0 was used for this setup. Start a fresh Codex session from the VibeConsole
root so it loads [AGENTS.md](../../AGENTS.md).

## Skills: one shared set

Codex reads skills from `$REPO_ROOT/.agents/skills` (repo) and `$HOME/.agents/skills` (user),
and follows symlinked skill folders. Claude Code reads `.claude/skills` (repo) and
`~/.claude/skills` (user). Keep one copy and link the other side to it:

| Skills | Real location (edit here) | Codex sees it through |
|---|---|---|
| Project skills (`vibeconsole*`, `to-spec`, `to-tickets`) | `<repo>/.claude/skills/` | `<repo>/.agents/skills` → `../.claude/skills` |
| Ticket skills (`ticket-*`) | `~/.local/share/claude-scripts/skills/` | Individual links in `~/.agents/skills/` |
| Orca skills (`orca-cli`, `orca-computer-use`, `orchestration`) | `~/.agents/skills/` | Claude Code already links here |
| Personal skills (`21st-ai`, `21st-video`, `readme-style-match`, `ui-ux-pro-max`) | `~/.claude/skills/` | Individual links in `~/.agents/skills/` |

```sh
mkdir -p .agents ~/.agents/skills
[ -L .agents/skills ] || ln -sT ../.claude/skills .agents/skills
for skill in ~/.local/share/claude-scripts/skills/ticket-* ~/.claude/skills/*; do
  [ -f "$skill/SKILL.md" ] || continue
  name=${skill##*/}
  [ -d ".claude/skills/$name" ] && continue
  dest=~/.agents/skills/$name
  [ -e "$dest" ] || [ -L "$dest" ] || ln -sT "$skill" "$dest"
done
```

Run the commands from the repo root. This version of Codex follows the repo-level link
and also discovers older copies under `~/.codex/skills`. In `~/.codex/config.toml`,
`[[skills.config]]` entries disable the five older copies by absolute path.
`21st-ai`, `21st-video`, and `readme-style-match` use the shared personal links.
`to-spec` and `to-tickets` use the shared project link and are available inside
VibeConsole. The old copies remain on disk. Claude Code plugins and account-synced
skills have their own installation. Codex uses its installed `ponytail@ponytail` plugin.

Use `/skills` to browse or type a `$skill-name` mention. The ticket skills are
invoked by name per [AGENTS.md](../../AGENTS.md). Codex reads the shared skill and
runs its `` !`cmd` `` step explicitly, passing supplied arguments to `tickets`.
[Codex skills documentation](https://learn.chatgpt.com/docs/skills-and-plugins)

## Status line

Claude Code runs `vibeconsole statusline` (settings `statusLine`), which writes the agent's
context file and then runs the saved command `~/.config/vibeconsole/statusline-command` =
`~/.local/share/claude-scripts/statusline` = Orca's status hook + `tickets status`.

Codex 0.172.0 uses the local `vibeconsole_statusline` MCP provider at
`~/.codex/statusline/vibeconsole.py`. The user config enables
`features.tui_extensions_statusline` and selects the provider with
`tui.status_line_provider`. Start a fresh CLI after changing the provider config.

The footer shows model and reasoning, approval mode, Git branch, and context used.
In VibeConsole, it adds the working version, next ticket, triage and developer-test
counts from `tickets status`. Ticket and branch data refresh every 15 seconds.
Once Codex reports context usage, `ctx` shows the percentage used and adds
`start a new session?` above 50%, as in Claude Code.

Use `/statusline` → **Choose a status line provider** → **Default status line**
to return to the built-in footer. Run `echo '{}' | tickets status` for the ticket
summary on demand. `/model` selects the model and reasoning, `Ctrl+R` searches
past prompts, and `/resume` opens a saved conversation.
[Codex CLI commands](https://learn.chatgpt.com/docs/developer-commands?surface=cli)

## Hooks (agent status for VibeConsole's lights)

`~/.codex/hooks.json` reports `thinking` on UserPromptSubmit and PostToolUse,
`needs-input` on PermissionRequest, `finished` on Stop, and `ended` on SessionEnd.
It also contains the Orca hooks. Claude Code's `needs-decision` hook matches its
`AskUserQuestion` tool.

The Codex plugin's trusted SessionStart and UserPromptSubmit hooks are enabled.
Ponytail starts at Full with the current local defaults. Say `stop ponytail` or
`normal mode` to turn it off for the session. Use `/hooks` to inspect their status.
[Codex hooks documentation](https://learn.chatgpt.com/docs/hooks)

## Memory

Claude Code's memory for this repo is `~/.claude/projects/<repo path, / as ->/memory/`
(`MEMORY.md` index + one file per fact, frontmatter `name`, `description`, `metadata.type`).
Codex reads it (AGENTS.md, Start of a session). To share new facts, write a file in the same
format there and add one line to `MEMORY.md`; check for an existing file first.

## Docs lookup (CLAUDE.md, Research first)

Claude Code uses the `context7` and `firecrawl` MCP servers. `codex mcp list` shows neither
today; add them with `codex mcp add` if wanted, else Codex reads official docs on the web.

## Permissions

Claude Code runs in auto mode (`permissions.defaultMode = "auto"`). Choose Codex's approval and
sandbox settings to match; CLAUDE.md's confirms (risky root steps, controller writes, real
keyboard output) still apply whatever the mode.
