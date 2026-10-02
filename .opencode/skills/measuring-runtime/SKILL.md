---
name: measuring-runtime
description: Use when measuring anything about this app's speed, latency, or memory - frame rate, launch time, time-to-first-pixels, decode cost, A/B-ing two builds, running a tests/probe_*.ps1 harness, or reading a --perf report. Also use before claiming a change made something faster or slower.
---

# Measuring runtime

## Overview

Most of this work is establishing that the harness can measure the thing, before
measuring the thing. A number from an invalid harness is worse than none, because
it is believed.

## Check before you run

| Check | Command | Fails when |
|---|---|---|
| Console, not RDP | `quser` says `console`; no `Microsoft Remote Display Adapter` | injected keys silently no-op, so frame and input numbers are fiction — four wrong conclusions came from this |
| Visible window | never `-WindowStyle Hidden` | no paint/vsync delivery |
| Settle past the slowest decode | longer than any decode, ~800 ms | you measure the decode, not the app |
| Binary matches source | `target\release` newer than HEAD | `Copy-Item` keeps a backup's old mtime, cargo skips the rebuild, the green run is a lie |
| Close, don't kill | `CloseMainWindow`, then wait | written by `on_exit`, so a killed process reports nothing — which looks like a broken feature |

Injected keystrokes need the foreground; `SetForegroundWindow` is refused unless
you own it, and a failed steal shows as a **clean zero**, not an error. Use
`Native::Focus`.

## A/B discipline

1. **Interleave** — all of A then all of B lets machine drift invent a regression.
2. **Prefer min over median** for launch timings; the median is ±100 ms when loaded.
3. **Warm up both arms with a discarded call first.** Min-of-N does not fix this:
   rayon's pool, `image`'s decoders, a font atlas — whichever arm runs first builds
   them, and the cost lands on that arm.
4. **Read the config back after writing it.** A BOM makes `Config::load` fall back
   to defaults silently, so every arm runs with the setting it was meant to disable.

## What the number actually measures

- **First visible pixels is committed in `frame_prelude`**, after the frame was
  presented. Never when the worker message arrives — that is not on screen yet.
- **`gallery open -> first thumbs` is the first cell, not the fill** — with
  equal-priority cells queued, the cheapest file wins the race.
- **`ms/frame` is CPU amortised over whatever frames happened**, so it is
  meaningless at 2 fps; use total CPU%. A frame count is not spinning — read
  `frames with input events` (0 while idle).

`display-gap frames` and `stalls (>5s)` must be 0 — check `recorded_first`, not
just age, or the watchdog cries wolf on cache hits. Keep a worker panic's payload;
a bare "decoder panicked" hid the cause, and it is usually OOM.

Treat any "the harness cannot do X" claim with suspicion — the tell is that it
would excuse a bug. Failure stories: `references/wrong-conclusions.md`.