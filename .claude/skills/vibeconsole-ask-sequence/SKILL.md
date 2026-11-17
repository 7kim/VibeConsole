---
name: vibeconsole-ask-sequence
description: Use when VibeConsole's 0 AI asks for a prompt sequence ("Ask … for a prompt sequence"): one pad that sends step 1 on the first tap, step 2 on the next, built from the user's skills and saved prompts.
---

# Ask for a prompt sequence

A prompt sequence is one pad: tap 1 sends step 1, tap 2 step 2, up to the last. Each step is a saved prompt pasted into the agent the user picks.

VibeConsole sends: the user's wish, the installed skills (name and one-line description, never their text) and the saved prompts by name with how often each was sent in 30 days (never their text).

## Lines

Reply with tab-separated lines only:
- `sequence<TAB><name>`: the sequence's name (letters, digits, `-` and `_`), new.
- `step<TAB><prompt>`: 1–16 steps, in order. Each is a saved prompt's name, or a new prompt this reply makes.
- `new<TAB><prompt><TAB><text>`: a new prompt, one line of up to 2000 characters, with a new name. To use a skill, write it as `/<skill>` in the text.

## Steps

1. Order the work the way the user does it: plan, build, test, review, ship.
2. Reuse the most-sent saved prompts where they fit; make a new prompt only for a missing step.
3. Keep each new prompt to one clear instruction, and name skills by `/name` when one fits the step.

## Example

With a saved prompt `review`:

```reply
sequence	ship
new	plan-it	Plan the change in three steps before writing code.
step	plan-it
step	review
```

## What VibeConsole refuses

A missing or bad sequence name, 0 or more than 16 steps, a step that is neither saved nor made here, a new prompt that would replace a saved one, a new prompt with more than one line, and a sequence name that exists.

## Before it's saved

The steps are listed (saved or new, with the new text), the user picks where they're sent, then Save writes the new prompts and the sequence together.
