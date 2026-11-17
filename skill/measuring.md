# Measuring pleamar against Quickshell

How the numbers in [docs/why.md](../docs/why.md) were taken, so they can be taken again the same
way. The point is to compare like with like: same machine, same monitor, both
freshly started, both animating.

## What to measure

```sh
# CPU, memory and threads of a process, over N seconds
medir() {
    pid=$1; sec=${2:-20}; label=${3:-process}
    hz=$(getconf CLK_TCK)
    c0=$(awk '{print $14+$15}' /proc/$pid/stat); t0=$(date +%s.%N)
    sleep "$sec"
    c1=$(awk '{print $14+$15}' /proc/$pid/stat); t1=$(date +%s.%N)
    rss=$(awk '/VmRSS/{print $2/1024}' /proc/$pid/status)
    pss=$(awk '/^Pss:/{s+=$2} END{print s/1024}' /proc/$pid/smaps_rollup)
    echo "$label · cpu $(echo "$c1 $c0 $hz $t1 $t0" | awk '{printf "%.2f", ($1-$2)/$3/($4-$5)*100}') % · rss $rss MB · pss $pss MB"
}
```

PSS is the honest memory figure: shared pages counted once, split between whoever
shares them.

## Frames and the cost of one

pleamar prints its own cycle when it exits, and with `PLEAMAR_TIMING=1` it breaks
down every frame:

```
cycle · 600 frames · mean 0.31 ms · p99 1.16 ms · max 1.63 ms
crono · hueco 16.04 · apuntar 0.10 · cerrar 0.21 · mandar+presentar 0.23 ms
```

"hueco" is waiting for the monitor: that is **sleep**, not CPU. What costs is the
rest, and it is the floor of wgpu on Vulkan: about 0.5 ms per frame whatever is
drawn (limitation P11).

For Quickshell, `QSG_RENDER_TIMING=1` makes Qt print one line per frame with
`polish`, `sync`, `render` and `swap`, plus `perWindowFrameDelta`. Count the
lines to get frames per second, average the numbers to get the cost of a frame.

## Checking an animation against its contract

The other kind of measuring, and the one that gets used daily: not what it
costs, but **whether it lasts what it says**. `--record` prints the value of
properties, facts and live texts once per frame:

```sh
pleamar --scene marea.plm --no-hud --stall 0 --seconds 8 \
        --record calma,dibuja,capsula > log.tsv
```

```
record · there is nothing called 'px' · there is px#Hat2, px#Mug4
ms	calma	dibuja	capsula
1528.3	0.0000	0.0000	0.0000
1545.0	0.1220	0.0000	0.0000
```

- The names are the ones the scene declares. **Inside a component's copy they
  carry their mark** (`px#Hat2`), which is why it says what there is when it
  cannot find one.
- Driving it: `--mouse "360,30@400 click@1500"` for the pointer, `--say` from
  another shell for facts and events. Both are exact in time; the real mouse is
  not.
- **With `--film /tmp/f` it is exact to the frame**: the clock is virtual, the
  logic and the text workshop are waited for each frame, and the same run gives
  the same rows every time, on any machine, faster than real time. Events go in
  the script: `--mouse "emit:open@200"`.
- Reading it: `grep -P '^[\d.]+\t' log.tsv | awk …`. What is usually wanted is
  when something crosses 1 % and 99 % of its travel, because a spring declared
  `~320ms` is one that reaches 99 % at 320 ms.

```sh
grep -P '^[\d.]+\t' log.tsv | awk -F'\t' '
  {t=$1+0; v=$2+0; if(v>0.01 && a==0) a=t; if(a>0 && v>=0.99 && b==0) b=t}
  END{printf "1%%→99%%: %.0f ms\n", b-a}'
```

And two traps worth knowing:

- A one-frame spring (`~16ms`) used **to be able to measure** something adds its
  own lag: at 1 200 px/s that is 33 px. If what is wanted is the drawn value and
  it comes from an expression, register what it is made of and do the sum in
  `awk` instead of adding a property that changes the answer.
- With nothing moving, pleamar **sleeps**: a log with 32 rows in five seconds is
  not a stutter, it is the scene being still. Compare frame counts only between
  two runs that are both animating.

## Restarting someone's bar without losing it

Save its environment and its command line **before** killing it, and put it back
the same way:

```sh
pid=$(pgrep -x quickshell)
tr '\0' ' ' < /proc/$pid/cmdline        # the command
tr '\0' '\n' < /proc/$pid/environ       # its variables, which may matter
```

A shell usually carries variables of its own (`MAREA_ROOT`, `MAREA_AVISOS`…). A
Python script that reads `/proc/<pid>/environ` into a dict and calls `Popen(env=…,
start_new_session=True)` puts it back exactly as it was.

## When it stutters on someone else's machine

`pleamar --report` measures every scene running (or only those named:
`pleamar --report bar`) for 30 seconds (`--seconds N`) while the desktop is used
as usual, and writes a Markdown file (`~/pleamar-report-….md`, or `--out FILE`)
with no personal data in it. It holds:
- the verdict on its first line;
- frame times (median, p95, p99, worst) against the monitor's period;
- the late frames and which part of the round they went to: rules, composing,
  painting, the programs' windows, waiting for the card;
- the card, the CPU's governor, clock and temperature, and what else took the
  CPU meanwhile.

A scene going steadily below the monitor's rate is not stuttering: either its
own `rate:` paces it, which the report names, or the compositor's frame notices
do. Late frames are counted against that pace. pleamar-wm has the same command
for its session, `pleamar-wm report`, and Marea's launcher has `marea report`.

## What to watch out for

- **Quickshell's numbers move** with how long it has been up and what has been
  opened. Freshly started: 5.75 % and 386 MB. After hours: 13.2 % and 188 MB.
  Always compare bars with the same uptime.
- **Opening a panel leaves things behind in QML**: CPU goes up and does not come
  back down. In pleamar it returns, because nothing is instantiated.
- **The two bars rarely hold the same things.** Say so when reporting: a panel
  with a chat inside is not the same panel.
- Measure with the mouse away from both, or the hover animations skew it.
