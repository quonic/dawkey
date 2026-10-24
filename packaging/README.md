# Packaging

Dawkey's packages are built with GitHub Actions, each in a container of the distribution it's for, and attached to a release on the repository's [Releases page](https://github.com/quonic/dawkey/releases). The scripts here do the work, so they also run locally (as root in a container, from the repository root):

| Script | Runs on | Makes |
|---|---|---|
| `build-deb.sh` | Debian 13, Ubuntu 26.04 | `dist/dawkey_<version>-1~<codename>_<arch>.deb` |
| `build-rpm.sh` | Fedora 44 | `dist/dawkey-<version>-1.fc44.<arch>.rpm` |
| `build-arch.sh` | Arch Linux | `dist/dawkey-<version>-1-<arch>.pkg.tar.zst` (`arch/PKGBUILD`) |
| `build-tarball.sh` | after `build-deb.sh` on Debian 13 | `dist/dawkey-<version>-<arch>.tar.zst` |

Each build script installs the package it made and runs `dawkey --version`. The `.deb` and `.rpm` metadata is in the root `Cargo.toml` (`[package.metadata.deb]`, `[package.metadata.generate-rpm]`).

For example, to build the Fedora package with Podman:

```sh
podman run --rm -v "$PWD:/src:Z" -w /src registry.fedoraproject.org/fedora:44 packaging/build-rpm.sh
```

## Releasing

Releases go through release-plz (`.github/workflows/release-plz.yml`, configured in `release-plz.toml`). After each push to `main` it opens or updates a pull request labeled `release`. That pull request bumps each crate that changed since its version on crates.io, and each crate that depends on one that did; a library whose API broke gets a minor bump. It also adds the changes to `CHANGELOG.md`. Merging it publishes those crates to crates.io and tags dawkey's new version `v<version>` with a GitHub release. The release workflow (`.github/workflows/release.yml`) then builds every package and attaches them, with a `SHA256SUMS` file, to that release.

A tag `v<version>` pushed by hand works too: the release workflow builds it, and makes the release, with notes generated from the merged pull requests, if there isn't one yet. If the tag doesn't match dawkey's version in `Cargo.toml`, the builds fail and nothing is released.

Running the workflow by hand (Actions → Release → Run workflow) only builds the packages and keeps them as the run's artifacts, which is a way to try a packaging change before tagging.

## Installing a release

Download the package for your distribution from the [Releases page](https://github.com/quonic/dawkey/releases) and install it with your package manager, which also installs what it depends on. Packages from the Releases page don't update themselves: install the next release the same way.

### Fedora 44

```sh
sudo dnf install ./dawkey-<version>-1.fc44.x86_64.rpm
```

### Debian 13 and Ubuntu 26.04

Take the `.deb` built for your release (its name ends in `~<codename>_amd64.deb`):

```sh
sudo apt install ./dawkey_<version>-1~<codename>_amd64.deb
```

### Arch Linux

```sh
sudo pacman -U dawkey-<version>-1-x86_64.pkg.tar.zst
```

### Other distributions

Unpack `dawkey-<version>-x86_64.tar.zst` into a prefix, for example `~/.local`:

```sh
tar -xf dawkey-<version>-x86_64.tar.zst -C ~/.local --strip-components=1
```

It needs what's listed under [Runtime requirements](../README.md#runtime-requirements), including the PipeWire, lilv and libxkbcommon libraries.
