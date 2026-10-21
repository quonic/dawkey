#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Builds dist/dawkey-<version>-1.fc<N>.<arch>.rpm on Fedora, installs it and
# checks that it runs. Run as root from the repository root, in a container
# (.github/workflows/release.yml).

source packaging/lib.sh
check_tag

# rpm-build provides find-requires, which turns the binary's links into
# Requires (auto-req in Cargo.toml).
dnf install -y -q --setopt=install_weak_deps=False \
    cargo clang-devel gcc gcc-c++ libxkbcommon-devel lilv-devel pipewire-devel \
    pkgconf-pkg-config qt5-qtbase-devel qt6-qtbase-devel rpm-build rust wayland-devel

export PATH="$HOME/.cargo/bin:$PATH"
cargo install --locked cargo-generate-rpm
build_release

mkdir -p dist
cargo generate-rpm --set-metadata "release = \"1$(rpm -E %dist)\"" --output dist/

dnf install -y -q ./dist/dawkey-*.rpm
dawkey --version
