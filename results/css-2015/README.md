# A 2015-style chirp spread spectrum (CSS) mode image test

What could a small team have done in 2015 with a CSS mode chip and a ARM single-board computer? They would not
have sent live video. They would have sent still pictures, in small packets, and waited. This
test does that on the same Pi, HAT and RP2040-based device as the moq-subghz video, so the two can be
compared on the same bench.

**Status: measured on the air, 2026-11-20.** 72 runs on `rp2040-sx1276-fw` 0.5.1 are in
[`results/stick-0.5.1/`](../stick-0.5.1/README.md), with the factory software next to them. The airtime table
below is still the calculated one; the measured seconds match it within 1% when no frame is lost.

**Wire version.** The wire version these captures used is not recorded in this README, and the
current tools speak version 5 (`VERSION` in `moq-subghz-wire/src/lib.rs`). Treat the captures as
supporting frame-size and airtime comparisons only. Any current loss or latency claim needs a
fresh run of the current tools.

## What gets sent

One test picture, cut three ways. It is a frame of the `testsrc2` pattern (`media/frame-f350-rind-120s.png`).

| mode | what goes on the air | the 2015 idea |
|---|---|---|
| `chunk` | the JPEG cut into 255 byte frames, each with a 2 byte header (image id 4 bits, chunk number 6 bits, chunk count 6 bits). Send it again until every chunk has been heard. | the simplest thing that works |
| `ssdv` | SSDV no-FEC packets: 256 bytes, sent as 255 by **dropping the leading 0x55 sync byte**, which the receiver puts back. | what balloon trackers actually did ([Dave Akerman's CSS mode gateway](https://www.daveakerman.com/?p=1719) hands SSDV packets to the `ssdv` decoder) |
| `framed` | the same chunks inside framed-uplink 1.0 unconfirmed uplinks: MHDR 0x40, DevAddr, FCtrl 0, FCnt, FPort 1, AES-128 encrypted payload, 4 byte MIC. 242 byte payload, so 255 bytes on the air, as in US915 DR3 and DR4. | how you would have done it on a framed network |

**framed, no network.** The `framed` mode builds correct framed-uplink 1.0 uplink bytes with
published test keys (the RFC 4493 key, DevAddr 01020304). There is no join, no gateway and no
network server, and the stick uses the private sync word 0x12, not the public framed 0x34. A
real gateway would not hear it. It shows what the frame size and the extra 13 bytes cost, not
what a framed network does.

The test pictures are in `frames/`: 320x240 at ffmpeg quality 10 and 160x120 at quality 20,
as JPEG, and the same pictures as SSDV packets. SSDV needs sizes in multiples of 16, so its small
picture is 160x112. SSDV re-encodes the picture with its own tables, so it comes back a little
different from the JPEG (about 29 dB PSNR for the large one). Rebuild them with
`moq-subghz-bench css2015-frames`.

## Radio settings

All: explicit header, CRC on, coding rate 4/5, 8 preamble symbols, at most +5 dBm.

| name | carrier | spreading factor | bandwidth | note |
|---|---|---|---|---|
| `sf7-125` | 902.3 MHz | 7 | 125 kHz | US915 channel 0, DR3 |
| `sf7-500` | 903.0 MHz | 7 | 500 kHz | US915 channel 64. The plan uses SF7/500 only for downlinks (DR13). |
| `sf8-500` | 903.0 MHz | 8 | 500 kHz | US915 channel 64 at its real uplink rate, DR4 |
| `sf7-250` | 903.0 MHz | 7 | 250 kHz | not a US915 width. Akerman's fast SSDV mode is also 250 kHz at SF7, but at coding rate 4/6 (`--cr 6`). |

The stick firmware (`rp2040-sx1276-fw` v5) fixes the sync word at 0x12 and maps any bandwidth other than
125 and 250 kHz to 500 kHz. The tools therefore refuse other bandwidths instead of letting them
change silently.

## Airtime, calculated, not measured

Time on air from the datasheet formula, which `sx1276` implements and its tests check. Values for
the table below came from `moq-subghz-bench css2015-table`, which uses `css2015-send --dry-run`.
"Best fps" is one picture per airtime with no loss, no gaps and no repeat pass. "Images per 30 s" is how many
pictures fit in The Things Network's fair-use limit of 30 s of uplink airtime per day, which applies on
its public network and not on this bench.

| image | mode | setting | frames | bytes on air | airtime per image (s) | best fps | images per 30 s |
|---|---|---|---|---|---|---|---|
| 320x240 q10 JPEG | chunk | sf7-125 | 35 | 8688 | 13.64 | 0.073 | 2 |
| 320x240 q10 JPEG | framed | sf7-125 | 36 | 9158 | 14.36 | 0.070 | 2 |
| 320x240 q10 SSDV | ssdv | sf7-125 | 35 | 8925 | 13.99 | 0.071 | 2 |
| 160x120 q20 JPEG | chunk | sf7-125 | 10 | 2366 | 3.73 | 0.268 | 8 |
| 160x120 q20 JPEG | framed | sf7-125 | 10 | 2496 | 3.91 | 0.255 | 7 |
| 160x112 q20 SSDV | ssdv | sf7-125 | 11 | 2805 | 4.40 | 0.227 | 6 |
| 320x240 q10 JPEG | chunk | sf7-250 | 35 | 8688 | 6.82 | 0.147 | 4 |
| 320x240 q10 JPEG | framed | sf7-250 | 36 | 9158 | 7.18 | 0.139 | 4 |
| 320x240 q10 SSDV | ssdv | sf7-250 | 35 | 8925 | 6.99 | 0.143 | 4 |
| 160x120 q20 JPEG | chunk | sf7-250 | 10 | 2366 | 1.86 | 0.537 | 16 |
| 160x120 q20 JPEG | framed | sf7-250 | 10 | 2496 | 1.96 | 0.511 | 15 |
| 160x112 q20 SSDV | ssdv | sf7-250 | 11 | 2805 | 2.20 | 0.455 | 13 |
| 320x240 q10 JPEG | chunk | sf7-500 | 35 | 8688 | 3.41 | 0.293 | 8 |
| 320x240 q10 JPEG | framed | sf7-500 | 36 | 9158 | 3.59 | 0.279 | 8 |
| 320x240 q10 SSDV | ssdv | sf7-500 | 35 | 8925 | 3.50 | 0.286 | 8 |
| 160x120 q20 JPEG | chunk | sf7-500 | 10 | 2366 | 0.93 | 1.074 | 32 |
| 160x120 q20 JPEG | framed | sf7-500 | 10 | 2496 | 0.98 | 1.021 | 30 |
| 160x112 q20 SSDV | ssdv | sf7-500 | 11 | 2805 | 1.10 | 0.910 | 27 |
| 320x240 q10 JPEG | chunk | sf8-500 | 35 | 8688 | 6.03 | 0.166 | 4 |
| 320x240 q10 JPEG | framed | sf8-500 | 36 | 9158 | 6.35 | 0.158 | 4 |
| 320x240 q10 SSDV | ssdv | sf8-500 | 35 | 8925 | 6.19 | 0.162 | 4 |
| 160x120 q20 JPEG | chunk | sf8-500 | 10 | 2366 | 1.65 | 0.606 | 18 |
| 160x120 q20 JPEG | framed | sf8-500 | 10 | 2496 | 1.73 | 0.577 | 17 |
| 160x112 q20 SSDV | ssdv | sf8-500 | 11 | 2805 | 1.94 | 0.514 | 15 |

How to read it: at 125 kHz a full 255 byte frame takes 399.6 ms, just under the US915 400 ms
dwell limit on the 125 kHz channels, which is why the framed payload tops out at 242 bytes. One
320x240 picture takes about 14 s of airtime at SF7/125 and 3.4 s at SF7/500. moq-subghz sends 15
frames per second of the same size over FSK. These numbers are one picture, with no video
codec, so a "frame rate" of 0.07 to 1 per second is the honest comparison.

Europe is different and this bench is not Europe: the EU868 sub-bands allow 0.1% to 10% duty
cycle, usually 1%, so a 400 ms frame must be followed by about 40 s of silence. Use
`--duty-pct 1` to see that cost.

## What you need

* The board with the SX1276 GPS HAT, an antenna on it, and the 915 MHz stick on the Mac with its antenna.
* The stick on `rp2040-sx1276-fw` v5 (`firmware/rp2040-sx1276-fw-v5.uf2`), the image the other runs used, or 0.5.1
  (`firmware/rp2040-sx1276-fw-0.5.1.uf2`), which only fixes the radio start-up (see the CHANGELOG).
  On v5 the logger's first `I` can read `sx_version=0x00` right after power-up; the radio is
  fine, and 0.5.1 does not have this.
  **Do not reflash it for this test.** The logger checks the banner and refuses another version.
* The `ssdv` tool on the Mac, to turn rebuilt SSDV packets back into a picture (optional; the
  packets are saved either way): `git clone https://codeberg.org/fsphil/ssdv && cd ssdv && make`.

## Run it

Build the logger on the Mac and the sender for the board, as in [HOWTO section 3](../../HOWTO.md#3-build):

```sh
cargo build --release -p moq-subghz-css2015
rustup target add armv7-unknown-linux-musleabihf
cargo build --release -p moq-subghz-css2015 --target armv7-unknown-linux-musleabihf
ssh pi mkdir -p css2015
scp target/armv7-unknown-linux-musleabihf/release/css2015-send pi:css2015/
scp -r results/css-2015/frames pi:css2015/
```

**Before any radio.** Check the plan on the board (nothing is sent):

```sh
ssh pi 'cd css2015 && ./css2015-send --mode chunk --file frames/f160x120-q20.jpg --dry-run'
```

**1. Power and cables.** Power the board with the HAT on. Plug the stick into the Mac. Screw an
antenna on both. Never transmit without one.

**2. Find the stick's port** on the Mac: `ls /dev/cu.usbmodem*`.

**3. The 30 second smoke test.** Start the logger on the Mac first, then the sender on the board.
The small picture at SF7/125 takes about 3.7 s per pass, so 30 s is about 8 passes.

```sh
# Mac
./target/release/css2015-log --port /dev/cu.usbmodemXXXX --mode chunk --profile sf7-125 \
  --secs 40 --out runs/smoke --reference results/css-2015/frames/f160x120-q20.jpg
# Pi, in another terminal, a few seconds after the logger prints "listening"
ssh pi 'cd css2015 && ./css2015-send --mode chunk --file frames/f160x120-q20.jpg \
  --profile sf7-125 --power 5 --passes 20 --max-secs 30 --tx-log smoke-tx.csv --i-have-an-antenna --transmit'
```

It passes when `runs/smoke/images.csv` shows `complete` 1 and `matches_reference` 1, and
`runs/smoke/image0.jpg` opens as the test picture. If no frame arrives, check both ends use the
same `--profile`, both antennas are on, and the logger printed the stick's banner. If the sender
says the readback differs, stop and report it. Do not change registers by hand.

**4. The matrix.** Each run: logger first, sender second. Use the same `--profile` on both. Give the
logger `--secs` a little longer than the sender needs (the sender prints its airtime per pass and
the number of passes you ask for). Three passes lets a lost chunk arrive on a later pass.

| run | sender `--mode`, `--file` | `--profile` | extra sender flags |
|---|---|---|---|
| 1 | `chunk`, `f160x120-q20.jpg` | `sf7-125` | `--passes 3` |
| 2 | `chunk`, `f320x240-q10.jpg` | `sf7-125` | `--passes 3` |
| 3 | `chunk`, `f320x240-q10.jpg` | `sf7-500` | `--passes 3` |
| 4 | `ssdv`, `f320x240-q10.ssdv` | `sf7-125` | `--passes 3` |
| 5 | `ssdv`, `f320x240-q10.ssdv` | `sf7-500` | `--passes 3` |
| 6 | `framed`, `f320x240-q10.jpg` | `sf7-125` | `--dr dr3 --passes 3` |
| 7 | `framed`, `f320x240-q10.jpg` | `sf8-500` | `--dr dr4 --passes 3` |
| 8 | `framed`, `f160x120-q20.jpg` | `sf7-125` | `--dr dr3 --duty-pct 1 --passes 1` (the EU-style wait, about 7 minutes) |

The logger's `--mode` and `--profile` always match the sender's. Add `--tx-log NAME.csv` to the
sender to keep one line per frame it sent, and copy that file back with the results.

**5. The exact loss figure.** The sender ends with a line like `sent frames=105 ...`. Feed that count
back, reading the saved `raw.log` (this does not touch the radio):

```sh
./target/release/css2015-log --replay runs/RUN/raw.log --mode chunk --profile sf7-125 \
  --sent-frames 105 --out runs/RUN-exact --reference results/css-2015/frames/f320x240-q10.jpg
```

## What the logger writes

In the `--out` directory:

* `rx.csv`: every frame the stick reported: time, RSSI, SNR, length, image and chunk number, status
  (`new`, `dup`, `bad` with the reason such as `MIC mismatch`), and the calculated airtime of that frame.
* `images.csv`: one line per image. `seconds_to_image` runs from the first frame heard to the
  last frame needed, plus one frame of airtime, by the stick's clock. `effective_fps` is its
  inverse. `loss_est_pct` is guessed from gaps in the chunk numbers, pass by pass: it cannot see
  a lost tail or a whole lost pass. `loss_exact_pct` uses `--sent-frames` and is the figure to
  quote; it is filled only when the run holds one image, since the sender's count covers the
  whole run. `callsign` is the SSDV callsign (images are told apart by callsign and id). Columns ending `_calc` are the calculated airtime, not measured. `image_bytes_per_frame` is
  how much picture each frame carries (253 chunk, 240 framed, 237 ssdv), against 255 on the air.
* `run.csv`: frames heard, frames delivered intact, frames sent and `loss_exact_pct` for the
  whole run.
* `image<N>.jpg`: the rebuilt picture, written only when it is complete (and equal to `--reference`
  if you gave one). For SSDV, `image<N>.ssdv` holds the packets (gaps stay gaps) and the `ssdv`
  tool turns them into `image<N>.jpg`, with gray where packets are missing. SSDV names carry the
  callsign (`image<N>-<CALL>`), and a later image that reused an id gets `-2`, `-3`, so nothing
  is overwritten. A piece that conflicts with one already held starts a new image.
* `raw.log`: the stick's lines, kept so a run can be read again with `--replay`.

## Sources

* Dave Akerman, [Making a CSS mode Gateway](https://www.daveakerman.com/?p=1719) (21 Jan 2015): a board
  with a CSS mode board, `ssdv` for decoding images, and the gateway's CSS modes (mode 1 is "normal
  for SSDV", mode 3 is "fast SSDV": 250 kHz, SF7, 4/6). Also his
  [Portable CSS mode Gateway](https://www.daveakerman.com/?p=1905) (9 Dec 2015).
* SSDV: [codeberg.org/fsphil/ssdv](https://codeberg.org/fsphil/ssdv) for the packet format. The
  sync byte is not covered by the CRC (`ssdv_dec_is_packet` sets it itself). That the CSS mode
  trackers drop it to fit 255 bytes is described in
  [A third-party write-up of CSS mode SSDV](http://tt7hab.blogspot.com/2018/04/the-lora-ssdv.html); I did not
  find it in a post by Akerman himself.
* [Public MAC-frame specification v1.0](https://resources.lora-alliance.org/document/lorawan-specification-v1-0)
  (January 2015, the version of the time): MAC message format, MIC (section 4.4), payload
  encryption (4.3.3), US902-928 channels and the 400 ms limit (section 7.2). The tests check the
  framing against a vector computed separately in Python, and the AES-CMAC code against RFC 4493.
* Community network operator docs, [US902-928 MHz band](https://www.thethingsnetwork.org/docs/lorawan/regional-parameters/us915/):
  channel plan, no duty cycle but a 400 ms dwell time on channels 0 to 63, payload limits per data
  rate. The table there lists 222 bytes for DR3 and DR4 when repeaters must work and 242 without
  them. This test uses 242 (`--payload 222` for the other).
* Community network operator docs, [Duty Cycle](https://www.thethingsnetwork.org/docs/lorawan/duty-cycle/): 1%
  and 0.1% EU sub-bands, and the Fair Use Policy of 30 s of uplink airtime per day.

## Limits

* Not run on the radios. The sender's CSS mode path (`open_hat_raw`, then the driver's `init`,
  `set_tx_power` and `transmit`) and the stick's `L` and `R` path have not been run by whoever wrote this.
  The unit tests, a no-radio loopback and a fake serial stick covered everything else.
* The sender uses the same Pi glue as `moq-subghz-send`; its FSK path is unchanged.
* One picture, one link, indoors, a few metres. No range test, no interference test.
* No retransmit request: the link is one way, so "send it again" means repeat passes.
* Same rules as [HOWTO section 9](../../HOWTO.md#9-radio-rules). This is a 125 to 500 kHz wide
  signal at 3 mW: keep runs short and indoors.
