# Changelog

## Unreleased

Changes since the runs in `results/video/` were measured (2026-11-17). None of these has been
run on the radios yet, only through the no-radio loopback and the unit tests.

### CI and hygiene

* The leak guard fails closed (no git, an unreadable tracked file or a private licence file that
  does not load fails the test), scans bytes with no size or UTF-8 limit, reads `.gz` reports,
  matches names in JSON and in any case, and checks callsign shapes in any single letter case.
* `rust-toolchain.toml` pins 1.90.0 (CI) and `moq-subghz-publish` pins 1.100.0. Clippy is clean on
  current stable too (`as_chunks` in `moq-subghz-wire`), and a non-blocking `clippy-stable` job keeps it so.
* CI: `--locked` throughout, doc tests, least-privilege `permissions:`, actions pinned by commit.
* `docs/DEPENDENCIES.md` records why `rind` is a git dependency pinned by rev.

### Tools

* New crate `moq-subghz-bench`, the Rust replacement for the `scripts/` shell and Python harness.
  This first step adds `analyze` (was `scripts/analyze-051.py`), `factory-bench`, `stick-cap` and
  `factory-send` (were `results/factory-firmware/tools/`) and `elf2uf2` (was
  `firmware/catwan-fw/elf2uf2.py`). The old scripts stay until the later steps land.

### Docs

* README rewritten around a per-link and per-hardware status table (Measured, Modeled, Planned, Not started).
  New pages: `docs/LINKS.md`, `docs/HARDWARE.md`, `docs/BENCHMARKS.md`, `docs/REGULATORY.md` and
  `docs/AMATEUR.md` (a planned, receive-only amateur radio and satellite bridge). Documentation only.

### Renamed

* **The project is now moq-subghz** (it was moq-lora), and the repository moved to
  `steelhead99x/moq-subghz`. The old address redirects. The video never went over LoRa
  modulation, and the framing isn't tied to one radio chip.
* The crates and tools follow: `moq-subghz-send`, `moq-subghz-recv`, `moq-subghz-wire`,
  `moq-subghz-pi`, `moq-subghz-report` and `moq-subghz-publish`. The default key file is
  `moq-subghz-key.hex`, and the publisher reads `MOQ_SUBGHZ_RELAY`.

### Added

* **`moq-subghz-licence` crate: licensed vs unlicensed mode.** Licence file loader, US region rules (ID shape, 47 CFR 97.119 interval), band table, identification frames and the `check_transmit` gate, with a CI guard against committing a licence ID or name. Wired into the tools since the safety-gate change below; amateur transmit stays disabled. See `docs/LICENSING.md`. Rules: receive-only on an amateur band in unlicensed mode needs an explicit `--rx-amateur` (transmit stays blocked); station ID frames go out only on amateur profiles in licensed mode, never on ISM profiles; the scheduler sends an ID at start, a fixed 60 s before the region's limit (every 540 s for the US 600 s rule) and at the end, and refuses intervals of 60 s or less; region `other` is blocked and the user-supplied mask and interval keys are removed, so only the US is enabled.

### Changed

* **The licence gate is now wired into `moq-subghz-send`, `moq-subghz-recv` and `lora2015-send`.**
  New flags `--licence-file`, `--rx-amateur`, `--transmit` and `--redact-licence`. Transmitting a
  real radio now also needs `--transmit` (default off) and a permit from the gate, in addition to
  `--i-have-an-antenna` and the existing band and power checks; scripts and HOWTO pass it.
  `moq-subghz-recv` gains `--amateur` (receive only). The gate takes the tuned frequency and signal
  width and looks the band up itself, so a signal that crosses a band edge is refused (a carrier
  at 902.0 MHz with a 500 kHz wide signal no longer passes). No tool sends station ID frames, so
  amateur transmit is refused. The shipped CatWAN images are unchanged and still accept any
  carrier and up to +17 dBm.
* `moq-subghz-licence` API (breaking): `Permit` has private fields and accessors, only the gate
  builds one; `check_start` and `check_transmit` take a `Tuning` instead of a `Band`;
  `IdScheduler::new(&Permit)` is replaced by `Permit::id_plan()` returning a `#[must_use]` `IdPlan`
  (`NoIdRequired`, `Schedule`, `MustNotTransmit`); the scheduler uses `Instant` and treats a
  reading before the last ID as due; `final_id_needed` is false when nothing was sent;
  `log_line`, `id` and `name` take a `Redaction` instead of a `bool`, and the tools write the
  licence through one function. The loader checks the opened file (regular file, owner, mode, a
  capped read); names with bidirectional or invisible characters and the `YOUR` false positive are
  fixed; a relative `XDG_CONFIG_HOME` is ignored. `.gitignore` and the leak guard cover both
  `licence.toml` and `license.toml` and variants.
* `lora2015-log` runs the receive gate before opening the stick (`--licence-file`, `--rx-amateur`, `--amateur`); an amateur profile needs licensed mode or `--rx-amateur`.

### Fixed

* **The emulated UDP link can name its peer.** `moq-subghz-send --emu-udp-from ADDR` fixes the
  datagrams' loopback source and `moq-subghz-recv --emu-udp-peer ADDR` accepts only that source;
  the default (any local process) and the loopback-only rule are unchanged.
* **Link contract, uniformly.** `Link::idle` on the CatWAN and the SX1276 FSK backends returns the
  error when the radio cannot be stopped (it was printed or dropped). The real radios stop
  themselves on `Drop` (best effort, never transmits, never panics). `CatwanTx::send` and
  `FileSink::send` refuse empty and oversize packets like every other backend, through
  `moq_subghz_link::check_packet`, and `moq_subghz_link::conformance` runs the same checks against
  the loopback, the emulation, the CatWAN stick (on a pty) and the file sink. `CatwanTx` reports
  the computed airtime of each frame instead of 0. `Duration::MAX` no longer panics in the FSK
  `recv`. No wire or on-air change; the golden v5 test is unchanged for its six original rows.
* **A failed send is no longer counted as sent** in `moq-subghz-send`: `frames_tx`, `bytes_tx`
  and `radio_busy_pct` count only what the link took.
* **Models.** HaLow retries, overhead, MTU and timings are bounded and saturate (a huge
  `--halow-retries` used to overflow or loop); `TxReport` gains `acked` next to `delivered`
  (breaking for code that builds a `TxReport`); LoRa bandwidth and SF6 are checked against what
  the SX1276 supports (new `--lora-implicit` selects the implicit header SF6 needs); the Gilbert-Elliott chain starts in its stationary distribution, so seeded
  GE results differ from earlier runs (MODELED only). In-memory links are bounded.
* **`R` lines have one parser.** `moq_subghz_wire::parse_r_line` is strict (every field, no extra
  token) and `lora2015` uses it, so the receiver and the logger accept the same lines. A `--paced`
  replay waits at most 2 s between lines.
* **Golden v5 test** covers every FEC k from 0 to 15, sealed and unsealed, `min_frame` padding and
  the sequence wrap, and a receive-side test with and without a loss per FEC block.
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

* **`moq-subghz-radio-emu`: modeled links behind `Link`**: SX1276 FSK, SX1276 LoRa and an 802.11ah
  (HaLow) channel as seeded, virtual-time models, with in-memory, loopback UDP and `R`-file
  transports. `moq-subghz-send` and `moq-subghz-recv` gain `--link fsk-emu|lora-emu|halow-emu`,
  `--emu-udp` and the `--halow-*`, `--lora-*`, `--fsk-*`, `--emu-loss`, `--emu-seed` flags; the
  sender packs whole wire frames into one packet when the link allows it. `moq-subghz-chan` pushes
  a recording through a model offline. Everything it reports is MODELED (`"provenance":"modeled"`),
  never measured. `--link fsk` stays the default and the bytes on the air are unchanged. See
  `docs/HALOW.md`.
* `moq-subghz-wire/tests/golden_v5.rs`: SHA-256 of the exact frame stream (k = 0, 4, 8, sealed and
  unsealed) for a fixed set of synthetic objects. It fails if a byte on the air changes.
* `RLine` (a parsed `R` line) keeps the line's `t_us` and `snr`, which `parse_r_line` used to drop.

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

* `moq-subghz-link`: the `Link` trait (`caps`, `send` returning a `TxReport`, `recv(buf, timeout)`, `idle`)
  and an in-memory `loopback::pair` with deterministic drops. Nothing on the air changes.

### Link trait for the SX1276 code

* **The SX1276 FSK and LoRa-mode code and the CatWAN writer now sit behind `moq_subghz_link::Link`**, in the new
  `radios/moq-subghz-radio-sx1276` crate: `Sx1276FskTx` and `Sx1276FskRx` (the Pi HAT, from the two `radio.rs`
  files), `Sx1276LoraTx` (from `lora2015-send`), `CatwanTx` (the `Q <hex>` writer from `moq-subghz-send`) and
  `FileSink` (`--loopback`). The code was moved, not rewritten; frames, FEC, pacing, power and band checks and
  the summary fields are unchanged. `moq-subghz-send` holds a `Box<dyn Link>` instead of its own `enum Link`.
* `moq-subghz-send --link fsk` is the default and is the only value (`--catwan` and `--loopback` still pick the stick
  and the file). The receiver's CatWAN and replay sources still read the stick's text lines (they feed `serial.log`
  and the `X` line count), so only its `--radio` path goes through `Link`.
* `Link::idle` returns a `Result` and `Link::take_tx_errors` is new (the stick reports `ERR tx` after `send`
  returned); both have defaults. `LinkError::Io` prints its message as is.

### Wire crate

* `moq-subghz-wire` has a `seal` feature (on by default, so downstream builds are unchanged). Without it
  the crate drops `rind` and `libc` and has no dependencies; framing, FEC and the Annex-B splitter are the
  same. CI runs `cargo test -p moq-subghz-wire --no-default-features`.
* New `moq_subghz_wire::plan(len, k, min_frame)` returns the length of every frame the `Fragmenter` makes for an
  object, for airtime and overhead math without data. A test checks it against the real `Fragmenter`.

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
