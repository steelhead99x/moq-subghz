# Radio rules per band

**Not legal advice. You are responsible for what you transmit.** Rules change and differ by country.
Check your regulator's current text. This page is orientation for the project's tools.

## What the tools enforce today

* They **never transmit by default**, and need `--i-have-an-antenna`.
* 915.0 MHz and +5 dBm (about 3 mW) by default. Refused: anything outside 902-928 MHz, anything within
  0.3 MHz of 925.0 MHz, and sender power outside 2 to 5 dBm.
* The checks are in the host tools, not in the shipped stick firmware. See [HOWTO section 9](../HOWTO.md#9-radio-rules).
* Nothing enforces rules for any other band, because no code uses another band yet.

## Sub-GHz ISM (SX1276 today, HaLow and SX1262 planned)

* Varies by region. 915 MHz is licence-free in the Americas, **not in Europe and many other places**.
  Europe uses 868 MHz with duty-cycle limits this video link breaks.
* Even in the US this setup may not comply: one fixed channel transmitting up to about 80% of the time
  is allowed under FCC 15.247 only if the signal is at least 500 kHz wide at the -6 dB points, and that
  was not measured. Short indoor bench tests at 3 mW only.
* Rule texts: [FCC 47 CFR Part 15](https://www.ecfr.gov/current/title-47/chapter-I/subchapter-A/part-15);
  Europe: ETSI EN 300 220 for short range devices (see [etsi.org](https://www.etsi.org/standards)).
* HaLow channels, bandwidth and power also differ per country. Never override a module's regulatory table.

## 2.4 GHz and 5.8 GHz (no code yet)

* Both are licence-free ISM or unlicensed bands in many regions, **with limits** on power, antenna
  gain and modulation that vary by country. FCC 15.247 and Subpart E are in the Part 15 link above;
  Europe: ETSI EN 300 328 (2.4 GHz) and EN 301 893 (5 GHz).
* **5 GHz**: some channels require radar avoidance (DFS) and transmit power control, and available
  channels differ by region. Some 5.8 GHz video gear needs a licence or is restricted in some countries.
* Both bands are crowded, so benchmark results will vary with neighbours.
* A planned profile for either band must carry the region, channel plan and power, and refuse blank values.

## Amateur radio bands

Different rules again: a licence, a callsign, and generally no encryption. See [AMATEUR.md](AMATEUR.md).
The project is receive-only on amateur bands and no amateur transmit path is enabled.

## Planned enforcement

The plan moves these rules from docs into code: region files with band, power and duty limits, a
duty-cycle accountant, and a profile that must be reviewed before any real transmit. Planned, not built.
