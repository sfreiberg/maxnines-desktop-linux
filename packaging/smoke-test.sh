#!/bin/sh
# Starts the app as a desktop would, under a virtual display and its own session
# bus, and checks that it comes up, stays up, and that starting it again hands over
# to the copy that's running rather than opening a second. Then, with it stopped,
# presses a notification's button from that run, as the desktop does through D-Bus,
# and checks that this starts it, and that it opens its window and stays.
#
#   packaging/smoke-test.sh [path to maxnines-desktop]
#
# Needs xvfb-run, dbus-run-session and gdbus.
set -eu
program=$(realpath "${1:-target/release/maxnines-desktop}")
home=$(mktemp -d)
export HOME="$home" XDG_CONFIG_HOME="$home/.config" XDG_DATA_HOME="$home/.local/share"

"$program" --version

# What D-Bus starts it with, as the packages install, with a virtual display of its own.
mkdir -p "$XDG_DATA_HOME/dbus-1/services"
printf '[D-BUS Service]\nName=com.maxnines.Desktop\nExec=%s -a %s --gapplication-service\n' \
  "$(command -v xvfb-run)" "$program" > "$XDG_DATA_HOME/dbus-1/services/com.maxnines.Desktop.service"

dbus-run-session -- sh -eu -c '
program=$1
owned() {
  gdbus call --session --dest org.freedesktop.DBus --object-path /org/freedesktop/DBus \
    --method org.freedesktop.DBus.NameHasOwner com.maxnines.Desktop | grep -q true
}
xvfb-run -a "$program" > "$HOME/app.log" 2>&1 &
started=$!
waited=0
until owned; do
  if ! kill -0 "$started" 2>/dev/null || [ "$waited" -ge 30 ]; then
    echo "It never came up:"; cat "$HOME/app.log"; exit 1
  fi
  sleep 1; waited=$((waited + 1))
done
echo "ok: it came up and took its name on the session bus"
sleep 3
kill -0 "$started" || { echo "It stopped:"; cat "$HOME/app.log"; exit 1; }
echo "ok: it stayed up"
# Started again, it shows the copy that is running and leaves at once.
timeout 20 xvfb-run -a "$program" || { echo "Starting it again did not hand over"; exit 1; }
kill -0 "$started" || { echo "Starting it again stopped the first"; exit 1; }
echo "ok: starting it again hands over to the one running"
kill "$started"
waited=0
while owned; do
  [ "$waited" -lt 20 ] || { echo "It did not stop"; exit 1; }
  sleep 1; waited=$((waited + 1))
done
echo "--- what it said ---"; cat "$HOME/app.log"

# Open in MaxNines, pressed on a notification from the run that just ended.
gdbus call --session --timeout 60 --dest com.maxnines.Desktop --object-path /com/maxnines/Desktop \
  --method org.freedesktop.Application.ActivateAction open "[<(uint64 1, \"/alerts\")>]" "{}" \
  || { echo "D-Bus could not start it to press the button"; exit 1; }
owned || { echo "Pressing the button did not start it"; exit 1; }
pid=$(gdbus call --session --dest org.freedesktop.DBus --object-path /org/freedesktop/DBus \
  --method org.freedesktop.DBus.GetConnectionUnixProcessID com.maxnines.Desktop | sed "s/.* \([0-9]*\),.*/\1/")
# Started for D-Bus with nothing open, it would quit within seconds.
sleep 15
owned || { echo "Started by the button, it did not stay: it opened no window"; exit 1; }
echo "ok: a notification pressed with the app not running starts it, with its window open"
kill "$pid"
' sh "$program"
