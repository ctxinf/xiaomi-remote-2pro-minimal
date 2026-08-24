#!/bin/sh
# "end" hook: voice -> ASR -> put the transcript on the clipboard + notify.
#
# The ASR step is stubbed (see transcribe() below): it emits a fixed sentence so
# the rest of the chain can be tested without a model. Swap in whisper/vosk/an
# API call when ready.
#
# ---------------------------------------------------------------------------
# 为什么是"复制到剪贴板"，而不是直接替你输入？
# Why the clipboard, instead of typing into the focused window?
#
# 简短版：Fedora + GNOME/Wayland 上没有 Windows SendInput() 的等价物。
# Short answer: on Fedora + GNOME/Wayland there is no equivalent of the Windows
# SendInput() API, which lets any process send Unicode text to the focused
# window. Wayland deliberately omits this -- if any app could type into any
# other app's window, that is a keylogger/injection vector. The isolation is
# the point, not an oversight.
#
# 以下每条都在本机实测过（Fedora 43, GNOME Shell 49.6, Wayland）：
# Every option below was measured on this machine (Fedora 43, GNOME 49.6):
#
#   1. wtype  ->  "Compositor does not support the virtual keyboard protocol"
#      GNOME 的 Mutter 不实现 virtual-keyboard-v1 协议，wtype 直接失败。
#      (It works on wlroots compositors like Sway, but not on GNOME.)
#
#   2. XDG RemoteDesktop portal  ->  只有 NotifyKeyboardKeycode / Keysym
#      即使是官方 portal，也只能发送*键码*，没有"发送一段文本"的方法；
#      而且每次使用都会弹出权限授权对话框。
#      Even the official portal only forwards keycodes, never text, and it
#      prompts for permission on every session.
#
#   3. GNOME Shell Eval D-Bus  ->  返回 false
#      GNOME 41+ 出于安全考虑已永久禁用脚本注入。Disabled since GNOME 41.
#
#   4. AT-SPI 无障碍接口  ->  toolkit-accessibility=false，且它用于*读取*
#      界面内容，不是注入输入。Reads the UI; it is not an input API.
#
#   5. /dev/uinput 虚拟键盘  ->  唯一能用的，但不稳定：
#      The only one that works, but it is at the mercy of ambient global state.
#      实测：本机运行着 fcitx5。中文模式下注入 "nihao"，内核层确实发出了
#      键码 [49,23,35,30,24]，但 fcitx5 把它们截获成拼音候选词，
#      输入框里得到的并不是 "nihao"。
#      Measured: with fcitx5 in Chinese mode, injecting "nihao" emits the right
#      keycodes at the kernel level, but the IME turns them into a pinyin
#      candidate list -- the text never arrives.
#      同类问题还有：中文等非 ASCII 无法用键码表达（只能退回剪贴板粘贴）、
#      用户按住 Shift/Ctrl 时全部注入键被污染、终端里 Ctrl+V 语义不同。
#
# 结论：模拟键盘这条路要靠不断打补丁去对抗全局状态，每个补丁都是新的失败点。
# 停在剪贴板反而简单稳定：不碰输入法、不碰焦点、不碰修饰键状态，
# 唯一代价是你多按一次 Ctrl+V。
# Conclusion: stopping at the clipboard touches no global state at all. The
# only cost is one keypress, and it always does what you expect.
# ---------------------------------------------------------------------------
#
# For a version that calls a real ASR API instead of the stub, see ../3.asr/.
#
# Usage:
#   xiaomi-remote-pro2-gateway --hook end=./examples/hooks/2.typed/asr_clipboard.sh
#
# Hold the mic button, speak, release, then press Ctrl+V wherever you want it.

set -eu

# Logs go to stderr: stdout of transcribe() is captured as the transcript, so
# anything logged there would end up inside the text.
log() { echo "$*" >&2; }

# --- ASR stage (stubbed) ----------------------------------------------------
# Replace the body with a real transcriber, e.g.:
#   whisper "$1" --model base --output_format txt --output_dir /tmp >/dev/null 2>&1 \
#       && cat "/tmp/$(basename "${1%.wav}").txt"
transcribe() {
    audio_file=$1
    if [ -n "${XIAOMI_FAKE_TEXT:-}" ]; then
        printf '%s' "$XIAOMI_FAKE_TEXT"
        return
    fi
    log "[asr] (stub) would transcribe: $audio_file"
    printf 'hello from the xiaomi remote, %s' "$(date +%H:%M:%S)"
}

notify() {
    command -v notify-send >/dev/null 2>&1 || return 0
    notify-send --app-name="Xiaomi Remote" --icon=microphone-sensitivity-high \
        --expire-time=4000 "$1" "$2" >/dev/null 2>&1 || true
}

# --- main -------------------------------------------------------------------
audio="${XIAOMI_AUDIO_FILE:-}"
if [ -z "$audio" ] || [ ! -f "$audio" ]; then
    log "[error] no audio file in XIAOMI_AUDIO_FILE"
    exit 1
fi

log "[asr] audio=$audio duration=${XIAOMI_DURATION_SEC:-?}s"

text=$(transcribe "$audio")
if [ -z "$text" ]; then
    log "[asr] empty transcript, nothing to copy"
    notify "识别为空" "没有识别到文字"
    exit 0
fi
log "[asr] text: $text"

if ! command -v wl-copy >/dev/null 2>&1; then
    log "[error] wl-copy not found (sudo dnf install wl-clipboard)"
    notify "复制失败" "缺少 wl-clipboard"
    exit 1
fi

# wl-copy daemonizes to serve the selection and holds the pipe open, so detach
# it or this script would never return.
printf '%s' "$text" | wl-copy >/dev/null 2>&1 &
sleep 0.2

log "[clip] copied ${#text} chars to clipboard -- press Ctrl+V to paste"
notify "已复制到剪贴板，按 Ctrl+V 粘贴" "$text"

# The recording is ours to dispose of once transcribed.
# rm -f "$audio"
