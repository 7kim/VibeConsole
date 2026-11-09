# Issue tracker: Local Markdown (`docs/<version>/`)

Specs, tickets and tests live as markdown files in the release folder `docs/<version>/`: `spec/`, `tickets/`, `agent-tests/` and `developer-tests/`. `docs/v*/` and `docs/README.md` are git-ignored (the repo is public), so they stay local. V1–V6 are in `archive/tickets/` and `archive/tests/`; don't add to them.

## Conventions

- `<version>` is the release folder: `v6.1`, `v7`, and so on. New tickets go in the highest one that `docs/README.md` lists; start the next version only when the user says so, and add its row to `docs/README.md`.
- The spec is `docs/<version>/spec/SPEC-<version>-<name>.md`. Its header records how it was made: the user's messages (quoted), the user's decisions, and the facts checked. It restates the CLAUDE.md Project rules it relies on, under **Safety Rules**. Write it with `/to-spec <version>`.
- One feature per directory, named with its version first: `docs/<version>/tickets/<version>-<feature>/issues/<NN>-<slug>.md`, numbered from `01` across the version, never a single combined tickets file. "Make this better" items filed with `x` go in `docs/<version>/tickets/<version>-ideas/issues/`.
- Triage state is a `Status:` line near the top: `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix` or `resolved` (see `triage-labels.md`).
- Comments and conversation history append to the bottom under `## Comments`. The builder signs off with `### Builder · <date>`.

## Ticket template

```markdown
# NN: <Title>

Status: ready-for-agent
Blocked by: none | NN, NN
Spec: `docs/<version>/spec/SPEC-<version>-<name>.md` (<section>). <the user's words that asked for it, dated>
Tests: `docs/<version>/agent-tests/NN-<slug>.md`, `docs/<version>/developer-tests/NN-<slug>.md`

**What to build:** <the behavior, in the user's terms>

- **Today:** <what happens now, with the evidence>
- <edge cases and rules>

- [ ] <one acceptance check per line; `(user)` marks one only the user can see>
- [ ] `cargo fmt --check`, `cargo check --locked` and `cargo test --locked` pass.

## Comments
```

## Tests

Whatever needs testing is written up separately from its ticket, split by who runs it:

- **Agent tests** at `docs/<version>/agent-tests/<NN>-<slug>.md`: what the agent checks itself (`cargo test`, the installed build's output, files on disk).
- **Developer tests** at `docs/<version>/developer-tests/<NN>-<slug>.md`: what only the user can see (controller, desktop effects, logins, reboots). Template and run order: `live-tests.md`.
- Numbered like the ticket. A ticket with both kinds gets both files, and its `Tests:` line names them.
- A `Ticket:` line near the top points at the ticket; a `Status:` line records its state; results append under `## Comments` (developer tests: `## Result`).

Agent test template:

```markdown
# Test NN: <Title>

Ticket: `docs/<version>/tickets/<version>-<feature>/issues/NN-<slug>.md`
Status: ready-for-agent
How: by the builder, <in `cargo test` (name the test) / with the installed build / ...>.

## Checks

- [ ] <one observable check per line>

## Comments
```

## When a skill says "publish to the issue tracker"

Create a new file under `docs/<version>/tickets/<version>-<feature>/issues/` (next free number in the version, creating the directory if needed), plus its agent-test and developer-test files when it needs testing.

## When a skill says "fetch the relevant ticket"

Read the file at the referenced path. The user will normally pass the path or the ticket number directly. Old tickets are in `archive/tickets/`.

## Wayfinding operations

Used by `/wayfinder`. The **map** is a file with one **child** file per ticket.

- **Map**: `docs/<version>/tickets/<effort>/map.md` (the Notes / Decisions-so-far / Fog body).
- **Child ticket**: `docs/<version>/tickets/<effort>/issues/NN-<slug>.md`, numbered from `01`, with the question in the body. A `Type:` line records the ticket type (`research`/`prototype`/`grilling`/`task`); a `Status:` line records `claimed`/`resolved`.
- **Blocking**: a `Blocked by: NN, NN` line near the top. A ticket is unblocked when every file it lists is `resolved`.
- **Frontier**: scan `docs/<version>/tickets/<effort>/issues/` for files that are open, unblocked, and unclaimed; first by number wins.
- **Claim**: set `Status: claimed` and save before any work.
- **Resolve**: append the answer under an `## Answer` heading, set `Status: resolved`, then append a context pointer (gist + link) to the map's Decisions-so-far in `map.md`.
