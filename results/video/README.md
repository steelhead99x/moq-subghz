# results/video: rind off/on benchmark, 2026-11-17

Eight 330 s runs, 10:43-11:32 PT, back to back, in this order: f300_off, f300_on, r300_off,
r300_on, f350_off, f350_on, r350_off, r350_on. `f` = Pi 2 -> CatWAN/Mac, `r` = Mac -> CatWAN -> Pi 2.

Fixed settings: 915.0 MHz, +5 dBm, 320x240, 15 fps, GOP 15, x264 ultrafast/zerolatency at 180 kbit/s,
FEC k=8, 96 B minimum frame, max object age 1000 ms. CatWAN firmware v5
(`firmware/catwan-fw-v5.uf2`, sha256 fe86cc05...). rind main d2dc35bd796aaadc42df56aee0f1526cdee94b66,
ChaCha20-Poly1305. Antennas ~1 m apart, indoors.

These runs were recorded with frame format version 3. The tools on `main` now speak version 4
(per-run encryption salts) and count loss differently when frames are replayed or injected, so the
current tools will not reproduce this table from a new run byte for byte. A re-measurement is planned.

The results are in [table.md](table.md). The README shows the four forward runs in short form.

## Reading the columns

* **video kbit/s**: H.264 that reached the player, after the receiver dropped broken GOPs.
* **radio goodput**: bytes per second of radio frames received, including headers, padding and parity.
* **radio frame loss**: radio frames missing, counted by sequence number.
* **obj lost**: video frames that FEC couldn't repair.
* **video frame loss**: frames that never reached the player, either lost or dropped with the rest of their GOP.
* **obj lat**: from capture on the sender to the frame being complete on the receiver.
* **g2g**: from capture to the frame being decoded on the receiver, read from the barcode in the
  picture. It includes the one frame (~67 ms) that the ffmpeg CLI decoder holds back.
  Both latencies are corrected for the clock difference between the machines, using a clock check every 5 s.
  These runs used a decoder that dropped the first GOP it was given (`-fflags nobuffer`): seven of
  the eight decoded exactly 15 frames (one GOP) fewer than were delivered, and r300_on 6 fewer, so
  their decoded counts and g2g leave out the first second. Receivers since PR #24 keep every frame,
  which raises g2g p95 by including the slow startup frames; steady-state latency measured the same.
* **stalls**: gaps over 250 ms between delivered frames (obj) and between decoded frames (dec).
* **Pi CPU**: percent of one Cortex-A7 core (the Pi 2 has four). For Pi to laptop runs this is the
  sender plus its ffmpeg. For laptop to Pi it's the receiver, which keeps checking the radio
  because the HAT can't signal that data has arrived, plus the ffmpeg decoder.
* **bytes/obj**: what encryption adds per frame, and everything sent on air beyond the H.264
  itself: object header, encryption, 12 B per radio frame, padding and parity.

## What got it here

Fixes made along the way. All Pi to laptop at 300 kbit/s, 330 s runs.

| change | before | after |
|---|---|---|
| equal-size fragments + 96 B minimum frame (short frames right after long ones were getting lost) | 0.27% radio frame loss | 0.05% |
| end-to-end CRC-16 per radio frame (the receiver could splice two frames into one that passed the radio's CRC) | corrupt AUs reached the decoder | 0 (rejected and counted) |
| AUD appended after each AU | decoder held 2 frames | 1 frame |
| stick firmware v3 to v4 (faster hex output), FEC k=8 | 1.5% video frame loss, 6 GOPs cut | 0.28%, 1 GOP cut |
| stick firmware v3 to v4, no FEC | 6.2% video frame loss, 22 GOPs cut | 4.8%, 22 GOPs cut |
| stick firmware v4 to v5 (per-packet TX timeout), laptop to Pi | stream corrupt after 60 s | 330 s runs as in the table |
| x264 instead of the Pi's hardware encoder | ~275 ms encoder delay | ~20-24 ms |

## Exact commands

Machine-specific paths from the bench machine are written here as variables. Set them to
your own ssh host, serial device, and key file (mode 0600, never commit it):

```sh
export PI=pi                     # ssh host of the Pi
export PORT=/dev/cu.usbmodemXXX  # macOS CatWAN port; Linux is typically /dev/ttyACM0
export LKEY="$HOME/moq-subghz-key.hex"
export PKEY="$LKEY"              # path of that same file on the Pi
( umask 077; openssl rand -hex 32 > "$LKEY" )
scp "$LKEY" "$PI:moq-subghz-key.hex"
```

Pi -> Mac (`moq-subghz-bench run NAME PROFILE 330 ...`), e.g. f300_on:

```sh
# laptop: clock offset, then receiver
moq-subghz-recv --clock-check "$PI"                       # -> remote_minus_local_ms = OFF
moq-subghz-recv --port "$PORT" --profile 300000:100000:6 --secs 340 --out runs/f300_on \
    --play --decode --raw-log --clock-offset-ms=$OFF --key-file "$LKEY"
# Pi (3 s later)
./moq-subghz-send --profile 300000:100000:6 --secs 330 --log /tmp/f300_on_send.jsonl --i-have-an-antenna \
    --fec-k 8 --key-file "$PKEY"
```

Mac -> Pi (`moq-subghz-bench rev`), e.g. r350_off (12 B preamble at 300k, 14 B at 350k):

```sh
moq-subghz-recv --clock-check "$PI"                       # X = Pi minus laptop; the Pi receiver gets -X
ssh "$PI" "cd ~/moq-subghz && ./moq-subghz-recv --radio --profile 350000:100000:14:x --secs 340 \
    --out /tmp/r350_off --decode --raw-log --clock-offset-ms=-$X"
moq-subghz-send --catwan "$PORT" --profile 350000:100000:14:x --secs 330 \
    --log runs/r350_off.send.jsonl --i-have-an-antenna --fec-k 8
```

Forward profiles: `300000:100000:6`, `350000:100000:6:x`. A clock check ran every 5 s throughout
(`moq-subghz-bench clocklog` -> `clock_log.jsonl`).

Table:

```sh
cargo run --release -p moq-subghz-report -- --clock-log clock_log.jsonl \
    f300_off f300_on f350_off f350_on r300_off r300_on r350_off r350_on > table.md
```

## Files per run

| file | what |
|---|---|
| `summary.json` | receiver summary (loss, latency with the start offset, stalls, rind, CPU on the Pi for `r` runs) |
| `send_summary.json` | sender summary (video bitrate, frames, airtime, overhead per object, seal time, CPU on the Pi for `f` runs) |
| `objects.jsonl.gz` | one line per object received: time, group, object, frame, size, FEC repairs, gate decision, latency |
| `decoded.jsonl.gz` | one line per decoded frame: time, barcode capture time, glass-to-glass latency |
| `send.jsonl.gz` | one line per object sent: enqueue time, capture time, encoder delay, frames |
| `serial.log.gz` | every receiver radio line (`R` frames in hex, `X` errors), timestamped |
| `clock_start.txt`, `clock_end.json` | offsets before and after the run |
| `recv_stderr.txt`, `send_stderr.txt` | tool output |

`table.md` recomputes latency with the 5 s clock log and drops the frame the ffmpeg CLI decoder flushes
when its input closes at the end of a run (it shows up ~9 s late in the raw `summary.json` as one
decoded "stall" and the g2g max). Not committed: the `rec.h264` recordings (7 MB each).
