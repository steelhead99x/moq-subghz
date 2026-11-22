# Licensed and unlicensed mode

**Not legal advice. You are responsible for what you transmit.** This page describes
the `moq-subghz-licence` crate. It is a library with a gate and tests. **It is not wired into
`moq-subghz-send` or `moq-subghz-recv` yet** (a follow-up change does that), so today nothing in
the tools reads a licence file. **Status: the gate is a library only.** Sections below that say
"the tools" send or refuse describe the intended behaviour once it is wired, not what the binaries
do now. Nothing here transmits, and all amateur transmit stays disabled.

## Modes

| | Unlicensed (default) | Licensed |
|---|---|---|
| Meaning | licence-free ISM operation only (for example 915 MHz in the Americas, 868 MHz, 2.4 GHz, HaLow where allowed) | operation under a radio licence (amateur or other licensed bands) |
| Licensed-only bands | **blocked**, including receive-only starts | allowed with a valid licence |
| Amateur profiles | blocked, except **receive-only with `--rx-amateur`** | allowed |
| Needs a licence ID and name | no | **yes, both**, or the tool refuses to start, transmit or enable an amateur profile |
| Station ID on air | never | only on amateur profiles, and only when transmitting is explicitly allowed. Never on ISM profiles |

The owner of this repo is not licensed yet, so no licence file ships and licensed mode cannot be
entered without real config. The example file holds fake placeholders that the loader rejects.

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
and `chmod 600` it. The loader refuses a file that group or others can read, a file over 4 KiB,
unknown or duplicate keys, and anything that is not `key = "value"` (a strict subset of TOML,
parsed by hand so the crate has no dependencies). `licence.toml` is in `.gitignore`.

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
  The tools send an ID at the start, then a fixed 60 s before the region's limit (every 540 s for
  the US 600 s rule), and once more at the end of a run. An interval of 60 s or less is refused
  (the scheduler returns an error and the tool must not transmit).
* **Region `other` is blocked.** Only the US is enabled. A licence file with `region = "other"`
  is rejected, and there is no way to supply your own ID mask or interval. A country is added
  only after its rules have been reviewed with citations (ID shape, identification interval and
  a band table in `band.rs`). The code is structured so adding one is a small, reviewable change.

## Station ID on air

In licensed mode, on an amateur profile only, and only when transmitting is explicitly allowed
(`--transmit`), the tools send identification frames: `MQID`, a version byte, a length and the ID in clear text
(`ident.rs`). The **operator name is never sent on air**. Frames are never sealed. In unlicensed
mode, and on ISM profiles in licensed mode, there are none.

## Logs, reports and privacy

In licensed mode the ID and name are written into run logs and reports as one `licence_id` / `operator` pair per run.
`--redact-licence` replaces both with `[redacted]` and scrubs them from any text meant for publishing.
Use it for anything you publish. `Debug` output of the config never prints the values, and error
messages never contain them.

A test (`moq-subghz-licence/tests/leak_guard.rs`, run in CI) fails if a tracked file or anything
under `results/` has a callsign-shaped token, a logged ID or name value (the `key=value` form
used in logs and reports) other than a placeholder or `[redacted]`, or a tracked `licence*.toml`. Where a private licence
file is present it also checks that your real ID and name appear nowhere public. It guards against accidents; it
does not replace looking at what you publish.

## For tool authors

```rust
use moq_subghz_licence::{check_transmit, Flags, Mode, Profile};
// band from moq_subghz_licence::band::find(...), flags from the CLI (default: never transmit)
let permit = check_transmit(mode, band, Profile::ISM, &flags)?;
```

`check_start` is the same checks without the `--transmit` requirement. Refusals carry no values.
