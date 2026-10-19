#!/usr/bin/env bash
# Entry point for the real-use systemd deployment.
#
#   ./install-service.sh                     # build + install/update + start
#   ./install-service.sh secret add KEY VAL  # store a credential (encrypted)
#   ./install-service.sh doctor|status|logs|restart|uninstall
#
# The logic lives in gatewayctl.py; this shim only picks an interpreter and
# fails early with a readable message if it cannot. Python 3.11+ is required
# and uses only the standard library.

set -euo pipefail

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)

find_python() {
    local candidate
    for candidate in python3 python3.14 python3.13 python3.12 python3.11 python; do
        command -v "$candidate" >/dev/null 2>&1 || continue
        # Ask the interpreter itself rather than parsing `--version` output.
        if "$candidate" -c 'import sys; sys.exit(0 if sys.version_info >= (3, 11) else 1)' 2>/dev/null; then
            printf '%s' "$candidate"
            return 0
        fi
    done
    return 1
}

if ! python=$(find_python); then
    echo "error: need Python 3.11+ ; none found on PATH." >&2
    echo "       Fedora: sudo dnf install python3" >&2
    exit 1
fi

exec "$python" "$here/gatewayctl.py" "$@"
