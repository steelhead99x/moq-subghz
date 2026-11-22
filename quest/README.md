# moq-subghz questline

## Goal

Live H.264 over an SX1276 in FSK mode, from a ARM single-board computer to a USB stick, that anyone with the
parts can reproduce from the README and HOWTO, with tools that never lose or misreport a run and
numbers that hold up on re-measurement.

## Plan

m0 holds bugs in the shipped tools: anything that crashes, writes a wrong or unreadable result, or
hides a failure. m1 holds the hardening the code review deferred (firmware limits, the AEAD's gaps,
loss accounting, the receive driver), much of which changes on-air behavior or the firmware image
and so needs a re-measured bench run. m2 holds features from the roadmap that nobody waits on yet.

A change that alters on-air framing, FEC, pacing, loss accounting for honest input, or the
firmware image says so in its PR and names the bench runs it invalidates. Hardware work lists
what the maintainer must run under `## Waiting`, since no agent here has radios.
