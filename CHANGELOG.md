# Changelog

## Unreleased

Changes since the runs in `results/video/` were measured (2026-11-17). None of these has been
run on the radios yet, only through the no-radio loopback and the unit tests.

### Renamed

* **The project is now moq-subghz** (it was moq-lora), and the repository moved to
  `steelhead99x/moq-subghz`. The old address redirects. The video never went over LoRa
  modulation, and the framing isn't tied to one radio chip.
* The crates and tools follow: `moq-subghz-send`, `moq-subghz-recv`, `moq-subghz-wire`,
  `moq-subghz-pi`, `moq-subghz-report` and `moq-subghz-publish`. The default key file is
  `moq-subghz-key.hex`, and the publisher reads `MOQ_SUBGHZ_RELAY`.

### Fixed

* **The sender no longer splits a picture at a pause on ffmpeg's pipe.** A 4 ms gap inside one
  access unit made two objects in two groups and shifted capture timestamps. Access units now end
  only at the next AUD (or EOF), which adds up to one frame interval of latency. Changes what goes
  on the air; not run on the radios.
* **A failed publish or replay read fails the receiver.** `summary.json` gains `publish_ok` and
  `input_error`, and the receiver exits 1 after writing the recording.
* **Relay tokens stay off command lines.** `moq-subghz-recv --publish` with no URL reads
  `MOQ_SUBGHZ_RELAY`, passes the URL to the publisher in its environment, and refuses a URL with
  a query string on the command line.
* **`catwan-fw` enforces the band and power limits and never drops a queued frame.** Source
  only: the shipped images predate it (see REVIEW.md).
* **`sx1276` long-FSK receive can't return a stale or spliced packet.** New
  `FskConfig::crc_auto_clear`, off in the long-mode readers. Needs a radio test.
* **LoRa logger:** images keyed by SSDV callsign and id, SSDV completion checks every index,
  conflicting pieces start a new image, exact loss is per run (`run.csv`), and output files of a
  reused id no longer overwrite each other. `lora2015-send` refuses a bad `--duty-pct`.
* **Bench scripts fail a run when either end fails**, instead of printing DONE.
* **The factory comparison uses one timing window** for both stacks: about 8 times, not 9.

* **`catwan-fw` 0.5.1: the radio answers `I` right after power-up.** The firmware left the SX1276
  RESET line low (the chip held in reset) until the first `L` or `F` command, so an `I` sent
  first read `sx_version=0x00` and looked like a dead radio. The radio is not dead. 0.5.1 pulses
  RESET at boot and retries until `RegVersion` reads 0x12, retries once more inside `L` and `F`
  if the version check fails, and adds `X` to re-initialise the radio on demand
  (`X sx_version=0x12 tries=1`). Radio settings, framing and pacing are unchanged.
  New image `firmware/catwan-fw-0.5.1.uf2` (sha256 in `firmware/SHA256SUMS`). The measured runs
  used `catwan-fw-v5.uf2` (0.5.0), which stays in the repo.

### Added

* `results/stick-0.5.1/`: the full over-the-air benchmark on `catwan-fw` 0.5.1. 72 still-picture runs
  (chunked JPEG, SSDV, LoRaWAN-framed at SF7/125, SF7/250, SF7/500, SF8/500) and 8 live FSK video runs,
  next to the factory baseline, plus a 6 minute demo (`scripts/demo-051.sh`). `scripts/run2.sh` now works
  without extra sender arguments and without ffplay (`PLAY=`).

* `results/factory-firmware/`: still pictures over LoRa with factory software at both ends
  (Dragino's sample app, Electronic Cats' stock receiver), measured. Vendor images and fetch
  scripts are in `firmware/vendor/`.

* **`moq-subghz-lora2015`: a 2015-style LoRa image test**, in `results/lora-2015/`. A Pi sender
  (`lora2015-send`) sends a test picture in real LoRa mode as chunked JPEG, SSDV packets or
  LoRaWAN-framed uplinks (no network behind them), and a host logger (`lora2015-log`) reads it on the
  CatWAN stick and writes CSV. Built and tested without radios; the airtime table is calculated,
  not measured. The FSK path and the existing tools are unchanged.
  `moq-subghz-pi` gains `open_hat_raw`, the part of `open_hat` that opens spidev and GPIO.

### Changed on the air

* **Frame format version 4.** Encrypted objects now carry a per-run salt, and each run encrypts
  under its own subkey derived from the pre-shared key. The first frame of each GOP carries 28
  bytes of overhead and the rest 20. Version 3 and version 4 tools refuse each other's frames.
* **Breaking: frame format version 5.** The key-derivation label (the HKDF info string) is now
  `moq-subghz object key v2`, and the frame header version went from 4 to 5. A sender and
  receiver built before and after this change refuse each other's frames, and encrypted output
  recorded before it can't be opened. Update the Pi and the laptop together. The pre-shared key
  still works once the file is named `moq-subghz-key.hex`.

### Receiver

* Replayed and injected radio frames no longer break the object in progress or re-open old
  groups. They are counted as `radio_frames_stale`. The first frame, and any large jump ahead,
  must be confirmed by the next frame.
* The expected-frame count ignores a garbled or injected frame number, so `video_frame_loss`
  can no longer go negative or count billions of frames.
* Loss accounting separates late, stray and stale frames.
* The receiver keeps every decoded frame, including the first GOP.

### Republish to MoQ

* New `moq-subghz-publish` crate: republishes the received groups and objects 1:1 as a real MoQ
  broadcast (hang format) through moq.dev's `moq-net`, `moq-tokio` and `hang` crates, so a
  moq.dev relay or moq.pro can fan it out and the moq.dev web player can show it. Separate
  crate (Rust 1.91+), so the Pi binaries are unchanged. See [docs/MOQ.md](docs/MOQ.md).
* `moq-subghz-recv --publish URL` feeds it the objects that pass the group gate.
  `summary.json` gains `publish_dropped_objects`.

### Sender and tools

* The sender stops if the encoder sends more than 256 frames without a keyframe, and stops when
  its video source stalls.
* Key files must be readable only by their owner.
* The bench scripts quote key paths and pass the receiver's key separately.
* The Pi radio setup code is shared by the sender and receiver in a new `moq-subghz-pi` crate.
* Two committed `.pyc` files are removed, `.gitignore` covers Python bytecode, and CI fails if one is committed again.

### Results

* A loss sweep with no radio in it, in `results/loopback/`: what the viewer sees from 0 to 10%
  radio loss, with and without FEC, and under the loss patterns of the measured radio runs.

### Docs

* README and HOWTO rewritten for a public release. Added SECURITY, CONTRIBUTING and this file.

## Bench release, 2026-11-17

The first working version: live H.264 over SX1276 FSK from a Raspberry Pi 2 to a CatWAN USB
stick, with optional encryption, and the eight measured runs in `results/video/`.
