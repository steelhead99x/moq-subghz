# Roadmap

Planned work lives in the [quest log](quest/README.md), one Markdown file per pull request:

* [m0](quest/m0/README.md): bugs in the shipped tools
* [m1](quest/m1/README.md): hardening the code review deferred, including firmware v6
* [m2](quest/m2/README.md): features, such as a camera benchmark and bitrate adaptation

Known limits of the current release are in the Limits section of [docs/DETAILS.md](docs/DETAILS.md#limits), and the
full code review is in [REVIEW.md](REVIEW.md).

## Direction at a glance

One page, kept short on purpose. States use the same words as the [README](README.md): **done**,
**open PR**, **planned**, **not started**. An open PR is not merged and not in `main`. Nothing here
is a promise or a date, and nothing has been run over the air unless the README says Measured.

| item | state | where |
|---|---|---|
| Golden test for the v5 frame stream | **done** | [#45](https://github.com/steelhead99x/moq-subghz/pull/45) |
| `Link` trait crate with a loopback pair | **done** | [#46](https://github.com/steelhead99x/moq-subghz/pull/46) |
| Multi-link README and per-topic docs (hardware profiles described) | **done** | [#47](https://github.com/steelhead99x/moq-subghz/pull/47) |
| Repo hygiene (committed bytecode removed, guarded) | **done** | [#44](https://github.com/steelhead99x/moq-subghz/pull/44) |
| SX1276 FSK and CSS-mode code behind `Link`, `--link fsk` default | **open PR** | [#48](https://github.com/steelhead99x/moq-subghz/pull/48) |
| `seal` feature gate and `plan()` for the frame plan | **open PR** | [#49](https://github.com/steelhead99x/moq-subghz/pull/49) |
| Licensed and unlicensed modes (gate crate, no wiring into the tools yet) | **open PR** | [#50](https://github.com/steelhead99x/moq-subghz/pull/50) |
| Hardware profiles as code (host and radio presets) | **planned** | next after the `Link` work in #48 |
| Emulated 802.11ah sub-GHz Wi-Fi link | **planned** | no module needed, so it can come first |
| Real 802.11ah driver | **not started** | waits for a module and its pins to be chosen |
| ESP32-S3 and ESP32-C6 nodes | **not started** | feasibility spike first, see [docs/HARDWARE.md](docs/HARDWARE.md) |
| Newer ARM single-board computers and small-board hosts | **not started** | untested, CPU budget unknown |
| P2P mesh networks LR1110 tracker node bridge, SX1262 and LR1121 drivers | **planned** | host-side serial bridge only, new drivers, nothing written |
| Receive-only amateur radio and AMSAT bridge | **not started** | policy in [docs/AMATEUR.md](docs/AMATEUR.md), receive side and SSTV first |
| Amateur receive-side SDR | **not started** | direction only, see [docs/HARDWARE.md](docs/HARDWARE.md#amateur-receive-side-sdr) |

The longer working plan stays private. This page is the public summary, and the quest log above is
where individual pull requests are tracked.
