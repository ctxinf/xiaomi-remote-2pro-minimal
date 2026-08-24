#!/bin/sh
# Grants this user access to /dev/uinput, which the gateway needs to suppress
# the remote's mic button (otherwise it reaches the desktop as F5/F20 and can
# trigger things like the VS Code debugger).
#
# Run once:  sudo ./scripts/setup-uinput.sh
# Then log out and back in (or run: newgrp input) for the group to take effect.

set -eu

if [ "$(id -u)" -ne 0 ]; then
    echo "Please run with sudo: sudo $0" >&2
    exit 1
fi

TARGET_USER="${SUDO_USER:-$(logname 2>/dev/null || echo root)}"

echo "==> Loading the uinput kernel module"
modprobe uinput
echo uinput > /etc/modules-load.d/uinput.conf

echo "==> Installing udev rule for /dev/uinput (group 'input', mode 0660)"
echo 'KERNEL=="uinput", GROUP="input", MODE="0660", OPTIONS+="static_node=uinput"' \
    > /etc/udev/rules.d/99-uinput.rules

echo "==> Reloading udev rules"
udevadm control --reload-rules
udevadm trigger --name-match=uinput || true

echo "==> Adding user '$TARGET_USER' to the 'input' group"
usermod -aG input "$TARGET_USER"

echo
echo "Done. Current permissions:"
ls -l /dev/uinput
echo
echo "Log out and back in (or run 'newgrp input') so the group membership applies."
