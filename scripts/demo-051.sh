#!/bin/bash
# A short guided demo of moq-subghz on the RP2040-based device (firmware 0.5.1): about 6 minutes, all over the air.
#   1. checks the stick and the Pi
#   2. live FSK video for 45 s, then a still and a short clip from what was received
#   3. one small picture sent three 2015 ways (chunked JPEG, SSDV, LoRaWAN-framed) at SF7/500
#   4. the same chunked picture at SF7/125, the speed a 2015 setup would have run
# Needs: built tools (see HOWTO), the Pi set up as in results/stick-0.5.1/README.md, ffmpeg, python3, antennas on both radios.
# usage: PORT=/dev/ttyACM0 PI=pi scripts/demo-051.sh [OUTDIR]
set -u; D=$(dirname "$0"); source "$D/env.sh"
OUTD=${1:-$RUNS/demo}; RUNS=$OUTD; export RUNS PLAY=${PLAY-}; mkdir -p "$OUTD"
say() { printf '\n== %s\n' "$*"; }
say "1. preflight"
python3 - "$PORT" <<'PY' || exit 1
import os, sys, termios, time, select
fd = os.open(sys.argv[1], os.O_RDWR | os.O_NOCTTY | os.O_NONBLOCK)
t = termios.tcgetattr(fd); t[0] = t[1] = t[3] = 0; t[2] = termios.CS8 | termios.CREAD | termios.CLOCAL; t[4] = t[5] = termios.B115200
t[6][termios.VMIN] = 0; t[6][termios.VTIME] = 0; termios.tcsetattr(fd, termios.TCSANOW, t)
os.write(fd, b'I\n'); end = time.time() + 1.5; out = b''
while time.time() < end:
    if select.select([fd], [], [], 0.1)[0]: out += os.read(fd, 4096)
line = [l for l in out.decode(errors='replace').splitlines() if l.startswith('I catwan-fw')]
print(line[-1] if line else 'no firmware banner: check the USB serial port and image'); sys.exit(0 if line and 'sx_version=0x12' in line[-1] else 1)
PY
ssh "$PI" "test -x ~/$PI_DIR/moq-subghz-send && test -x ~/lora2015/lora2015-send && echo pi: senders present" || exit 1
say "2. live FSK video, 45 s, FEC k=8"
"$D/run2.sh" fsk-live 300000:100000:6 45 --fec-k 8 > /dev/null || exit 1
python3 - "$OUTD/fsk-live/summary.json" <<'PY'
import json, sys
s = json.load(open(sys.argv[1]))
print(f"radio frame loss {100*s['radio_frame_loss']:.2f}%  video frame loss {100*(s['video_frame_loss'] or 0):.1f}%  glass to glass p50/p95 {s['g2g_lat_ms']['p50']}/{s['g2g_lat_ms']['p95']} ms  video {s['delivered_video_bps']/1000:.0f} kbit/s  RSSI {s['rssi_mean']} dBm")
PY
ffmpeg -v error -y -framerate 15 -i "$OUTD/fsk-live/rec.h264" -c copy "$OUTD/fsk-live.mp4"
ffmpeg -v error -y -ss 20 -i "$OUTD/fsk-live.mp4" -frames:v 1 "$OUTD/fsk-live-still.png"
say "3. one small picture, three 2015 ways, SF7/500, 2 passes"
for mode in chunk ssdv lorawan; do
  f=f160x120-q20.jpg; [ $mode = ssdv ] && f=f160x112-q20.ssdv
  "$D/lora2015-run.sh" demo-$mode-sf7-500 $mode $f sf7-500 2 > /dev/null || exit 1
  echo "$mode: $(sed -n 2p "$OUTD/demo-$mode-sf7-500/exact/images.csv" | cut -d, -f2,7,10,17,18)   (complete, seconds, loss %, RSSI, SNR)"
done
say "4. chunked picture at SF7/125, 2 passes"
"$D/lora2015-run.sh" demo-chunk-sf7-125 chunk f160x120-q20.jpg sf7-125 2 > /dev/null || exit 1
echo "chunk SF7/125: $(sed -n 2p "$OUTD/demo-chunk-sf7-125/exact/images.csv" | cut -d, -f2,7,10,17,18)"
say "done: $OUTD (fsk-live.mp4, fsk-live-still.png, demo-*/image*.jpg)"
