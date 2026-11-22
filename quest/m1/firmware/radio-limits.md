# [S] Stick refuses power and frequencies outside the limits

## Goal

The firmware's `L` and `F` commands reject anything outside 902-928 MHz, 925.0 MHz itself, and any
power above +5 dBm, so a raw serial session cannot key the stick out of bounds.

## Plan

Today they clamp to 2..=17 dBm and program any carrier (`firmware/rp2040-sx1276-fw/src/main.rs`, the `L`
and `F` arms). Reply `ERR` with the reason instead of clamping, matching the host tools' checks.
