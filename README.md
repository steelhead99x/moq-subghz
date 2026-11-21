# moq-subghz

**Live video over sub-GHz radio. Raspberry Pi 2 to laptop.**

A Raspberry Pi 2 sends H.264 directly to a CatWAN USB stick using the Semtech SX1276 in FSK mode. The link runs offline with no gateway or server. The Rust tools use Media over QUIC (MoQ) ideas to recover from packet loss and can republish the received video to a MoQ relay.

[**Watch the over-the-air demo**](results/stick-0.5.1/demo/fsk-live.mp4) · [**Try it without radios**](#try-it-without-radios) · [**Build the radio link**](HOWTO.md) · [**See the benchmark**](results/stick-0.5.1/README.md)

[![Received test pattern with the sender clock and frame number burned into the picture](results/stick-0.5.1/fsk-live-still.png)](results/stick-0.5.1/fsk-live-120s.mp4)

*Frame from a 300 kbit/s FSK bench run. Select the image to open the full 120-second capture.*

## What the bench measured

| Video format | Delivered video | Capture-to-decode latency | Video frame loss |
| --- | --- | --- | --- |
| 320 × 240 at 15 fps | 162.2 kbit/s | 136.7 ms p50 / 243.7 ms p95 | 10.3% |

These are means from four 120-second Pi-to-stick runs with CatWAN firmware 0.5.1, 300 kbit/s FSK, +5 dBm configured power, and one parity packet for every eight data packets. Latency comes from the timestamp encoded in each picture and read back after decoding. [Setup, raw logs, and results](results/stick-0.5.1/README.md#part-3-fsk-live-video-300-kbits-measured)

## Try it without radios

From a checkout with Rust 1.90 or later and ffmpeg with libx264 installed, record a five-second test pattern and replay it through the receiver.

```sh
cargo build --release --locked
demo_dir="$(mktemp -d)"
./target/release/moq-subghz-send --loopback "$demo_dir/air.txt" --fec-k 8 --secs 5
./target/release/moq-subghz-recv --replay "$demo_dir/air.txt" --paced --decode --out "$demo_dir/received"
```

The receiver writes `rec.h264` and `summary.json` under `"$demo_dir/received"`. If ffplay is installed, run the command below to watch the recording. Replay is useful for checking decoding and loss counters. Measure live latency on the radio link.

```sh
ffplay -f h264 -framerate 15 "$demo_dir/received/rec.h264"
```

## How the link works

```text
Raspberry Pi 2 + SX1276  →  915 MHz FSK  →  CatWAN USB stick  →  Laptop
                                                               ↳ optional MoQ relay
```

- **Video over an off-the-shelf radio.** The SX1276 is commonly used for LoRa. Its FSK mode carries this live stream at 300 kbit/s.
- **Recovery at the next keyframe.** The sender puts each keyframe interval into a group and each H.264 access unit into an object. XOR parity can repair a missing fragment. If an object is lost, the receiver skips the rest of its group and resumes at a keyframe.
- **Optional encryption and relay publishing.** A pre-shared key enables ChaCha20-Poly1305 on the radio link. The radio uses its own framing inspired by MoQ. The optional publisher uses the [moq.dev](https://moq.dev) libraries to send a MoQ broadcast to a relay. [Encryption setup](HOWTO.md#8-more-things-to-try) · [MoQ relay setup](docs/MOQ.md)

## Go deeper

| What you want to do | Where to start |
| --- | --- |
| Assemble the Pi and CatWAN setup | [Hardware, flashing, and first picture](HOWTO.md) |
| Inspect the wire format or find a crate | [Architecture and repository map](docs/DETAILS.md) |
| Compare random packet loss with measured loss patterns | [Loopback loss sweep](results/loopback/README.md) |
| Send still pictures in LoRa mode | [Chunked JPEG, SSDV, and LoRaWAN-framed experiments](results/lora-2015/README.md) |
| Read the project background | [Broadcast over MoQ, starting small](https://hikd.ai/posts/broadcast-over-moq/) |
| Contribute or report a security issue | [Contributing](CONTRIBUTING.md) · [Roadmap](ROADMAP.md) · [Changelog](CHANGELOG.md) · [Security policy](SECURITY.md) |

The bench uses 915 MHz. Check local rules and attach the antenna before transmitting. [Radio setup and limits](HOWTO.md#9-radio-rules)

## Credits and license

The group and object model draws on [moq.dev](https://moq.dev) and the [IETF MoQ working group](https://datatracker.ietf.org/wg/moq/about/). The radio link uses [rind](https://github.com/kixel-ai/rind) for encryption, the Semtech SX1276, and the Electronic Cats CatWAN USB Stick.

Available under [MIT](LICENSE-MIT) or [Apache 2.0](LICENSE-APACHE).
