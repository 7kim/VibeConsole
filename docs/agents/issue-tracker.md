# Issue tracker: Local Markdown (`tickets/`)

Issues and specs for this repo live as markdown files in `tickets/` at the repo root. `tickets/` is git-ignored, so tickets stay local.

## Conventions

- One file per ticket: `tickets/<NN>-<slug>.md`, numbered from `01` in dependency order (blockers first), never a single combined tickets file. Body follows the `/to-tickets` local template: `# <NN>: <Title>`, **What to build**, **Blocked by**, **Status**, then an acceptance checklist.
- `tickets/README.md` is the index: one numbered line per ticket with its link, title and `Blocked by:`, then a suggested order. Keep it in sync whenever a ticket is added or its blockers change.
- A spec, when there is one, sits alongside in `tickets/` and each ticket links to its section.
- Triage state is the `**Status:**` line near the top of each ticket (see `triage-labels.md` for the role strings).
- Comments and conversation history append to the bottom of the file under a `## Comments` heading.
- Live checks the user must perform go in `tests/v<N>/<NN>-slug.md` (Tests, How to do, Result) and the run order in `tests/README.md`, not in the ticket.
- When a batch of tickets is finished, the user moves it into `archive/` (e.g. `archive/others/`, `archive/V<N>.0/tickets/`). Don't move or delete tickets yourself.

## When a skill says "publish to the issue tracker"

Create a new `tickets/<NN>-<slug>.md` (next free number) and add it to `tickets/README.md`, creating either if needed.

## When a skill says "fetch the relevant ticket"

Read `tickets/<NN>-<slug>.md`. The user will normally pass the path or the ticket number directly. Finished tickets may already be in `archive/`.

## Wayfinding operations

Used by `/wayfinder`. The **map** is a file with one **child** file per ticket.

- **Map**: `tickets/<effort>/map.md` (the Notes / Decisions-so-far / Fog body).
- **Child ticket**: `tickets/<effort>/<NN>-<slug>.md`, numbered from `01`, with the question in the body. A `Type:` line records the ticket type (`research`/`prototype`/`grilling`/`task`); a `Status:` line records `claimed`/`resolved`.
- **Blocking**: a `Blocked by: NN, NN` line near the top. A ticket is unblocked when every file it lists is `resolved`.
- **Frontier**: scan `tickets/<effort>/` for files that are open, unblocked, and unclaimed; first by number wins.
- **Claim**: set `Status: claimed` and save before any work.
- **Resolve**: append the answer under an `## Answer` heading, set `Status: resolved`, then append a context pointer (gist + link) to the map's Decisions-so-far in `map.md`.
