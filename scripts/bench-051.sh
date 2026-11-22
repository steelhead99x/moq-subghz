#!/bin/bash
# The full over-the-air matrix for results/stick-0.5.1/ (catwan-fw 0.5.1 on the stick).
# Part 1: FSK 300 kbit/s live video, Pi -> stick, 120 s each. Part 2: the lora2015 profiles, 3 repeats.
# usage: [PART=fsk|fsk-extra|lora|all] [REPS=3] [FSK_SECS=120] bench-051.sh
# Needs PORT, PI (see env.sh), LKEY (key file here) and PKEY (same key on the Pi). Set PLAY= if ffplay is missing.
set -u; D=$(dirname "$0")
source "$D/env.sh"
: "${PART:=all}" "${REPS:=3}" "${FSK_SECS:=120}" "${LKEY:=$HOME/moq-subghz-key.hex}" "${PKEY:=../moq-subghz-key.hex}"
P300=300000:100000:6
stamp() { echo "$(date +%F\ %T) $*" | tee -a "$RUNS/bench.log"; }
# Keep going after a failed run, but log it and fail the matrix at the end.
fails=0
run() { "$@" || { fails=$((fails + 1)); stamp "FAILED: $*"; }; }
if [ "$PART" = fsk ] || [ "$PART" = all ]; then
  for i in 1 2 3 4; do
    stamp "fsk f300-fec8-p5-$i"; run "$D/run2.sh" f300-fec8-p5-$i $P300 "$FSK_SECS" --fec-k 8
  done
  for i in 1 2; do
    stamp "fsk f300-fec8-p2-$i"; run "$D/run2.sh" f300-fec8-p2-$i $P300 "$FSK_SECS" --fec-k 8 --power 2
  done
fi
if [ "$PART" = fsk-extra ] || [ "$PART" = fsk ] || [ "$PART" = all ]; then
  stamp "fsk f300-nofec-p5-1"; run "$D/run2.sh" f300-nofec-p5-1 $P300 "$FSK_SECS"
  stamp "fsk f300-rind-fec8-p5-1"; RX_KEY="$LKEY" run "$D/run2.sh" f300-rind-fec8-p5-1 $P300 "$FSK_SECS" --fec-k 8 --key-file "$PKEY"
fi
if [ "$PART" = lora ] || [ "$PART" = all ]; then
  for rep in $(seq 1 "$REPS"); do
    for prof in sf7-125 sf7-250 sf7-500 sf8-500; do
      dr=dr3; [ "$prof" = sf8-500 ] && dr=dr4
      for size in small large; do
        if [ $size = small ]; then J=f160x120-q20.jpg; S=f160x112-q20.ssdv; else J=f320x240-q10.jpg; S=f320x240-q10.ssdv; fi
        stamp "lora chunk $prof $size rep$rep";   run "$D/lora2015-run.sh" chunk-$prof-$size-r$rep   chunk   $J $prof 3
        stamp "lora ssdv $prof $size rep$rep";    run "$D/lora2015-run.sh" ssdv-$prof-$size-r$rep    ssdv    $S $prof 3
        stamp "lora lorawan $prof $size rep$rep"; run "$D/lora2015-run.sh" lorawan-$prof-$size-r$rep lorawan $J $prof 3 --dr $dr
      done
    done
  done
fi
if [ "$fails" -gt 0 ]; then stamp "BENCH_FAILED: $fails run(s) failed"; exit 1; fi
stamp BENCH_DONE
