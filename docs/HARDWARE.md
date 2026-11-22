# Hardware

What each board or chip is for in this project, and what has actually been run. Specs are only given
where a source is cited; otherwise the row says so.

Labels: **Measured** (run over the air, logs in `results/`), **Modeled**, **Planned**, **Not started**.
Do not read a board here as "supported" unless it says Measured.

| hardware | role | state | notes |
|---|---|---|---|
| Raspberry Pi 2 Model B + Dragino LoRa/GPS HAT (915 MHz) | sender | **Measured** | the host and radio of every published run |
| Electronic Cats CatWAN USB Stick (RP2040 + SX1276) | receiver | **Measured** | runs this repo's Rust firmware, `firmware/catwan-fw` |
| Raspberry Pi 4, Pi 5 | host | **Not started** | untested. HOWTO says any Pi with a 40-pin header should work, which has not been tried here. Pi 5 GPIO access differs, treat it as unknown |
| Raspberry Pi Zero, Zero 2 W | host | **Not started** | CPU budget unknown. The Pi receive path already uses a whole core polling, which a single-core Zero would feel. Measure, do not extrapolate |
| Seeed XIAO ESP32-S3 | node, 2.4 GHz candidate | **Not started** | no firmware in this repo. A private workshop has an unverified skeleton, never flashed |
| ESP32-C6 | node, 2.4 GHz candidate | **Not started** | no firmware anywhere yet |
| Seeed T1000-E (Meshtastic) | slow data lane, range reference | **Planned** | would be read through its serial API by a host. The repo would never flash it |
| SX1262, LR1121 boards | other sub-GHz radios | **Planned** | needs a new driver, nothing written |
| HaLow (802.11ah) modules, in general | HaLow link | **Modeled** | no module chosen, no pins, no driver |

## Raspberry Pi hosts

* **Pi 2 B** is what was measured: an armv7 board, static musl binaries built from this repo, ffmpeg on
  the Pi. Pinouts for the Dragino HAT are in the [HOWTO](../HOWTO.md#1-hardware).
* Pi 4, Pi 5 and Pi Zero are listed in the plan as hosts that need their own profiles and CPU-budget
  runs. Until then, treat them as untried. Product documentation:
  [Raspberry Pi hardware docs](https://www.raspberrypi.com/documentation/computers/raspberry-pi.html).
* A Pi Zero (the original) is armv6, and CI builds armv7 only today.

## CatWAN USB Stick (RP2040 + SX1276)

* An RP2040 microcontroller board with an SX1276 radio, from Electronic Cats. This repo ships its own
  firmware (`firmware/catwan-fw`, prebuilt `.uf2` with sha256 sums) and the receiver tools talk to it over
  a USB serial port.
* The shipped `.uf2` images predate the source's band and power checks (see [REVIEW.md](../REVIEW.md)),
  so the checks live in the host tools. Do not type radio commands into a raw serial session.
* Flashing the stick is the owner's action. The HOWTO has a backup-first procedure.

## Radio chips

| chip | what we use it for | source |
|---|---|---|
| Semtech SX1276 | FSK video and LoRa-mode pictures (Measured) | [Semtech product page and datasheet](https://www.semtech.com/products/wireless-rf/lora-connect/sx1276) |
| Semtech SX1262 | planned second driver | [Semtech](https://www.semtech.com/products/wireless-rf/lora-connect/sx1262) |
| Semtech LR1121 | planned, a dual-band part from the same family with its own command set | [Semtech](https://www.semtech.com/products/wireless-rf/lora-connect/lr1121) |

Differences that matter for planning (read the datasheets, none are asserted from memory here): the
SX1262 needs different handling than the SX1276 (command interface, busy pin, FSK limits), so the
350 kbit/s overclock seen on the SX1276 is not expected to carry over. The LR1121 has a different
command set again.

## ESP32 boards (2.4 GHz and node candidates)

* ESP32-S3 datasheet: [Espressif](https://www.espressif.com/sites/default/files/documentation/esp32-s3_datasheet_en.pdf).
  ESP32-C6 datasheet: [Espressif](https://www.espressif.com/sites/default/files/documentation/esp32-c6_datasheet_en.pdf).
* Two things to settle before any code: the wire library would need a `no_std` core with bounded
  buffers, and the S3 needs Espressif's Rust toolchain while the C6 builds with a stock RISC-V target.
  A microcontroller will not run x264, so such a node would relay, receive, or send pre-encoded data.
* ChaCha20-Poly1305 cost on these chips has **not been measured** by anyone here. No number exists.

## Seeed T1000-E and Meshtastic

* The plan is a host-side bridge that reads the node's serial API. [Meshtastic docs](https://meshtastic.org/docs/).
  The node applies its own duty-cycle and routing, and a mesh is not a point-to-point link, so video does
  not fit. Value: a range reference and a slow data lane.
* This repo would never flash or reconfigure a node. Meshtastic firmware and protobufs are GPL licensed,
  so their code is not copied into this MIT or Apache repo.

## HaLow modules

* No module is chosen. A module's interface (SPI, SDIO or a command channel), driver, firmware blob and
  regulatory table decide the whole design, so the plan keeps them blank rather than guessing.
* Firmware blobs and GPL drivers must not be committed here: document how to fetch them instead.
