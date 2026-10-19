#!/usr/bin/env bash
# 安装/更新用户级 systemd 服务；不管理 hooks 凭据，不应用 keyd 配置。
set -euo pipefail
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
exec python3 "$here/service-manage.py" "$@"
