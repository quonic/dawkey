#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Builds dist/dawkey_<version>-1~<codename>_<arch>.deb on Debian or Ubuntu,
# installs it and checks that it runs. Run as root from the repository root,
# in a container (.github/workflows/release.yml).

source packaging/lib.sh
check_tag
. /etc/os-release

export DEBIAN_FRONTEND=noninteractive
apt-get update -q
apt-get install -y -q --no-install-recommends \
    build-essential ca-certificates clang curl dpkg-dev libclang-dev \
    liblilv-dev libpipewire-0.3-dev libwayland-dev libxkbcommon-dev pkg-config \
    qt6-base-dev qtbase5-dev

install_rustup
cargo install --locked cargo-deb
build_release

mkdir -p dist
cargo deb --no-build --no-strip --deb-revision "1~$VERSION_CODENAME" --output dist/

apt-get install -y -q ./dist/dawkey_*~"$VERSION_CODENAME"_*.deb
dawkey --version
