---
name: vibeconsole-ask-prompt
description: Use when VibeConsole's 0 AI asks for a prompt ("Ask … for a prompt"): first the questions to ask the user, then the prompt a pad will paste into a coding agent.
---

# Ask for a prompt

A saved prompt is text a pad pastes into a coding agent (Claude Code, Codex…) or the focused window. It is sent as it is, every time the pad is pressed, so it must work without the user editing it.

The ask has two calls.

## Call 1: the questions

VibeConsole sends what the user wants. Reply with one to three short questions whose answers you need, one per line, each starting with `Q:`. Nothing else. Each is shown in its own text entry; Esc there saves nothing.

```reply
Q: Which language is the project in?
Q: Should it also run the tests?
```

Ask about what changes the prompt: the project, the language, the tools, how strict, what to output. Lines without `Q:` are ignored; more than three are cut.

## Call 2: the prompt

VibeConsole sends the wish and each question with the user's answer (`Q: …` then `A: …`; an empty answer means no preference). Reply with only the prompt text: no code fences, no title, nothing before or after it.

A good pad prompt:
- says the task in the first sentence ("Review the staged diff in this Rust project.");
- names what to check or do, as a short list;
- says what to answer with ("List each problem with file and line; say 'no problems' if none.");
- stays generic enough to paste again tomorrow: no dates, no file names the user didn't give.

## Before it's saved

The prompt is shown; the user picks "Name it, then open it in $EDITOR", names it (letters, digits, `-`, `_` and `.`), edits, saves. A name that exists is refused.
