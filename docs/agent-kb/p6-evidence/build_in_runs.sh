#!/bin/bash
# Build the whole semantic index the way the scheduled collections would: one automatic run after
# another (each limited to 50 s of computing, lowest priority), until nothing is left.
# Prints one line per run: wall seconds, peak memory (MB), outcome, vectors done/total.
#
#   build_in_runs.sh <BIN> <HOME of the test> [max runs]
set -u
BIN=$1; H=$2; MAX=${3:-200}
n=0; postponed=0; start=$(date +%s)
while [ $n -lt "$MAX" ]; do
  n=$((n+1))
  out=$(HOME=$H /usr/bin/time -l "$BIN" --kb-embed --auto 2> /tmp/p6-time.txt)
  secs=$(grep -E " real" /tmp/p6-time.txt | awk '{print $1}')
  rss=$(grep "maximum resident" /tmp/p6-time.txt | awk '{printf "%d", $1/1048576}')
  line=$(echo "$out" | python3 -c "import sys,json; d=json.load(sys.stdin); s=d['state']; print(d['outcome'], s['done'], s['total'], 'postponed_rounds', s['postponed_rounds'])")
  echo "run $n: ${secs}s wall, peak ${rss} MB, $line"
  case "$line" in Postponed*) postponed=$((postponed+1)); sleep 30;; esac
  case "$line" in UpToDate*) break;; esac
  d=$(echo "$line" | awk '{print $2" "$3}'); set -- $d
  if [ "$1" = "$2" ] && [ "$2" != "0" ]; then break; fi
done
echo "finished after $n runs ($postponed postponed), $(( $(date +%s) - start )) s of wall time in total"
