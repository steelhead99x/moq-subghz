# moq-subghz-bench

The bench harness that produced the tables in `results/`, and the tools that analyze it. One
binary, one subcommand per job. They shell out only to `ssh`, `scp`, `ffmpeg`, `ssdv` and this
project's own binaries.

```sh
cargo build --release          # the tools it starts are built next to it, in target/release
export PORT=/dev/ttyACM0       # or /dev/cu.usbmodem* on macOS. No default: it is opened.
export PI=pi                   # ssh host
export LKEY=$HOME/moq-subghz-key.hex
export PKEY=$LKEY              # path of that same file on the Pi
B=target/release/moq-subghz-bench
$B run --i-have-an-antenna --transmit NAME 300000:100000:6 330 --fec-k 8     # Pi to laptop
$B rev --i-have-an-antenna --transmit NAME 300000:100000:12 330 --fec-k 8     # laptop to Pi
$B bench --i-have-an-antenna --transmit                                       # the 8-run rind off/on matrix
```

Every subcommand that goes over the air needs both `--i-have-an-antenna` and `--transmit`, the
same two flags the senders need (the licence gate in `moq-subghz-send` and `lora2015-send` also
wants a licence or permit, see [docs/LICENSING.md](../docs/LICENSING.md)). The bench never adds
either one for you: it forwards them to the sender only when you pass them, as options of the
subcommand or among the sender arguments after the profile, and `--dry-run` shows them only then.
A real run without them stops with `refusing to transmit without ...` before anything starts.
Sender arguments are passed on untouched, so any other flag the sender requires is yours to give.
`--dry-run` prints the exact commands and runs nothing but the sender's own `--dry-run`. A run that
fails ends `FAILED <run>: <why>` with exit 1; a matrix ends `BENCH_FAILED: <n> run(s) failed`.

The settings that `scripts/env.sh` held are flags with environment fallbacks: `--pi`/`PI`,
`--pi-dir`/`PI_DIR` (default `moq-subghz`), `--bin`/`BIN` (default: the directory of this binary),
`--runs`/`RUNS` (default `runs`), `--port`/`PORT`, `--lkey`/`LKEY`, `--pkey`/`PKEY`. `--no-play`
replaces `PLAY=` when ffplay is not installed.

| subcommand | direction | what |
|---|---|---|
| `run` | Pi -> CatWAN/laptop | clock check, receiver on the laptop, sender on the Pi |
| `rev` | laptop -> CatWAN -> Pi | clock check, receiver on the Pi, sender on the laptop |
| `clocklog` | both | Pi-minus-laptop offset every 5 s until `RUNS/clocklog.stop` |
| `bench` | both | the 8-run rind off/on matrix (300k and 350k, 330 s each) |
| `lora2015-run` | Pi -> stick | one picture run for `results/stick-0.5.1/`: logger here, `lora2015-send` on the Pi, then the exact loss from a replay |
| `bench-051` | Pi -> stick | the whole `results/stick-0.5.1/` matrix (`--part fsk`, `fsk-extra`, `lora` or `all`) |
| `demo-051` | Pi -> stick | the 6 minute guided demo of `results/stick-0.5.1/` |
| `analyze` | none | builds the CSVs and charts of `results/stick-0.5.1/` from the runs (ffmpeg for the SSDV PSNR) |
| `package-051` | none | copies the data worth keeping into `results/stick-0.5.1/`, without host paths |
| `lora2015-table` | none | prints the calculated airtime table of `results/lora-2015/README.md` |
| `lora2015-frames` | none | rebuilds the test pictures of `results/lora-2015/frames/` (ffmpeg, and `ssdv` for the SSDV files) |
| `factory-bench`, `stick-cap`, `factory-send` | Pi -> stock stick | the factory-vs-factory picture test of `results/factory-firmware/`; `factory-send` runs on the Pi, so build this crate for it too |
| `elf2uf2` | none | ELF to UF2 for the RP2040 |

`run` and `rev` stay separate because they drive opposite directions and do not share pacing: the
forward path waits 3 s before the Pi transmits, the reverse path 4 s before the laptop does.

`--rx-key` is the receiver's key file: your `LKEY` for `run` (the receiver is the laptop) and your
`PKEY` for `rev` (the receiver is the Pi). It is quoted on both sides of ssh, so paths and sender
arguments may contain spaces. `--rx` adds other receiver arguments and is split on spaces.

## Input checks

* `--pi`/`PI` must be a host name, `user@host` or an IPv4 or IPv6 literal. A leading `-`, white
  space, control characters and shell characters are refused before anything runs, and every
  `ssh` and `scp` the bench starts has `-o BatchMode=yes --` before the host, so a host can never be
  read as an option and an unattended run never waits for a prompt.
* The Pi's scratch files live in a private directory made with `mktemp -d` (mode 0700) for each
  run, removed at the end, never in a name another user could predict.
* `--label` and run names: letters, digits, `_`, `-` and `.`, at most 64, no `..`, no leading `-` or `.`.
* `factory-bench` refuses a picture that makes more than 999 chunks (the counters have three
  digits), a packet over 255 bytes, or an empty picture, before it transmits.
* `stick-cap` stops with an error if its output files exist (add `--append` to add to them),
  does not follow symlinks, rejects a `--seconds` that is not a number from 0 to 30 days, and opens
  the stick read only.
* Every file the bench reads whole has a size cap (64 MiB for tables, JSON and logs, 16 MiB for a
  picture, 32 MiB for an ELF, 1 GiB streamed for the raw log `package-051` compresses): a bigger
  one is an error naming the file. `analyze` treats `nan`, `inf` and other malformed numbers in a
  table as missing or as an error, never a panic.
* `clocklog --dry-run` prints the one command it repeats and writes nothing.

The published runs used an ssh alias and a macOS `cu.usbmodem*` port. Those values are not
defaults here: set `PI` and `PORT` to your own.

`scripts/loss_sweep.py` is the one script left (see `results/loopback/`).
