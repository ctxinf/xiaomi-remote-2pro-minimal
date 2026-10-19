#!/bin/sh
# "end" hook: voice -> real ASR (Alibaba Bailian / DashScope) -> clipboard + notification.
#
# This is 2.typed's stub with the stub removed: the audio file is base64-encoded
# and posted to an OpenAI-compatible /chat/completions endpoint as an
# `input_audio` part, which is the shape the qwen3-asr-* models expect.
# Ported from esp32-eve/backend/examples/asr/asr_base64.js.
#
# Credentials come from the `.env` sitting next to this script. The gateway runs
# a hook with `current_dir` set to the script's own directory (see src/hooks.rs),
# so `./.env` resolves here no matter where the gateway itself was started from.
#
# Setup:
#   cp .env.example .env      # then fill in your key
#   chmod +x ./asr_clipboard.sh
#   xiaomi-remote-pro2-gateway --hook-config ./examples/hooks/3.asr/hooks.jsonc
#
# Requires: curl, base64 (coreutils), wl-clipboard. python3 is used for JSON
# decoding when available, with a sed fallback if it is not.
#
# On why this stops at the clipboard instead of typing for you, see the long
# note in ../2.typed/asr_clipboard.sh — short version: Wayland has no
# SendInput() equivalent, and every workaround fights global state.

set -eu

# Logs go to stderr: stdout is relayed into the gateway log at INFO, and keeping
# the two apart makes a failed transcription easy to spot.
log() { echo "$*" >&2; }

notify() {
    command -v notify-send >/dev/null 2>&1 || return 0
    notify-send --app-name="Xiaomi Remote" --icon=microphone-sensitivity-high \
        --expire-time=4000 "$1" "$2" >/dev/null 2>&1 || true
}

die() {
    log "[error] $1"
    notify "${2:-识别失败}" "$1"
    exit 1
}

# --- config -----------------------------------------------------------------
# Load .env without echoing it. Only KEY=VALUE lines are honoured; comments and
# blanks are skipped. Values are exported as-is, so quotes in the file are
# stripped the way a shell would strip them.
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
env_file="${XIAOMI_ASR_ENV_FILE:-$script_dir/.env}"

if [ -f "$env_file" ]; then
    # Snapshot the vars this script cares about before sourcing, so anything
    # already set in the environment can be restored afterwards: an explicit
    # `ASR_MODEL=... gateway ...` must beat the value sitting in .env.
    _pre_key=${ASR_API_KEY:-}; _pre_alibaba=${ALIBABA_API_KEY:-}
    _pre_url=${ASR_BASE_URL:-}; _pre_model=${ASR_MODEL:-}
    _pre_lang=${ASR_LANGUAGE:-}; _pre_timeout=${ASR_TIMEOUT_SEC:-}

    # `set -a` exports every assignment so the values reach curl's environment.
    # Nothing here is ever echoed -- the file's contents stay unread.
    set -a
    # shellcheck disable=SC1090  # path is intentionally dynamic
    . "$env_file"
    set +a

    [ -n "$_pre_key" ] && ASR_API_KEY=$_pre_key
    [ -n "$_pre_alibaba" ] && ALIBABA_API_KEY=$_pre_alibaba
    [ -n "$_pre_url" ] && ASR_BASE_URL=$_pre_url
    [ -n "$_pre_model" ] && ASR_MODEL=$_pre_model
    [ -n "$_pre_lang" ] && ASR_LANGUAGE=$_pre_lang
    [ -n "$_pre_timeout" ] && ASR_TIMEOUT_SEC=$_pre_timeout
    unset _pre_key _pre_alibaba _pre_url _pre_model _pre_lang _pre_timeout

    # grep -c exits nonzero when it counts zero matches, so guard the count or
    # `set -e`-adjacent command substitution would swallow the rest of the line.
    _key_count=$(grep -cE '^[A-Za-z_][A-Za-z0-9_]*=' "$env_file" 2>/dev/null) || _key_count=0
    log "[env] loaded $_key_count keys from $(basename "$env_file")"
    unset _key_count
else
    log "[env] no .env next to the script; relying on the inherited environment"
fi

# Fall back across the key names the reference backend uses. Note these are
# `:-` (not `-`) on purpose: in the sample .env, ASR_API_KEY/ASR_BASE_URL/
# ASR_MODEL exist but are *empty*, and an empty value must not win.
api_key="${ASR_API_KEY:-${DASHSCOPE_API_KEY:-${ALIBABA_API_KEY:-}}}"
base_url="${ASR_BASE_URL:-${ALIBABA_BASE_URL:-https://dashscope.aliyuncs.com/compatible-mode/v1}}"
model="${ASR_MODEL:-qwen3-asr-flash}"

# The API wants a bare code ("zh", "en"); the reference backend's .env ships the
# BCP-47 form ("zh-CN"), which the model rejects outright with HTTP 400. Trim the
# region subtag so either spelling works.
language="${ASR_LANGUAGE:-}"
case "$language" in
    *-*) language=${language%%-*} ;;
esac

[ -n "$api_key" ] || die "缺少 API Key：在 $env_file 里设置 ASR_API_KEY 或 ALIBABA_API_KEY" "配置缺失"

for tool in curl base64; do
    command -v "$tool" >/dev/null 2>&1 || die "缺少命令：$tool" "依赖缺失"
done

# --- input ------------------------------------------------------------------
audio="${XIAOMI_AUDIO_FILE:-}"
[ -n "$audio" ] && [ -f "$audio" ] || die "no audio file in XIAOMI_AUDIO_FILE" "没有音频"

# The API rejects oversized payloads, and base64 inflates by ~4/3.
max_bytes="${MAX_AUDIO_BYTES:-8388608}"
size=$(wc -c < "$audio" | tr -d ' ')
[ "$size" -le "$max_bytes" ] || die "音频过大：${size} 字节 > ${max_bytes}" "音频过大"

log "[asr] audio=$audio duration=${XIAOMI_DURATION_SEC:-?}s size=${size}B model=$model${language:+ lang=$language}"

# --- request ----------------------------------------------------------------
work_dir=$(mktemp -d) || die "mktemp failed"
# Clean up the temp dir on every exit path, including the die() calls below.
trap 'rm -rf "$work_dir"' EXIT INT TERM

# -w0 keeps the base64 on a single line; BSD/macOS base64 has no -w, so fall
# back to stripping newlines by hand.
if ! base64 -w0 "$audio" > "$work_dir/audio.b64" 2>/dev/null; then
    base64 "$audio" | tr -d '\n' > "$work_dir/audio.b64"
fi

# Build the request body with a here-doc rather than string interpolation so the
# base64 blob (which can be megabytes) never becomes a shell argument.
{
    printf '{"model":"%s","stream":false,"asr_options":{"enable_itn":false' "$model"
    [ -n "$language" ] && printf ',"language":"%s"' "$language"
    printf '},"messages":[{"role":"user","content":[{"type":"input_audio","input_audio":{"data":"data:audio/wav;base64,'
    cat "$work_dir/audio.b64"
    printf '"}}]}]}'
} > "$work_dir/body.json"

# --data-binary @file (not --data) so curl does not mangle the payload, and
# @- would still have to pass through the shell.
http_code=$(curl -sS -o "$work_dir/response.json" -w '%{http_code}' \
    --max-time "${ASR_TIMEOUT_SEC:-60}" \
    -X POST "$base_url/chat/completions" \
    -H "Authorization: Bearer $api_key" \
    -H 'Content-Type: application/json' \
    --data-binary "@$work_dir/body.json" 2>"$work_dir/curl.err") || {
        die "请求失败：$(tr -d '\n' < "$work_dir/curl.err" | cut -c1-200)" "网络错误"
    }

if [ "$http_code" != "200" ]; then
    # The error body may quote the key back; log only the API's message field.
    detail=$(sed -n 's/.*"message"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$work_dir/response.json" | head -1)
    die "HTTP $http_code${detail:+: $detail}" "识别失败"
fi

# --- parse ------------------------------------------------------------------
# python3 handles the JSON escaping (你 etc.) correctly; the sed path is a
# best-effort fallback for minimal systems.
if command -v python3 >/dev/null 2>&1; then
    text=$(python3 -c '
import json, sys
try:
    d = json.load(open(sys.argv[1]))
    c = d["choices"][0]["message"]["content"]
    # Some ASR responses return content as a list of typed parts.
    if isinstance(c, list):
        c = "".join(p.get("text", "") for p in c if isinstance(p, dict))
    sys.stdout.write((c or "").strip())
except Exception as e:
    print(f"parse error: {e}", file=sys.stderr)
    sys.exit(1)
' "$work_dir/response.json") || die "无法解析返回结果" "解析失败"
else
    text=$(sed -n 's/.*"content"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$work_dir/response.json" | head -1)
fi

if [ -z "$text" ]; then
    log "[asr] empty transcript"
    notify "识别为空" "没有识别到文字"
    exit 0
fi
log "[asr] text: $text"

# --- clipboard --------------------------------------------------------------
command -v wl-copy >/dev/null 2>&1 || die "wl-copy 未安装（sudo dnf install wl-clipboard）" "复制失败"

# wl-copy daemonizes to serve the selection and holds the pipe open, so detach
# it or this script would never return.
printf '%s' "$text" | wl-copy >/dev/null 2>&1 &
sleep 0.2

# ${#text} counts bytes in POSIX sh, which reads as nonsense for CJK (22 chars
# would report 66), so count characters when a tool for it is around.
char_count=$(printf '%s' "$text" | wc -m | tr -d ' ')
log "[clip] copied ${char_count:-${#text}} chars -- press Ctrl+V to paste"
notify "已复制到剪贴板，按 Ctrl+V 粘贴" "$text"

# The recording is ours to dispose of once transcribed.
[ "${XIAOMI_ASR_KEEP_AUDIO:-0}" = "1" ] || rm -f "$audio"
