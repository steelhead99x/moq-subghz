# moq-subghz in detail

The long version of the [README](../README.md): how it works, the measured results, what is on the
air, the repo map, limits and radio rules. Everything here used to live in the README.

**Live H.264 over a LoRa radio chip, from a 2015 Raspberry Pi 2. 320x240, 15 fps, ~135 ms glass-to-glass.**

![A frame received over the radio link, encrypted](../media/frame-f350-rind-45s-3x.png)

▶ [60 seconds of received video](../media/moq-subghz-f350-rind-60s.mp4) (exactly what came out of the receiver, not re-encoded).

The Semtech SX1276 is the chip inside most LoRa gadgets. LoRa itself is slow (tens of kbit/s),
but the same chip has a plain FSK mode that does 300 kbit/s. That's enough for a small, low-bitrate
H.264 stream, so this project sends one. Everything between ffmpeg and the antenna is Rust.

## The idea, for video people

Radio links lose packets. When a P-frame goes missing, most players carry on decoding
broken references until the next keyframe, and you get a smeared picture.

This project frames video the way [Media over QUIC (MoQ)](https://datatracker.ietf.org/wg/moq/about/) does:

* **one GOP = one group**, starting at an IDR
* **one access unit (frame) = one object**

If a frame can't be recovered, the receiver drops **the rest of that GOP** and waits for the
next IDR. Instead of a smear you get a short freeze, at most the rest of the second (GOP 15 at
15 fps). The decoder only ever sees clean reference chains.

```
 Pi 2                                                                        laptop
 testsrc2 ─▶ burn clock    ─▶ x264      ─▶ split AUs ─▶ 255-byte  ─▶ ))) 915 MHz FSK ))) ─▶ CatWAN USB ─▶ reassemble ─▶ drop broken ─▶ ffplay
             + barcode        180 kbit/s    into         radio                               stick         + FEC repair   GOPs
             into pixels      GOP 15        objects      frames                                                           ─▶ ffmpeg reads the
                                                                                                                             barcode = latency
```

**Latency is measured from the pixels.** The sender burns its capture time into the bottom 16
rows of each frame as a barcode. The receiver decodes the stream, reads the barcode back out of
the decoded picture and subtracts. That's true glass-to-glass: encode, airtime, reassembly
and decode are all included.

## Results

Pi 2 to laptop, 330 s per run, test pattern, 180 kbit/s x264 `ultrafast`/`zerolatency`, GOP 15,
antennas about 1 m apart.

| radio rate | encrypted | H.264 delivered | frames shown | glass-to-glass p50 / p95 |
|---|---|---|---|---|
| 300 kbit/s | no | 171 kbit/s | 95.5% | 139 / 239 ms |
| 300 kbit/s | yes | 172 kbit/s | 95.4% | 138 / 241 ms |
| 350 kbit/s ¹ | no | 172 kbit/s | 96.0% | 131 / 212 ms |
| 350 kbit/s ¹ | yes | 173 kbit/s | 96.5% | 132 / 214 ms |

¹ Outside the SX1276 datasheet's tested range. It works on our units.

* **Missing frames are freezes, not smears.** About 1 in 20 frames wasn't shown. Each loss
  costs the rest of its GOP, which shows up as about 20 stalls (freezes over 250 ms) per 5.5 minutes.
* **Encryption is basically free.** ChaCha20-Poly1305 per frame adds 20 bytes (28 on the first frame
  of each GOP) and ~95 µs on the Pi 2.
* **Loss changes a lot from run to run.** An earlier pass with the same settings lost no frames at all.
* **The reverse direction (laptop to Pi) works but loses more**, 8 to 15% of frames. The Pi's
  radio board can't tell it when data has arrived, so the Pi has to keep checking for incoming
  data, which uses a whole CPU core and still misses some.

All eight runs with every column (loss, CPU, overhead, both directions) are in
[`results/video/table.md`](../results/video/table.md). The raw logs are next to it.

**How it holds up as the link gets worse** is in
[`results/loopback/`](../results/loopback/README.md). That sweep has no radio in it: the real tools
run through a file with radio frames removed. Short version: with the parity frames on, random
loss of up to about 1% costs under 2% of the picture. The loss pattern of the real radio runs
costs about twice what random loss does.

These runs were measured on 2026-11-17. The tools have changed since then (see the
[changelog](../CHANGELOG.md)), and the newer code has only been run through the no-radio
loopback, not on the radios. The table will be re-measured before a tagged release.

## What's on the air

```
 a frame of video (access unit)
   └─▶ object  = 17-byte header (frame number, capture time, keyframe flag) + Annex-B AU
         └─▶ cut into equal pieces of up to 243 bytes
               └─▶ radio frame = 12-byte header + piece   (≤ 255 bytes, the chip's limit)
                     + one XOR parity frame per 8 data frames (repairs any single loss)
```

* At 180 kbit/s a P-frame is about 7 radio frames and the IDR at the start of each second is about 22.
* Every radio frame carries its own CRC-16 on top of the radio's CRC.
* The sender drops whole GOPs that sat in its queue too long (`--max-age-ms`), so the link
  never falls behind live.
* Each AU goes to the decoder with an AUD appended, so ffmpeg outputs the frame right away
  instead of waiting for the next one.

## Quick start (no radio needed)

You need Rust 1.90+ and ffmpeg. This runs the full pipeline through a file instead of the air:

```sh
cargo build --release
./target/release/moq-subghz-send --loopback /tmp/air.txt --fec-k 8 --secs 20
./target/release/moq-subghz-recv --replay /tmp/air.txt --paced --play --decode --out /tmp/run
```

The first command records 20 s of "air" to a file at real radio speed. The second plays it back and
shows the test pattern with its clock and barcode. `/tmp/run/rec.h264` is the received stream and
`/tmp/run/summary.json` has the counters. The latency figures in that summary are meaningless here,
because the playback happens after the recording.

With real radios: **[HOWTO.md](../HOWTO.md)** walks through parts, Pi setup, flashing the USB
stick and your first live picture.

## Hardware

Other links and boards (HaLow, 2.4 GHz, 5.8 GHz, ESP32 and more) are in [LINKS.md](LINKS.md) and [HARDWARE.md](HARDWARE.md); only what is listed below has been measured.

* Raspberry Pi 2 + Dragino LoRa/GPS HAT (915 MHz): the sender
* Electronic Cats CatWAN USB Stick RP2040 (915 MHz): the receiver, plugs into a laptop
* Two 915 MHz antennas

Parts list and pinouts are in the [HOWTO](../HOWTO.md#1-hardware).

## Radio rules

The tools default to 915.0 MHz at +5 dBm (about 3 mW). They refuse anything outside 902-928 MHz
and anything within 0.3 MHz of 925.0 MHz, the sender refuses power outside 2 to 5 dBm, and it won't
transmit at all without `--i-have-an-antenna` and `--transmit`. 915 MHz is a licence-free band in the Americas,
but **not in Europe and many other places**. You are responsible for what you transmit.
Details in the [HOWTO](../HOWTO.md#9-radio-rules).

## Repo map

| path | what |
|---|---|
| `moq-subghz-send` | sender: ffmpeg capture + x264, barcode, objects, radio TX |
| `moq-subghz-recv` | receiver: radio RX, reassembly, GOP gate, ffplay, recording, latency |
| `moq-subghz-wire` | the format: framing, FEC, Annex-B splitter, barcode, encryption |
| `moq-subghz-pi` | the Pi's radio HAT glue (SPI + GPIO), used by the SX1276 backends |
| `moq-subghz-link` | the `Link` trait every radio implements (send, recv, caps), and an in-memory loopback pair |
| `radios/moq-subghz-radio-sx1276` | the SX1276 backends of `Link`: FSK and LoRa on the Pi HAT, the CatWAN stick's `Q` writer, the `R`-line loopback file |
| `moq-subghz-report` | turns run logs into the results table |
| `moq-subghz-lora2015` | the 2015-style LoRa image test: `lora2015-send` on the Pi, `lora2015-log` on the host ([results/lora-2015](../results/lora-2015/README.md)) |
| `firmware/catwan-fw` | Rust firmware for the USB stick, plus a prebuilt `.uf2` |
| `vendor/sx1276` | `no_std` SX1276 radio driver |
| `moq-subghz-bench` | the bench harness that produced the results, and the analysis of them ([moq-subghz-bench](../moq-subghz-bench/README.md)); `scripts/loss_sweep.py` is the no-radio loss sweep |
| `results/video/` | raw logs from every run |

## Write-ups

* [Broadcast over MoQ, starting small](https://hikd.ai/posts/broadcast-over-moq/): the idea behind
  this project, with diagrams, and where it is headed.
* [A quiet note on rind](https://hikd.pro/posts/rind-quiet-note/): a short note on the encryption library.

## Limits

* Test pattern only. A camera should work (`--src /dev/video0`) but hasn't been benchmarked.
* The bitrate is set by hand. There's no adaptation.
* The ffmpeg CLI decoder holds back one frame (~67 ms), and that's included in the latency numbers.
* Encryption uses a pre-shared key. Frames are private and tamper-proof, but a recorded frame
  can be replayed and the radio headers aren't encrypted. Each run encrypts under its own subkey,
  derived from the key and a random 96-bit salt, so runs never share a keystream.
  See [ROADMAP.md](../ROADMAP.md) and the code review in [REVIEW.md](../REVIEW.md).

## Contributing and security

See [CONTRIBUTING.md](../CONTRIBUTING.md) for how to build, test and send a change, and
[SECURITY.md](../SECURITY.md) for what the encryption does and doesn't cover and how to report a problem.

## License

[MIT](../LICENSE-MIT) or [Apache 2.0](../LICENSE-APACHE), your choice.

<sub>This project follows the [Media over QUIC (MoQ)](https://moq.dev) model from moq.dev. The radio side does not use moq.dev's code; the optional republish step does use moq.dev's Rust libraries (see [MOQ.md](MOQ.md)). This project is not affiliated with or endorsed by moq.dev.</sub>
