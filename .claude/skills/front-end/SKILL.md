---
name: front-end
description: Use before designing, building or changing any VibeConsole page or window UI (today there is none; the Tauri desktop app idea would be the first): design artifact first, then components, then the approval flow.
---

# Front end

Before changing any page or window UI, design it as a Claude design artifact (claude-artifacts `list_types`, a Design type) with the `ui-ux-pro-max` skill (scripts: `python3`).

## Components

Add them with `bunx --bun shadcn@latest add @<registry>/<name>` in the front end's folder. Its registries are in that folder's `components.json`: `@shadcn` for dashboards, `@aceternity` for landing pages. Not there → 21st, through the `21st` plugin (`mcp__21st__*`, `21st:` skills). 21st free plan: search plus 2 code fetches a day, no AI generation.

## Changes and approval

- "Change X in the artifact" → publish a new artifact version as the draft; leave the page's code alone.
- `approved` → read the artifact's full code, `/to-spec` it with the artifact's slug and approved version, then build the page to match it exactly.
