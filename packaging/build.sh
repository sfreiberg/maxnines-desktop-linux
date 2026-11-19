#!/bin/sh
# Builds the release binary on Rocky Linux 9, run in its container:
#
#   docker run --rm -v "$PWD:/work" -w /work rockylinux:9 sh packaging/build.sh
#
# Rocky 9 has the oldest C library of the systems the app is for, and GTK and
# libadwaita new enough, so what's built here runs on all of them: Ubuntu 22.04,
# Debian 12, Fedora, RHEL 9 and anything newer.
set -eu
# Some of the -devel packages are in CodeReady Builder.
dnf install -y -q dnf-plugins-core
dnf config-manager --set-enabled crb
dnf install -y -q --setopt=install_weak_deps=False gcc gtk4-devel libadwaita-devel pkgconf-pkg-config
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
. "$HOME/.cargo/env"
cargo build --release --locked -p maxnines-desktop
# For the packaging that follows, outside the container.
chmod -R a+rwX target
target/release/maxnines-desktop --version
