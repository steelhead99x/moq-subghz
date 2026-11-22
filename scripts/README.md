# scripts

Harness for the bench in `results/video/`. These are the scripts that produced the
table; they are not scratch. `run2.sh` and `rev.sh` stay separate because they
drive opposite directions and do not share pacing (the forward path waits 3 s
before the Pi transmits, the reverse path waits 4 s before the laptop transmits).

| script | direction | what |
|---|---|---|
| `env.sh` | both | `PORT`, `PI`, `PI_DIR`, `BIN`, `RUNS`. Sourced, not executed. |
| `run2.sh` | Pi -> RP2040-based device/laptop | clock check, receiver on the laptop, sender on the Pi |
| `rev.sh` | laptop -> RP2040-based device -> Pi | clock check, receiver on the Pi, sender on the laptop |
| `clocklog.sh` | both | Pi-minus-laptop offset every 5 s until `$RUNS/clocklog.stop` |
| `bench.sh` | both | the 8-run rind off/on matrix (300k and 350k, 330 s each) |
| `lora2015-frames.sh` | none | rebuilds the test pictures of `results/lora-2015/frames/` (ffmpeg, and `ssdv` for the SSDV files) |
| `lora2015-table.sh` | none | prints the calculated airtime table of `results/lora-2015/README.md` from `lora2015-send --dry-run` |
| `loss_sweep.py` | none | no radio: replays loopback recordings with radio frames removed, for `results/loopback/` |
| `lora2015-run.sh` | Pi -> stick | one picture run for `results/stick-0.5.1/`: logger here, `lora2015-send` on the Pi, then the exact loss from a replay |
| `bench-051.sh` | Pi -> stick | the whole `results/stick-0.5.1/` matrix (`PART=fsk`, `fsk-extra`, `lora` or `all`) |
| `demo-051.sh` | Pi -> stick | the 6 minute guided demo of `results/stick-0.5.1/` |
| `analyze-051.py`, `package-051.sh` | none | build the CSVs and charts of `results/stick-0.5.1/` from the runs, and copy the data worth keeping |

```sh
export PORT=/dev/ttyACM0          # or /dev/cu.usbmodem* on macOS
export PI=pi                      # ssh host
export LKEY=$HOME/moq-subghz-key.hex
export PKEY=$LKEY                 # path of that same file on the Pi
scripts/run2.sh  NAME 300000:100000:6 330 --fec-k 8
scripts/rev.sh   NAME 300000:100000:12 330 --fec-k 8
scripts/bench.sh
```

`RX_KEY` is the receiver's key file: `RX_KEY="$LKEY"` for `run2.sh` (the
receiver is the laptop) and `RX_KEY="$PKEY"` for `rev.sh` (the receiver is the
Pi). It is quoted on both sides of ssh, so the path may contain spaces, as may
any sender argument. `RX` adds other receiver arguments and is split on spaces.

The published runs used an ssh alias and a macOS `cu.usbmodem*` port. Those
values are not defaults here. Set `PI` and `PORT` to your own.
