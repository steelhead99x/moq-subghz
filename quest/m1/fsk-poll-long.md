# [M] board receive driver never returns a spliced packet

## Goal

`fsk_poll_long` in `vendor/sx1276` cannot return the start of a CRC-failed packet joined to the
next one, and a long packet first seen at PayloadReady is drained instead of erroring.

## Plan

The end-to-end CRC-16 drops splices today, so this is about loss, not integrity. Changes the
reverse-direction numbers.
