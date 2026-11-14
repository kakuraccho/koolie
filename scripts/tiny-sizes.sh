#!/bin/sh
# Drive `koolie --demo` through every view at tiny terminal sizes and report
# how it exited. Every line should read `EXITED:0` with no panic text.
# Build first: cargo build
cd "$(dirname "$0")/.." || exit 1
OUT=$(mktemp -d) || exit 1
S=koolie-tiny
for size in "20 5" "8 3" "2 2" "40 8" "60 12"; do
  set -- $size
  : > "$OUT/rc"; : > "$OUT/err"
  tmux kill-session -t $S 2>/dev/null
  tmux new-session -d -s $S -x "$1" -y "$2" \
    "./target/debug/koolie --demo 2>$OUT/err; echo EXITED:\$? > $OUT/rc; sleep 60"
  sleep 1.5
  for k in Down Enter i x Escape Escape c Tab Escape Escape H a Down Escape Escape u Tab Escape "?" x q; do
    tmux send-keys -t $S "$k" 2>/dev/null; sleep 0.2
  done
  sleep 1
  rc=$(cat "$OUT/rc"); [ -z "$rc" ] && rc=NOT-EXITED
  echo "${1}x${2}: $rc $(head -c 400 "$OUT/err")"
done
tmux kill-session -t $S 2>/dev/null
