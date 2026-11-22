# Amateur radio and amateur satellite bridge (PLANNED, NOT STARTED)

**Status: planned. Nothing here is implemented. No code in this repo transmits or receives on an amateur
band, and none will be enabled until the licence and callsign conditions below are met.**

## The idea

Bridge MoQ, which lives on the internet side as objects and relays, with the amateur radio world on the
RF side. Things that exist in amateur radio today, and are candidates to carry or to receive:

* analog FM voice, and SSTV (slow-scan TV) images, for example from the ISS programme
  ([ARISS](https://www.ariss.org))
* AFSK and AX.25 packet radio, and APRS digipeaters ([aprs.org](http://www.aprs.org),
  [AX.25 overview](https://en.wikipedia.org/wiki/AX.25))
* FM and linear transponders on amateur satellites ([AMSAT](https://www.amsat.org),
  [ARRL satellites](https://www.arrl.org/satellites))
* digital voice with open codecs: [Codec2](https://github.com/drowe67/codec2) and [FreeDV](https://github.com/drowe67/freedv-gui)
* chirp spread spectrum (CSS) mode or FSK experiments on the 70 cm and 33 cm amateur bands (which bands, and where, depends on your
  country and ITU region; see your national band plan and the [IARU](https://www.iaru.org))

Which satellites are active, and on which frequencies and modes, changes. This page names none; use the
satellites' own documentation and the AMSAT pages above.

## Policy (decided by the owner)

1. **Receive side first, SSTV first.** Then telemetry and APRS, then voice and Codec2, all under one
   object model.
2. **Receive-only and recording and replay** for amateur bands. The owner is not licensed yet. **No
   amateur-band transmit code path is enabled** until a licence and a callsign are set in a private
   config that is never committed.
3. Receiving amateur signals generally needs no licence. **Verify your local rules.**
4. Encryption is off. See the next section.

## Hard rules

* **Amateur bands generally prohibit encryption and messages with obscured meaning.** In the US this is
  [47 CFR 97.113](https://www.ecfr.gov/current/title-47/chapter-I/subchapter-D/part-97/subpart-A/section-97.113);
  many other countries have similar rules. Check yours.
* A **licensed operator** is required, and **station identification (a callsign)** must be sent as the rules
  require.
* **Third-party and content rules** apply: what may be relayed for others, and what content is allowed,
  differs. An internet-to-RF path (a viewer on the internet causing an amateur transmission) raises these questions
  and needs a control operator. RF-to-internet republishing of what you receive is a separate question: verify it too.
* **Satellite uplinks** have coordination and frequency rules from the satellite's operators and IARU
  coordination. Follow the satellite's own documentation.
* Digital methods generally must be publicly documented. The moq-subghz frame format is open source, but
  confirm what your rules require.

## What this means for the code (planned)

* The current **ChaCha20-Poly1305 sealing must be off** on any amateur-band profile. That includes the
  AES payload in the uplink-style frames of `css2015`.
* The planned region and profile system must enforce it, for example a profile flag
  `encryption = "forbidden"`, a required callsign, and **no transmit without a configured callsign**.
* Objects are **signed (authentication only)** or sent in the clear with callsign ID. Whether even a
  signature is acceptable is rule-dependent, so the default for an amateur profile is none, until reviewed.
* The tools must **refuse to transmit** otherwise. Nothing of this exists yet.

## Bridge directions (plausible, all planned)

```
 receive first:    SDR or radio  ──▶  gateway  ──▶  MoQ objects  ──▶  relay  ──▶  viewers
                   (a pass: SSTV image, telemetry, audio)

 later, licensed:  MoQ objects  ──▶  gateway  ──▶  amateur-band uplink   (text or telemetry; a control operator,
                                                                          callsign ID and no encryption required)
```

* **Receive path (first):** a gateway receives a pass through an SDR or radio, decodes SSTV, telemetry
  or audio, and republishes the result as MoQ objects (one image or one packet per object). No
  transmitter is involved, so it fits the "never transmit by default" rule. Recording and replay of a
  pass makes it testable without hardware, like the existing `--loopback` and `--replay`.
* **Transmit path (later, only after licensing):** MoQ to a ham-band uplink for text or telemetry.

## Open questions

* SDR or radio hardware for the receive side (nothing chosen).
* Which SSTV modes to decode, and whether to write a decoder or wrap an existing one (licences matter).
* How a "pass" maps to groups: probably one group per image or per packet burst.
* What the object model looks like for audio and Codec2.
* Rules for republishing received content on the internet in your country.
