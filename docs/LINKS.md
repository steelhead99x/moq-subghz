# Links: what each radio is, and what exists

moq-subghz started as one radio (SX1276 FSK) and is growing into a project that can carry the same
MoQ-style video over several kinds of link. This page is honest about which links are real.

Labels: **Measured** (real over-the-air run, logs in `results/`), **Modeled** (calculation or
emulation, no radio), **Planned** (written down, no code), **Not started** (an idea with open
questions). Numbers from outside this project are marked **Reported** with a source.

Not legal advice. You are responsible for what you transmit. See [REGULATORY.md](REGULATORY.md).

## Summary

| link | band | state | where |
|---|---|---|---|
| [SX1276 FSK](#sx1276-fsk) | sub-GHz ISM (tools allow 902-928 MHz only) | **Measured** | `vendor/sx1276`, `moq-subghz-send`, `moq-subghz-recv`, `results/video` |
| [SX1276 LoRa mode](#sx1276-lora-mode) | same | **Measured** (still pictures only) | `moq-subghz-lora2015`, `results/stick-0.5.1` |
| [Wi-Fi HaLow](#wi-fi-halow-80211ah) | sub-GHz ISM, varies by country | **Modeled** | not in `main`; see below |
| [2.4 GHz](#24-ghz) | 2.4 GHz ISM | **Not started** | nothing in this repo |
| [5.8 GHz](#58-ghz) | 5 GHz / 5.8 GHz | **Not started** | nothing in this repo |
| [Amateur radio / satellite](AMATEUR.md) | amateur bands | **Not started** | receive-only first |

## Comparison

The qualitative columns are general radio knowledge, not measurements made here. Only the measured
column is this project's own data.

| | SX1276 FSK | SX1276 LoRa | HaLow | 2.4 GHz | 5.8 GHz |
|---|---|---|---|---|---|
| rate class | low (hundreds of kbit/s configured) | very low (a few to tens of kbit/s) | higher than the other sub-GHz links (model only) | high | highest |
| range class | short to medium | longest of the five for the power | long for Wi-Fi, per the design goal of 802.11ah (Reported, [Wi-Fi Alliance](https://www.wi-fi.org/discover-wi-fi/wi-fi-halow)) | short to medium, crowded band | shortest, walls hurt most |
| licence-free? | in some regions and bands | in some regions and bands | in some regions and bands | widely, with limits | widely, with limits and DFS on some channels |
| this project measured | 162 to 172 kbit/s of H.264 delivered, about 135 ms glass to glass, about 4% (2026-11-17) to about 10% (2026-11-20) of video frames not shown | about 5 to 22 kbit/s of picture data, stills only | nothing | nothing | nothing |

Measured rows: [results/video](../results/video/table.md) and
[results/stick-0.5.1](../results/stick-0.5.1/README.md). The 10% figure is the later FSK runs; the earlier
runs lost about 3.5 to 4.6%. The cause of the extra loss is not found.

## SX1276 FSK

* **State: Measured.** Live H.264 (320x240, 15 fps) from a Raspberry Pi 2 B with a Dragino HAT to a
  RP2040-based device on a laptop. Chip: Semtech SX1276 ([product page](https://www.semtech.com/products/wireless-rf/lora-connect/sx1276)).
* 300 kbit/s is inside the datasheet's tested range; 350 kbit/s is outside it and worked on the units
  tested.
* The Pi side cannot be told when data arrives (DIO1 is not wired on the HAT), so receiving on the Pi
  polls and uses a core, and the reverse direction loses 8 to 15% of frames.
* Details: [DETAILS.md](DETAILS.md), [HOWTO.md](../HOWTO.md).

## SX1276 LoRa mode

* **State: Measured**, still pictures only (chunked JPEG, SSDV, LoRaWAN-style frames with no network).
  There is no live video in LoRa mode: it is too slow.
* Settings measured: SF7 at 125, 250 and 500 kHz, and SF8 at 500 kHz. 72 runs, 8 of 4932 frames lost.
* Code: `moq-subghz-lora2015`. Results: [results/stick-0.5.1](../results/stick-0.5.1/README.md),
  [results/lora-2015](../results/lora-2015/README.md).

## Wi-Fi HaLow (802.11ah)

* **State: Modeled only.** The HaLow module and its pins are not decided, so there is no real driver
  and nothing was ever sent over a HaLow radio.
* An emulation (channel timing, loss models, packing several 255-byte frames into one larger packet)
  exists on an unmerged local branch and in a private workshop. It is planned to land in small pull
  requests. Its figures are a **best case**: no aggregation, real preambles or power save are modeled.
* Rules differ by country: channels, bandwidth (1, 2 or 4 MHz in the standard) and power. Modules
  often ship with a per-country table; do not override it. The standard is IEEE 802.11ah-2016; the
  industry programme is [Wi-Fi CERTIFIED HaLow](https://www.wi-fi.org/discover-wi-fi/wi-fi-halow).
* Open questions: which module, SPI or SDIO, whether the module runs its own stack, driver licence
  (many Linux drivers are GPL and could not be vendored into an MIT or Apache repo), and which country
  profile to ship first.

## 2.4 GHz

**Not started. No code in this repo.** A candidate link, not a decision.

* Candidates: ordinary Wi-Fi (UDP over a LAN or access point), ESP-NOW between ESP32 boards
  ([ESP-IDF docs](https://docs.espressif.com/projects/esp-idf/en/latest/esp32/api-reference/network/esp_now.html)),
  or raw 802.11 frames from an ESP32. Candidate boards: Seeed XIAO ESP32-S3 and ESP32-C6, see
  [HARDWARE.md](HARDWARE.md).
* Open questions: does the 255-byte frame size make sense here (a Wi-Fi payload is far bigger, so
  packing may be the better fit); how much a microcontroller can buffer (an object can be up to 61965
  bytes); who encodes video (a microcontroller will not run x264, so a host would); whether the
  existing XOR parity helps on a link with its own ACKs; how much interference from other 2.4 GHz
  users the benchmark must tolerate.

## 5.8 GHz

**Not started. No code in this repo.** A candidate link, not a decision.

* Candidates: Wi-Fi 5 or Wi-Fi 6 on a single-board computer, or a dedicated 5.8 GHz video link
  (digital FPV style links). Which products expose a usable data path is **unverified**, so none is
  named here.
* Open questions: DFS and regional channel limits (see [REGULATORY.md](REGULATORY.md)); power and
  licence rules on 5.8 GHz video gear differ by country and some of it overlaps other services;
  whether a closed video link can carry our objects at all.

## Architecture (planned)

**Everything in this section is planned. None of it is built.** It comes from an owner proposal, the
evolution plan, which is not yet published in this repo.

Today the radio code is not behind a trait: `moq-subghz-send` and `moq-subghz-recv` choose between
the Pi HAT, the RP2040 device serial port, and a replay file inside their own `main.rs`. The plan:

```
moq-subghz-wire        unchanged: frames, objects, XOR parity, sealing
moq-subghz-link        planned: a small Link trait (send a packet, receive a packet, report caps),
                       emulated links, file and loopback links
radios/                planned: one crate per radio (SX1276 moved as-is, later HaLow, SX1262, LR1121)
profiles/              planned: board TOML (pins, limits) and region TOML (band, power, duty)
```

Rules the plan sets for itself:

1. The wire format does not change in the first steps. A golden test proves it.
2. Never transmit by default. Real transmit needs a real backend chosen explicitly, a profile whose
   region allows it, and the antenna flag. Emulation and replay are the defaults in docs and CI.
3. Effective limits are the minimum of the command line, the board profile and the region file.
4. Every number carries a label: Measured, Reported or Modeled. A Modeled row is never placed in a
   Measured column.
5. A HaLow board profile starts with the module and every pin blank, and a real backend refuses to
   load it blank.

The ordered pull request series, and the open questions it needs the owner to answer, are in
the plan. The shorter list is in [ROADMAP.md](../ROADMAP.md).
