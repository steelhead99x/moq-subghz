# Related work

Research outside this project that works on the same problems: MoQ delivery over links that degrade,
and how such links are modeled. Labels follow [LINKS.md](LINKS.md). Numbers here are **Reported** by the
authors, not measured by this project. Nothing below is implemented in this repository yet.

## MultiMoQ: multi-access Media over QUIC (2026)

Yitong Li, Ruonan Chai, Xinjiao Li, Dirk Kutscher (The Hong Kong University of Science and Technology,
Guangzhou). *MultiMoQ: Multi-Access Media-Over-QUIC for Robust Immersive Video Streaming.* arXiv:2608.06102,
August 2026. <https://arxiv.org/pdf/2608.06102>

**What it is.** A redesign of MoQ-Lite group delivery for tiled 360° video when an access link gets
worse, together with client-driven switching to a second access link. On the sender, a pending queue is
kept apart from one active group per track. The next group is chosen by a score that weighs how stale a
group is against the gaps in its sequence, and a watchdog frees an active group that has stalled. On the
client, per-track latency is compared with a baseline. If it stays worse, a second access is warmed up,
the switch happens only at a keyframe the decoder can start from (make before break), and a failed
attempt rolls back.

**How it was evaluated (Reported).** Mininet emulation only, no radio: 96 clients in a four-layer
topology. Primary access links follow bandwidth and delay distributions measured on a campus network,
with fixed secondary and backhaul links. Adaptive bitrate was turned off to isolate the transport.
Against DASH and standard MoQ, the authors report higher goodput in the bad condition, stall time cut
from roughly 14 to 29% (standard MoQ) to roughly 0.1 to 1.3%, and a lower freeze ratio in viewport tests.
The authors note that this is one topology, one 4K source and fixed 1 s media units.

**How it relates to moq-subghz.** The two projects share the MoQ object model (tracks, groups, objects)
and the question of what to send when the link cannot carry everything. The settings are far apart:
MultiMoQ works at Mbit/s on emulated wired-style access links, while this project works at tens to
hundreds of kbit/s over one half-duplex sub-GHz radio with XOR parity and no retransmission. Its results
do not transfer as numbers. What can transfer is a set of mechanisms worth trying here:

### Shaping and scheduling: candidate improvements

* **Group choice by completeness as well as staleness.** The sender here already drops stale groups,
  oldest first, as a MoQ relay does (`moq-subghz-send`). MultiMoQ also weighs how complete a group still
  is. On a link that loses frames in bursts, preferring a group whose XOR parity can still recover it
  over one that cannot could cut the frames that are sent but never shown. Evaluate first on the
  no-radio loopback ([results/loopback](../results/loopback/README.md)) and the emulated links
  ([S1G-EMULATION.md](S1G-EMULATION.md)), then on the radios.
* **A watchdog for a stalled group.** Release a group that has stopped making progress instead of
  letting it hold the airtime: the same idea, applied to the radio send queue.
* **Make-before-break switching at a keyframe.** For the planned multi-link `Link` architecture
  ([LINKS.md](LINKS.md#architecture-planned)), for example FSK alongside 802.11ah: warm up the second
  link, move over only at a keyframe, and roll back if the move fails. The rule of switching at a
  keyframe matches this project's existing rule that a loss smears the picture until the next keyframe
  ([DETAILS.md](DETAILS.md)).
* **A latency trigger with a baseline.** Decide to switch or shed from latency that stays above a
  per-track baseline, not from one bad sample. The receiver already records per-frame timing to start from.

### Channel modeling: candidate improvements

* **Fit link models to measured distributions.** MultiMoQ draws link bandwidth and delay from
  distributions measured on a real network. This project's Gilbert-Elliott burst loss parameters are
  illustrative and not fitted to a site ([S1G-EMULATION.md](S1G-EMULATION.md)). Fitting them to the
  per-frame logs of the measured FSK and CSS runs (`results/video`, `results/stick-0.5.1`) would make the
  emulated benchmarks closer to the radios.
* **Report a stall ratio and a freeze ratio.** `moq-subghz-report` already counts stalls over 250 ms.
  MultiMoQ reports the share of time stalled and a freeze ratio next to goodput. Adding those two ratios
  would make comparisons with MoQ work at other rates easier to read.
