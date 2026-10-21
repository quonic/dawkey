#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Uploads packages to Forgejo's package registry and links them to the
# repository: publish.sh FILE...
# Each file goes where its name says: a .deb to the Debian distribution after
# the `~` in its revision (component main), an .rpm to the group fedora/<N>
# (from .fc<N>), a .pkg.tar.zst to the Arch group "dawkey", and a .tar.zst
# to the generic package "dawkey".
# Needs FORGEJO_SERVER (e.g. https://git.example.com), FORGEJO_OWNER,
# FORGEJO_REPO and PACKAGES_TOKEN (a token that can write packages).
# Forgejo refuses a version that's already there (409); delete it first.

set -euo pipefail

: "${FORGEJO_SERVER:?}" "${FORGEJO_OWNER:?}" "${FORGEJO_REPO:?}" "${PACKAGES_TOKEN:?}"
api="${FORGEJO_SERVER%/}/api/packages/$FORGEJO_OWNER"

upload() {
    echo "uploading $(basename "$1") to $2"
    curl --silent --show-error --fail-with-body \
        --user "$FORGEJO_OWNER:$PACKAGES_TOKEN" \
        --upload-file "$1" "$2"
    echo
}

declare -A types=()
for file in "$@"; do
    name=$(basename "$file")
    case "$name" in
    *.deb)
        codename=$(sed -n 's/.*~\([a-z]*\)_[^_]*\.deb$/\1/p' <<<"$name")
        [[ -n "$codename" ]] || { echo "no distribution in $name" >&2; exit 1; }
        upload "$file" "$api/debian/pool/$codename/main/upload"
        types[debian]=1
        ;;
    *.rpm)
        fedora=$(sed -n 's/.*\.fc\([0-9]*\)\.[^.]*\.rpm$/\1/p' <<<"$name")
        [[ -n "$fedora" ]] || { echo "no Fedora release in $name" >&2; exit 1; }
        upload "$file" "$api/rpm/fedora/$fedora/upload"
        types[rpm]=1
        ;;
    *.pkg.tar.zst)
        upload "$file" "$api/arch/dawkey"
        types[arch]=1
        ;;
    *.tar.zst)
        version=$(sed -n 's/^dawkey-\(.*\)-[^-]*\.tar\.zst$/\1/p' <<<"$name")
        [[ -n "$version" ]] || { echo "no version in $name" >&2; exit 1; }
        upload "$file" "$api/generic/dawkey/$version/$name"
        types[generic]=1
        ;;
    *)
        echo "don't know where $name goes" >&2
        exit 1
        ;;
    esac
done

# Show the packages on the repository's page. Older Forgejo versions lack
# this endpoint, and linking twice fails; neither stops the upload.
for type in "${!types[@]}"; do
    curl --silent --show-error --fail-with-body -X POST \
        --user "$FORGEJO_OWNER:$PACKAGES_TOKEN" \
        "${FORGEJO_SERVER%/}/api/v1/packages/$FORGEJO_OWNER/$type/dawkey/-/link/$FORGEJO_REPO" ||
        echo "couldn't link the $type package to $FORGEJO_REPO (already linked?)"
    echo
done
