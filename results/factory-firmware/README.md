# Factory firmware and factory software, still pictures over chirp spread spectrum (CSS) mode

This is the baseline for the post: what the two radios do **as they were sold**, with nothing
of this project's firmware or Rust code in the path. A small JPEG goes from a ARM single-board computer to a
RP2040-based device over the air, in text chunks, the way someone with the vendors' own example
programs would have done it.

Everything marked **measured** below came off the air on the bench on 2026-11-20. Everything
marked **calculated** is from the SX1276 datasheet airtime formula.

**Wire version.** The factory software does not use this project's wire format at all. Where
this README compares against moq-subghz runs, those runs used older wire versions than the
current version 5 (see [`../video/`](../video/README.md) and [`../loopback/`](../loopback/README.md)),
so the comparison supports frame-size and airtime points only. Any current loss or latency claim
needs a fresh run.

## What ran at each end

| | sender (ARM single-board computer) | receiver (RP2040-based device, RP2040 + SX1276) |
|---|---|---|
| radio | SX1276 GPS HAT (SX1276) | onboard sub-GHz CSS transceiver module (SX1276) |
| software | the HAT maker's official sample app `hat_app`, from [upstream repository](https://github.com/dragino/rpi-lora-tranceiver) commit `f6e1a55` (2018-05-11), built with g++ 14.2 and WiringPi v3.20 | the board maker stock receiver, `factory-receiver-rp2040.uf2` from release `v3.1.0.1` (sketch `factory-receiver.ino`, Sandeep Mistry's an SX127x library) |
| in this repo | [`firmware/vendor/sx1276-gps-hat/`](../../firmware/vendor/sx1276-gps-hat/README.md) | [`firmware/vendor/rp2040-factory-receiver/`](../../firmware/vendor/rp2040-factory-receiver/README.md) |

board OS: Debian 13 (Debian-based OS), kernel 6.18.60. None of this project's firmware (`rp2040-sx1276-fw`)
and none of this project's Rust tools ran in these tests. The stick's flash content before this
test was byte for byte the board maker image (see the vendor README).

### The the HAT maker app was changed in four constants

The app is built for 868 MHz and the public-network sync word, and it is not a receiver for
this stick's stock sketch as it stands. The edit is four lines, nothing else
([`gps-hat-bench.patch`](../../firmware/vendor/sx1276-gps-hat/gps-hat-bench.patch)):

| line | stock | here | why |
|---|---|---|---|
| carrier | 868.1 MHz | **915.0 MHz** | the stick's sketch calls `CSS mode.begin(915E6)` |
| sync word | 0x34 (framed public) | **0x12** | the stick's library default is 0x12 |
| message buffer | 32 bytes | **256 bytes** | the stock app can only send 31 characters; chunks are 250 |
| TX power | `configPower(23)`, clamps to +17 dBm | **`configPower(3)`, +5 dBm** | bench at short range, same cap as the rest of this repo |

Everything else is the HAT maker's: spreading factor 7, 125 kHz, coding rate 4/5, explicit header,
payload CRC on, 8 preamble symbols, the pins (CS GPIO25, RESET GPIO17, DIO0 GPIO4), the reset
and version check, and the SPI code. The stick's stock sketch sets no modem options, so its
defaults are the same: SF7, 125 kHz, CR 4/5, 8 preamble, explicit header. The power register
value is what the app programs; the actual output power was not measured.

## How the picture goes over

The stock sender sends one text message and repeats it every 5 seconds, and the stock receiver
prints text. So, like a 2015 hobbyist with a shell loop:

* The JPEG is cut into 120 byte pieces. Each piece goes out as a text line
  `A:<number>/<total>:<hex>`, 250 characters at most, so nothing binary crosses the stock
  serial print.
* A loop on the board starts the stock app once per piece (`timeout 1.3 ./hat_app sender "<line>"`).
  The app resets the radio, sends, and the timeout ends it after the packet has left the air.
  That is about 1.34 s per packet, mostly the app's own start-up and reset waits
  (`moq-subghz-bench factory-send`).
* The stick prints `Received packet '<text>' with RSSI <dBm>` over USB serial. A logger on the
  host saves every byte it prints (`moq-subghz-bench stick-cap`) and the pieces are
  put back together in order (`moq-subghz-bench factory-bench`).
* After a pass, only the missing pieces are sent again, up to 4 passes, which is the usual
  2015 answer to loss. There is no acknowledgement over the air: the host decides what is missing.

What the stock stick cannot give: **no SNR** (the sketch prints RSSI only), no per packet
timestamps, and no CRC error count (a packet with a bad CRC is dropped silently by the library).
Loss here means "sent and not printed".

## Results (measured)

Settings for all runs: 915.0 MHz, SF7, 125 kHz, CR 4/5, +5 dBm register setting, short range on the bench.
Raw data: [`results.csv`](results.csv) and one folder per run under [`data/`](data/): the chunk
lists, the board's per packet send log, the stick's raw serial bytes, the parsed packets, and the
reassembled JPEG.

Pass 1 sends every piece once. "Passes" counts sends until the picture was complete.

| run | picture | JPEG bytes | pieces | pass 1 received | pass 1 loss | passes | packets sent | RSSI, dBm | time to a complete picture | goodput |
|---|---|---|---|---|---|---|---|---|---|---|
| small-1 | 160x120 q20 | 2346 | 20 | 18 | 10.0% | 2 | 22 | -38 to -35 | 37.6 s | 499 bit/s |
| small-2 | 160x120 q20 | 2346 | 20 | 19 | 5.0% | 2 | 21 | -37 to -35 | 36.3 s | 517 bit/s |
| small-3 | 160x120 q20 | 2346 | 20 | 20 | 0.0% | 1 | 20 | -37 to -35 | 30.5 s | 615 bit/s |
| large-1 | 320x240 q10 | 8618 | 72 | 66 | 8.3% | 2 | 78 | -39 to -35 | 112.5 s | 613 bit/s |
| large-2 | 320x240 q10 | 8618 | 72 | 67 | 6.9% | 3 | 78 | -39 to -35 | 116.7 s | 591 bit/s |
| large-3 | 320x240 q10 | 8618 | 72 | 63 | 12.5% | 4 | 83 | -39 to -35 | 127.3 s | 541 bit/s |
| smoke (30 s) | 160x120 q20 | 2346 | 20 | 17 | 15.0% | 1 (stopped) | 20 | -38 to -35 | | |

* **All six full runs rebuilt the picture byte for byte** (SHA-256 identical to the sent JPEG), after 1 to 4 passes.
* Pass 1 over all seven runs: 296 pieces sent, 270 printed, **8.8% lost**. Mean RSSI about -37 dBm, which is a strong signal,
  so the loss is not a weak signal. This test cannot say why packets were lost; the stock stick does not report it.
* **Time per picture:** 30 to 38 s for the 160x120 picture, 112 to 127 s for the 320x240 picture. "Time to a complete picture"
  runs from the first send to the last piece printed and includes about 2 s of wait and ssh set-up per pass.
* **Goodput** is the JPEG bits divided by that time: about 500 to 615 bit/s. The text format costs half
  (two characters per JPEG byte) and the app's per packet start-up costs most of the rest.

### Calculated, not measured

One 250 byte packet at SF7 / 125 kHz / CR 4/5 / CRC on / 8 preamble symbols is 389.4 ms on air (datasheet formula,
the same one `sx1276` in this repo implements). Sending the pieces back to back with no start-up gap would take
7.8 s for the 160x120 picture and 28.0 s for the 320x240 picture with no loss, or about 2.5 kbit/s of JPEG data
(120 JPEG bytes per 389 ms packet). The measured 1.34 s per packet is 3.4 times the airtime: the
stock app, not the radio, sets the pace.

## Compare

moq-subghz sends 15 frames per second of live video over FSK at 300 kbit/s with the project's own firmware
([`results/video/`](../video/README.md)). The stock pair above takes about 30 to 127 seconds for one small still
picture. The earlier 2015-style calculations are in [`results/css-2015/`](../css-2015/README.md); those were
for this project's own sender and logger, not these factory programs, and were not measured at the time.

## Limits

* One bench, one antenna pair, short range. No range test, no interference test.
* Output power was not measured; the +5 dBm is the register setting.
* The stock stick prints no SNR, so there is none here.
* The reason for the 5 to 15% loss on pass 1 is not known. It could be the sender's reset on every packet, the stick's
  polled receive loop, or RF at close range. Nothing here separates them.
* Not framed: the stock the HAT maker app is plain CSS mode, no join, no network server.

## Run it again

1. Stick on the stock image (`firmware/vendor/rp2040-factory-receiver/`), plugged into the host. Pi with the HAT, antennas on both.
2. On the board: `firmware/vendor/sx1276-gps-hat/fetch-and-build.sh`, then build `moq-subghz-bench` for the board (`cargo build --release -p moq-subghz-bench --target armv7-unknown-linux-musleabihf`), copy it to
   `~/factory-bench/` and make the built app available at `~/factory-bench/gps-hat/hat_app`.
3. On the host (repo root, `ssh pi` reaches the board): `target/release/moq-subghz-bench factory-bench --i-have-an-antenna --dev /dev/ttyACM0 --label try1 --jpeg results/css-2015/frames/f160x120-q20.jpg --out results/factory-firmware/data`.
   Add `--limit 20 --passes 1` for the 30 s smoke test. `--dev` is the stick's serial port; there is no default. `--dry-run` prints the commands. The published runs were 3 of each picture (`--passes 4`), run one after another.

The Rust port does the same and is gated: `moq-subghz-bench factory-bench --label try1 --jpeg ... --dev /dev/ttyACM0 --i-have-an-antenna --transmit --freq-mhz 915.0 --power-dbm 5`.
It refuses the unpatched stock build (868.1 MHz, +17 dBm); see [docs/LICENSING.md](../../docs/LICENSING.md).

(Written by Grok Bot)
