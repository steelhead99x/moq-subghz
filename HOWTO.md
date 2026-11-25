# HOWTO

From a bare ARM single-board computer and a USB radio stick to a live picture in `ffplay`. Budget an hour.

Just want to see it work? The [quick start in the README](README.md#try-it-no-radio-needed)
runs the whole pipeline through a file, with no radios.

> **Before you transmit:** read [section 9](#9-radio-rules). Short version: 915 MHz, 3 mW,
> always with an antenna attached, and only where 915 MHz is allowed.

---

## 1. Hardware

| part | job |
|---|---|
| ARM single-board computer (2015 class) (any board with a 40-pin header works) | sender: makes the video and transmits it |
| SX1276 GPS HAT, **915 MHz version** | the board's radio (the GPS part goes unused) |
| the board maker **RP2040-based device**, 915 MHz | receiver: plugs into your laptop |
| two 915 MHz antennas | both boards come with one |
| a laptop (macOS or Linux) | runs the receiver and the player |

You don't need a camera. The sender uses ffmpeg's `testsrc2` pattern by default.

<details>
<summary>Pinouts (you only need these if you use a different board)</summary>

**the SX1276 GPS HAT on the board**

| radio pin | board GPIO (BCM) | header pin |
|---|---|---|
| SCK / MISO / MOSI | 11 / 9 / 10 | 23 / 21 / 19 (`/dev/spidev0.0`) |
| NSS (chip select) | 25 | 22 |
| RESET | 17 | 11 |
| DIO0 | 4 | 7 |
| DIO1 | not wired | |

**RP2040-based device**

| radio pin | RP2040 GPIO |
|---|---|
| MISO / NSS / SCK / MOSI | 16 / 17 / 18 / 19 |
| RESET | 21 |
| DIO0 | 10 |
| RX LED | 25 |

</details>

## 2. Set up the board

1. Install Debian-based OS. Turn on SPI with `sudo raspi-config`
   (Interface Options, then SPI) and reboot.
2. Let your user reach the radio without root:
   ```sh
   sudo usermod -aG spi,gpio $USER     # then log out and back in
   ```
3. Install ffmpeg: `sudo apt install ffmpeg`.
4. Set up passwordless ssh from the laptop (`ssh-copy-id`). An alias such as `Host pi` in
   `~/.ssh/config` makes everything below shorter. The latency tools use ssh to compare clocks.

## 3. Build

On the laptop, with Rust 1.90 or newer and ffmpeg installed:

```sh
git clone https://github.com/steelhead99x/moq-subghz && cd moq-subghz
cargo build --release

# The board binaries: static, no cross compiler needed
rustup target add armv7-unknown-linux-musleabihf
cargo build --release -p moq-subghz-send -p moq-subghz-recv --target armv7-unknown-linux-musleabihf
ssh pi mkdir -p moq-subghz
scp target/armv7-unknown-linux-musleabihf/release/moq-subghz-{send,recv} pi:moq-subghz/
```

The only thing the board needs besides these two files is ffmpeg.

### Release archives

Instead of building, you can unpack a [release](https://github.com/steelhead99x/moq-subghz/releases).
Archives made by `scripts/release.sh` (from the release after v0.1.0) are musl, statically linked, so they run on any
Linux of their architecture, and each carries a `REQUIREMENTS.txt` that states any libc requirement.
They also carry the reference inputs `moq-subghz-bench` reads (`results/css-2015/frames`,
`results/factory-firmware`, the source picture under `media/`) beside `bin/`; the bench tool finds them
relative to its own path, so keep the archive's layout. Elsewhere, pass `--repo-root` (or set
`MOQ_SUBGHZ_ROOT`) to a checkout.

Both archives hold the same tools as v0.1.0: x86-64 has `moq-subghz-send`, `-recv`, `-bench`,
`-report`, `-chan`, `-publish`, `css2015-send` and `css2015-log`; ARMv7 has all of them except
`moq-subghz-publish`.

To build the archives yourself: `scripts/release.sh VERSION` writes them and `SHA256SUMS` to `dist/`.
It needs rustup, binutils and a musl C compiler for `moq-subghz-publish`'s TLS code (`apt install
musl-tools`, or name one in `CC_x86_64_unknown_linux_musl`), and checks all of these before it builds.
It builds into `RELEASE_BUILD_DIR` (default `target/release-build`; a relative path is taken from
where you start the script) whatever `CARGO_TARGET_DIR` says. It builds and checks in a staging
directory and replaces `dist/` only when everything passed, so a failed build keeps the last
archives. CI runs the same script, and pushing a tag `vX.Y.Z` on `main` runs it in
`.github/workflows/release.yml`, which publishes the archives, `SHA256SUMS` and the firmware image as
that tag's release.

**v0.1.0 caveats.** Its Linux x86-64 binaries are glibc builds: `moq-subghz-send`, `moq-subghz-recv`,
`moq-subghz-bench` and `css2015-log` need glibc 2.39 or newer, `moq-subghz-publish` 2.38, the others 2.34
(check with `ldd --version`). On an older system, build from source. Its archives also lack the bench
tool's reference inputs and default `--repo-root` to the build machine's path, so `css2015-table`,
`css2015-frames`, `analyze` and the CSS runs need `--repo-root` pointing at a checkout of the repository.

## 4. Flash the USB stick

The stick needs this repo's firmware. A prebuilt image is at `firmware/rp2040-sx1276-fw-0.5.1.uf2`. The runs in `results/video/` used an
earlier image of the same name, `rp2040-sx1276-fw-v5.uf2` (0.5.0, old sha256 `fe86cc05057067ea03bfdcb8122c9bb2a571da88991229148afe5fec145cb317`), which answers `I` with `sx_version=0x00` until the first `L` or
`F` command; 0.5.1 fixes that and is otherwise the same. That measured image is no longer in the repo: the file `firmware/rp2040-sx1276-fw-v5.uf2` was rebuilt from the current source (band and power checks added, reports 0.5.1) and is NOT the image behind those runs.

1. **Back up the stick first.** Hold its BOOT button while plugging it in. Then, with
   [picotool](https://github.com/raspberrypi/picotool):
   ```sh
   picotool info -a                              # check that it's the stick
   picotool save -a rp2040-backup.bin
   ```
2. Flash it: `picotool load -x firmware/rp2040-sx1276-fw-0.5.1.uf2`, or drag the `.uf2` onto the
   `RPI-RP2` drive that appears. Which image: `rp2040-sx1276-fw-0.5.1.uf2` is the 0.5.1 source as released
   and has NO band or power checks in the firmware (the host tools still check). `rp2040-sx1276-fw-v5.uf2`
   is rebuilt from the current source and includes those checks, but it has not been flashed or run yet.
3. Check it: open the serial port (macOS `/dev/cu.usbmodem*`, Linux `/dev/ttyACM0`), type `I`
   and press Enter. It should answer `I rp2040-sx1276-fw 0.5.1 sx_version=0x12 ...`. If it shows
   `sx_version=0x00`, send `X` (re-initialise the radio); on 0.5.0 send an `L` command first.

To build the firmware yourself instead:

```sh
cd firmware/rp2040-sx1276-fw
rustup target add thumbv6m-none-eabi
cargo build --release
cargo run --release --manifest-path ../../Cargo.toml -p moq-subghz-bench -- elf2uf2 target/thumbv6m-none-eabi/release/rp2040-sx1276-fw rp2040-sx1276-fw.uf2
```

## 5. First picture

Screw on both antennas. Start the receiver on the laptop first, because it waits for frames:

```sh
./target/release/moq-subghz-recv --port /dev/cu.usbmodemXXXX --secs 120 --out runs/first --play --decode
```

Then start the sender on the board:

```sh
cd ~/moq-subghz && ./moq-subghz-send --fec-k 8 --secs 110 --i-have-an-antenna --transmit
```

A window called **SX1276 FSK live video (moq-subghz)** opens with the test pattern, a running clock
and a barcode strip along the bottom. When the run ends, both programs print a one-line JSON summary.

The receiver also saves these files in `runs/first/`:

| file | what |
|---|---|
| `rec.h264` | the received stream, Annex-B |
| `summary.json` | loss, latency, freezes, CPU |
| `objects.jsonl` | one line per frame received |
| `decoded.jsonl` | one line per decoded frame, with its glass-to-glass latency |

To turn the recording into a shareable clip:

```sh
ffmpeg -framerate 15 -i runs/first/rec.h264 -c copy first.mp4
```

## 6. Tuning the video

The sender takes the usual knobs:

| flag | default | notes |
|---|---|---|
| `--bitrate` | `180000` | x264 bitrate in bit/s |
| `--gop` | `15` | keyframe interval, which is also the group length, 1 to 256 (a group's frames are numbered in one byte). A loss freezes until the next IDR, so a shorter GOP means shorter freezes but bigger bitrate spikes. |
| `--fps` | `15` | |
| `--size` | `320x240` | the width must be a multiple of 40, because the barcode is 20 blocks wide |
| `--src` | `testsrc2` | any ffmpeg lavfi source, or a file (loops) or device path |
| `--encoder` | `x264` | `hw` uses the board's `h264_v4l2m2m`. It uses less CPU but adds ~250 ms of encoder delay. |
| `--fec-k` | `0` | one parity frame per K radio frames. `8` is a good start: 12.5% more airtime and repairs one loss per 8. |

**The bitrate budget.** Keep the video bitrate to about **60% of the radio rate**. At 300 kbit/s FSK,
180 kbit/s of video used about 225 kbit/s of air once headers, padding and FEC were added, and the
rest is margin for IDR bursts. If you go higher, the sender's queue grows and it starts dropping
whole GOPs to stay live (`objects_dropped`, or `objects_dropped_queue_full` once the queue is
full, in its summary).

**The radio rate.** `--profile BITRATE:FDEV:PREAMBLE[:x]` must match on both ends. The default
is `300000:100000:6`. `350000:100000:6:x` was slightly better in our tests, but `:x` means
"outside the datasheet's tested range", so it's your call.

## 7. Measuring latency properly

The barcode gives you capture time from the sender's clock, but the receiver reads it with its own
clock. Those two clocks need comparing:

```sh
./target/release/moq-subghz-recv --clock-check pi
# {"host":"pi","best_rtt_ms":11.2,"remote_minus_local_ms":-23.4}
```

Pass that number to the receiver as `--clock-offset-ms`. The clocks drift by tens of ms between
runs, so `moq-subghz-bench` checks before and after each run and log the offset every 5 s
during it:

```sh
export PORT=/dev/cu.usbmodemXXXX PI=pi
B=target/release/moq-subghz-bench
$B run --i-have-an-antenna --transmit myrun 300000:100000:6 330 --fec-k 8     # board to laptop
$B rev --i-have-an-antenna --transmit myrun 300000:100000:12 330 --fec-k 8    # laptop to board
$B bench --i-have-an-antenna --transmit                                       # the full 8-run set from the README
cargo run --release -p moq-subghz-report -- runs/*/        # results table
```

These commands transmit, so they need `--i-have-an-antenna --transmit` from you, like the senders
themselves; the bench never adds them. More detail is in [moq-subghz-bench/README.md](moq-subghz-bench/README.md). The meaning of each column is in
[results/video/README.md](results/video/README.md).

## 8. More things to try

* **Encryption.** Each frame can be encrypted with ChaCha20-Poly1305 (from
  [rind](https://github.com/kixel-ai/rind)). Make one key and give the same file to both ends:
  ```sh
  ( umask 077; openssl rand -hex 32 > moq-subghz-key.hex )    # mode 0600, never commit it
  scp moq-subghz-key.hex pi:
  ./moq-subghz-send ... --key-file ~/moq-subghz-key.hex         # on the board
  ./moq-subghz-recv ... --key-file moq-subghz-key.hex           # on the laptop
  ```
  It costs 20 bytes per frame, 28 on the first of each GOP, which carries the run's
  salt. A frame that fails the check is treated as lost. Know what this doesn't cover:
  there's no key exchange, a recorded frame can be replayed, and the radio headers are
  sent unencrypted, so an attacker can cause loss or skew the loss counts. Apart from
  replaying recorded frames, it can't read or change the picture. The tools refuse a key
  file that other users can read.
* **Reverse direction (laptop to board).** Run `moq-subghz-send --rp2040 /dev/cu.usbmodemXXXX` on
  the laptop and `moq-subghz-recv --radio` on the board. Use a longer preamble, `300000:100000:12`
  (or `350000:100000:14:x`). The board's HAT can't signal when data has arrived, so the board has to
  keep checking, which uses a whole core. Expect more loss than the forward direction.
* **A camera.** `--src /dev/video0` should work but is untested. The clock and barcode are drawn
  onto whatever the source gives, so latency measurement still works.
* **Watch it over the internet.** Add `--publish URL` to the receiver to republish the video as a
  real MoQ broadcast on a [moq.dev](https://moq.dev) relay or [moq.pro](https://moq.pro), and watch
  it in a browser. See [docs/MOQ.md](docs/MOQ.md).
* **Without the gate.** Add `--no-group-gate` to the receiver to feed every frame to the decoder,
  even after a loss. That shows you the smearing the gate avoids.

## Troubleshooting

| problem | try |
|---|---|
| `no "OK S" reply` | wrong `--port`, or the stick isn't running this firmware (check the `I` command from section 4) |
| the window opens but stays black | the sender isn't running, or the `--profile` differs between the two ends |
| lots of freezes | check both antennas are on and try another spot, use `--fec-k 8`, or lower `--bitrate`. Loss also varies a lot from run to run. |
| `objects_dropped` or `objects_dropped_queue_full` grows on the sender | the video bitrate is too high for the radio rate (see section 6) |
| `open /dev/spidev0.0` fails on the board | SPI isn't enabled, or you haven't logged in again after `usermod` |
| `refusing to transmit` | add `--i-have-an-antenna --transmit`, after attaching one |

## 9. Radio rules

* **915.0 MHz, +5 dBm (about 3 mW).** The sender and receiver refuse anything outside
  902-928 MHz and anything within 0.3 MHz of 925.0 MHz; the sender also refuses power outside 2 to
  5 dBm.
* **Always attach an antenna** before transmitting. Transmitting without one can damage the radio.
* **915 MHz is licence-free in the Americas, not everywhere.** Europe uses 868 MHz, with duty-cycle
  limits this link breaks. Check your local rules.
* **Even in the US, this setup may not comply.** It's one fixed channel, transmitting up to ~80%
  of the time. FCC Part 15.247 allows that only if the signal is at least 500 kHz wide at the
  -6 dB points, and we haven't measured this one. We ran short indoor bench tests at 3 mW.
* **Transmitting needs `--i-have-an-antenna` and `--transmit`.** The licence gate runs before a real
  radio is opened ([docs/LICENSING.md](docs/LICENSING.md)).
* **The checks are in the host tools, not the stick.** The firmware's own serial commands
  accept up to +17 dBm on any frequency. Don't type them into a raw terminal session.
* You are the operator, and the emissions are your responsibility.
* Other bands (2.4 GHz, 5.8 GHz, amateur bands) have their own rules and no code here: see [docs/REGULATORY.md](docs/REGULATORY.md).
