# Licensed and unlicensed mode

**Not legal advice. You are responsible for what you transmit.** This page describes
the `moq-subghz-licence` crate and how the tools use it.

## What is enforced, and where

* `moq-subghz-send`, `moq-subghz-recv`, `css2015-send`, `css2015-log` and the bench crate's
  `factory-send` / `factory-bench` run the gate before they open a real
  radio (an RP2040-based device port or the board's SX1276). Transmitting needs the gate to return a permit, which
  needs `--transmit` as well as `--i-have-an-antenna`. Without `--transmit` the senders never
  transmit; the receivers never transmit at all. The existing frequency (902-928 MHz, not within
  0.3 MHz of 925.0) and power (2 to 5 dBm) checks still run first.
* `moq-subghz-bench` is gated too. Its orchestration commands (`run`, `rev`, `bench`, `bench-051`,
  `css2015-run`, `demo-051`) start the project's own programs, which run the gate; the bench adds
  neither transmit flag for you, and `demo-051` runs the transmit check before it writes to the
  stick or calls the board. `factory-send` and `factory-bench` drive the **stock the HAT maker app**, which
  cannot be queried, so they need `--transmit`, `--i-have-an-antenna` and a declaration of what the
  build on the board transmits (`--freq-mhz`, `--power-dbm`); the gate checks the declaration and runs
  on the board as well as on the laptop, before anything is read or started. **The stock build
  (868.1 MHz, up to +17 dBm) is refused**: it is outside the project's US ISM rules and the gate
  has no 868 MHz profile (the EU region is not enabled, and region `other` is blocked). To bench
  it legitimately, apply `firmware/vendor/sx1276-gps-hat/gps-hat-bench.patch` (915.0 MHz,
  +5 dBm) and declare `--freq-mhz 915.0 --power-dbm 5`. The declaration is yours: the tool cannot
  verify that the app on the board really is the patched build.
* In the radio crates every `open` takes the gate's permit (`TxPermit` to transmit, `RxPermit` to
  receive) and refuses a frequency the permit did not check, so opening a radio without asking the
  gate does not compile. The lower board glue crate (`moq-subghz-pi`) is below that line. A raw serial
  session, `stick-cap` (read only) and the RP2040-based device firmware images are not covered.
* The bench passes every program it starts either `--licence-file PATH` (`--licence-file` for the
  programs here, `--pi-licence-file` for those on the board) or `--no-licence`, which also stops the
  program from looking at `MOQ_SUBGHZ_LICENCE` and the XDG path. With neither option the bench
  says unlicensed explicitly: a licence file left on the board cannot switch a run to licensed mode
  unseen. It prints the mode of each end before running, and passes `--redact-licence` unless you
  give `--show-licence`. `package-051` scans every text file it copies (summaries, gzipped logs,
  CSVs) for the configured ID and name and refuses on a hit; it needs `--licence-file` (or finds
  one) or `--no-licence`. It does not check host paths or the `--pi` host.
* The gate takes the tuned frequency and signal width, not a band the caller picked, and looks
  the band up in its own table. A span that leaves the band, or crosses the 925.0 MHz guard, is
  refused.
* The no-radio paths (`--loopback`, `--replay`, the `-emu` links) drive no radio and need no
  permit. They still load the licence file if you give one, so a bad file is an error.
* `css2015-log` listens only. It runs the same receive gate before opening the stick, with
  `--licence-file`, `--rx-amateur` and `--amateur` (`--transmit` has no effect).
* **Not covered:** anything you type into a raw serial session; and the **shipped RP2040-based device firmware images**.
  `firmware/rp2040-sx1276-fw-0.5.1.uf2` (built from the 0.5.1 source as released) still accepts any carrier and up
  to +17 dBm on its `L` and `F` commands. `firmware/rp2040-sx1276-fw-v5.uf2` was rebuilt from the current source, which
  refuses those, but that image has not been flashed or run, and a stick on the 0.5.1 image ignores all of this. The checks are in
  the host tools only.
* **Amateur transmit is not available.** The scheduler and the identification frame exist, but no
  tool sends station ID frames yet, so a transmit permit that would need them is refused
  (`moq-subghz-send` and `css2015-send` only have ISM profiles anyway). Receiving on the amateur
  profile works in `moq-subghz-recv` (`--amateur`).

## Modes

| | Unlicensed (default) | Licensed |
|---|---|---|
| Meaning | licence-free ISM operation only (for example 915 MHz in the Americas, 868 MHz, 2.4 GHz, 802.11ah sub-GHz Wi-Fi where allowed) | operation under a radio licence (amateur or other licensed bands) |
| Licensed-only bands | **blocked**, including receive-only starts | allowed with a valid licence |
| Amateur profiles | blocked, except **receive-only with `--rx-amateur`** | allowed |
| Needs a licence ID and name | no | **yes, both**, or the tool refuses to start, transmit or enable an amateur profile |
| Station ID on air | never | only on amateur profiles, and only when transmitting is explicitly allowed. Never on ISM profiles. No tool sends them yet (see above) |

A tool is in licensed mode when it finds a licence file, and unlicensed otherwise. A file that is
given (`--licence-file`, `MOQ_SUBGHZ_LICENCE`) but missing, or found but bad, is an error: the
tool never falls back to unlicensed mode on a broken licence file. Only the XDG location may be
absent.

The owner of this repo is not licensed yet, so no licence file ships and licensed mode cannot be
entered without real config. The example file holds fake placeholders that the loader rejects.

## Flags

| flag | meaning |
|---|---|
| `--licence-file PATH` | the licence file (the path only; the values are never taken from the command line) |
| `--no-licence` | run unlicensed and do not look for a licence file anywhere (the bench passes this or `--licence-file` to every program) |
| `--transmit` | allow transmitting. Default off: the tools never transmit. `moq-subghz-recv` has no transmit path and ignores it |
| `--rx-amateur` | receive-only use of an amateur band in unlicensed mode |
| `--redact-licence` | write `[redacted]` instead of the ID and name in logs and reports |
| `--amateur` (`moq-subghz-recv`, `css2015-log`) | use the amateur 33 cm profile on 902-928 MHz |

## Amateur profiles

An amateur profile always means:

* `encryption = "forbidden"`: no ChaCha20-Poly1305 sealing and no AES payload. The gate refuses
  either (see [AMATEUR.md](AMATEUR.md)).
* authentication none until the rule text has been reviewed.
* receive-only unless licensed mode is valid **and** `--transmit` is passed. The default is never
  to transmit.

### Receive-only on an amateur band without a licence (`--rx-amateur`)

In unlicensed mode an amateur profile is blocked unless the tool is started with the explicit
`--rx-amateur` flag (`Flags::rx_amateur` in the gate). The flag permits receiving only. Transmitting
on an amateur band in unlicensed mode stays blocked whatever other flags are passed, the no
encryption and no authentication rules still apply, and the flag does nothing in licensed mode.
Receiving generally needs no licence, but that depends on where you are and what you listen to:
**verify your local rules** before relying on it. This page is not legal advice.

## The licence file

Private, never committed, and read from a file only (never from a command-line value, so it does
not land in shell history or process lists). Looked up in this order:

1. `--licence-file PATH`
2. the `MOQ_SUBGHZ_LICENCE` environment variable, holding a path
3. `$XDG_CONFIG_HOME/moq-subghz/licence.toml` (or `~/.config/moq-subghz/licence.toml`)

Copy [`licence.example.toml`](../licence.example.toml) to one of those places **outside the repo**
and `chmod 600` it. The loader opens the file once and checks that open file: it refuses anything that is not a
regular file (a FIFO, a device), a file owned by another user, a file that group or others can
read, and a file over 4 KiB (the read itself is capped too). It also refuses unknown or duplicate
keys and anything that is not `key = "value"` (a strict subset of TOML, parsed by hand). A symlink
is followed and its target is what gets checked. Operator names with control characters,
bidirectional overrides or invisible formatting characters are refused. Both spellings of the
file name, `licence.toml` and `license.toml`, and variants such as `licence.local.toml` or
`licence.toml.bak`, are in `.gitignore` and caught by the leak guard.

| key | meaning |
|---|---|
| `region` | `"us"`. `"other"` is recognised and rejected (blocked) |
| `licence_id` | your ID (callsign) |
| `operator_name` | your name |

There are no keys for your own ID mask or interval. Any other key is an error.

## What the tools cannot verify

* **They check the shape of the ID only.** For `us` that is a plausible FCC callsign shape (a
  one- or two-letter prefix, a digit, one to three letters). They cannot look the ID up in any
  database, and a plausible ID is not proof that you hold a licence or that it is yours. A
  made-up ID of the right shape passes. Holding a valid licence is your responsibility.
* They do not check licence class, sub-band privileges, power limits for your class, or
  third-party and control-operator rules.
* The `us` rule table is orientation, not a legal reading. Interval: station identification at
  least every 10 minutes and at the end of a communication,
  [47 CFR 97.119](https://www.ecfr.gov/current/title-47/chapter-I/subchapter-D/part-97/subpart-B/section-97.119).
  The scheduler asks for an ID at the start, then a fixed 60 s before the region's limit (every
  540 s for the US 600 s rule), and once more at the end of a run, on the monotonic clock
  (`Instant`), so a wall-clock step cannot delay one. An interval of 60 s or less gives
  `IdPlan::MustNotTransmit`, and a caller must match every `IdPlan` arm.
* **Region `other` is blocked.** Only the US is enabled. A licence file with `region = "other"`
  is rejected, and there is no way to supply your own ID mask or interval. A country is added
  only after its rules have been reviewed with citations (ID shape, identification interval and
  a band table in `band.rs`). The code is structured so adding one is a small, reviewable change.

## Station ID on air

In licensed mode, on an amateur profile only, and only when transmitting is explicitly allowed
(`--transmit`), a permit calls for identification frames: `MQID`, a version byte, a length and the
ID in clear text (`ident.rs`). The **operator name is never sent on air**. Frames are never
sealed. In unlicensed mode, and on ISM profiles in licensed mode, there are none. **No tool sends
them yet**, so the tools refuse such a permit instead of transmitting without the ID.

## Logs, reports and privacy

In licensed mode the ID and name are written into the run's summary as one `licence_id` /
`operator` pair (`"licence"` in the JSON summary of `moq-subghz-send` and `moq-subghz-recv`, a
line with the same pair on `css2015-send`'s stdout). Redaction is opt-in, as the owner
chose: `--redact-licence` replaces both with `[redacted]`. Use it for anything you publish. Every
tool writes the pair through one function (`Station::log_line`), and tests run each tool and scan
its output for the configured ID and name with redaction on. `Debug` output of the config never
prints the values, and error messages never contain them.

A test (`moq-subghz-licence/tests/leak_guard.rs`, run in CI) fails if a tracked file or anything
under `results/` has a callsign-shaped token, a logged ID or name value (the `key=value` form
used in logs and reports) other than a placeholder or `[redacted]`, or a tracked `licence*.toml`. Where a private licence
file is present it also checks that your real ID and name appear nowhere public. It guards against accidents; it
does not replace looking at what you publish.

## For tool authors

```rust,ignore
use moq_subghz_licence::{Auth, Encryption, GateArgs, Profile, Tuning};
// clap: #[command(flatten)] gate: GateArgs   (feature "cli"; default: never transmit)
let station = args.gate.station()?;
let permit = station.check_transmit_without_station_id(
    Tuning::occupied(freq_hz, width_hz), Profile::ISM, Encryption::None, Auth::None)?;
// open the radio with the permit; there is no other way to build one
```

`check_start` is the same checks without the `--transmit` requirement. A `Permit` has private
fields and only the gate builds one. `Permit::id_plan()` returns an `IdPlan` that must be matched
(`NoIdRequired`, `Schedule`, `MustNotTransmit`). Refusals carry no values.
