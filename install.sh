#!/bin/sh
# SPDX-License-Identifier: GPL-2.0-only
set -eu
[ "$(id -u)" = 0 ] || { echo "Run sudo ./install.sh"; exit 1; }
cd "$(dirname "$0")"
for tool in gcc python3 install systemctl loginctl runuser; do
    command -v "$tool" >/dev/null || { echo "Missing dependency: $tool"; exit 1; }
done
# Prevent accidental migration over an active profile.
[ ! -e /run/prismswitch/eco.json ] || { echo "Restore active profile first"; exit 1; }
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT HUP INT TERM
gcc -O2 -Wall -o "$tmp/SmcDumpKey" SmcDumpKey.c
PYTHONPYCACHEPREFIX="$tmp/cache" python3 -m py_compile prismswitch/prismswitch.py
install -D -o root -g root -m 755 "$tmp/SmcDumpKey" /usr/local/libexec/prismswitch/SmcDumpKey
install -D -o root -g root -m 755 prismswitch/prismswitch.py /usr/local/bin/prismswitch
install -D -o root -g root -m 644 branding/logo-mark.svg /usr/local/share/icons/hicolor/scalable/apps/prismswitch.svg
if [ ! -f /etc/prismswitch/config.json ]; then
    install -D -o root -g root -m 644 config.example.json /etc/prismswitch/config.json
fi
/usr/local/bin/prismswitch validate
echo "Installed. Run sudo prismswitch configure for customizable hotkeys."
