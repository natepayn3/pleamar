#!/bin/bash
# usage: runbin2.sh <binary> <out> <seconds> [ENV=val ...]
BIN=$1; OUT=$2; SECS=$3; shift 3
P=/home/nick/.config/pleamar/shells/vitreus/vitreus.plm
L=/home/nick/.local/share/pleamar/vitreus/vitreus.log
DATA=/home/nick/.local/share/pleamar/vitreus
pkill -f "^/bin/sh .*vitreus-keep $P"; sleep 0.5
pkill -f "^(pleamar|/home/nick/work/pleamar-visualizer/target/release/pleamar) --scene $P"; sleep 2
pgrep -f "^(pleamar|/home/nick/work/pleamar-visualizer/target/release/pleamar) --scene $P" && { echo "a shell is still running"; exit 1; }
fonts=$(/home/nick/.local/bin/vitreus-fonts "$DATA" 2>/dev/null)
( export PLEAMAR_TIMING=1; [ -n "$fonts" ] && export FONTCONFIG_FILE="$fonts"; for e in "$@"; do export "$e"; done
  setsid "$BIN" --scene "$P" --no-hud > "$OUT" 2>&1 & )
sleep "$SECS"
pkill -f "^(pleamar|/home/nick/work/pleamar-visualizer/target/release/pleamar) --scene $P"; sleep 1
setsid nohup /home/nick/.local/bin/vitreus-keep "$P" "$L" >/dev/null 2>&1 &
sleep 4
