#!/bin/sh
# Builds toolswitch_VERSION_amd64.deb from an already built binary.
# Usage: packaging/linux/build-deb.sh path/to/toolswitch VERSION
set -eu

binary=$1
version=$2
root=$(mktemp -d)
trap 'rm -rf "$root"' EXIT
chmod 755 "$root"

install -Dm755 "$binary" "$root/usr/bin/toolswitch"
install -Dm644 assets/icon.png "$root/usr/share/icons/hicolor/256x256/apps/toolswitch.png"
install -Dm644 LICENSE "$root/usr/share/doc/toolswitch/copyright"
install -Dm644 /dev/stdin "$root/usr/share/applications/toolswitch.desktop" <<EOF
[Desktop Entry]
Name=toolswitch
Comment=Small everyday tools in one tiny app
Exec=/usr/bin/toolswitch
Icon=toolswitch
Terminal=false
Type=Application
Categories=Utility;
StartupWMClass=toolswitch
EOF

# The window, OpenGL and keyboard libraries are opened at run time, so dpkg-shlibdeps can't
# see them: they are listed by hand.
install -Dm644 /dev/stdin "$root/DEBIAN/control" <<EOF
Package: toolswitch
Version: $version
Architecture: amd64
Maintainer: berchj <berchj@users.noreply.github.com>
Installed-Size: $(du -sk "$root/usr" | cut -f1)
Depends: libc6 (>= 2.35), libgl1, libegl1, libx11-6, libx11-xcb1, libxcursor1, libxrandr2, libxi6, libxkbcommon0, libxkbcommon-x11-0, curl
Recommends: libnotify-bin, xdg-desktop-portal
Section: utils
Priority: optional
Homepage: https://berchj.github.io
Description: Small everyday tools in one tiny app
 Clipboard history, wallpapers, text from screen, reminders, breaks, file compressor,
 background remover, PDF signing, tidy downloads, backup, clean up, PC monitor,
 shut down timer and updates. Everything runs offline, no account.
EOF

dpkg-deb --root-owner-group --build "$root" "toolswitch_${version}_amd64.deb"
