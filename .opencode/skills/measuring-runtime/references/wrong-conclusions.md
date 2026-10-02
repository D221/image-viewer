# Wrong conclusions this project has drawn

Each of these looked like a finding. Every one was an artefact of the harness.
Read the one nearest your measurement before you trust it.

## Over RDP

A 30 fps animation measured 12.7 fps. A "LOW_LATENCY made it worse" experiment
was noise. A resize-debounce comparison (11 vs 9 re-decodes) was meaningless.
Synthetic key injection silently did nothing — `frames with input events` stayed
0.

This cost four wrong conclusions. Verify before trusting any frame rate, input
event, or resize event: `quser` must say `console`, not `rdp-tcp#N`, and
`Get-CimInstance Win32_VideoController` must not show a *Microsoft Remote Display
Adapter*.

## A hidden window

`-WindowStyle Hidden` gets no ordinary paint/vsync delivery. Every fps measured
that way was garbage: 12, 13, 19, 59 across runs. A visible window is required,
and settle longer than any decode.

## Recording first visible pixels at the wrong moment

The report is written when the worker message arrives, which is not on screen.
The commit is in `frame_prelude`, after the painting frame was presented. A
harness that timed "the message arrived" produced a plausible wrong number for a
hang that was three steps away.

## Measuring the step you can see

Timing the frame, then the decode, then adding a per-step timer inside the viewer
is how the truncated-GIF hang was localised. The harness and the raw decoder both
said "fine". Measure the step you suspect, not the step you can see.

## The stall watchdog crying wolf

It fires when nothing was painted at all, so it misfires on a cache hit. Check
`recorded_first`, not just age.

## Interleaving and min are not enough

A unit test comparing a 16-frame GIF's cell against a 1-frame one — identical
first frames — reported 550 ms against 53 ms. A clean 10x, entirely one-time cost
in the process. Min of two interleaved rounds did not fix it: the real culprit was
**rayon's global thread pool**, built by the first `linear_resize`, worth ~450 ms
in a debug build, still landing on whichever arm went first. Before believing a
ratio in-process, do a discarded warm-up call of **both** arms, and know that any
lazily-initialised global in the path will find the first arm.

Same failure shape as the RDP conclusions: a number that looks like a finding and
is an artefact.

## All of A then all of B

Machine drift invents a 120 ms regression. Interleave the arms.

In `bench_launch_ab.ps1` the parameters are `$PathA`/`$PathB`. PowerShell variable
names are case-insensitive, and a loop variable `$a` silently overwrote `$A` and
ran `Start-Process -FilePath 809.4` — indistinguishable from a missing binary.

## The median on a loaded box

±100 ms. Prefer min for launch timings.

## A config written with a BOM

`Set-Content -Encoding UTF8` under PowerShell 5.1 emits a BOM. `Config::load`
prints a parse error and **silently falls back to defaults**, so every arm ran
with the setting it was supposed to disable. Caught only because both arms came
out identical. Write BOM-free, then **read the config back and assert the value**
— and prefer verifying behaviourally (cache off → zero entries written) over
verifying the file.

## A stale binary

`Copy-Item` restores a backup's old timestamp, cargo skips the rebuild, and you
measure the previous commit. The tell is `target\release` older than HEAD. `touch`
the file. The green run is a lie.

## The killed process

The perf report is written by `on_exit`. `Kill` is the fallback for a *hang*, not
the normal path. A harness that kills the app gets no report, and no report looks
exactly like a broken feature.

## The keystroke that went to the terminal

`SetForegroundWindow` is refused unless the calling process owns the current
foreground window. A harness launched from a terminal does not — the terminal
does. Every key goes to the terminal, the feature "proves" itself broken, and
**the tell is a clean zero, not an error**. `Native::Focus` does it properly:
`AttachThreadInput` to the foreground thread's queue, plus a topmost/notopmost
`SetWindowPos`. Check `ForegroundIs` afterwards regardless — the steal can fail
too.