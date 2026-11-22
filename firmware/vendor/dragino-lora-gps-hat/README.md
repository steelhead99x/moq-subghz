# Dragino LoRa/GPS HAT for Raspberry Pi: official sample app

The sender in the [factory benchmark](../../../results/factory-firmware/README.md). Dragino's own
example program for the HAT's SX1276, built on the Pi.

| | |
|---|---|
| Vendor | Dragino |
| Upstream | https://github.com/dragino/rpi-lora-tranceiver ("Simple LoRa Transmit/ Receive library in RPi for LoRa GPS HAT") |
| Source used | directory `dragino_lora_app/`, file `main.c`, commit `f6e1a55f14c04b16d1a376160e9ccf6891a648e1` (2018-05-11, "LoRa_Shield_DHT11"). The repository has no tags. |
| Retrieved | 2026-11-20 |
| License | **none published.** The repository has no license file and the file header says only "Copyright (c) 2018 Dragino". |
| Library it needs | WiringPi (LGPL-3.0). Tag `v3.20`, commit `31f49ecdf7865c10483a91994aa51f73a7c656aa`, https://github.com/WiringPi/WiringPi |

## Why the source is not in this repository

Without a license, Dragino's code is all rights reserved, so it is not redistributed here. This
folder holds a fetch script that clones it at the pinned commit and checks the hash, plus the
small patch this project applies.

| file | what |
|---|---|
| `fetch-and-build.sh` | clone at the pinned commit, check `main.c` against the hash below, apply the patch, check the result, build |
| `dragino-bench.patch` | the four-constant bench patch (below), `diff -U0`, 8 changed lines |
| `SHA256SUMS` | hashes of the upstream file, the patched file, and the patch |

```
355bbdb97e514742af81cec1546bf3fc3ec74f45de15d5b2930bb96741c3c73b  dragino_lora_app/main.c  (upstream, at the pinned commit)
d58d14872dbac0aa3d6698a8daccf42f934907cc7d6982ac542bd7ad780281af  dragino_lora_app/main.c  (after dragino-bench.patch)
```

## The patch

Four lines: carrier 868.1 to 915.0 MHz, sync word 0x34 to 0x12, message buffer 32 to 256 bytes,
TX power `configPower(23)` to `configPower(3)` (+5 dBm). It makes the app talk to the stock
RP2040 device receiver and send 250 character chunks. See the benchmark README for the reasons.
Unpatched, the app builds and runs for 868 MHz and the LoRaWAN public sync word.

## Build and use

On a Raspberry Pi with the HAT, SPI enabled, g++ and WiringPi:

```sh
firmware/vendor/dragino-lora-gps-hat/fetch-and-build.sh      # builds ./build/rpi-lora-tranceiver/dragino_lora_app/dragino_lora_app
./dragino_lora_app sender "hello"     # sends "hello" every 5 s
./dragino_lora_app rec                # prints packet RSSI, SNR, length and text
```

Raspberry Pi OS Trixie has no WiringPi package. Build tag `v3.20` without installing it and
point the script at it (the comments in `fetch-and-build.sh` have the three commands). The app
drives the HAT's chip select, reset and DIO0 pins through WiringPi's numbering (6, 0 and 7; GPIO25,
GPIO17 and GPIO4), so no pin changes are needed on the Dragino LoRa/GPS HAT.

## Restore

There is nothing to flash: the HAT has no firmware of its own that this project changes. To get
back to the stock app, run `fetch-and-build.sh`, or use upstream `dragino_lora_app` unpatched
by skipping the patch step (`git checkout` the pinned commit and build).
