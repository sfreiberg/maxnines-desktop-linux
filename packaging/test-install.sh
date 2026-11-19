#!/bin/sh
# Installs the package for this system from packages/, runs what it installed, and
# removes it again, in a container of each system it's meant for:
#
#   docker run --rm -v "$PWD:/work" -w /work debian:12 sh packaging/test-install.sh 0.1.0
set -eu
version=$1
if command -v apt-get >/dev/null; then
  export DEBIAN_FRONTEND=noninteractive
  apt-get update -qq
  apt-get install -y -qq desktop-file-utils >/dev/null
  apt-get install -y -qq ./packages/maxnines-desktop_*.deb >/dev/null
  remove() { apt-get remove -y -qq maxnines-desktop >/dev/null; }
else
  dnf install -y -q desktop-file-utils
  dnf install -y -q ./packages/maxnines-desktop-*.rpm
  remove() { dnf remove -y -q maxnines-desktop; }
fi

said=$(maxnines-desktop --version)
[ "$said" = "maxnines-desktop $version" ] || { echo "It said: $said"; exit 1; }
echo "ok: it runs, with the libraries it needs installed"
desktop-file-validate /usr/share/applications/com.maxnines.Desktop.desktop
test -f /usr/share/icons/hicolor/scalable/apps/com.maxnines.Desktop.svg
echo "ok: its menu entry and icon are in place"

remove
if command -v maxnines-desktop >/dev/null; then echo "Removing it left it behind"; exit 1; fi
echo "ok: removing it takes it away"
