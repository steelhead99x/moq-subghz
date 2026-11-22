# CatWAN firmware v6 image

## Goal

A rebuilt `firmware/catwan-fw-v6.uf2` carrying this epic's fixes replaces v5, with `SHA256SUMS`
updated, and the forward and reverse bench runs are re-measured on it.

## Plan

The v5 image is what every published run used, so this epic lands the firmware changes first and
releases one image for all of them.

## Waiting

- Maintainer: flash the v6 image, confirm `I` reports it, and re-run `moq-subghz-bench bench`; done when
  `results/video` holds the new runs.
