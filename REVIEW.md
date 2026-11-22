# Code review

This review was written on 2026-11-17 against commit `826f1b9`, and sections were updated as
fixes merged. File and line references (`lib.rs:494`) point at the tree as it was reviewed and
have drifted since; the function and type names are the reliable pointers. Sections marked
Fixed describe the code on `main`.

Review of the `video-demo` tree for a public release: `moq-subghz-wire`, `moq-subghz-send`,
`moq-subghz-recv`, `moq-subghz-report`, `vendor/sx1276`, `firmware/catwan-fw`, and `scripts/`.

No radio hardware is attached to this environment. Nothing here was checked on a Pi or a
RP2040-based device, and the 330 s numbers in `results/video/` were not regenerated. Changes that
would alter on-air framing, FEC, pacing, or loss accounting were left in place and listed
below. What was fixed is covered by unit tests (`cargo test --all-targets` on the
workspace, plus the sx1276 driver tests).

Host build, clippy, and tests were run with rustc 1.90.0. `moq-subghz-send` and
`moq-subghz-recv` were also built for `armv7-unknown-linux-musleabihf` (rust-lld, static)
and `armv7-unknown-linux-gnueabihf` (`arm-linux-gnueabihf-gcc`, dynamic). The firmware
was built and clippy-clean for `thumbv6m-none-eabi`. `firmware/catwan-fw-v5.uf2` was
not replaced.

## Critical

None. No path was found that leaks the AEAD key, reuses a nonce on the measured
configuration (15 fps, GOP 15, one run), or panics on a well-formed 255-byte frame.
The items under High are real, but they are either gated by the host tools or they
need a protocol or firmware change that would invalidate the measured runs.

## High

### Firmware will transmit above +5 dBm and on any carrier. Fixed in source; needs a new image.

`firmware/catwan-fw/src/main.rs:232` and `:259` clamp `dbm` to `2..=17` for both the
`L` and `F` commands and program the frequency they are given. There is no check for
902-928 MHz and no refusal of 925.0 MHz. `moq-subghz-send` (`main.rs:273` and `:276`, before it opens the stick) and
`moq-subghz-recv` (`main.rs:313` hard-codes 5 dBm on the `F` command)
enforce the limits only when the host tools are used. A raw serial session can key the
stick at +17 dBm on an arbitrary frequency.

Fixed in source (2026-11-21): `L` and `F` refuse a carrier outside 902-928 MHz or within
0.3 MHz of 925.0 and power outside 2..=5 dBm, and `L` refuses a bandwidth, SF or coding
rate it used to replace silently. The same change makes a `Q` line wait for a queue slot
(one USB read with several short `Q` lines used to drop the later ones) and reports a bad
frame as `ERR tx`. The shipped `catwan-fw-v5.uf2` (measured) and `catwan-fw-0.5.1.uf2`
predate it: until a new image is built, flashed and run on the stick, a stick on those
images still accepts anything.

### Replayed frames. Mostly fixed; long replays deferred.

`Reassembler` (`moq-subghz-wire/src/lib.rs`) tracks the newest object it has placed.
Frames for an object behind it are counted as `radio_frames_stale` and never
reassembled, so a replayed old object 0 no longer reaches `GroupGate` as a new group
and a replayed frame no longer finalizes the object in progress. A frame far ahead is
held until the next frame continues from it, so one injected frame can't stall the
stream. Measured: 17 replayed frames in a 30 s loopback run used to raise
`objects_lost` from 15 to 60; now it stays at 15.

What remains:

* A replay of `RECOVER_RUN` (32) or more consecutive old frames is followed, as the
  way back after two forged frames confirm a wrong jump, so it can deliver old objects
  again. With `--key-file` those still authenticate: `ObjectSealer::open` does not
  remember which `(salt, group, obj)` it has accepted.
* Replayed object 0s from two older runs push the live salt out of the receiver's
  two-entry key cache, so the live run's objects fail as `auth_fail` until its next
  object 0.
* One forged frame just ahead (in its group, or an early object up to 2 groups on)
  costs up to about 32 honest frames, about two GOPs.

Closing these needs authenticated sequence numbers (see the next section).

### Radio headers and FEC are outside the AEAD. By design.

`moq-subghz-wire/src/seal.rs` binds only `group` and `obj` as AAD; the nonce is built
from the run's salt, `group` and `obj`. Sequence number, fragment index, `nfrag`, the
parity bit, `k`, and `objlen` are clear and unauthenticated. What that lets an on-path attacker do,
with `--key-file`:

* Drop, reorder, or retarget fragments, or alter parity: the reassembled object then
  fails `open` and is counted as `auth_fail`, a lost object. That is denial of service,
  which jamming the channel achieves anyway.
* Skew the loss statistics: with two or more consecutive forged frames far ahead it can
  fake a gap (one frame can add at most `SEQ_STEP - 1`; see `moq-subghz-wire/src/lib.rs`).
  The video itself is unaffected.

It cannot read the picture or make altered content authenticate: any change to how
fragments reassemble changes the sealed bytes and fails the tag. It can replay recorded
objects at their original position, which the previous section covers. The headers also
show object sizes, so keyframe timing is visible. Authenticating the headers would cost a tag on
every radio frame (16 bytes, about 6% of airtime, or 4 bytes truncated, about 1.6%) and
a wire change, to protect only statistics. Decided 2026-11-18 (maintainer): keep the
headers clear. Without `--key-file` nothing is authenticated and an attacker can inject
Annex-B.

### `fsk_poll_long` can splice or reject a long packet. Fixed; needs a radio test.

`vendor/sx1276/src/lib.rs:557`. On PayloadReady the driver reads whatever is still
needed in one `read_raw_burst` (`:574`). `read_raw_burst` (`:425`) rejects a read
longer than the 64-byte FIFO, so a long packet that is first observed only when
PayloadReady is already set comes back as an error and can leave `FskLongRx.got`
non-zero. Separately, a CRC failure clears the chip FIFO after the poller has already
copied a prefix; the next packet's bytes continue that prefix. The end-to-end CRC-16
in `FrameHdr::decode` drops the splice. It does not stop the driver from returning it.

Fixed 2026-11-21. More than the 64-byte FSK FIFO still to read at PayloadReady is an error
(the driver used to burst-read up to 255 bytes past the data, since its size check was the
256-byte LoRa FIFO), and every error clears the half-read state. `FskConfig::crc_auto_clear`
(default on, as measured) can be turned off so a CRC-failed packet raises PayloadReady with
CrcOk clear and is dropped whole; the long-mode readers (`catwan-fw` with `long=1`,
`moq-subghz-recv --radio`) turn it off, which closes the splice. This is the receive path
that produced the Pi-side loss numbers, so they need measuring again. `FskLongRx::payload`
no longer panics when no packet is stored.

### Nonce uniqueness is a caller contract. Fixed with per-run subkeys.

`Nonce::assume_unique_for_key` will silently reuse a ChaCha20 or AES-GCM keystream if a
nonce repeats under one key. Each run used to draw a random 32-bit session id as the
nonce prefix, so two runs under one key collided after about 2^16 runs. Now each run
draws a 96-bit salt and encrypts under a subkey derived from the key and the salt
(HKDF-SHA256, `moq-subghz-wire/src/seal.rs`); object 0 of each group carries the salt in
the clear, other objects its first 4 bytes. The sender draws a new salt when the
16-bit group id wraps. Reusing a keystream now needs two equal 96-bit salts. The
receiver caches a salt's subkey only after an object under it authenticates, so a
forged object 0 can't evict the real one (a replayed one can; see "Replayed frames").
Until an object 0 authenticates, a receiver that joins mid-run counts the rest of that
group as `auth_fail`, so that counter mixes "no key yet" with tampering. Frame `VERSION` was 4 for this format and is 5 since the rename changed the key label.

Tag comparison is rind's `ct_equal` (ChaCha20-Poly1305 and AES-GCM `open`). This crate
does not compare tags itself. rind's `Key` drop wipes its copy; `open_sealer`
wipes the hex file bytes and the temporary key after `Key::new`.

## Medium

### A backward sequence number rewinds loss accounting. Deferred.

`moq-subghz-wire/src/lib.rs:494`. A duplicate (`d == 0`) is ignored. A gap with
`d < 0x8000` counts as loss. Any other delta, including a single step backwards, still
updates `last_seq` and is fed to the reassembler. One old or hostile sequence number
then makes the next honest frame look like a large gap. Ignoring backward sequence
numbers would also drop a fragment that arrives late, which can be the fragment that
completes an object. The measured link does not reorder. Not changed.

### The sender's TX queue is unbounded. Deferred.

`moq-subghz-send/src/main.rs` pushes every object onto the queue and only drops stale
groups when the radio thread reaches them. At 180 kbit/s into a 300 kbit/s radio the
queue stays small (the runs recorded `max_queue_frames` around 50). A faster encoder
or a stalled radio grows it without a cap. A cap would drop groups the current code
still sends. Not changed.

### Object id 255 starts a new group that may not be a keyframe. Fixed.

`advance` in `moq-subghz-send/src/main.rs` starts a group only on a keyframe; an encoder
that sends more than 256 frames without one stops the run (`gop_overrun`) instead of
handing the decoder a P-frame as a random access point.

### If the capture queue is empty, the object is stamped `frame_no = u32::MAX`. Deferred.

`moq-subghz-send/src/main.rs:561`. An extra access unit from the encoder is still sent,
with capture time set to "now". It shows up in the logs as a bogus frame number. The
measured runs have `captured == objects`. Not changed.

### A slow player drops access units. Deferred.

`moq-subghz-recv/src/main.rs:135`. `try_send` into a 256-deep channel discards the
access unit when ffplay or the decoder is behind. That is intentional so the radio
loop does not block. It is not counted. Not changed.

### Inconsistent headers are counted as both good and bad. Deferred.

`Reassembler::push` increments `frames_ok` before it notices that `objlen` or `k`
disagrees with the object in progress, then also increments `frames_bad`. Loss rate
uses `frames_missing / (frames_ok + frames_missing)`, so this does not move the
published loss column, but `radio_frames_bad` can include frames that were also
counted as ok. Not changed.

### `fsk_transmit_long` blocks the USB loop. Deferred.

`firmware/catwan-fw/src/main.rs` calls `fsk_transmit_long` with a 1_000_000 spin limit
from inside the main loop, so USB is not polled for the whole long packet. That is
how v5 was measured. The per-packet TX timeout at the `tx_pkt_t` check is the v5 fix
and was not touched.

### Key file mode is checked, then the file is read. Fixed for the check; TOCTOU deferred.

`read_key_file` (`moq-subghz-wire/src/seal.rs`). On Unix the file must be a regular file with no
group/other permission bits. A mode change between `metadata` and `read` is not
closed. Good enough for a pre-shared key on a single-user bench machine.

### Harness word-split the receiver's key path. Fixed.

`scripts/run2.sh` and `scripts/rev.sh` reject a clock offset that is not a number,
quote the sender arguments and remote paths with `printf %q`, and take the receiver's
key as `RX_KEY`, quoted on both sides of ssh, so a key path may contain spaces. `RX`
still adds other receiver arguments and is split by the shell. `run2.sh` and `rev.sh`
were kept separate: they drive opposite directions and the forward path waits 3 s, the
reverse path 4 s. Folding them together would be a new harness, not a cleanup.

## Low

### `fn seal` and `mod seal` shared a name. Fixed.

`moq-subghz-wire/src/lib.rs` had both `pub mod seal` (AEAD) and `pub fn seal` (CRC-16).
The CRC function is now `write_crc`, which says what it does.

### The Pi GPIO glue was copied twice. Fixed.

`moq-subghz-send/src/radio.rs` and `moq-subghz-recv/src/radio.rs` duplicated `CdevIrq` and
`Radio::open`. Both now come from `moq-subghz-pi` (`CdevIrq`, `Hat`, `open_hat`, `sx_err`), moved verbatim:
the same register writes in the same order. Each binary keeps only its own TX or RX
methods, so the TX path is the code it was.

### `localtime_r` failure is ignored. Deferred.

`moq-subghz-send/src/main.rs` `local_clock`. A failure leaves the zeroed `tm`, so the
burned-in clock reads `00:00:00`. The barcode does not use that string. Not changed.

### RP2040 device TX formats each byte with `format!("{:02x}")`. Deferred.

`moq-subghz-send` `Link::send` for `Catwan`. It allocates per byte. Backpressure is the
stick's 12-frame queue, not this formatting. Changing it could change host CPU during
TX. Not changed.

### Report tool trusts the JSON it reads. Partially fixed.

`moq-subghz-report/src/main.rs:92` no longer divides by zero when two clock samples
share a timestamp, and a trailing `--clock-log` without a path is an error (`:107`)
instead of a panic. `pct` no longer panics on NaN (`:45`). A run directory that is
not a directory still hits `file_name()` and prints an empty name rather than
panicking. The tool does not validate that `summary.json` came from this program.

### Window title said "LoRa". Fixed.

`moq-subghz-recv` ffplay title is now `SX1276 FSK live video (moq-subghz)`. The link is
FSK. The README's "best known as the LoRa chip" sentence is about the silicon, and
was left.

## Fixed in this change

| what | where | note |
|---|---|---|
| Reject frames longer than 255 before the CRC copy | `moq-subghz-wire/src/lib.rs:178` | a hostile replay line used to allocate the whole buffer |
| `chunk_size` / `chunk_len` do not divide by zero or underflow | `lib.rs:220` | property test over every legal object length |
| FEC recovery does not `unwrap` a missing chunk | `lib.rs:407` | honest single-erasure repair is unchanged |
| Serial and replay lines capped at 1024 bytes | `lib.rs:48`, recv `main.rs:280` and `:367`, send `main.rs:152` | real `R` lines are about 550 bytes |
| Barcode read/write refuse a tiny plane instead of panicking | `stamp.rs:20`, `stamp.rs:47` | 320x240 is unchanged |
| Annex-B tail above `MAX_OBJECT` is dropped | `annexb.rs:54` | a sendable access unit is still emitted |
| Key file must be mode 0600 (or stricter); hex bytes wiped | `read_key_file`, `open_sealer` in `seal.rs` | both binaries use `open_sealer` |
| Profile parser rejects bitrate 0 and a suffix other than `x` | `lib.rs:95` | `0:...` used to panic in the airtime division |
| Per-run subkeys from a 96-bit salt replace the 32-bit session id | `seal.rs` | no birthday bound at 2^16 runs |
| Empty `FskLongRx::payload` does not panic | `vendor/sx1276/src/lib.rs:169` | |
| Stats line from the stick is JSON-escaped | `moq-subghz-recv/src/main.rs:176` | normal `OK S ...` lines are unchanged |
| `panic = "abort"` on the firmware profiles | `firmware/catwan-fw/Cargo.toml` | thumbv6m cannot unwind; rustc 1.90 would not link without it |

Tests for the wire fixes are in `moq-subghz-wire` (`hostile_frames_do_not_panic`,
`chunk_lengths_sum_to_objlen`, `read_line_limited_caps_and_keeps_short_lines`,
`key_file_mode_and_roundtrip`, and the existing FEC round-trips).

## Crypto notes (what was checked)

* Nonce is 12 bytes, unique per `(group, obj)` under one run's subkey; the subkey comes
  from a random 96-bit salt per run, redrawn on group wrap.
* AAD is the group and object ids. Moving a ciphertext to another position fails open.
  Flipping the clear salt prefix fails open (it picks the subkey and is in the nonce).
* Tag check is rind's constant-time compare, not a `==` in this repo.
* Key material is a file path, not an argument. The tools refuse a group- or
  world-readable file. No key file is in git, and `.gitignore` ignores
  `moq-subghz-key.hex` and `*.key`.
* There is no key exchange and no replay cache. Both are documented in HOWTO and
  ROADMAP.

## Private data and history

No key, token, password, or private-key block was found in the tree or in any of the
16 commits. No LAN address was found. The scan was repeated on 2026-11-20 over every commit
on every branch (115 on `main`) with the same result.

Scrubbed from the working tree:

* `results/video/README.md` from `4d7f226` had a key file path on the receiver host,
  the receiver's account name, its ssh alias, and a local serial device path. The file now
  uses `$PI`, `$PORT`, `$LKEY`, `$PKEY`.
* `scripts/env.sh` from `e358988` defaulted `PORT` to `/dev/cu.usbmodem21301` and
  `PI` to the bench's ssh alias. Defaults are now `/dev/ttyACM0` and `pi`.

Left in measured files on purpose (`results/video/clock_log.jsonl`, 471 lines, and
each run's `clock_end.json`): the bench's ssh alias as `"host"`. Editing those would
change the logs the table was computed from. The alias is not a hostname with a public DNS name,
but it is this bench's ssh alias.

`firmware/catwan-fw/src/main.rs:157` hard-codes USB serial `613863E6354E26A3`. The
same bytes are in `firmware/catwan-fw-v5.uf2`. It looks like one board's id, so
every stick flashed with this image enumerates as that serial. It was not changed,
because the UF2 was not rebuilt.

AppleDouble `._*` files under `results/` were removed. A scan of their printable
strings did not find home paths or names.

Update 2026-11-17: after this review the owner chose a fresh root commit, so the
earlier history (and the strings above that lived only there) is not in this repository.
The license is `MIT OR Apache-2.0`, with both texts at the top level.
