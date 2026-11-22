# Benchmarks: how to run them and where the reports live

## Labels

Every number in this project should carry one of:

* **MEASURED**: a real radio and real hardware run, raw logs kept under `results/`.
* **REPORTED**: a figure from outside (datasheet, regulation, someone else's measurement). Name the source.
* **MODELED**: computed from a formula or an emulation. Name the model. A fit of measured data is MODELED.

A MODELED number is never placed in a MEASURED column. Today the only MEASURED numbers are SX1276 FSK and
chirp spread spectrum (CSS) mode on a Pi 2 B and an RP2040-based device. 802.11ah sub-GHz Wi-Fi, 2.4 GHz and 5.8 GHz have none.

## Reproduce without a radio

You need Rust 1.90+ and ffmpeg.

```sh
cargo build --release
# 1. record 20 s of "air" to a file, then play it back
./target/release/moq-subghz-send --loopback /tmp/air.txt --fec-k 8 --secs 20
./target/release/moq-subghz-recv --replay /tmp/air.txt --paced --play --decode --out /tmp/run

# 2. the airtime table for the CSS-mode pictures (calculated, no transmit)
target/release/moq-subghz-bench css2015-table
```

`/tmp/run/summary.json` has the counters. Latency in a replay is meaningless, because the playback
happens after the recording.

The loss sweep (how the picture holds up as the link gets worse, no radio) is `scripts/loss_sweep.py`,
documented in [results/loopback](../results/loopback/README.md). Its output is a replay with frames
removed, not an over-the-air result.

## Reproduce with radios

Needs the Pi 2 B, the HAT and the RP2040-based device from the [HOWTO](../HOWTO.md). Set `PORT` and `PI` to your
own values, then:

```sh
B=target/release/moq-subghz-bench
$B run --i-have-an-antenna --transmit myrun 300000:100000:6 330 --fec-k 8    # Pi to laptop
$B rev --i-have-an-antenna --transmit myrun 300000:100000:12 330 --fec-k 8   # laptop to Pi
$B bench --i-have-an-antenna --transmit                                      # the 8-run set
$B bench-051 --i-have-an-antenna --transmit                                  # the stick 0.5.1 matrix (--part fsk, css, all)
cargo run --release -p moq-subghz-report -- runs/*/     # table from run directories
```

These transmit. Read [REGULATORY.md](REGULATORY.md) first, attach an antenna, and pass
`--i-have-an-antenna` and `--transmit` yourself (the bench never adds them), plus a licence or
permit where the gate asks for one ([LICENSING.md](LICENSING.md)). See the HOWTO for the other flags. Details: [moq-subghz-bench/README.md](../moq-subghz-bench/README.md).

## Where the reports live

| path | what | label |
|---|---|---|
| [results/video](../results/video/README.md) | the 8-run FSK video set, 2026-11-17 | MEASURED, frame format 3 |
| [results/stick-0.5.1](../results/stick-0.5.1/README.md) | 72 still-picture runs and 8 FSK video runs on firmware 0.5.1, 2026-11-20 | MEASURED, with some calculated columns marked |
| [results/factory-firmware](../results/factory-firmware/README.md) | the factory software baseline | MEASURED |
| [results/css-2015](../results/css-2015/README.md) | the 2015-style picture test and its airtime table | calculated table, measured runs in stick-0.5.1 |
| [results/loopback](../results/loopback/README.md) | the loss sweep, no radio | replay with frames removed, not over the air |

## Caveats to carry with the numbers

* The 2026-11-20 FSK runs lost about 10% of video frames and 1.3% of radio frames, more than the
  2026-11-17 runs (about 4%). The cause is not found; it is not the new firmware, the signal level
  or the USB link.
* Published video numbers were recorded with older frame formats than `main` speaks today (format 3 for
  `results/video`; newer code has only run through the loopback). They will be re-measured before a tagged release.
* Real radio loss costs about twice what random loss does, because it comes in bursts.
* Doc drift: some older READMEs under `results/` still say "frame format 3" or "version 4". Trust the
  [changelog](../CHANGELOG.md) and the code, and see [REVIEW.md](../REVIEW.md) for known mismatches.
* The 350 kbit/s FSK rate is outside the SX1276 datasheet's tested range.

## Planned

A single `moq-subghz-bench` command with a matrix of links, loss models and hosts, with labels enforced
in code, is in the evolution plan. It is **planned, not built**, and a benchmark of an emulated link will
always be labelled MODELED.
