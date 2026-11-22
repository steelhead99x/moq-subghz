# HaLow (802.11ah) and the other modeled links

Status: **modeled only.** The HaLow module model and its SPI pins are not known yet, so no HaLow
hardware is driven by anything in this repository. Every number produced by the `-emu` links is a
simulation built from the assumptions below. None of it is an over-the-air measurement, and every
summary these links write says `"provenance":"modeled"`.

## What is here

| path | what |
|---|---|
| `moq-subghz-link` | the `Link` trait the emulation implements (`caps`, `send` -> `TxReport`, `recv`, `idle`) |
| `radios/moq-subghz-radio-emu/src/model.rs` | the channel models and their settings: SX1276 FSK, SX1276 LoRa, HaLow. Seeded, virtual time |
| `radios/moq-subghz-radio-emu/src/link.rs` | `EmuLink`: a `Link` over a model, with in-memory (`pair`), UDP and `R`-line file transports |
| `radios/moq-subghz-radio-emu/src/s1g.rs` | S1G rate table derived from OFDM parameters, preamble and symbol-quantised PPDU time |
| `radios/moq-subghz-radio-emu/src/loss.rs` | splitmix64 RNG, iid and Gilbert-Elliott loss |
| `radios/moq-subghz-radio-emu/src/pack.rs` | several 255 B wire frames per packet: `[len u8][frame]` records |
| `radios/moq-subghz-radio-emu/src/args.rs` | the emulation flags shared by send, recv and the chan tool |
| `radios/moq-subghz-radio-emu/src/bin/moq-subghz-chan.rs` | offline: push a recorded `R`-line file through a model |
| `moq-subghz-send`, `moq-subghz-recv` | `--link fsk` (default) `\| fsk-emu \| lora-emu \| halow-emu`, `--emu-udp ADDR` |

The default is `--link fsk` and nothing else changes: the SX1276, CatWAN and `--loopback` paths are
the code that was there before. The wire format and `VERSION` are untouched (the golden v5 test
guards that) and sealing happens before fragmentation as before. An emulated link never opens a
serial port or SPI device and never transmits: its UDP side accepts loopback addresses only.

## How it plugs into `Link`

`EmuLink::send(packet)` runs the packet through the model and returns a `TxReport`: `busy_us`
(backoff, every attempt, ACK waits), `airtime_us` (the data attempts only), `attempts`,
`delivered` (`Some`; a real radio could not know, the model does) and `deliver_after_us`. Its
caps carry `kind: Emu` and `provenance: Modeled`. `max_payload` is the HaLow MTU and
`max_frames_per_packet` is how many whole wire frames fit in it; the sender packs them
(`pack::take_batch`, `pack::pack`) when it is above 1, and the receiver unpacks. FSK and LoRa
models carry one frame per packet, as the SX1276 does.

Time is virtual by default: a send adds its busy time to the link clock and returns at once, so
tests and `moq-subghz-chan` are repeatable. `moq-subghz-send` turns on real-time pacing: a send
sleeps until the modeled channel is free and datagrams are released at their delivery time.

## How to run it (no radio, no network beyond 127.0.0.1)

```sh
# receiver first, then sender; both name the same link and settings
moq-subghz-recv --link halow-emu --emu-udp 127.0.0.1:47000 --secs 40 --decode --out run
moq-subghz-send --link halow-emu --emu-udp 127.0.0.1:47000 --halow-bw 2 --halow-mcs 4 \
    --bitrate 1000000 --size 640x480 --fps 30 --gop 30 --fec-k 8 --secs 30
# lossy channel: Gilbert-Elliott p_gb:p_bg:loss_good:loss_bad, broadcast (no ACK) mode, to a file
moq-subghz-send --link halow-emu --halow-bcast --emu-loss 0.01:0.1:0:0.8 --loopback out.air ...
# offline: a recording through the model, then replay
moq-subghz-chan --link halow-emu --halow-mcs 4 --in k8.air --out survivors.air --emu-loss iid:0.01
moq-subghz-recv --replay survivors.air --out run
# deterministic airtime of a frame mix
moq-subghz-chan --link halow-emu --lens 255x34,96x2
```

`--link lora-emu` and `--link fsk-emu` use the same machinery with the SX1276 LoRa time-on-air
formula and the FSK frame time. They are separate names from `--link fsk` on purpose: `fsk` is
the real radio. `--emu-seed N` makes a run repeatable: the same seed and settings give
byte-equal output.

## Packing and the 255 byte wire frame

`idx` and `nfrag` are `u8` and a wire frame is at most 255 B, so the wire format cannot carry
larger frames. A HaLow packet can, so the sender glues up to `mtu / 256` whole wire frames of the
same object into one packet as `[len][frame]` records (1 B overhead each). Each frame keeps its
own CRC-16. The receiver unpacks and feeds the frames to the unchanged reassembler. Consequence:
one lost packet removes several consecutive frames, which usually sit in the same XOR parity block,
so FEC cannot repair them. `--halow-pack 1` sends one frame per packet to get FEC back at a large
airtime cost. With the MAC ACK and retries on (the default) the loss is hidden before FEC matters.
`tests/end_to_end.rs` shows both effects with real `Fragmenter` output.

## Parameter table (every value is an assumption unless a source is named)

| parameter | default | label |
|---|---|---|
| rate table, MCS0..10, 1/2/4/8/16 MHz, 1 stream, long GI | derived: N_SD x bits x code rate / 40 us | SOURCE: IEEE 802.11ah-2016 S1G MCS tables as reproduced at en.wikipedia.org/wiki/IEEE_802.11ah ("S1G MCS data rates"); every cell is unit tested against it (`s1g::tests`). 2 MHz MCS9 and MCS10 above 1 MHz are N/A |
| N_SD 24/52/108/234/468, symbol 40 us | | SOURCE: same standard; the 4x subcarrier ratios reproduce the table |
| default width and MCS | 1 MHz, MCS0 | ASSUMPTION: the most robust choice, not a recommendation |
| preamble + SIG | 560 us at 1 MHz, 360 us otherwise | ASSUMPTION: the frame structure of 802.11ah-2016 as recalled, not checked against the standard text or a datasheet. The BCC tail is a flat 6 bits (one encoder); a second encoder at the highest rates would add at most one 40 us symbol per packet, not modeled |
| MAC + LLC + IP + UDP per packet | 64 B | ASSUMPTION: 28 (an 802.11 data header and FCS as recalled) + 8 + 20 + 8. A raw-frame driver would pay less |
| PHY payload limit | 1472 B | ASSUMPTION: 1500 B IP MTU. Real modules differ |
| SIFS / slot / AIFSN | 160 us / 52 us / 3 | ASSUMPTION: recalled sub-GHz S1G values, not checked against the standard text |
| CWmin / CWmax | 15 / 1023 | ASSUMPTION: EDCA best effort defaults. Backoff is always charged, even on an idle channel (conservative) |
| ACK | 14 B at MCS0 of the same width; unicast retries 7 | ASSUMPTION: 802.11 control response and short retry limit defaults. S1G NDP ACK would be shorter |
| receive latency / jitter | 1.5 ms / 0..1 ms | ASSUMPTION: driver and stack, not measured on any module |
| loss | none, iid:P, or Gilbert-Elliott | ASSUMPTION (model choice): GE steps once per PHY attempt, the ACK uses the same state, and the chain starts in its stationary distribution (not always Good), so short runs see the mean loss. Burst parameters used in the bench are illustrative, not fitted to a site |
| rate control, rate fallback, A-MPDU, power save, TWT, RAW | none | NOT MODELED. Fixed MCS, one packet at a time, single station, no contention |
| duty cycle | none | NOT MODELED. US 902-928 MHz is assumed; EU sub-GHz duty limits are not enforced here |
| FSK airtime | preamble + 2 sync + 1 + payload + 2 CRC at the bit rate | SOURCE: `vendor/sx1276/src/fsk.rs` and the sender |
| LoRa airtime | Semtech SX1276 datasheet formula | SOURCE: datasheet formula. The tests' reference values are the formula's own outputs, a regression guard, not an independent measurement. Bandwidth must be one the SX1276 selects, and SF6 needs the implicit header (`--lora-implicit`). The implicit-header airtime is the same formula with IH = 1, hand-checked in `model::tests`; ASSUMPTION: no published reference value is cited, and the datasheet's extra preamble symbols for SF6 are not modeled |
| argument bounds | retries <= 255, overhead and ACK size <= 4096 B, MTU <= 65507 B, latency, jitter, SIFS, slot and fixed latency <= 10 s, CWmin <= CWmax <= 65535 | model guard: larger values are refused (`EmuCfg::validate`), and the arithmetic saturates instead of overflowing |
| delivered and acked | `TxReport.delivered`: the far side got a copy; `acked`: the sender saw an ACK | With ACK on, `delivered` and not `acked` is the case where every ACK was lost: the receiver has the packet and a real sender would report failure. `acked` is `None` without ACKs |
| UDP receiver | any local process may send to it | loopback only. By default any local process may send to it; `--emu-udp-from` on the sender and `--emu-udp-peer` on the receiver fix the source address and the kernel drops everything else. A test channel, not a security boundary. In-memory pairs hold at most 16 MiB per direction, then `send` is `Busy` |
| FSK and LoRa loss, latency | none unless `--emu-loss`; delivery time = airtime | ASSUMPTION: no fixed receive latency |

## What the tests check

* Rate table: every cell of the published S1G table against the derived value (`s1g`).
* Airtime: FSK against the sender's formula, LoRa against the Semtech reference values, a clean
  HaLow attempt against the sum of its parts (`model`).
* Loss: iid mean, Gilbert-Elliott mean loss and mean burst length by simulation (`loss`).
* Determinism: the same seed gives byte-equal `R` files and equal reports (`link`).
* Packing: round trip, malformed input, MTU and cap limits (`pack`).
* End to end: real `Fragmenter` output through the modeled link and back through the unchanged
  `Reassembler`, with and without ACK, packed and unpacked (`tests/end_to_end.rs`).

## Checklist to replace the emulation with a real driver

1. Pick the module and write down model, firmware, regulatory domain and bus (SPI pins, IRQ, reset,
   or SDIO/USB). Nothing below can start before this. (UNKNOWN today.)
2. Decide how frames travel: a Linux `wlan`/S1G netdev with UDP sockets (what the default overhead
   assumes), or a raw-frame or vendor SPI API. This sets `overhead` and the real MTU.
3. Implement `Link` for it in a new backend crate next to `radios/moq-subghz-radio-emu`:
   `send` blocking until the MAC reports success or failure, returning real busy time and attempt
   count, with `Provenance::Measured`. Keep `max_payload` and `max_frames_per_packet` honest.
4. Wire `--link halow` next to `halow-emu` in `moq-subghz-send` and `moq-subghz-recv` (the packing
   and unpacking code is already shared), behind the same `--i-have-an-antenna` and `--transmit` guards.
5. Fix the regulatory side first: channel plan, EIRP and duty cycle for the region. No transmit
   before that.
6. Replace each ASSUMPTION row above by a measurement: preamble (scope or driver stats), SIFS/slot
   (driver), real MTU, ACK behaviour, retry counts, rate control, receive latency, and a loss trace
   from a site test. Re-run the bench with the real link and label the results MEASURED.
7. Re-check the `moq-subghz-radio-emu` tests and the cross-check against published S1G figures.
8. Decide the FEC and packing policy from measured burst loss (with ACK, FEC is mostly idle; in
   broadcast mode packing makes FEC weak).
9. Remove or relabel the emulation wording in the bench report.
