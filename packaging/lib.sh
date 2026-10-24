# SPDX-License-Identifier: AGPL-3.0-or-later
# shellcheck shell=bash
# Shared by the build-*.sh scripts, which source it from the repository root.

set -euo pipefail

# The dawkey package's version in the root Cargo.toml (the first `version =`
# line).
dawkey_version() {
    sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1
}

# On a tag push, the tag has to name the version being packaged.
check_tag() {
    local version
    version=$(dawkey_version)
    if [[ "${GITHUB_REF_TYPE:-}" == tag && "${GITHUB_REF_NAME:-}" != "v$version" ]]; then
        echo "tag ${GITHUB_REF_NAME} doesn't match Cargo.toml's version $version (expected v$version)" >&2
        exit 1
    fi
}

# Rust stable through rustup, for distributions whose rustc is older than
# Dawkey's rust-version.
install_rustup() {
    if [[ ! -x "$HOME/.cargo/bin/cargo" ]]; then
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs |
            sh -s -- -y --profile minimal --no-modify-path
    fi
    export PATH="$HOME/.cargo/bin:$PATH"
}

# The release binary, stripped (packages don't ship debug symbols).
build_release() {
    CARGO_PROFILE_RELEASE_STRIP=symbols cargo build --release --locked
}
