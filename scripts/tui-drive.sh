#!/bin/sh
# Run koolie (or any command) in a detached tmux session and drive it.
#
#   scripts/tui-drive.sh start "<command>"   start; COLS/ROWS set the size (130x36)
#   scripts/tui-drive.sh keys <tmux keys...> send keys (Down, Enter, Escape, C-c, ...)
#   scripts/tui-drive.sh lit "<text>"        type text literally
#   scripts/tui-drive.sh shot                print the current screen
#   scripts/tui-drive.sh stop                kill the session
#
# WAIT sets the pause after start/keys (seconds). The session name is
# KOOLIE_TMUX_SESSION (default koolie-test).
S=${KOOLIE_TMUX_SESSION:-koolie-test}
case "$1" in
  start)
    tmux kill-session -t "$S" 2>/dev/null
    tmux new-session -d -s "$S" -x "${COLS:-130}" -y "${ROWS:-36}" "$2; echo EXITED:\$?; sleep 600"
    sleep "${WAIT:-1.5}"
    ;;
  keys)
    shift
    tmux send-keys -t "$S" "$@"
    sleep "${WAIT:-0.8}"
    ;;
  lit)
    shift
    tmux send-keys -t "$S" -l "$*"
    sleep "${WAIT:-0.8}"
    ;;
  shot)
    tmux capture-pane -t "$S" -p
    ;;
  stop)
    tmux kill-session -t "$S" 2>/dev/null
    ;;
  *)
    sed -n '2,12p' "$0"
    exit 2
    ;;
esac
