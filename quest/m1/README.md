# m1: deferred hardening

## Goal

The gaps the code review left open are closed or explicitly accepted: the stick enforces the radio
limits itself, encryption covers what an on-path attacker can change, loss accounting cannot be
skewed by one bad frame, and the sender's memory is bounded.

## Plan

Items that change on-air framing, the receive path, or the firmware invalidate the published runs
in `results/video`; each says which, and the re-measurement is the maintainer's (no radios here).
Not planned, as not worth their weight: the zeroed `localtime_r` fallback (cosmetic), per-byte
hex formatting on the RP2040-based device TX path (not the bottleneck), and `moq-subghz-report` printing an empty
run name for an argument that is not a directory (cosmetic; it still reads the files).
