#!/bin/sh
# Points the AUR package at a release: sets its version, downloads the release to fill in the
# checksums, and regenerates .SRCINFO. Run after the release is published, then copy PKGBUILD,
# .SRCINFO (and any other files here) into the AUR repository and push.
#   packaging/aur/update.sh 0.3.1
set -eu
version=${1:?usage: update.sh <version, such as 0.3.1>}
cd "$(dirname "$0")/dayz-map-overlay-bin"
sed -i -e "s/^pkgver=.*/pkgver=$version/" -e "s/^pkgrel=.*/pkgrel=1/" PKGBUILD
src=$(mktemp -d)
trap 'rm -rf "$src"' EXIT
SRCDEST=$src updpkgsums
makepkg --printsrcinfo > .SRCINFO
echo "Updated dayz-map-overlay-bin to $version"
