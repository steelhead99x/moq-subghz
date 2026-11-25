# RP2040-based device: factory receiver firmware

The stock chirp spread spectrum (CSS) mode receiver image that the RP2040-based device runs when it is set up as a plain CSS mode
receiver. The factory benchmark in [`results/factory-firmware/`](../../../results/factory-firmware/README.md)
ran with this image on the stick.

| | |
|---|---|
| Vendor | Electronic Cats |
| Release | `v3.1.0.1` (published 2025-03-11) |
| Source commit | `ea7742739199728c11eb93bc991dc99aeadcf612` (tag `v3.1.0.1`) |
| Sketch | the receiver sketch under the upstream `Fw/` tree, copied here as `factory-receiver.ino`, unchanged |
| Retrieved | 2026-11-20 |
| License | GPL-3.0 (`LICENSE` here is the upstream file) |

## Files

* `factory-receiver-rp2040.uf2`: the release asset, byte for byte. `sha256sum -c SHA256SUMS` checks it.
* `factory-receiver.ino`: the sketch it is built from, copied from the tag above. It is the
  corresponding source for the GPL. Build tools and libraries it pulls in (the RP2040 board-support core, Sandeep
  Mistry's SX127x library, the Pico SDK) keep their own licenses upstream.
* `LICENSE`: GPL-3.0, upstream's copy.

This folder is the only place the GPL applies. The rest of this repository stays under
MIT OR Apache-2.0, and nothing here is linked into it.

## What the stick prints

```
<receiver banner line>                             (once, at start; the sketch waits for a serial port to open)
Received packet '<payload bytes as text>' with RSSI <dBm>
```

The sketch starts the library with `begin(915E6)` and nothing else, so the library defaults apply: 915 MHz,
spreading factor 7, 125 kHz bandwidth, coding rate 4/5, sync word 0x12, preamble 8, explicit
header. It prints the packet RSSI only, not the SNR. It prints the payload with
`Serial.print((char)byte)`, so binary bytes come out raw and cannot be told from the framing text.

## Is the pre-reflash stick backup the same image?

The stick's flash was dumped before the project firmware was put on it. The dump (2 MiB, not
published) was compared with this release asset. **It matches.** All 364 UF2 blocks of
`factory-receiver-rp2040.uf2` are identical to the dump at the same addresses, and every other byte
of the dump is blank (`0x00` or `0xFF`). It holds no WiFi settings, keys, serial-specific
configuration or user data, so the official release image is published and the dump is not.

## Put the stock firmware back on a stick

1. Unplug the stick. Hold its BOOT button while plugging it in, or send `BOOT` over serial to
   the project firmware, or open the port at 1200 baud and drop DTR. A drive named `RPI-RP2`
   appears.
2. Copy `factory-receiver-rp2040.uf2` onto that drive. The stick reboots by itself.
3. Open the serial port (any baud rate). It prints its receiver banner line. If it prints
   a start-failed line the radio did not answer.

To go back to the project firmware, do the same with the project image in `firmware/` (for example `rp2040-sx1276-fw-v5.uf2`).
