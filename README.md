# moq-subghz

DIY live video over sub-GHz radio, heading toward a multi-link, multi-hardware project.

**Live video from a Raspberry Pi over a chirp spread spectrum (CSS) mode radio chip, written in Rust. One radio works today; the rest of this page says exactly which links are real and which are only plans.**

![A frame received over the radio link, encrypted](media/frame-f350-rind-45s-3x.png)

A Raspberry Pi 2 sends live H.264 video over a common CSS mode radio chip to a USB stick on a laptop.
320x240 at 15 fps, about 135 ms glass to glass (capture to screen). It is not framed, and it needs no internet,
gateway or server. Caveat: a later 2026-11-20 run lost about 10% of video frames, more than the first runs,
and the cause is not found ([results/stick-0.5.1](results/stick-0.5.1/README.md)). The published numbers predate
some code changes, see the [changelog](CHANGELOG.md).

```
Pi 2  ──▶  915 MHz FSK radio  ──▶  RP2040-based device  ──▶  laptop
```

## Why it's interesting

* **Live video on a CSS mode chip.** The SX1276 is slow in CSS mode, but its plain FSK mode does
  300 kbit/s, enough for a small video stream.
* **Lost frames freeze, they don't smear.** Video is split into MoQ groups (one keyframe interval
  is one group) and objects (one frame is one object). If a frame is lost, the receiver drops the
  rest of that group and waits for the next keyframe.
* **Rust from ffmpeg to the antenna.** The framing, the radio driver and the USB stick firmware
  are all Rust. Frames are encrypted with ChaCha20-Poly1305.

## How the video is framed

The video framing uses the Media over QUIC model (groups and objects) in a small Rust version that runs
on a radio instead of QUIC. Want viewers on the internet? `moq-subghz-recv --publish` republishes the
stream to a MoQ relay; see [docs/MOQ.md](docs/MOQ.md).

## Status at a glance

Read the legend first. Nothing here is rounded up: a link is only "Measured" if a real radio run
exists in [`results/`](results/).

| label | meaning |
|---|---|
| **Measured** | code is in this repo and was run over the air on real hardware, logs kept in `results/` |
| **Modeled** | only a calculation or emulation exists, no radio. Not in this repo's `main` yet unless noted |
| **Planned** | written down in the plan, no code yet |
| **Not started** | an idea and open questions, nothing written |

| link | state | what exists today |
|---|---|---|
| SX1276 FSK, live video | **Measured** | Pi 2 B to RP2040-based device, 915 MHz, 300 and 350 kbit/s radio rate ([results](results/video/table.md)) |
| SX1276 CSS mode, still pictures | **Measured** | chunked JPEG, SSDV, uplink-style frames, Pi 2 B to RP2040-based device ([results](results/stick-0.5.1/README.md)) |
| Wi-Fi 802.11ah sub-GHz Wi-Fi (802.11ah) | **Modeled** | emulation and airtime math only. The module and its pins are not decided, there is no real driver. The emulation is on an unmerged branch, not in `main` |
| 2.4 GHz (Wi-Fi, ESP-NOW, raw 802.11) | **Not started** | no code. Candidate link, open questions in [docs/LINKS.md](docs/LINKS.md#24-ghz) |
| 5.8 GHz (Wi-Fi 5/6 or a dedicated video link) | **Not started** | no code. Candidate link, open questions in [docs/LINKS.md](docs/LINKS.md#58-ghz) |
| SX1262 / LR1121 | **Planned** | a driver is in the plan, nothing written |
| P2P mesh networks node (LR1110 tracker node class) | **Planned** | a read-only host bridge is in the plan, nothing written here |
| Amateur radio and amateur satellite bridge | **Not started** | receive-only and recording first, see [docs/AMATEUR.md](docs/AMATEUR.md) |
| `Link` trait and hardware profiles | **Planned** | the direction in the [evolution plan](docs/LINKS.md#architecture-planned) |

Hardware, same legend (details and sources in [docs/HARDWARE.md](docs/HARDWARE.md)):

| hardware | role | state |
|---|---|---|
| Raspberry Pi 2 B + SX1276 GPS HAT | sender | **Measured** (the only host used in the published runs) |
| RP2040-based device (RP2040 + SX1276) | receiver | **Measured**, firmware in this repo |
| Raspberry Pi 4, Pi 5, Pi Zero | host | **Not started**: untested here, profiles and CPU budgets are planned |
| ESP32-S3, ESP32-C6 boards | 2.4 GHz and node candidates | **Not started**: a feasibility spike is planned |
| LR1110 tracker node (P2P mesh networks) | host-side bridge | **Planned** |
| SX1262, LR1121 boards | other sub-GHz radios | **Planned** |
| 802.11ah modules (any) | 802.11ah link | **Modeled** only. No module chosen |

The only link with real over-the-air numbers is SX1276, in FSK and CSS mode. Every 802.11ah figure is a
model and must be read as a best case.

## How the links relate

```
camera/ffmpeg ─▶ MoQ groups + objects ─▶ frames (+ optional parity, sealing) ─▶ a link ─▶ ... ─▶ receiver ─▶ player
                                                                                 │
              today: SX1276 FSK, SX1276 CSS mode, file replay                        └─ planned: 802.11ah, SX1262/LR1121, 2.4 GHz, 5.8 GHz
```

The framing (MoQ groups and objects, XOR parity, optional sealing) does not depend on the radio. The
plan is to put each radio behind one small `Link` trait and describe each board in a TOML hardware
profile, with a region file that sets legal limits. **That is planned, not built.** See
[docs/LINKS.md](docs/LINKS.md#architecture-planned). The long-form evolution plan is an owner proposal that is not published in this repo yet.

## Try it (no radio needed)

You need Rust 1.90+ and ffmpeg.

```sh
cargo build --release
./target/release/moq-subghz-send --loopback /tmp/air.txt --fec-k 8 --secs 20
./target/release/moq-subghz-recv --replay /tmp/air.txt --paced --play --decode --out /tmp/run
```

The first command records 20 s of "air" to a file. The second plays it back. Nothing is transmitted.
To see how the picture holds up as the link gets worse, with no radio, and to re-run the benchmarks,
see [docs/BENCHMARKS.md](docs/BENCHMARKS.md). With real radios, follow **[HOWTO.md](HOWTO.md)**.

## Radio rules (all bands)

* The tools **never transmit by default**. They need `--i-have-an-antenna` and `--transmit`, the
  licence gate must permit it ([docs/LICENSING.md](docs/LICENSING.md)), and they refuse
  frequencies and powers outside built-in limits. That includes the bench crate's `factory-send` and
  `factory-bench`: the stock the HAT maker app's 868.1 MHz at +17 dBm is refused. The shipped RP2040-based device
  firmware images do not enforce any of this.
* The built-in limits cover **915 MHz, about 3 mW**, only. 915 MHz is licence-free in the Americas,
  but **not in Europe and many other places**. Sub-GHz ISM bands differ by region.
* 2.4 GHz and 5.8 GHz rules differ by region too, and some 5 GHz channels need radar avoidance (DFS)
  or have regional limits. No code here touches those bands yet.
* **Amateur radio bands are different again**: they need a licence and a callsign, and encryption is
  generally not allowed. The current ChaCha20-Poly1305 sealing must be off there. See
  [docs/AMATEUR.md](docs/AMATEUR.md).
* You are responsible for what you transmit. Not legal advice. Per-band notes are in
  [docs/REGULATORY.md](docs/REGULATORY.md), and the details for 915 MHz are in the
  [HOWTO](HOWTO.md#9-radio-rules).

## Planned: amateur radio and amateur satellite bridge

**Planned and not started. Nothing is implemented.** The idea is to bridge MoQ (relays and objects
on the internet side) with amateur radio on the RF side: SSTV images, packet and APRS telemetry, and
digital or analog voice. The stated policy:

* **Receive side first, SSTV first.** Receiving, recording and replay only. No amateur-band transmit
  code path exists, and none will be enabled until the owner holds a licence and a callsign configured
  in a private config. Receiving generally needs no licence: verify your local rules.
* Amateur bands generally prohibit encryption or obscured meaning (for example 47 CFR 97.113 in the US),
  so planned amateur profiles set `encryption = "forbidden"`, objects are signed or sent in the clear
  with callsign ID, and the tools must refuse to transmit otherwise.

Full text, with sources and open questions: [docs/AMATEUR.md](docs/AMATEUR.md).

## Roadmap

The next steps are written up in an evolution plan (an owner proposal, not yet in this repo): first tidy and a golden test for the wire format,
then the `Link` trait, emulated 802.11ah and CSS mode links, hardware profiles, a benchmark command,
and only then real 802.11ah hardware once a module is chosen. Today's planned work lives in
[ROADMAP.md](ROADMAP.md) and the [quest log](quest/README.md); the multi-link direction is in
[docs/LINKS.md](docs/LINKS.md#architecture-planned).

## Contributing

Small pull requests, say how you checked them, and say plainly what you could not run (for example a
radio test). A change to what goes on the air means the published numbers need re-measuring. Never
commit key files, board MACs or serials, or flash dumps. Details: [CONTRIBUTING.md](CONTRIBUTING.md).

## Learn more

* [docs/LINKS.md](docs/LINKS.md): each link (FSK, CSS mode, 802.11ah, 2.4 GHz, 5.8 GHz), a comparison table, the planned architecture
* [docs/S1G-EMULATION.md](docs/S1G-EMULATION.md): the modeled 802.11ah sub-GHz Wi-Fi channel (formerly `docs/HALOW.md`)
* [docs/HARDWARE.md](docs/HARDWARE.md): boards and radio chips, with sources
* [docs/BENCHMARKS.md](docs/BENCHMARKS.md): how to run and reproduce the benchmarks, labels, where reports live
* [docs/REGULATORY.md](docs/REGULATORY.md): rules per band, and what the tools enforce
* [docs/AMATEUR.md](docs/AMATEUR.md): the planned amateur radio and satellite bridge (not started)
* [docs/DETAILS.md](docs/DETAILS.md): how it works, measured results, repo map and limits
* [docs/LICENSING.md](docs/LICENSING.md): licensed and unlicensed mode, the private licence file, what the tools cannot verify
* [docs/MOQ.md](docs/MOQ.md): republishing to a MoQ relay
* [HOWTO.md](HOWTO.md): parts, setup and your first live picture
* [results/loopback](results/loopback/README.md): how the picture holds up as the link gets worse (a replay, no radio)
* [results/css-2015](results/css-2015/README.md): what a 2015 team could have sent over real CSS mode and uplink-style frames
* [results/stick-0.5.1](results/stick-0.5.1/README.md): the measured over-the-air benchmark against the factory software
* [CHANGELOG.md](CHANGELOG.md), [ROADMAP.md](ROADMAP.md), [CONTRIBUTING.md](CONTRIBUTING.md), [SECURITY.md](SECURITY.md), [REVIEW.md](REVIEW.md)

## License

[MIT](LICENSE-MIT) or [Apache 2.0](LICENSE-APACHE), your choice.

<sub>Credits and references: the MoQ model (groups and objects) follows [moq.dev](https://moq.dev) and the
[IETF MoQ working group](https://datatracker.ietf.org/wg/moq/about/); the radio side uses none of moq.dev's
code, the optional republish step uses its Rust libraries, and this project is not affiliated with or endorsed
by moq.dev. Frame encryption uses the pure-Rust ChaCha20-Poly1305 library [rind](https://github.com/kixel-ai/rind).
Built in [Rust](https://www.rust-lang.org) on the Semtech SX1276 and the the board maker RP2040-based device.
Background post: [Broadcast over MoQ, starting small](https://hikd.ai/posts/broadcast-over-moq/). Amateur
radio rules cited in [docs/AMATEUR.md](docs/AMATEUR.md) (47 CFR 97.113 and regional rules) and
[docs/REGULATORY.md](docs/REGULATORY.md).</sub>
