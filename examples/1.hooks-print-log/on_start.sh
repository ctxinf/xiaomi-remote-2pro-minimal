#!/bin/sh
# Example "start" hook: fires the moment the mic button opens a voice stream.
#
# No audio file exists yet ("audio_file" is null), so this is the place for
# feedback — a beep, a notification, waking a transcription service.
#
# Usage:
#   chmod +x examples/1.hooks-print-log/on_start.sh
#   xiaomi-remote-pro2-gateway --hook start=./examples/1.hooks-print-log/on_start.sh

set -eu

echo "[hook] recording started (sample_rate=${XIAOMI_SAMPLE_RATE} Hz)"

# Example: desktop notification while the user is speaking.
#   notify-send "Xiaomi Remote" "Listening..."
