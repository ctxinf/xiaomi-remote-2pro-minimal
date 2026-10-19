#!/bin/sh
set -eu

# 可以替换成你自己的命令；网关通过环境变量传入录音信息。
notify-send "Xiaomi Remote" "录音结束：${XIAOMI_DURATION_SEC} 秒"
