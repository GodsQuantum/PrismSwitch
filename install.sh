#!/bin/sh
# SPDX-License-Identifier: GPL-2.0-only
set -eu
cd "$(dirname "$0")"
if [ "$#" -eq 0 ]; then
    if [ "$(id -u)" -eq 0 ]; then
        [ -n "${SUDO_USER:-}" ] && [ "$SUDO_USER" != root ] || { echo "Build as a regular user or use --prebuilt." >&2; exit 1; }
        runuser -u "$SUDO_USER" -- ./build.sh
        exec "$0" --prebuilt
    fi
    ./build.sh
    exec sudo "$0" --prebuilt
fi
[ "$#" -eq 1 ] && [ "$1" = --prebuilt ] || { echo "Usage: ./install.sh [--prebuilt]" >&2; exit 2; }
[ "$(id -u)" -eq 0 ] || exec sudo "$0" --prebuilt
for tool in install sha256sum systemctl loginctl runuser timeout; do
    command -v "$tool" >/dev/null || { echo "Missing dependency: $tool" >&2; exit 1; }
done
[ ! -e /run/prismswitch/eco.json ] || { echo "Restore active PrismSwitch profile first." >&2; exit 1; }
(cd target/release && sha256sum -c SHA256SUMS)
target/release/prismswitch validate
# Install only built outputs and the default configuration. No switching,
# package manager, keyboard configuration or user services are changed.
install -D -o root -g root -m 755 target/release/SmcDumpKey /usr/local/libexec/prismswitch/SmcDumpKey.new
mv /usr/local/libexec/prismswitch/SmcDumpKey.new /usr/local/libexec/prismswitch/SmcDumpKey
install -D -o root -g root -m 755 target/release/prismswitch /usr/local/bin/prismswitch.new
mv /usr/local/bin/prismswitch.new /usr/local/bin/prismswitch
install -D -o root -g root -m 644 branding/logo-mark.svg /usr/local/share/icons/hicolor/scalable/apps/prismswitch.svg
install -D -o root -g root -m 644 target/release/BUILDINFO /usr/local/share/prismswitch/BUILDINFO
install -D -o root -g root -m 644 target/release/SHA256SUMS /usr/local/share/prismswitch/SHA256SUMS
if [ ! -f /etc/prismswitch/config.json ]; then
    install -D -o root -g root -m 644 config.example.json /etc/prismswitch/config.json
fi
/usr/local/bin/prismswitch validate
/usr/local/bin/prismswitch --version
echo "Installed. Run sudo prismswitch configure for customizable hotkeys."
