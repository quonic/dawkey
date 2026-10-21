#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Packs dist/dawkey-<version>-<arch>.tar.zst from an already built release
# binary (build-deb.sh on Debian builds it, against an older glibc than
# most distributions have). Unpacked into a prefix (e.g. ~/.local) with
# --strip-components=1, it puts dawkey in bin/ and its desktop file and icon
# in share/.

source packaging/lib.sh

command -v zstd >/dev/null || apt-get install -y -q --no-install-recommends zstd

name="dawkey-$(dawkey_version)-$(uname -m)"
stage=$(mktemp -d)
root="$stage/$name"
install -Dm755 target/release/dawkey "$root/bin/dawkey"
install -Dm644 packaging/dawkey.desktop "$root/share/applications/dawkey.desktop"
install -Dm644 packaging/dawkey.svg "$root/share/icons/hicolor/scalable/apps/dawkey.svg"
install -Dm644 README.md "$root/share/doc/dawkey/README.md"
install -Dm644 LICENSE "$root/share/licenses/dawkey/LICENSE"

mkdir -p dist
tar --create --zstd --owner=0 --group=0 --file "dist/$name.tar.zst" -C "$stage" "$name"
rm -rf "$stage"
tar --list --zstd --file "dist/$name.tar.zst"
