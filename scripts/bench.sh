#!/bin/bash
# The matched rind off/on matrix from README: both directions, 300k and 350k(x), FEC k=8, 330 s each.
# LKEY = key file on the laptop, PKEY = the same key on the Pi (see "Encryption" in HOWTO section 8).
set -u; D=$(dirname "$0")
: "${LKEY:=$HOME/moq-subghz-key.hex}" "${PKEY:=moq-subghz-key.hex}" "${S:=330}"
source "$D/env.sh"
"$D/clocklog.sh" & CL=$!
trap 'touch "$RUNS/clocklog.stop"; wait $CL' EXIT
fails=0
run() { "$@" || fails=$((fails + 1)); }
for br in 300 350; do
  if [ $br = 300 ]; then FP=300000:100000:6; RP=300000:100000:12; else FP=350000:100000:6:x; RP=350000:100000:14:x; fi
  RX_KEY=""      run "$D/run2.sh" f${br}_off "$FP" "$S" --fec-k 8
  RX_KEY="$LKEY" run "$D/run2.sh" f${br}_on  "$FP" "$S" --fec-k 8 --key-file "$PKEY"
  RX_KEY=""      run "$D/rev.sh"  r${br}_off "$RP" "$S" --fec-k 8
  RX_KEY="$PKEY" run "$D/rev.sh"  r${br}_on  "$RP" "$S" --fec-k 8 --key-file "$LKEY"
done
[ "$fails" -eq 0 ] || { echo "BENCH_FAILED: $fails run(s) failed" >&2; exit 1; }
echo BENCH_DONE
