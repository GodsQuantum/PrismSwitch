#!/bin/sh
# SPDX-License-Identifier: GPL-2.0-only
set -eu
[ "$(id -u)" = 0 ] || { echo "Run sudo ./uninstall.sh"; exit 1; }
if [ -x /usr/local/bin/prismswitch ]; then
    /usr/local/bin/prismswitch restore
    echo "Leaving the current display input unchanged."
fi
if [ -f /etc/keyd/prismswitch.conf ]; then
    rm /etc/keyd/prismswitch.conf
    if systemctl is-active --quiet keyd.service; then keyd reload; fi
fi
rm -f /usr/local/bin/prismswitch /usr/local/libexec/prismswitch/SmcDumpKey /usr/local/share/icons/hicolor/scalable/apps/prismswitch.svg
rm -f /usr/local/share/prismswitch/BUILDINFO /usr/local/share/prismswitch/SHA256SUMS
echo "Removed program and its bindings; saved configuration and system packages preserved."
