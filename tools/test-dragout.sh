#!/bin/bash
# Tests dragging items out of a Slerp window on Wayland, with no person and
# without touching the real desktop.  A nested compositor runs in a private
# runtime folder: a headless sway, or a virtual KWin (the compositor of
# Plasma, where Dolphin lives).  The dragout demo window and a drop target
# (dragout_testbed receiver) sit side by side in it, and a fake pointer
# (zwlr_virtual_pointer_v1 in sway, org_kde_kwin_fake_input in KWin, both
# existing only inside the nested compositor) presses on a row of the demo,
# drags it over the drop target and lets go.
#
#   tools/test-dragout.sh                     # sway, every scenario
#   tools/test-dragout.sh kwin                # KWin, every scenario
#   tools/test-dragout.sh sway drop           # one scenario: drop, hover, many, cancel, reject, icon, viewer
#   tools/test-dragout.sh kwin dolphin DIR    # drag a row into a real Dolphin window
#                                             # showing DIR (KWin only)
#
# Windows render with lavapipe (software Vulkan), since the nested
# compositors have no GPU buffers to share.  The whole run happens inside one
# heavy-slerp.slice scope like every other heavy Slerp command, and what it
# starts is stopped on exit.  The scenarios' logs stay in /tmp/dragout-test/.
set -euo pipefail
cd "$(dirname "$0")/.."

ROOT=$PWD
RT=/tmp/dragout-test
LVP=/usr/share/vulkan/icd.d/lvp_icd.json
TARGET=${CARGO_TARGET_DIR:-$ROOT/local/target}
BIN=$TARGET/${PROFILE:-debug}/examples
compositor=${1:-sway}
only=${2:-all}
case $compositor in sway|kwin) ;; *) echo "usage: $0 [sway|kwin] [drop|hover|cancel|reject|dolphin DIR]" >&2; exit 2;; esac

fail() { echo "FAIL: $*" >&2; exit 1; }

# 1. Build both examples (debug builds are enough: the code under test is
#    the same, and the dev profile optimises dependencies), then run the rest
#    in one scope so that everything started below can be stopped together.
if [ -z "${DRAGOUT_TEST_SCOPED:-}" ]; then
  if [ "${PROFILE:-debug}" = release ]; then flag=--release; else flag=; fi
  # DRAGOUT_NO_BUILD=1 reuses the examples already built.
  [ -n "${DRAGOUT_NO_BUILD:-}" ] || tools/cargo-capped build -p slerp-viewer --example dragout_demo --example dragout_testbed $flag
  DRAGOUT_TEST_SCOPED=1 exec systemd-run --user --scope --quiet --slice=heavy-slerp.slice "$0" "$@"
fi

pids=()
# Stops a job and what it started (dbus-run-session starts KWin).
kill_tree() {
  local child
  for child in $(pgrep -P "$1" 2>/dev/null || true); do kill_tree "$child"; done
  kill "$1" 2>/dev/null || true
}
# Everything started below is a descendant of this script, so stopping the
# descendants (and no process by name or by cgroup, which could be someone
# else's) is enough: the D-Bus services of the nested session are children of
# its bus, which is a child of dbus-run-session.
cleanup() {
  for p in "${pids[@]:-}"; do kill_tree "$p"; done
  # D-Bus services of the nested session (portals, the accessibility bus)
  # detach from their parent; they are found by the private runtime folder in
  # their environment, which no process of the real session has.
  local proc
  for proc in /proc/[0-9]*; do
    if grep -zqx "XDG_RUNTIME_DIR=$RT" "$proc/environ" 2>/dev/null; then
      kill "${proc#/proc/}" 2>/dev/null || true
    fi
  done
  sleep 0.5
  for p in "${pids[@]:-}"; do kill -9 "$p" 2>/dev/null || true; done
}
trap cleanup EXIT

# 2. The nested compositor, in its own runtime folder so nothing it starts can
#    find the real session's sockets, and with its own D-Bus session.
rm -rf "$RT"
mkdir -p "$RT"
chmod 700 "$RT"
# What every program of the test runs with.
run() {
  env -u DISPLAY XDG_RUNTIME_DIR=$RT WAYLAND_DISPLAY=$SOCKET VK_DRIVER_FILES=$LVP \
    DBUS_SESSION_BUS_ADDRESS="$(cat "$RT/bus" 2>/dev/null || echo disabled:)" "$@"
}
# The same in the background, with the program itself (not a shell around
# it) as the job, so that killing the job stops it.  Output goes to $LOG.
spawn() {
  local log=$1
  shift
  (exec > "$log" 2>&1; exec env -u DISPLAY XDG_RUNTIME_DIR=$RT WAYLAND_DISPLAY=$SOCKET VK_DRIVER_FILES=$LVP \
    DBUS_SESSION_BUS_ADDRESS="$(cat "$RT/bus" 2>/dev/null || echo disabled:)" "$@") &
  pids+=($!)
}

case $compositor in
sway)
  SOCKET=wayland-1
  cat > "$RT/sway.conf" <<'EOF'
default_border none
default_floating_border none
output HEADLESS-1 resolution 1280x720
for_window [app_id="slerp-dragout-demo"] floating enable, resize set 460 400, move position 40 40
for_window [app_id="dragout-receiver"] floating enable, resize set 400 300, move position 700 40
for_window [app_id="org.kde.dolphin"] floating enable, resize set 400 300, move position 700 40
for_window [app_id="^slerp$"] floating enable, resize set 680 640, move position 10 10
EOF
  env -u DISPLAY -u WAYLAND_DISPLAY XDG_RUNTIME_DIR=$RT WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 \
    WLR_RENDERER=pixman sway -c "$RT/sway.conf" > "$RT/compositor.log" 2>&1 &
  pids+=($!)
  ;;
kwin)
  SOCKET=kw
  cat > "$RT/place.js" <<'EOF'
function place(w) {
  if (w.resourceClass == "slerp-dragout-demo") {
    w.noBorder = true;
    w.frameGeometry = {x: 40, y: 40, width: 460, height: 400};
  } else if (w.resourceClass == "slerp") {
    w.noBorder = true;
    w.frameGeometry = {x: 10, y: 10, width: 680, height: 640};
  } else if (w.resourceClass == "dragout-receiver" || w.resourceClass == "org.kde.dolphin") {
    w.noBorder = true;
    w.frameGeometry = {x: 700, y: 40, width: 400, height: 300};
  }
}
workspace.windowList().forEach(place);
workspace.windowAdded.connect(place);
EOF
  env -u DISPLAY -u WAYLAND_DISPLAY XDG_RUNTIME_DIR=$RT KWIN_WAYLAND_NO_PERMISSION_CHECKS=1 \
    QT_QPA_PLATFORM=offscreen KWIN_COMPOSE=Q \
    dbus-run-session -- bash -c "echo \$DBUS_SESSION_BUS_ADDRESS > $RT/bus
      exec kwin_wayland --virtual --socket kw --width 1280 --height 720 --no-lockscreen --no-global-shortcuts" \
    > "$RT/compositor.log" 2>&1 &
  pids+=($!)
  ;;
esac
for _ in $(seq 100); do [ -S "$RT/$SOCKET" ] && break; sleep 0.1; done
[ -S "$RT/$SOCKET" ] || fail "$compositor did not start (see $RT/compositor.log)"

if [ "$compositor" = kwin ]; then
  # Windows are placed by a KWin script, since Wayland clients cannot choose
  # where they appear.
  sleep 2
  id=$(run busctl --user call org.kde.KWin /Scripting org.kde.kwin.Scripting loadScript ss "$RT/place.js" place | awk '{print $2}')
  run busctl --user call org.kde.KWin "/Scripting/Script$id" org.kde.kwin.Script run > /dev/null
fi

# Waits until a log file has a line matching a pattern.
wait_for() {
  local file=$1 pattern=$2 secs=${3:-30}
  for _ in $(seq $((secs * 10))); do
    grep -q -- "$pattern" "$file" 2>/dev/null && return 0
    sleep 0.1
  done
  fail "no '$pattern' in $file after $secs s"
}

# Where the demo's rows are, in output coordinates: the window sits at 40,40,
# its first row starts 50 down, and the rows are 56 high with 8 between them.
row_x=270
row_y() { echo $((40 + 50 + 28 + $1 * 64)); }

start_apps() {  # receiver options...
  rm -f "$RT"/demo.log "$RT"/recv.log
  spawn "$RT/recv.log" "$BIN/dragout_testbed" receiver "$@"
  wait_for "$RT/recv.log" "receiver ready"
  spawn "$RT/demo.log" ${DEMO_ENV:-env} "$BIN/dragout_demo"
  wait_for "$RT/demo.log" "first row at"
  sleep 1.5
}

stop_apps() {
  for p in "${pids[@]:1}"; do kill_tree "$p"; done
  pids=("${pids[0]}")
  sleep 0.5
}

# A fake pointer is a device: while it exists the seat has a pointer.
drive() { run "$BIN/dragout_testbed" drive "$@"; }

# The drop target's centre.
tx=900
ty=190

staging_dirs() { find /tmp -maxdepth 1 -name 'slerp-drag-*' -user "$USER" 2>/dev/null | wc -l; }

scenario_drop() {
  echo "== drop: one row, dragged twice in one session"
  rm -rf /tmp/slerp-drag-*
  start_apps --exit-after 2
  drive "at:$row_x,$(row_y 0)" wait:300 press wait:100 "drag:$tx,$ty" wait:400 release wait:2500 \
    "at:$row_x,$(row_y 0)" wait:500 press wait:100 "drag:$tx,$ty" wait:400 release wait:2500
  sed 's/^/   receiver: /' "$RT/recv.log"
  grep 'drag\|material\|buttons' "$RT/demo.log" | sed 's/^/   demo: /'
  [ "$(grep -c '^drop$' "$RT/recv.log")" = 2 ] || fail "the receiver should have had two drops"
  grep -q 'file .*/demo picture.png size .* head 89504e47' "$RT/recv.log" || fail "the PNG did not arrive"
  [ "$(grep -c 'materialized demo picture.png' "$RT/demo.log")" = 2 ] || fail "each drag should write the picture once"
  [ "$(grep -c 'drag ended: Dropped' "$RT/demo.log")" = 2 ] || fail "both drags should end as dropped"
  [ "$(grep 'buttons down' "$RT/demo.log" | tail -1)" = "buttons down: false" ] || fail "egui still thinks a button is down"
  stop_apps
}

scenario_hover() {
  echo "== hover: all three rows, the target reads the list on entering as Dolphin does"
  rm -rf /tmp/slerp-drag-*
  start_apps --exit-after 1 --read-on-enter
  drive "at:$row_x,$(row_y 3)" wait:300 press wait:100 "drag:$tx,$ty" wait:600 release wait:4000
  sed 's/^/   receiver: /' "$RT/recv.log"
  grep 'drag\|material' "$RT/demo.log" | sed 's/^/   demo: /'
  grep -q 'read on enter' "$RT/recv.log" || fail "the receiver never read the list on entering"
  [ "$(grep -c 'materialized' "$RT/demo.log")" = 3 ] || fail "each of the three files is written once, though the list was read twice"
  grep -q 'notes #1 (50%).txt size 63' "$RT/recv.log" || fail "the text file with # and % in its name did not arrive intact"
  grep -q 'random 20 MB.bin size 20971520' "$RT/recv.log" || fail "the 20 MB file did not arrive"
  grep -q 'drag ended: Dropped' "$RT/demo.log" || fail "the drag should end as dropped"
  stop_apps
}

scenario_many() {
  echo "== many: 2000 files in one drag, a list longer than a pipe holds"
  rm -rf /tmp/slerp-drag-*
  start_apps --exit-after 1 --read-on-enter
  drive "at:$row_x,$(row_y 4)" wait:300 press wait:100 "drag:$tx,$ty" wait:600 release wait:4000
  grep 'uri-list has\|missing\|plain text' "$RT/recv.log" | sed 's/^/   receiver: /'
  grep 'drag\|material' "$RT/demo.log" | sed 's/^/   demo: /'
  [ "$(grep -c 'uri-list has 2000 entries' "$RT/recv.log")" = 2 ] || fail "both reads should have listed 2000 files"
  [ "$(grep -c ' 0 of the files are missing' "$RT/recv.log")" = 2 ] || fail "every file named should exist"
  grep -q 'plain text has 2000 lines' "$RT/recv.log" || fail "the plain text list should have 2000 lines"
  [ "$(grep -c 'materialized 2000' "$RT/demo.log")" = 1 ] || fail "the files are written once"
  grep -q 'drag ended: Dropped' "$RT/demo.log" || fail "the drag should end as dropped"
  stop_apps
}

scenario_cancel() {
  echo "== cancel: dropped on empty desktop"
  rm -rf /tmp/slerp-drag-*
  start_apps --seconds 20
  drive "at:$row_x,$(row_y 2)" wait:300 press wait:100 "drag:1100,600" wait:400 release wait:200
  wait_for "$RT/demo.log" "drag ended" 10
  drive "at:$row_x,$(row_y 2)" wait:300
  grep 'drag\|material\|buttons' "$RT/demo.log" | sed 's/^/   demo: /'
  grep -q 'drag ended: Cancelled' "$RT/demo.log" || fail "the drag should end as cancelled"
  ! grep -q materialized "$RT/demo.log" || fail "nothing should be written when no target asked"
  [ "$(staging_dirs)" = 0 ] || fail "a cancelled drag left a folder in /tmp"
  [ "$(grep 'buttons down' "$RT/demo.log" | tail -1)" = "buttons down: false" ] || fail "egui still thinks a button is down"
  stop_apps
}

scenario_reject() {
  echo "== reject: the target refuses the drop"
  rm -rf /tmp/slerp-drag-*
  start_apps --reject --seconds 20
  drive "at:$row_x,$(row_y 1)" wait:300 press wait:100 "drag:$tx,$ty" wait:400 release wait:100
  wait_for "$RT/demo.log" "drag ended" 10
  sed 's/^/   receiver: /' "$RT/recv.log"
  grep 'drag\|material' "$RT/demo.log" | sed 's/^/   demo: /'
  grep -q 'drag ended: Cancelled' "$RT/demo.log" || fail "a rejected drop should end as cancelled"
  [ "$(staging_dirs)" = 0 ] || fail "a rejected drag left a folder in /tmp"
  stop_apps
}

# The real viewer with an archive open: dragging an entry out of its Contents
# panel into the drop target.  VIEWER_ASSET is the archive; VIEWER_ROW the
# first entry's place "x,y" (see the picture the scenario saves) and
# VIEWER_NAME what should arrive (a part of its name).  With VIEWER_PREHOVER=1
# the pointer is already over the window when it opens.
scenario_viewer() {
  local asset=${VIEWER_ASSET:?give an archive in VIEWER_ASSET}
  rm -rf /tmp/slerp-drag-* "$RT/viewer.log" "$RT/recv.log"
  spawn "$RT/recv.log" "$BIN/dragout_testbed" receiver --exit-after 1 --read-on-enter
  wait_for "$RT/recv.log" "receiver ready"
  local row=${VIEWER_ROW:-120,182}
  if [ -n "${VIEWER_PREHOVER:-}" ]; then
    # The pointer is already over the place where the window will open, so it
    # enters the window before drag-out's own pointer exists.
    drive "at:$row" wait:40000 &
    pids+=($!)
    sleep 1
  fi
  spawn "$RT/viewer.log" ${VIEWER_ENV:-env} "$BIN/../slerp" view "$asset"
  sleep 12
  if [ "$compositor" = sway ]; then run "$BIN/dragout_testbed" shot "$RT/viewer.png"; fi
  # A small move first: a window that was placed after it opened (KWin does)
  # leaves the viewer with the pointer's old position until it moves.
  drive "at:$(( ${row%,*} + 3 )),${row#*,}" wait:200 "at:$row" wait:300 press wait:100 "drag:$tx,$ty" wait:600 release wait:4000
  sed 's/^/   receiver: /' "$RT/recv.log"
  { grep -i 'drag\|slerp' "$RT/viewer.log" || true; } | head -5 | sed 's/^/   viewer: /'
  grep -q "uri-list has" "$RT/recv.log" || fail "the receiver was never offered files"
  grep -q "${VIEWER_NAME:-THINGS.lmp}" "$RT/recv.log" || fail "no file named ${VIEWER_NAME:-THINGS.lmp} arrived"
  stop_apps
}

# A picture of the desktop in the middle of a drag, to see the drag icon
# (sway only: KWin has no wlr-screencopy).
scenario_icon() {
  [ "$compositor" = sway ] || fail "the icon scenario needs sway"
  echo "== icon: the screen while an item is held over the drop target"
  rm -rf /tmp/slerp-drag-*
  start_apps --seconds 30
  drive "at:$row_x,$(row_y 0)" wait:300 press wait:100 "drag:$tx,$ty" wait:300 "drag:$((tx + 40)),$((ty + 20))" wait:5000 release wait:500 &
  local driver=$!
  sleep 4
  run "$BIN/dragout_testbed" shot "$RT/drag.png"
  wait $driver
  grep 'drag ended' "$RT/demo.log" | sed 's/^/   demo: /'
  echo "   picture: $RT/drag.png"
  stop_apps
}

# A real Dolphin as the drop target: the first row (the PNG) is dragged into
# its window.  Dolphin asks what to do with files from another program in a
# menu (Copy Here, Link Here, ...), and the test clicks Copy Here.  The folder
# shown is DIR; DROP_POINTS="x,y x,y" tries several places, one drag each, and
# DOLPHIN_ROW picks the demo's row (0 to 4).
scenario_dolphin() {
  local dir=${DOLPHIN_DIR:?give the folder to show in Dolphin}
  mkdir -p "$dir"
  rm -rf /tmp/slerp-drag-*
  # Dolphin takes the receiver's place (the sway rules and place.js move it).
  spawn "$RT/demo.log" ${DEMO_ENV:-env} "$BIN/dragout_demo"
  wait_for "$RT/demo.log" "first row at"
  spawn "$RT/dolphin.log" env QT_QPA_PLATFORM=wayland dolphin --new-window "$dir"
  sleep 8
  if [ "$compositor" = sway ]; then run "$BIN/dragout_testbed" shot "$RT/dolphin-before.png"; fi
  # Dolphin's drop menu is a popup, which Qt can only open once the window
  # has received input: a click in its file view gives it that.
  drive "at:1055,210" wait:200 press wait:100 release wait:500
  # F9 hides Dolphin's Places panel, which would otherwise take much of the
  # small window and take drops itself.
  press_key F9 67
  sleep 1
  local before
  before=$(ls "$dir" | wc -l)
  for point in ${DROP_POINTS:-$tx,$((ty + 40))}; do
    echo "   drop at $point"
    drive "at:$row_x,$(row_y "${DOLPHIN_ROW:-0}")" wait:300 press wait:100 "drag:$point" wait:1500 release wait:2500
    if [ "$compositor" = sway ]; then run "$BIN/dragout_testbed" shot "$RT/dolphin-menu.png"; fi
    # The menu opens at the drop point, "Copy Here" first in it.
    local px=${point%,*} py=${point#*,}
    drive "at:$((px + 41)),$((py + 18))" wait:300 press wait:100 release wait:500
    sleep 2.5
  done
  grep 'drag\|material' "$RT/demo.log" | sed 's/^/   demo: /'
  ls -la "$dir"
  [ "$(ls "$dir" | wc -l)" -gt "$before" ] || fail "nothing arrived in $dir"
}

# Presses a key in the nested session: wtype in sway, fake input in KWin.
press_key() {  # wtype-name evdev-code
  if [ "$compositor" = kwin ]; then drive "key:$2"; else run wtype -k "$1"; fi
}

for s in drop hover many cancel reject; do
  if [ "$only" = all ] || [ "$only" = "$s" ]; then "scenario_$s"; fi
done
if [ "$only" = icon ]; then scenario_icon; fi
if [ "$only" = viewer ]; then scenario_viewer; fi
if [ "$only" = dolphin ]; then DOLPHIN_DIR=${3:-} scenario_dolphin; fi
echo "all scenarios passed"
