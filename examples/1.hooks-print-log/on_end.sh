#!/bin/sh
# Example "end" hook: fires when a voice recording finished with a valid audio file.
#
# The gateway passes the event two ways:
#   - JSON on stdin: {"event":"end","audio_file":"...","duration_sec":1.83,
#                     "samples":29280,"sample_rate":16000}
#   - environment:   XIAOMI_EVENT, XIAOMI_AUDIO_FILE, XIAOMI_DURATION_SEC,
#                    XIAOMI_SAMPLES, XIAOMI_SAMPLE_RATE
#
# The gateway never deletes the file — this script owns it. Move, upload, or
# remove it here, otherwise it stays in the temp directory.
#
# Usage:
#   chmod +x examples/1.hooks-print-log/on_end.sh
#   xiaomi-remote-pro2-gateway --hook end=./examples/1.hooks-print-log/on_end.sh

set -eu

payload=$(cat)

echo "[hook] event=${XIAOMI_EVENT} file=${XIAOMI_AUDIO_FILE} duration=${XIAOMI_DURATION_SEC}s"
echo "[hook] payload: ${payload}"

# Example: hand the recording to a transcriber, then discard it.
#   whisper "${XIAOMI_AUDIO_FILE}" --model base --output_dir ~/voice-notes
#   rm -f "${XIAOMI_AUDIO_FILE}"

# Example: keep the take instead of letting it pile up in the temp directory.
#   mkdir -p ~/voice-notes
#   mv "${XIAOMI_AUDIO_FILE}" ~/voice-notes/
