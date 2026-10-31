#!/bin/bash
# Live bend A/B: card path (new) vs shared memory (PLEAMAR_BACKDROP_SHM=1), new-old-new.
# Samples the threads of pleamar-wm and of the Vitreus shell, 1 s apart.
# Only the shell is restarted (never the compositor); the keeper is put back at the end.
OUT=/tmp/claude-1000/-home-nick/0a5a4b3f-022e-43ff-8750-6454da9c303f/scratchpad/ab
mkdir -p "$OUT"
P=/home/nick/.config/pleamar/shells/vitreus/vitreus.plm
L=/home/nick/.local/share/pleamar/vitreus/vitreus.log
DATA=/home/nick/.local/share/pleamar/vitreus
SAMPLES=20
WARM=8
SELF_PID=""

shell_pid() { pgrep -f "^pleamar --scene $P( |\$)" | head -1; }
keeper_pid() { pgrep -f "^/bin/sh .*vitreus-keep $P"; }
wm_pid() { pgrep -x pleamar-wm | head -1; }

say() { echo "[$(date +%T)] $*"; }

start_keeper() {
    setsid nohup /home/nick/.local/bin/vitreus-keep "$P" "$L" > /dev/null 2>&1 &
}

restore() {
    say "restoring the shell under its keeper"
    [ -n "$SELF_PID" ] && kill "$SELF_PID" 2> /dev/null
    sleep 1
    if [ -z "$(keeper_pid)" ]; then
        s=$(shell_pid); [ -n "$s" ] && kill "$s" 2> /dev/null; sleep 1
        start_keeper
    fi
}
trap restore EXIT

wait_new_shell() { # $1 = old pid
    for _ in $(seq 1 40); do
        n=$(shell_pid)
        [ -n "$n" ] && [ "$n" != "$1" ] && { echo "$n"; return 0; }
        sleep 0.5
    done
    return 1
}

sample() { # $1 = label
    local wm sh
    wm=$(wm_pid); sh=$(shell_pid)
    say "run $1: sampling wm=$wm shell=$sh for ${SAMPLES}s (DRAG A WINDOW UNDER THE BAR NOW)"
    notify-send -u critical "Live bend test: $1" "Keep dragging a window under the bar for ${SAMPLES} s" 2> /dev/null
    top -H -b -d 1 -n $((SAMPLES + 1)) -p "$wm,$sh" > "$OUT/$1.top"
    grep -E "captures of a glass|stay in shared|would not copy" "$L" | tail -3 > "$OUT/$1.log"
    say "run $1: done"
}

mode_new() { # card path: let the keeper restart the shell
    local old new
    old=$(shell_pid)
    if [ -n "$SELF_PID" ]; then
        kill "$SELF_PID" 2> /dev/null; SELF_PID=""; sleep 1
        start_keeper; sleep 3
        new=$(shell_pid)
    else
        kill "$old"
        new=$(wait_new_shell "$old") || { say "shell did not come back"; exit 1; }
    fi
    say "shell (card path) pid $new, warming ${WARM}s"
    sleep "$WARM"
}

mode_old() { # shared memory: our own copy of the shell with the switch set
    local k s fonts
    for k in $(keeper_pid); do kill "$k" 2> /dev/null; done
    s=$(shell_pid); [ -n "$s" ] && kill "$s"
    sleep 2
    fonts=$(/home/nick/.local/bin/vitreus-fonts "$DATA" 2> /dev/null) || fonts=""
    if [ -n "$fonts" ]; then
        FONTCONFIG_FILE="$fonts" PLEAMAR_BACKDROP_SHM=1 setsid pleamar --scene "$P" --no-hud >> "$L" 2>&1 &
    else
        env -u FONTCONFIG_FILE PLEAMAR_BACKDROP_SHM=1 setsid pleamar --scene "$P" --no-hud >> "$L" 2>&1 &
    fi
    sleep 2
    SELF_PID=$(shell_pid)
    say "shell (shared memory) pid $SELF_PID, warming ${WARM}s"
    sleep "$WARM"
}

notify-send -u critical "Live bend test starts in 15 s" "About 2 minutes. Drag a window around under the bar the whole time. The bar blinks twice." 2> /dev/null
say "starting in 15 s: drag a window under the bar from now until it says done"
sleep 15

mode_new; sample 1-card
mode_old; sample 2-shm
mode_new; sample 3-card
notify-send "Live bend test finished" "You can stop dragging." 2> /dev/null
say "all done"
