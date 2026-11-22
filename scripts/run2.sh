#!/bin/bash
# Pi -> RP2040-based device/laptop.  usage: [RX_KEY=path] [RX="extra recv args"] run2.sh NAME PROFILE SECS [sender args...]
# Set PLAY= (empty) when ffplay is not installed. See scripts/README.md. Sender arguments are quoted for the remote shell.
set -u
N=$1
P=$2
S=$3
shift 3
source "$(dirname "$0")/env.sh"
clock=$("$BIN/moq-subghz-recv" --clock-check "$PI") || { echo "clock check failed" >&2; exit 1; }
OFF=$(printf '%s\n' "$clock" | sed 's/.*remote_minus_local_ms"://; s/}//')
printf '%s\n' "$OFF" | grep -Eq '^-?[0-9]+(\.[0-9]+)?$' || { echo "clock offset is not a number: $OFF" >&2; exit 1; }
echo "$OFF" > "$RUNS/$N.clock_start"
# shellcheck disable=SC2086
"$BIN/moq-subghz-recv" --port "$PORT" --profile "$P" --secs $((S + 10)) --out "$RUNS/$N" ${PLAY---play} --decode --raw-log \
    --clock-offset-ms="$OFF" ${RX_KEY:+--key-file "$RX_KEY"} ${RX:-} > "$RUNS/$N.out" 2>&1 &
RXP=$!
sleep 3
remote_args=''; [ $# -gt 0 ] && remote_args=$(printf '%q ' "$@")
# shellcheck disable=SC2086
# The sender's output goes to a file on the Pi, not through a pipe, so ssh exits with the
# sender's own status.
SO=$(printf '%q' "/tmp/${N}_send.out")
ssh "$PI" "cd ~/$(printf '%q' "$PI_DIR") && ./moq-subghz-send --profile $(printf '%q' "$P") --secs $(printf '%q' "$S") --log $(printf '%q' "/tmp/${N}_send.jsonl") --i-have-an-antenna ${remote_args}> $SO 2>&1; st=\$?; tail -n 1 $SO; rm -f $SO; exit \$st" > "$RUNS/$N.send"
send_st=$?
wait "$RXP"; recv_st=$?
mkdir -p "$RUNS/$N"
ssh "$PI" "cat $(printf '%q' "/tmp/${N}_send.jsonl") && rm -f $(printf '%q' "/tmp/${N}_send.jsonl")" > "$RUNS/$N/send.jsonl"
log_st=$?
tail -1 "$RUNS/$N.send" > "$RUNS/$N/send_summary.json"
"$BIN/moq-subghz-recv" --clock-check "$PI" > "$RUNS/$N.clock_end"
# Everything above is kept for a look; the run only counts when both ends succeeded.
fail() { echo "FAILED $N: $*" >&2; exit 1; }
[ "$send_st" -eq 0 ] || fail "sender exited $send_st (see $RUNS/$N.send)"
[ "$recv_st" -eq 0 ] || fail "receiver exited $recv_st (see $RUNS/$N.out)"
[ "$log_st" -eq 0 ] && [ -s "$RUNS/$N/send.jsonl" ] || fail "no sender log"
grep -q '"role":"send"' "$RUNS/$N/send_summary.json" || fail "no sender summary in $RUNS/$N.send"
grep -q '"role":"recv"' "$RUNS/$N/summary.json" 2>/dev/null || fail "no receiver summary.json"
echo "DONE $N"
