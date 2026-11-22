# Loss sweep (no radio)

**These numbers did not come off a radio.** They come from the real sender and receiver, run
through a file, with radio frames removed on purpose. They answer one question the eight radio
runs in [`../video/`](../video/) can't: how does the picture hold up as the link gets worse?

Measured 2026-11-20 with frame format version 4. **That is not the current wire version**: `main`
now speaks version 5 (`VERSION` in `moq-subghz-wire/src/lib.rs`), and the radio runs these are
compared with used version 3. The recordings support frame-size comparisons only. The loss and
latency figures describe the v4 tools of that day; a current loss or latency claim needs a fresh
run of the v5 tools.

## What was run

1. `moq-subghz-send --loopback` recorded 300 s of "air" four times: no FEC, FEC k=4, FEC k=8, and
   FEC k=8 with encryption. Same video settings as the radio runs: 320x240, 15 fps, GOP 15,
   x264 at 180 kbit/s, test pattern. FSK profile `300000:100000:6`, the one the board-to-laptop
   runs used.
2. `scripts/loss_sweep.py` removed radio frames from each recording and played what was left
   through `moq-subghz-recv --replay`. Every case with loss was replayed 20 times with different
   frames removed, and the table shows the mean. [`sweep.csv`](sweep.csv) has the spread
   (`not_shown_sd`).
3. Frames were removed two ways:
   * **Random:** each radio frame is lost on its own with the same chance.
   * **Radio trace:** the loss pattern of a real radio run, taken from its serial log and laid
     over the recording from a random starting point.

## The recordings

| recording | FEC | encrypted | air rate, kbit/s | radio busy | bytes on air per video frame, on top of the video |
|---|---|---|---|---|---|
| k0 | off | no | 190.7 | 72.8% | 97 |
| k4 | 1 parity per 4 | no | 251.9 | 96.2% | 607 |
| k8 | 1 parity per 8 | no | 222.5 | 84.8% | 361 |
| k8enc | 1 parity per 8 | yes | 224.9 | 85.9% | 383 |

A video frame averages 1490 bytes (a keyframe 4217, the rest 1295). A keyframe takes about 18
radio frames and the others about 6, so a one-second GOP is about 100 radio frames.

## Video frames not shown, random loss

| radio frames lost | no FEC | FEC k=4 | FEC k=8 | FEC k=8, encrypted |
|---|---|---|---|---|
| 0% | 0.00% | 0.00% | 0.00% | 0.00% |
| 0.1% | 5.25% | 0.00% | 0.01% | 0.02% |
| 0.2% | 10.74% | 0.03% | 0.08% | 0.15% |
| 0.5% | 24.66% | 0.36% | 0.59% | 0.54% |
| 1% | 42.37% | 1.31% | 1.78% | 2.31% |
| 1.5% | 55.99% | 2.98% | 4.78% | 4.42% |
| 2% | 65.43% | 4.91% | 7.57% | 8.06% |
| 5% | 89.23% | 25.71% | 36.05% | 35.76% |
| 10% | 97.45% | 64.75% | 74.02% | 75.84% |

## Video frames not shown, loss pattern from the radio runs

| loss pattern from run (radio frames lost) | no FEC | FEC k=4 | FEC k=8 | FEC k=8, encrypted |
|---|---|---|---|---|
| r300_off (1.50%) | 46.99% | 8.55% | 9.71% | 9.60% |
| r300_on (1.56%) | 53.33% | 8.55% | 9.34% | 9.61% |
| r350_off (2.56%) | 70.23% | 12.27% | 14.68% | 15.01% |
| r350_on (2.94%) | 75.17% | 15.41% | 18.83% | 18.55% |

## What it shows

* **Dropping the rest of the GOP is expensive without FEC.** Losing one radio frame in a thousand
  costs one video frame in twenty. One lost radio frame takes out, on average, half a second of
  picture.
* **One parity frame per eight fixes most of that for random loss.** At 1% loss, 42% of frames
  not shown becomes 1.8%, for 17% more air time.
* **Real loss is worse than random loss at the same rate.** The two 300 kbit/s radio runs lost
  1.5% of their frames, and about 10% of their loss events were two or more frames in a row
  (7 to 8% in the 350 kbit/s runs, see [`traces.csv`](traces.csv)). Under those patterns about
  9.6% of the picture is lost, where random loss at 1.5% costs 4.8%. The likely reason is that
  one XOR parity frame can't repair two losses in its block. That fits k=4 barely helping,
  since neighbours still land in the same block, but it was not tested directly.
* **Past about 2% loss this scheme is not enough.** A third of the picture is gone at 5%.
* **Encryption does not change the picture.** The encrypted and plain columns differ by less
  than the spread between replays in every row (for example 1.78% and 2.31% at 1% loss, with a
  spread of about 0.7).
* **A freeze is about 0.6 s.** That is the mean with FEC k=8 from 0.5% to 2% loss, and on the
  radio patterns. The worst single freeze at 1% loss was 1.1 s plain and 1.7 s encrypted. The full numbers are in
  [`sweep.csv`](sweep.csv).

## Too much video for the link

A separate pair of 120 s recordings asked the encoder for 300 kbit/s, more than this link carries
with FEC k=8. One used the default rule (drop anything older than 1 s, and the rest of its GOP),
the other switched the rule off with `--max-age-ms 600000`.

| | age limit 1 s | no age limit |
|---|---|---|
| video frames sent on | 1243 of 1804 | 1804 of 1804 |
| GOPs cut short by the sender | 61 | 0 |
| longest queue, radio frames | 249 | 6615 |
| longest queue, as air time | about 1.8 s | about 48 s |

With the rule, the sender gives up 31% of the frames and stays within two seconds of live. Without
it, nothing is dropped and after two minutes the picture is 48 seconds old and still falling
behind. (Queue time is the queue length times 7.2 ms, the air time of one radio frame in these
recordings.)

## How far to trust it

* The replay lands near the radio. Laying the loss pattern of each run over a new recording
  gives 9.7%, 9.6%, 14.7% and 18.6% of frames not shown (the plain or encrypted k=8 column,
  whichever matches the run). The radio runs themselves measured 10.3%, 8.5%, 14.9% and 14.9%.
  Three are within 1.2 points. The last is 3.7 points off. The radio runs used frame format
  version 3, these replays version 4 and the current tools are on version 5; the FEC and the GOP rule are the same in both.
* Two checks that don't use the receiver's own counters, both from
  `scripts/loss_sweep.py check`:
  * Every video frame delivered under loss was compared, byte for byte, with what the sender
    produced, in 120 replays at 2%, 5% and 10% loss. All 276,548 matched.
  * The no-FEC column matches arithmetic. If each radio frame is lost on its own, a video frame
    is shown only when every radio frame before it in its GOP arrived. Worked out from the
    sender's log, that gives 5.66%, 10.94%, 24.79%, 42.52%, 55.38%, 64.86%, 89.25% and 97.51%.
* Only the four runs towards the board (`r*`) are used as loss patterns. The serial logs of the four
  runs towards the laptop don't mark which lines failed their frame check, so the pattern can't
  be rebuilt from them. The script checks each log against the receiver's own count and refuses
  one that disagrees by more than 5%. For three of the four runs used, the lost frames it finds
  are exactly the receiver's count; for `r300_off` it finds 581 against 567.
* The radio runs were on a bench, antennas about a metre apart. Nothing here says what loss looks
  like at range.
* Latency is not measured here. A file has no air time.
* In the encrypted column, `auth_fail` in `sweep.csv` counts frames that arrived before the first
  intact keyframe of the run. They can't be opened until then. None was a damaged frame that got
  through.

## Run it again

```sh
cargo build --release
S=$PWD/target/release/moq-subghz-send
mkdir -p /tmp/rec
(umask 077; head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n' > /tmp/rec/key.hex)
(cd /tmp/rec
 $S --loopback k0.air    --secs 300 --fec-k 0 --log k0.send.jsonl > k0.send_summary.json &
 $S --loopback k4.air    --secs 300 --fec-k 4 > k4.send_summary.json &
 $S --loopback k8.air    --secs 300 --fec-k 8 > k8.send_summary.json &
 $S --loopback k8enc.air --secs 300 --fec-k 8 --key-file key.hex > k8enc.send_summary.json &
 wait)
# the overload pair
(cd /tmp/rec && mkdir -p over && cd over
 $S --loopback a.air --secs 120 --fec-k 8 --bitrate 300000 > overload_age1s.send_summary.json &
 $S --loopback b.air --secs 120 --fec-k 8 --bitrate 300000 --max-age-ms 600000 > overload_noage.send_summary.json &
 wait)
python3 scripts/loss_sweep.py sweep --recv target/release/moq-subghz-recv --rec /tmp/rec \
  --runs results/video/*/ --out /tmp/sweep
python3 scripts/loss_sweep.py check --recv target/release/moq-subghz-recv --rec /tmp/rec
```

A new recording differs a little from the one measured here, so expect the tables to move a
little. On the same recording the script gives the same numbers every time.
