---
name: verifying-a-change
description: Use when about to claim a change in this repo is correct, finished, or ready to commit - a bug fix, a new cache, a new guard, a refactor, or any edit whose tests pass. Also use when a test passes on the first run, when a fix seems obviously unrelated to a symptom, or before writing a commit message that says something works.
---

# Verifying a change

A passing test proves the code does not crash, not that the test exercises what
it appears to. This repo has shipped bugs that compiled, type-checked, passed
every test and were wrong.

**Violating the letter of these rules is violating the spirit of them.**

## Rule 1 — reintroduce the bug

Every guard, fix, or invariant you added needs a test that **fails without it**.
Remove it, confirm red, restore, confirm green. Still green means the test is
decoration and the code is uncovered.

The first draft of a fix routinely contains a piece that does nothing. Find it by
deleting each piece and watching for a test that does not fail, then *delete it*.

## Rule 2 — assert the premise, not the comment

When a test relies on something being true — a chord never arrives as a `Key`
event, a probe is contained, a queue is drained — check it against a real
`egui::Context`, a real event and queue.

## Rule 3 — write the effect, not the gesture

"A test exists" is not a claim. "This test fails if the guard is removed" is. For
each load-bearing claim in your commit message, name the test that proves it, or
write it, or drop the claim. A user-visible effect has a proof that cannot be
faked: clipboard contents, a new file on disk, cache *hits* in the report, a
screenshot.

## Rule 4 — check the traps for the area you touched

AGENTS.md's section for that area is the list. The recurring ones: **never
transcribe a table**, **every failure in a cache is a miss** and never a panic,
**`failed` is permanent for the session**, **a bare letter may only lose
nothing**, **untrusted file bytes are bounds-checked**.

## Rule 5 — the run you report is the run you did

**Touch restored files before trusting a green run** — `Copy-Item` preserves the
backup's timestamp, cargo skips the rebuild, and you test the previous commit.

## Rationalisations

| Excuse | Reality |
|---|---|
| "The tests pass, so it's correct" | They prove no crash, not that they test the thing. |
| "This obviously can't be the cause" | The live bugs here all looked unrelated to their symptom. |
| "I'll keep it as insurance" | An untested no-op is a trap for the next reader. |

**Red flags:** a test that passed first time and you never watched fail; a commit
message claiming a number with nothing behind it; a guard with no failing test.

Rule 1's findings: `references/verified-and-unverifiable.md`.