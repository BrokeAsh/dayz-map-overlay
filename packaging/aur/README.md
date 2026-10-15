# AUR package

`dayz-map-overlay-bin` installs the Linux release's `dayz-map` as `/usr/bin/dayz-map`. (The
[DayZ Server Browser](https://github.com/BrokeAsh/dayz-server-browser) package has its own copy of
the overlay, so the two don't overlap.)

For each release, once it's published on GitHub:

```sh
packaging/aur/update.sh 0.3.1     # version, checksums, .SRCINFO
```

Commit the result here (the README's Arch Linux steps build from it), then copy `dayz-map-overlay-bin/PKGBUILD` and `.SRCINFO` into a clone of
`ssh://aur@aur.archlinux.org/dayz-map-overlay-bin.git`, commit, and push.
