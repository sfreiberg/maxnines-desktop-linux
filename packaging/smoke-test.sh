#!/bin/sh
# Starts the app as a desktop would, under a virtual display and its own session
# bus, and checks that it comes up, stays up, and that starting it again hands over
# to the copy that's running rather than opening a second.
#
#   packaging/smoke-test.sh [path to maxnines-desktop]
#
# Needs xvfb-run, dbus-run-session and gdbus.
set -eu
program=$(realpath "${1:-target/release/maxnines-desktop}")
home=$(mktemp -d)
export HOME="$home" XDG_CONFIG_HOME="$home/.config" XDG_DATA_HOME="$home/.local/share"

"$program" --version

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
echo "--- what it said ---"; cat "$HOME/app.log"
' sh "$program"
