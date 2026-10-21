#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Builds dist/dawkey-<version>-1-<arch>.pkg.tar.zst on Arch Linux from this
# checkout (packaging/arch/PKGBUILD), installs it and checks that it runs.
# Run as root from the repository root, in a container
# (.github/workflows/release.yml); makepkg itself runs as a build user.

source packaging/lib.sh
check_tag

pacman -Syu --noconfirm --needed \
    base-devel clang lilv libxkbcommon pipewire qt5-base qt6-base rust vulkan-icd-loader \
    wayland

id builder &>/dev/null || useradd --create-home builder
mkdir -p dist
chown -R builder: .

su builder -c "cd packaging/arch && \
    DAWKEY_SOURCE='$PWD' DAWKEY_VERSION='$(dawkey_version)' \
    PKGDEST='$PWD/dist' BUILDDIR=/tmp/makepkg \
    makepkg --nodeps --noconfirm --force"

pacman -U --noconfirm dist/dawkey-*.pkg.tar.zst
dawkey --version
